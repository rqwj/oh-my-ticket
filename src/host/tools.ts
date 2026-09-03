/**
 * Model-facing OMT tools: thin wrappers over the OMT runtime service
 * (OmtService → omt-daemon RPC) registered through ctx.tools. Names,
 * descriptions, parameter/output schemas, and renderer output are identical
 * to the pre-daemon tool layer (R13). Canonical values stay programmatic;
 * output.render produces the model-facing Chinese text. OmtError throws
 * surface to the model as isError results, which is the intended channel
 * for hierarchy violations and unknown ids.
 */
import type { Context } from '@deepseek-ai/cordis'
import { defineTool, type ToolRunContext } from '@deepseek-ai/dsh-tools'
import type { HomeRef, ClaimContextValue, OmtService } from './service.ts'
import { endsExecution, lineageOfHeader, type RunningRegistry } from './running.ts'
import {
  isRunItemStalled,
  NODE_TYPES,
  RUN_ITEM_STATES,
  RUN_REPORT_OUTCOMES,
  RUN_STATUSES,
  STATUSES,
  type OmtNode,
  type OmtRun,
  type OmtRunItem,
  type ReindexResult,
  type RunReportOutcome,
} from './types.ts'

const NODE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    id: { type: 'string', required: true },
    type: { type: 'string', enum: NODE_TYPES, required: true },
    title: { type: 'string', required: true },
    status: { type: 'string', enum: STATUSES, required: true },
    archived: { type: 'boolean', required: true },
    priority: { type: 'integer', required: true },
    path: { type: 'string', required: true },
    created_at: { type: 'string', required: true },
    updated_at: { type: 'string', required: true },
  },
} as const

interface NodeValue {
  id: string
  type: OmtNode['type']
  title: string
  status: OmtNode['status']
  archived: boolean
  priority: number
  path: string
  created_at: string
  updated_at: string
}

function nodeValue(node: OmtNode): NodeValue {
  return {
    id: node.id,
    type: node.type,
    title: node.title,
    status: node.status,
    archived: node.archived,
    priority: node.priority,
    path: node.path,
    created_at: node.created_at,
    updated_at: node.updated_at,
  }
}

function renderNodeLine(node: NodeValue): string {
  return `- ${node.id} [${node.type} · ${node.status}] ${node.title}（${node.path}）`
}

const CLAIM_CONTEXT_NODE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    node: NODE_SCHEMA,
    body: { type: 'string', required: true },
    truncated: { type: 'boolean', required: true },
    original_bytes: { type: 'integer', required: true },
    included_bytes: { type: 'integer', required: true },
  },
} as const

const CLAIM_CONTEXT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    ancestor_budget_bytes: { type: 'integer', required: true },
    ancestor_used_bytes: { type: 'integer', required: true },
    truncated: { type: 'boolean', required: true },
    ancestors: { type: 'array', items: CLAIM_CONTEXT_NODE_SCHEMA, required: true },
    read_errors: {
      type: 'array',
      required: true,
      items: {
        type: 'object',
        additionalProperties: false,
        properties: {
          node: NODE_SCHEMA,
          error: { type: 'string', required: true },
        },
      },
    },
    current: {
      type: 'object',
      additionalProperties: false,
      required: true,
      properties: {
        node: NODE_SCHEMA,
        body: { type: 'string', required: true },
      },
    },
  },
} as const

function renderContextNode(entry: {
  node: NodeValue
  body: string
  truncated: boolean
  original_bytes: number
  included_bytes: number
}): string {
  const body = entry.body === '' ? '（空）' : entry.body
  const truncation = entry.truncated
    ? `\n\n[上下文已截断：原始 ${entry.original_bytes} 字节，本次保留 ${entry.included_bytes} 字节]`
    : ''
  return `### ${entry.node.id} [${entry.node.type}] ${entry.node.title}\n\n${body}${truncation}`
}

