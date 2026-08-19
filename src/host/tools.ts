/**
 * Model-facing OMT tools: thin wrappers over OmtCore registered through
 * ctx.tools. Canonical values stay programmatic (node objects / counts);
 * output.render produces the model-facing Chinese text. OmtError throws
 * surface to the model as isError results, which is the intended channel
 * for hierarchy violations and unknown ids.
 */
import type { Context } from '@deepseek-ai/cordis'
import { defineTool, type ToolRunContext } from '@deepseek-ai/dsh-tools'
import type { OmtCore, ReindexResult, ShowResult } from './core.ts'
import type { OmtCorePool } from './pool.ts'
import type { RunningRegistry } from './running.ts'
import { stripChildrenBlock } from './markdown.ts'
import { NODE_TYPES, STATUSES, type OmtNode } from './types.ts'

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

function renderShow(result: ShowResult): string {
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
  const userBody = stripChildrenBlock(body)
  const childLines = children.length > 0
    ? children.map(child => `- ${child.id} [${child.type} · ${child.status}] ${child.title}`).join('\n')
    : '（无子节点）'
  return `${header}\n\n## 正文\n\n${userBody === '' ? '（空）' : userBody}\n\n## 子节点\n\n${childLines}`
}

/** Resolve the workspace-routed core for one execution (global fallback). */
function coreOf(pool: OmtCorePool, exec: ToolRunContext) {
  return pool.coreFor(exec.agent?.session.header.cwd)
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
 * Resolve the create target core. Children always land in their parent's
 * home; root epics honor an explicit scope, then ask the user (UI-backed),
 * then fall back to the automatic rule when no answerer is available.
 */
async function coreForCreate(
  pool: OmtCorePool,
  userQuestions: UserQuestionsLike | undefined,
  exec: ToolRunContext,
  args: { type: string; title: string; parentId?: string; scope?: 'workspace' | 'global' },
): Promise<OmtCore> {
  const cwd = exec.agent?.session.header.cwd
  if (args.parentId !== undefined) return pool.coreForNode(args.parentId, cwd)
  if (args.scope !== undefined) return pool.coreForScope(cwd, args.scope)
  if (cwd === undefined || userQuestions === undefined) return pool.coreFor(cwd)
  try {
    const answer = await userQuestions.ask({
      questions: [{
        id: 'scope',
        header: '创建位置',
        question: `Epic「${args.title}」创建到哪里？`,
        options: [
          { label: WORKSPACE_LABEL, description: `${cwd}/.omt（ticket 随项目走，可进 git）` },
          { label: GLOBAL_LABEL, description: `${pool.globalHome}（所有工作区共享）` },
        ],
      }],
      agent: exec.agent,
      signal: exec.signal,
    })
    const selected = answer.answers.find(item => item.id === 'scope')?.selected[0]
    if (selected === WORKSPACE_LABEL) return pool.coreForScope(cwd, 'workspace')
    if (selected === GLOBAL_LABEL) return pool.coreForScope(cwd, 'global')
    return pool.coreFor(cwd)
  } catch {
    // No UI provider / not a live agent / aborted: automatic rule applies.
    return pool.coreFor(cwd)
  }
}

/** Register all omt_* tools; executes route to the workspace's OMT home. */
export function registerOmtTools(
  ctx: Context,
  pool: OmtCorePool,
  touch?: (sessionId: string | undefined, id: string) => void,
  changed?: (home: string) => void,
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
    if (status === 'in_progress') running.start(id, sessionOf(exec) ?? '', labelOf(exec))
    if (status === 'done' || archived === true) running.stop(id)
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
      const cwd = exec.agent?.session.header.cwd
      const core = await coreForCreate(pool, userQuestions, exec, args)
      // Pool-wide unique id: syncs counters across homes so bare ids never
      // collide between the global and workspace homes.
      const id = await pool.allocateId(args.type, cwd, core.home !== pool.globalHome)
      const created = await core.create({
        type: args.type,
        title: args.title,
        parentId: args.parentId,
        body: args.body,
        priority: args.priority,
        id,
      })
      touch?.(sessionOf(exec), created.id)
      changed?.(core.home)
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
      const core = await coreOf(pool, exec)
      return core.list({ type: args.type, status: args.status, query: args.query }).map(nodeValue)
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
        text: renderShow({
          node: value.node as unknown as OmtNode,
          parent: value.parent as unknown as OmtNode | undefined,
          children: value.children as unknown as OmtNode[],
          body: value.body,
        }),
      }],
    },
    async execute(args, exec) {
      const core = await pool.coreForNode(args.id, exec.agent?.session.header.cwd)
      touch?.(sessionOf(exec), args.id)
      const result = await core.show(args.id)
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
      '更新一个 OMT 节点：标题、状态（open/in_progress/done/archived）、优先级、'
      + '替换正文（body）或追加进度记录（append）。至少需要提供一项变更。'
      + '开始处理某节点时将状态置为 in_progress，完成时置为 done 并 append 结论。',
    parameters: {
      id: { type: 'string', required: true, description: '节点 id' },
      title: { type: 'string', description: '新标题' },
      status: { type: 'string', enum: STATUSES, description: '新状态（open/in_progress/done；已归档节点不可改状态，先恢复）' },
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
      const core = await pool.coreForNode(args.id, exec.agent?.session.header.cwd)
      touch?.(sessionOf(exec), args.id)
      trackRunning(exec, args.id, args.status, args.archived)
      const updated = await core.update({
        id: args.id,
        title: args.title,
        status: args.status,
        priority: args.priority,
        body: args.body,
        append: args.append,
      })
      changed?.(core.home)
      return nodeValue(updated)
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
      const cwd = exec.agent?.session.header.cwd
      const core = await pool.coreForNode(args.id, cwd)
      if (core.getNode(args.newParentId) === undefined) {
        throw new Error('omt_move 不支持跨 home 移动（节点与目标父节点不在同一个 OMT home）')
      }
      const moved = await core.move(args.id, args.newParentId)
      touch?.(sessionOf(exec), args.id)
      changed?.(core.home)
      return nodeValue(moved)
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
      const core = await coreOf(pool, exec)
      const result = await core.reindex()
      changed?.(core.home)
      return result
    },
  }))
}