function renderClaimContext(context: ClaimContextValue): string {
  const ancestors = context.ancestors.length === 0
    ? '（无可用祖先背景）'
    : context.ancestors.map(renderContextNode).join('\n\n')
  const readErrors = context.read_errors.length === 0
    ? []
    : [
        '',
        '### 部分祖先上下文读取失败',
        '',
        ...context.read_errors.map(entry => `- ${entry.node.id} ${entry.node.title}：${entry.error}`),
      ]
  const currentBody = context.current.body === '' ? '（空）' : context.current.body
  return [
    '## 背景（只读，不可执行）',
    '',
    ancestors,
    ...readErrors,
    '',
    '## 当前执行项（唯一可执行、可报告）',
    '',
    `### ${context.current.node.id} [${context.current.node.type}] ${context.current.node.title}`,
    '',
    currentBody,
  ].join('\n')
}

// ── run tool values (EPIC-0003 / STORY-0011) ─────────────────────────────

const RUN_CONFIG_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    stopOnFailure: { type: 'boolean', required: true },
    autoContinue: { type: 'boolean', required: true },
    autoVerify: { type: 'boolean', required: true },
    concurrency: { type: 'integer', required: true },
  },
} as const

const RUN_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    id: { type: 'string', required: true },
    title: { type: 'string' },
    status: { type: 'string', enum: RUN_STATUSES, required: true },
    config: RUN_CONFIG_SCHEMA,
    created_at: { type: 'string', required: true },
    finished_at: { type: 'string' },
  },
} as const

const RUN_ITEM_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    run_id: { type: 'string', required: true },
    node_id: { type: 'string', required: true },
    position: { type: 'integer', required: true },
    state: { type: 'string', enum: RUN_ITEM_STATES, required: true },
    executor_session_id: { type: 'string' },
    attempts: { type: 'integer', required: true },
    last_error: { type: 'string' },
    /** Derived (TICKET-0062): pending item whose 续跑 nudge budget is exhausted — needs human attention. */
    stalled: { type: 'boolean' },
    started_at: { type: 'string' },
    finished_at: { type: 'string' },
    /** Joined node title (show/claim only). */
    title: { type: 'string' },
  },
} as const

interface RunValue {
  id: string
  title?: string
  status: OmtRun['status']
  config: OmtRun['config']
  created_at: string
  finished_at?: string
}

interface RunItemValue {
  run_id: string
  node_id: string
  position: number
  state: OmtRunItem['state']
  executor_session_id?: string
  attempts: number
  last_error?: string
  /** Derived stalled marker (pending + nudge budget exhausted, TICKET-0062). */
  stalled?: boolean
  started_at?: string
  finished_at?: string
  title?: string
}

function runValue(run: OmtRun): RunValue {
  return {
    id: run.id,
    ...(run.title !== undefined ? { title: run.title } : {}),
    status: run.status,
    config: run.config,
    created_at: run.created_at,
    ...(run.finished_at !== undefined ? { finished_at: run.finished_at } : {}),
  }
}

function runItemValue(item: OmtRunItem, title?: string): RunItemValue {
  const joined = title ?? item.title
  return {
    run_id: item.run_id,
    node_id: item.node_id,
    position: item.position,
    state: item.state,
    ...(item.executor_session_id !== undefined ? { executor_session_id: item.executor_session_id } : {}),
    attempts: item.attempts,
    ...(item.last_error !== undefined ? { last_error: item.last_error } : {}),
    ...(isRunItemStalled(item) ? { stalled: true } : {}),
    ...(item.started_at !== undefined ? { started_at: item.started_at } : {}),
    ...(item.finished_at !== undefined ? { finished_at: item.finished_at } : {}),
    ...(joined !== undefined ? { title: joined } : {}),
  }
}

function renderRunLine(run: RunValue): string {
  return `- ${run.id} [${run.status}]${run.title !== undefined ? ` ${run.title}` : ''}（创建 ${run.created_at}）`
}

function renderItemLine(item: RunItemValue): string {
  const parts = [`- ${item.node_id} [${item.state}]`]
  if (item.title !== undefined) parts.push(` ${item.title}`)
  if (item.executor_session_id !== undefined) parts.push(`（执行者 ${item.executor_session_id}）`)
  if (item.attempts > 0) parts.push(`（第 ${item.attempts + 1} 次尝试）`)
  if (item.last_error !== undefined) parts.push(`（上次失败：${item.last_error}）`)
  if (item.stalled === true) parts.push('（停滞：续跑 nudge 预算已耗尽，请人工介入或 omt_run_control retry 重置）')
  return parts.join('')
}

/** Render a show result (body arrives children-stripped from the daemon view). */
function renderShow(result: { node: NodeValue; parent?: NodeValue; children: NodeValue[]; body: string }): string {
  const { node, parent, children, body } = result
  const header = [
    `# ${node.id} ${node.title}`,
    ``,
    `- 类型: ${node.type}`,
    `- 状态: ${node.status}`,
    `- 优先级: ${node.priority}`,
    `- 文件: ${node.path}`,
    parent !== undefined ? `- 父节点: ${parent.id} ${parent.title}` : `- 父节点: （根节点）`,
    `- 创建: ${node.created_at}`,
    `- 更新: ${node.updated_at}`,
  ].join('\n')
  const userBody = body
  const childLines = children.length > 0
    ? children.map(child => `- ${child.id} [${child.type} · ${child.status}] ${child.title}`).join('\n')
    : '（无子节点）'
  return `${header}\n\n## 正文\n\n${userBody === '' ? '（空）' : userBody}\n\n## 子节点\n\n${childLines}`
}

/** Structural ctx.userQuestions face (UI-backed ask service). */
interface UserQuestionsLike {
  ask(request: {
    questions: {
      id: string
      question: string
      header?: string
      options?: { label: string; description?: string }[]
    }[]
    agent?: unknown
    signal?: AbortSignal
  }): Promise<{ answers: { id: string; selected: string[] }[] }>
}

const WORKSPACE_LABEL = '当前工作区（随项目存储）'
const GLOBAL_LABEL = '全局（所有项目共享）'

/**
 * Resolve the create target home. Children always land in their parent's
 * home; root epics honor an explicit scope or require a completed UI-backed
 * choice. They never guess a storage location when the choice is unavailable.
 */
async function homeForCreate(
  service: OmtService,
  userQuestions: UserQuestionsLike | undefined,
  exec: ToolRunContext,
  args: { type: string; title: string; parentId?: string; scope?: 'workspace' | 'global' },
): Promise<HomeRef> {
  const cwd = exec.agent?.session.header.cwd
  if (args.parentId !== undefined) return (await service.resolveNodeHome(args.parentId, cwd)).home
  if (args.type !== 'epic') return service.homeFor(cwd)
  if (args.scope !== undefined) return service.homeForScope(cwd, args.scope)
  if (cwd === undefined || userQuestions === undefined) {
    throw new Error('Epic scope selection is required; pass scope=workspace|global or use a live session with an answerer')
  }
  let answer: Awaited<ReturnType<UserQuestionsLike['ask']>>
  try {
    answer = await userQuestions.ask({
      questions: [{
        id: 'scope',
        header: '创建位置',
        question: `Epic「${args.title}」创建到哪里？`,
        options: [
          { label: WORKSPACE_LABEL, description: `${cwd}/.omt（ticket 随项目走，可进 git）` },
          { label: GLOBAL_LABEL, description: `${service.globalHome().path ?? service.globalHome().homeId}（所有工作区共享）` },
        ],
      }],
      agent: exec.agent,
      signal: exec.signal,
    })
  } catch (error) {
    throw new Error('Epic scope selection was not completed; pass scope=workspace|global to retry', { cause: error })
  }
  const selected = answer.answers.find(item => item.id === 'scope')?.selected[0]
  if (selected === WORKSPACE_LABEL) return service.homeForScope(cwd, 'workspace')
  if (selected === GLOBAL_LABEL) return service.homeForScope(cwd, 'global')
  throw new Error('Epic scope selection was not completed; pass scope=workspace|global to retry')
}

/** Register all omt_* tools; executes route through the runtime service. */
export function registerOmtTools(
  ctx: Context,
  service: OmtService,
  touch?: (sessionId: string | undefined, id: string) => void,
  changed?: (homeId: string) => void,
  running?: RunningRegistry,
): void {
  const userQuestions = (ctx as unknown as { userQuestions?: UserQuestionsLike }).userQuestions
  const sessionOf = (exec: ToolRunContext): string | undefined => exec.agent?.session.header.id
  const labelOf = (exec: ToolRunContext): string => {
    const cwd = exec.agent?.session.header.cwd
    const base = cwd === undefined ? undefined : cwd.split('/').filter(Boolean).pop()
    return base !== undefined ? `${base} 的会话` : '未知会话'
  }
  /** Status transitions drive the running marker (model-flow executions). */
  const trackRunning = (exec: ToolRunContext, id: string, status?: string, archived?: boolean): void => {
    if (running === undefined) return
    // Executor lineage snapshot (TICKET-0066) from the session header.
    if (status === 'in_progress') running.start(id, sessionOf(exec) ?? '', labelOf(exec), lineageOfHeader(exec.agent?.session.header))
    // done/blocked/skipped/archive all end active execution: clear the mark.
    if (endsExecution(status, archived)) running.stop(id)
  }
  ctx.tools.register(defineTool({
    name: 'omt_create',
    description:
      '创建一个 OMT ticket 节点（Epic/Story/SubStory/Ticket/SubTicket）。层级规则：'
      + 'epic→story，story→substory|ticket，substory→ticket，ticket→subticket；'
      + '只有 epic 可以没有父节点。子节点总是创建在父节点所在的 home；'
      + '创建 epic 时可用 scope 指定归属（workspace/global），未指定时会询问用户。'
      + '返回新节点的 id 与文件路径。',
    parameters: {
      type: { type: 'string', enum: NODE_TYPES, required: true, description: '节点类型' },
      title: { type: 'string', required: true, description: '节点标题' },
      parentId: { type: 'string', description: '父节点 id（epic 可省略，其余必填）' },
      body: { type: 'string', description: '正文 Markdown（省略时使用默认模板）' },
      priority: { type: 'integer', description: '优先级，默认 0' },
      scope: { type: 'string', enum: ['workspace', 'global'], description: '仅对创建 epic 有效：创建到当前工作区（.omt/）还是全局 home' },
    },
    output: {
      schema: NODE_SCHEMA,
      render: (_args, value: NodeValue) => [{ type: 'text', text: `已创建节点：\n${renderNodeLine(value)}` }],
    },
    async execute(args, exec) {
      const home = await homeForCreate(service, userQuestions, exec, args)
      const created = await service.createNode(home, {
        type: args.type,
        title: args.title,
        parentId: args.parentId,
        body: args.body,
        priority: args.priority,
      })
      touch?.(sessionOf(exec), created.id)
      changed?.(home.homeId)
      return nodeValue(created)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_list',
    description:
      '列出 OMT ticket 节点。可按类型/状态过滤，或用 query 做标题与正文的关键词搜索；'
      + '不带参数时返回全部节点。',
    parameters: {
      type: { type: 'string', enum: NODE_TYPES, description: '按类型过滤' },
      status: { type: 'string', enum: STATUSES, description: '按状态过滤' },
      query: { type: 'string', description: '标题/正文关键词搜索（多个词以空格分隔，需全部命中）' },
    },
    output: {
      schema: { type: 'array', items: NODE_SCHEMA },
      render: (_args, value: NodeValue[]) => [{
        type: 'text',
        text: value.length === 0 ? '没有匹配的节点。' : `共 ${value.length} 个节点：\n${value.map(renderNodeLine).join('\n')}`,
      }],
    },
    async execute(args, exec) {
      const home = await service.homeFor(exec.agent?.session.header.cwd)
      const nodes = await service.listNodes(home, { type: args.type, status: args.status, query: args.query })
      return nodes.map(nodeValue)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_show',
    description: '查看一个 OMT 节点的详情：元信息、正文、父节点与子节点清单。',
    parameters: {
      id: { type: 'string', required: true, description: '节点 id，如 TICKET-0001' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          node: NODE_SCHEMA,
          parent: NODE_SCHEMA,
          children: { type: 'array', items: NODE_SCHEMA },
          body: { type: 'string' },
        },
      },
      render: (_args, value: { node: NodeValue; parent?: NodeValue; children: NodeValue[]; body: string }) => [{
        type: 'text',
        text: renderShow(value),
      }],
    },
    async execute(args, exec) {
      touch?.(sessionOf(exec), args.id)
      const result = await service.showNode(args.id, exec.agent?.session.header.cwd)
      return {
        node: nodeValue(result.node),
        ...(result.parent !== undefined ? { parent: nodeValue(result.parent) } : {}),
        children: result.children.map(nodeValue),
        body: result.body,
      }
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_update',
    description:
      '更新一个 OMT 节点：标题、状态（open/in_progress/done/blocked/skipped/archived）、优先级、'
      + '替换正文（body）或追加进度记录（append）。至少需要提供一项变更。'
      + '开始处理某节点时将状态置为 in_progress，完成时置为 done 并 append 结论。',
    parameters: {
      id: { type: 'string', required: true, description: '节点 id' },
      title: { type: 'string', description: '新标题' },
      status: { type: 'string', enum: STATUSES, description: '新状态（open/in_progress/done/blocked/skipped；已归档节点不可改状态，先恢复）' },
      archived: { type: 'boolean', description: 'true 归档 / false 恢复；归档是独立于状态的封存维度，归档后节点只读' },
      priority: { type: 'integer', description: '新优先级' },
      body: { type: 'string', description: '替换整个正文（与 append 互斥，body 优先）' },
      append: { type: 'string', description: '向正文末尾追加一段内容（进度记录）' },
    },
    output: {
      schema: NODE_SCHEMA,
      render: (_args, value: NodeValue) => [{ type: 'text', text: `已更新节点：\n${renderNodeLine(value)}` }],
    },
    async execute(args, exec) {
      if ([args.title, args.status, args.archived, args.priority, args.body, args.append].every(v => v === undefined)) {
        throw new Error('omt_update 至少需要一项变更（title/status/archived/priority/body/append）')
      }
      const { node, home } = await service.updateNode({
        id: args.id,
        title: args.title,
        status: args.status,
        archived: args.archived,
        priority: args.priority,
        body: args.body,
        append: args.append,
      }, { cwd: exec.agent?.session.header.cwd, sessionId: sessionOf(exec) })
      touch?.(sessionOf(exec), args.id)
      trackRunning(exec, args.id, args.status, args.archived)
      changed?.(home.homeId)
      return nodeValue(node)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_move',
    description: '把一个 OMT 节点（连同其子树）移动到新的父节点下，层级规则与 omt_create 相同。',
    parameters: {
      id: { type: 'string', required: true, description: '要移动的节点 id' },
      newParentId: { type: 'string', required: true, description: '新父节点 id' },
    },
    output: {
      schema: NODE_SCHEMA,
      render: (_args, value: NodeValue) => [{ type: 'text', text: `已移动节点：\n${renderNodeLine(value)}` }],
    },
    async execute(args, exec) {
      const moved = await service.moveNode(args.id, args.newParentId, exec.agent?.session.header.cwd)
      touch?.(sessionOf(exec), args.id)
      changed?.(moved.home.homeId)
      return nodeValue(moved.node)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_reindex',
    description:
      '从磁盘上的 Markdown 文件重建 OMT 的 SQLite 索引。'
      + '当 ticket 文件被手工编辑（或索引与文件不一致）时使用。',
    parameters: {},
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          nodes: { type: 'integer' },
          edges: { type: 'integer' },
          skipped: { type: 'integer' },
        },
      },
      render: (_args, value: ReindexResult) => [{
        type: 'text',
        text: `重建完成：${value.nodes} 个节点、${value.edges} 条关系` + (value.skipped > 0 ? `，跳过 ${value.skipped} 项（非法或无法解析）` : ''),
      }],
    },
    async execute(_args, exec) {
      const home = await service.homeFor(exec.agent?.session.header.cwd)
      const result = await service.reindex(home)
      changed?.(home.homeId)
      return result
    },
  }))

  // ── run tools (EPIC-0003 / STORY-0011) ─────────────────────────────────
  // Runs live in exactly one home (single-home membership rule). create
  // routes by member ownership; id-addressed tools resolve the owning home
  // via service.resolveRunHome (run ids count per home, so the caller's
  // workspace context disambiguates, exactly like node ids).

  ctx.tools.register(defineTool({
    name: 'omt_run_create',
    description:
      '创建一个 run：Ticket/SubTicket 的有序执行批次（可跨 Story/Epic 挑选，创建时快照写入成员）。'
      + 'Epic/Story/SubStory 仅作为背景，不能成为 run 成员。所有成员必须同属一个 OMT home，重复成员会被拒绝。返回 run 与成员清单。',
    parameters: {
      title: { type: 'string', description: '可选标题（多 run 选择时展示）' },
      nodeIds: { type: 'array', items: { type: 'string' }, required: true, description: '成员 Ticket/SubTicket id（按执行顺序）' },
      config: {
        type: 'object',
        additionalProperties: false,
        description: '覆盖默认配置（缺省键取默认值）',
        properties: {
          stopOnFailure: { type: 'boolean', description: 'item failed 时 run 自动暂停（默认 false）' },
          autoContinue: { type: 'boolean', description: '允许 idle 续跑提醒（默认 true）' },
          autoVerify: { type: 'boolean', description: '信任策略（TICKET-0064）：false 时执行者会话未经 omt_run_report 直接落 done 的项进入 awaiting_confirmation 待确认；true 直接落 done（默认 false）' },
          concurrency: { type: 'integer', description: '并发执行上限（P3 预留，默认 1）' },
        },
      },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          run: RUN_SCHEMA,
          items: { type: 'array', items: RUN_ITEM_SCHEMA },
        },
      },
      render: (_args, value: { run: RunValue; items: RunItemValue[] }) => [{
        type: 'text',
        text: `已创建 run：\n${renderRunLine(value.run)}\n成员 ${value.items.length} 项：\n${value.items.map(renderItemLine).join('\n')}`,
      }],
    },
    async execute(args, exec) {
      const cwd = exec.agent?.session.header.cwd
      // Single-home rule (decision 2): every member must resolve to the
      // same owning home — resolve by ownership, not by caller cwd.
      let home: HomeRef | undefined
      for (const rootId of args.nodeIds) {
        const owner = (await service.resolveNodeHome(rootId, cwd)).home
        home ??= owner
        if (owner.homeId !== home.homeId) {
          throw new Error(`omt_run_create 的成员必须同属一个 OMT home（${rootId} 属于 ${owner.homeId}，与 ${home.homeId} 不同）`)
        }
      }
      home ??= await service.homeFor(cwd)
      const created = await service.createRun(home, { title: args.title, config: args.config, nodeIds: args.nodeIds })
      changed?.(home.homeId)
      return { run: runValue(created.run), items: created.items.map(item => runItemValue(item)) }
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_run_list',
    description: '列出当前工作区的 run，可按状态过滤；每个 run 附带成员状态统计（进度）。',
    parameters: {
      status: { type: 'string', enum: RUN_STATUSES, description: '按 run 状态过滤' },
    },
    output: {
      schema: {
        type: 'array',
        items: {
          type: 'object',
          additionalProperties: false,
          properties: {
            run: RUN_SCHEMA,
            progress: {
              type: 'object',
              additionalProperties: false,
              properties: Object.fromEntries([
                ['total', { type: 'integer', required: true }],
                ...RUN_ITEM_STATES.map(state => [state, { type: 'integer', required: true }]),
              ]),
            },
          },
        },
      },
      render: (_args, value: { run: RunValue; progress: Record<string, number> }[]) => [{
        type: 'text',
        text: value.length === 0
          ? '没有匹配的 run。'
          : `共 ${value.length} 个 run：\n${value.map(entry => `${renderRunLine(entry.run)} — ${entry.progress.done}/${entry.progress.total} 完成`).join('\n')}`,
      }],
    },
    async execute(args, exec) {
      const home = await service.homeFor(exec.agent?.session.header.cwd)
      const summaries = await service.listRunSummaries(home, args.status)
      return summaries.map(({ run, progress }) => ({ run: runValue(run), progress }))
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_run_show',
    description: '查看一个 run 的详情：状态、配置与成员清单（状态/执行者/attempts/last_error）。按 id 跨 home 解析。',
    parameters: {
      id: { type: 'string', required: true, description: 'run id，如 RUN-0001' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          run: RUN_SCHEMA,
          items: { type: 'array', items: RUN_ITEM_SCHEMA },
        },
      },
      render: (_args, value: { run: RunValue; items: RunItemValue[] }) => [{
        type: 'text',
        text: `# ${value.run.id}${value.run.title !== undefined ? ` ${value.run.title}` : ''} [${value.run.status}]\n\n${value.items.map(renderItemLine).join('\n')}`,
      }],
    },
    async execute(args, exec) {
      const home = await service.resolveRunHome(args.id, exec.agent?.session.header.cwd)
      // fetchRun throws NOT_FOUND for unknown runs (consistent error path);
      // item titles arrive joined from the daemon detail view.
      const snapshot = await service.fetchRun(home, args.id)
      return {
        run: runValue(snapshot.run),
        items: snapshot.items.map(item => runItemValue(item)),
      }
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_run_control',
    description:
      '控制一个 run：start（pending→running）/ pause（停止派发，进行中的项继续观察）/ resume（paused 或 interrupted 续跑）/'
      + 'cancel（冻结，不碰 ticket）/ retry（nodeId 指定的 failed/interrupted/停滞项就地重置回 pending）/'
      + 'remove（nodeId 指定移除成员，不改动对应 ticket 节点状态）。按 id 跨 home 解析。',
    parameters: {
      id: { type: 'string', required: true, description: 'run id' },
      action: { type: 'string', enum: ['start', 'pause', 'resume', 'cancel', 'retry', 'remove'], required: true, description: '控制动作' },
      nodeId: { type: 'string', description: 'retry/remove 的目标成员节点 id' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          run: RUN_SCHEMA,
          item: RUN_ITEM_SCHEMA,
        },
      },
      render: (args, value: { run: RunValue; item?: RunItemValue }) => [{
        type: 'text',
        text: `run ${value.run.id} 已执行 ${String((args as { action: string }).action)}：${renderRunLine(value.run)}`
          + (value.item !== undefined ? `\n${renderItemLine(value.item)}` : ''),
      }],
    },
    async execute(args, exec) {
      const home = await service.resolveRunHome(args.id, exec.agent?.session.header.cwd)
      if ((args.action === 'retry' || args.action === 'remove') && args.nodeId === undefined) {
        throw new Error(`omt_run_control ${String(args.action)} 需要 nodeId（指定要${args.action === 'retry' ? '重试' : '移除'}的成员）`)
      }
      if (args.action !== 'start' && args.action !== 'pause' && args.action !== 'resume' && args.action !== 'cancel' && args.action !== 'retry' && args.action !== 'remove') {
        throw new Error(`omt_run_control 不支持的动作: ${String(args.action)}（start/pause/resume/cancel/retry/remove）`)
      }
      const result = await service.controlRun(home, args.id, args.action, args.nodeId)
      changed?.(home.homeId)
      return {
        run: runValue(result.run),
        ...(result.item !== undefined ? { item: runItemValue(result.item) } : {}),
      }
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_run_claim',
    description:
      '原子认领 run 的下一个 pending Ticket/SubTicket：单事务置 running 并绑定当前会话为执行者。'
      + '认领结果包含当前执行项完整用户正文（不含插件管理的子节点块），以及认领成功后即时读取的 Epic/Story/SubStory/父 Ticket 祖先背景（非跨文件原子快照）；'
      + '祖先背景只读且受显式字节预算约束；单个祖先读取失败会显式标记并保留其他内容。'
      + '认领成功会自动把祖先链中仍为 open 的父 Ticket/SubStory/Story/Epic 置为 in_progress。并发认领不会拿到同一项；run 未 start 或已 pause 时拒绝；'
      + '无会话（agent-less）调用不可认领。',
    parameters: {
      id: { type: 'string', required: true, description: 'run id' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          run_id: { type: 'string', required: true },
          claimed: { type: 'boolean', required: true },
          item: RUN_ITEM_SCHEMA,
          ticket: NODE_SCHEMA,
          context: CLAIM_CONTEXT_SCHEMA,
          context_error: { type: 'string' },
        },
      },
      render: (_args, value: {
        run_id: string
        claimed: boolean
        item?: RunItemValue
        ticket?: NodeValue
        context?: ClaimContextValue
        context_error?: string
      }) => [{
        type: 'text',
        text: value.claimed && value.item !== undefined
          ? [
              `已认领 ${value.item.node_id}${value.ticket !== undefined ? `「${value.ticket.title}」` : ''}，执行者已绑定当前会话。`,
              value.context !== undefined
                ? renderClaimContext(value.context)
                : `## 执行上下文读取失败\n\n${value.context_error ?? '未知错误'}\n请用 omt_show 读取当前节点及其父级后再执行。`,
              '完成当前执行项后请用 omt_run_report 报告结果（done/failed/blocked/skipped）。',
            ].join('\n\n')
          : `run ${value.run_id} 没有可认领的待执行项（无 pending 成员）。`,
      }],
    },
    async execute(args, exec) {
      const sessionId = sessionOf(exec)
      if (sessionId === undefined) {
        throw new Error('omt_run_claim 需要执行者会话：agent-less 调用不可认领（无从绑定 executor）')
      }
      const home = await service.resolveRunHome(args.id, exec.agent?.session.header.cwd)
      const outcome = await service.claimItem(home, args.id, sessionId)
      changed?.(home.homeId)
      if (!outcome.claimed || outcome.item === undefined) return { run_id: args.id, claimed: false }
      return {
        run_id: args.id,
        claimed: true,
        item: runItemValue(outcome.item, outcome.ticket?.title),
        ...(outcome.ticket !== undefined ? { ticket: nodeValue(outcome.ticket) } : {}),
        ...(outcome.context !== undefined ? { context: outcome.context } : {}),
        ...(outcome.context_error !== undefined ? { context_error: outcome.context_error } : {}),
      }
    },
  }))

  ctx.tools.register(defineTool({
    name: 'omt_run_report',
    description:
      '显式报告 run 成员的执行结果：outcome ∈ done/failed/blocked/skipped，note 追加到 ticket 进度记录。'
      + 'done：ticket 与 item 同置 done；failed：仅 item 置 failed（ticket 保持 in_progress），note 记入 last_error；'
      + 'blocked/skipped：ticket 与 item 同步置对应状态。failed 会触发 run 的 stop-on-failure 判定。',
    parameters: {
      id: { type: 'string', required: true, description: 'run id' },
      nodeId: { type: 'string', required: true, description: '成员节点 id' },
      outcome: { type: 'string', enum: RUN_REPORT_OUTCOMES, required: true, description: '执行结果' },
      note: { type: 'string', description: '结果说明（追加到 ticket 正文；failed 时同时记入 last_error）' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          run: RUN_SCHEMA,
          item: RUN_ITEM_SCHEMA,
          node: NODE_SCHEMA,
        },
      },
      render: (_args, value: { run: RunValue; item: RunItemValue; node: NodeValue }) => [{
        type: 'text',
        text: `已报告 ${value.item.node_id} → ${value.item.state}：\n${renderItemLine(value.item)}\n${renderRunLine(value.run)}`,
      }],
    },
    async execute(args, exec) {
      const home = await service.resolveRunHome(args.id, exec.agent?.session.header.cwd)
      const result = await service.reportItem(
        home,
        args.id,
        args.nodeId,
        args.outcome as RunReportOutcome,
        args.note,
      )
      // A report concludes the current execution: clear the running mark
      // (failed keeps the ticket in_progress; a retry re-marks it).
      running?.stop(args.nodeId)
      changed?.(result.homeId)
      return {
        run: runValue(result.run),
        item: runItemValue(result.item, result.node.title),
        node: nodeValue(result.node),
      }
    },
  }))
}
