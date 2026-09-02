/**
 * OMT RPC channel: exposes read endpoints to the browser half through the
 * generic Connection RPC transport (`POST /omt/<endpoint>`). External
 * plugins cannot rely on Typert's monorepo-built strict descriptors, so
 * payloads are validated here with zod on both ends.
 *
 * U7a: every data op routes through the OmtService (omt-daemon RPC); the
 * wire shapes below are the adapter's OWN contract with the browser and are
 * unchanged — the service translates protocol views (camelCase, qualified
 * refs) into these pre-daemon shapes.
 *
 * Endpoints (mutations bump the change hub so SSE clients refetch):
 *   tree        { rootId? }                        → OmtTreeNode[] (full forest or subtree)
 *   search      { query?, limit? }                 → NodeSummary[] (@-trigger candidates)
 *   get         { id }                             → { node, parent?, children, body, runs }
 *   update      { id, ...changes }                 → NodeSummary
 *   execute     { id, sessionId }                  → NodeSummary (claim + running mark)
 *   recent      { sessionId }                      → NodeSummary[]
 *   reindex     {}                                 → { nodes, edges, skipped }
 *   run-list    {}                                 → { runs: RunSummary[] } (progress/stalled/grouping flags)
 *   run-show    { id }                             → { run: RunSummary & config, items: RunItemView[] }
 *   run-control { id, action, nodeId? }            → start/pause/resume/cancel/retry/remove
 *   run-create  { nodeIds, title? }                → 一键直建（默认配置，收集子树中的 ticket）
 *   run-add     { id, nodeIds }                    → 加入既有 run（本 daemon 版本无追加成员 RPC：明确拒绝）
 *   run-confirm { id, nodeId, decision }           → awaiting_confirmation 确认/打回
 *   filters-get { sessionId? }                     → SavedFilters（缺失/损坏回退默认）
 *   filters-set { sessionId?, filters }            → 校验合并后经 ui/filters-set 持久化
 */
import { z } from 'zod'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-client-connection'
import type { ConnectionRpcResult as RpcResult } from '@deepseek-ai/dsh-client-connection'
import type { AgentsLike } from './agents-like.ts'
import { pluginUserMessage } from './messages.ts'
import {
  coerceSavedFilters,
  savedFiltersSchema,
  type SavedFilters,
} from './ui-state.ts'
import { DSH_FILTERS_KEY, type ChangeHub, type HomeRef, type OmtService } from './service.ts'
import type { RecentRegistry } from './recent.ts'
import { endsExecution, lineageOfHeader, type RunningRegistry } from './running.ts'
import { describeBoundCatalog, type SkillCatalogEntry } from './prompt-settings.ts'
import {
  isRunActive,
  isRunHistory,
  isRunItemStalled,
  isRunMemberNodeType,
  OmtError,
  RUN_ITEM_STATES,
  type OmtNode,
  type OmtRun,
  type OmtRunItem,
  type OmtTreeNode,
  type RunItemState,
} from './types.ts'

export interface PromptRpcHost {
  getSettings(): { extraPrompt: string; boundSkillNames: string[] }
  listCatalog(sessionId?: string): Promise<readonly SkillCatalogEntry[]>
}

/** Structural agent face: sessionId → agent → session header (+ wake for run start). */
interface SessionAgent {
  session: { header: { cwd: string; parentSession?: string; origin?: 'subagent' } }
  followup?(message: unknown): void
}

// Every payload may carry the calling session so the handler can resolve
// the workspace home (absent sessionId → global fallback).
const sessionField = { sessionId: z.string().optional() }
const treePayloadSchema = z.object({ rootId: z.string().optional(), ...sessionField }).strict()
const searchPayloadSchema = z.object({
  query: z.string().default(''),
  limit: z.number().int().min(1).max(100).default(20),
  ...sessionField,
}).strict()
const getPayloadSchema = z.object({ id: z.string().min(1), ...sessionField }).strict()
const updatePayloadSchema = z.object({
  id: z.string().min(1),
  title: z.string().min(1).optional(),
  status: z.enum(['open', 'in_progress', 'done', 'blocked', 'skipped']).optional(),
  archived: z.boolean().optional(),
  priority: z.number().int().optional(),
  append: z.string().min(1).optional(),
  body: z.string().optional(),
  ...sessionField,
}).strict()
const reindexPayloadSchema = z.object({ ...sessionField }).strict()
const skillsPayloadSchema = z.object({ ...sessionField }).strict()
const recentPayloadSchema = z.object({ sessionId: z.string().min(1) }).strict()
const executePayloadSchema = z.object({ id: z.string().min(1), sessionId: z.string().min(1) }).strict()
const createPayloadSchema = z.object({
  type: z.enum(['epic', 'story', 'substory', 'ticket', 'subticket']),
  title: z.string().min(1),
  parentId: z.string().min(1).optional(),
  body: z.string().optional(),
  scope: z.enum(['workspace', 'global']).optional(),
  ...sessionField,
}).strict()

// ── run endpoints (STORY-0013 UI channel) ────────────────────────────────
const runListPayloadSchema = z.object({ ...sessionField }).strict()
const runShowPayloadSchema = z.object({ id: z.string().min(1), ...sessionField }).strict()
const runControlPayloadSchema = z.object({
  id: z.string().min(1),
  action: z.enum(['start', 'pause', 'resume', 'cancel', 'retry', 'remove']),
  nodeId: z.string().min(1).optional(),
  ...sessionField,
}).strict()
const runCreatePayloadSchema = z.object({
  /** Scope roots; only ticket/subticket nodes in each subtree are collected. */
  nodeIds: z.array(z.string().min(1)).min(1),
  title: z.string().min(1).optional(),
  ...sessionField,
}).strict()
const runAddPayloadSchema = z.object({
  id: z.string().min(1),
  /** Scope roots; only ticket/subticket nodes in each subtree are collected. */
  nodeIds: z.array(z.string().min(1)).min(1),
  ...sessionField,
}).strict()
const runConfirmPayloadSchema = z.object({
  id: z.string().min(1),
  nodeId: z.string().min(1),
  decision: z.enum(['confirm', 'reject']),
  ...sessionField,
}).strict()

// ── saved tree filters (STORY-0023 UI channel) ───────────────────────────
const filtersGetPayloadSchema = z.object({ ...sessionField }).strict()
const filtersSetPayloadSchema = z.object({
  /** Partial patch; the server merges onto defaults and validates the bag. */
  filters: z.record(z.string(), z.unknown()),
  ...sessionField,
}).strict()

export interface NodeSummary {
  readonly id: string
  readonly type: OmtNode['type']
  readonly title: string
  readonly status: OmtNode['status']
  readonly archived: boolean
  readonly priority: number
}

function summarize(node: Pick<OmtNode, 'id' | 'type' | 'title' | 'status' | 'archived' | 'priority'>): NodeSummary {
  return { id: node.id, type: node.type, title: node.title, status: node.status, archived: node.archived, priority: node.priority }
}

// ── run view values (STORY-0013 UI channel) ──────────────────────────────

type RunProgress = Record<RunItemState, number> & { total: number }

function emptyProgress(): RunProgress {
  return { total: 0, ...Object.fromEntries(RUN_ITEM_STATES.map(state => [state, 0])) } as RunProgress
}

export interface RunSummaryValue {
  readonly id: string
  readonly title?: string
  readonly status: OmtRun['status']
  /** 加入-run picker eligibility (TICKET-0067): pending/running/paused. */
  readonly active: boolean
  /** Folds into the 历史 group (TICKET-0068); interrupted stays in the main list. */
  readonly history: boolean
  readonly created_at: string
  readonly finished_at?: string
  readonly progress: RunProgress
  /** Pending items whose 续跑 nudge budget is exhausted (TICKET-0062). */
  readonly stalled: number
}

/** Progress from a protocol run view's derived counts. */
function progressFromCounts(counts: Record<string, number>): RunProgress {
  const progress = emptyProgress()
  for (const state of RUN_ITEM_STATES) progress[state] = counts[state] ?? 0
  progress.total = counts.total ?? RUN_ITEM_STATES.reduce((sum, state) => sum + (counts[state] ?? 0), 0)
  return progress
}

/**
 * Summary from a run + its items (single membership scan; stalled derives
 * from the nudge ledger overlay the service applies).
 */
function runSummary(run: OmtRun, items?: readonly OmtRunItem[]): RunSummaryValue {
  if (items === undefined) {
    throw new Error('runSummary requires the membership scan')
  }
  const progress = emptyProgress()
  let stalled = 0
  for (const item of items) {
    progress.total += 1
    progress[item.state] += 1
    if (isRunItemStalled(item)) stalled += 1
  }
  return {
    id: run.id,
    ...(run.title !== undefined ? { title: run.title } : {}),
    status: run.status,
    active: isRunActive(run.status),
    history: isRunHistory(run.status),
    created_at: run.created_at,
    ...(run.finished_at !== undefined ? { finished_at: run.finished_at } : {}),
    progress,
    stalled,
  }
}

/** One collectable run member: pending by default (running marks degrade to pending on this daemon build). */
interface CollectedMember {
  readonly nodeId: string
}

interface MemberCollection {
  readonly members: CollectedMember[]
  readonly skippedDone: number
  readonly skippedArchived: number
}

/**
 * 一键直建 collection (TICKET-0067): walk each root + its whole subtree in
 * DFS pre-order, but collect only executable ticket/subticket nodes. Epic,
 * story, and substory nodes provide context and scope; they never become run
 * items. done/archived executable nodes are skipped and counted (their
 * children are still visited). Overlapping roots dedupe via `seen`.
 */
function collectRunMembers(running: RunningRegistry | undefined, trees: readonly OmtTreeNode[], liveMarkOf: (nodeId: string) => boolean): MemberCollection {
  const members: CollectedMember[] = []
  const seen = new Set<string>()
  let skippedDone = 0
  let skippedArchived = 0
  const visit = (node: OmtTreeNode): void => {
    if (!seen.has(node.id)) {
      seen.add(node.id)
      if (isRunMemberNodeType(node.type)) {
        if (node.archived) {
          skippedArchived += 1
        } else if (node.status === 'done') {
          skippedDone += 1
        } else {
          // In-progress tickets join as pending on this daemon build: the
          // create-run RPC has no per-member state override (U7b open item).
          // A live running mark is still required to exist for the join
          // semantics to be considered — kept informational only.
          void running
          void liveMarkOf
          members.push({ nodeId: node.id })
        }
      }
    }
    for (const child of node.children) visit(child)
  }
  for (const tree of trees) visit(tree)
  return { members, skippedDone, skippedArchived }
}

type RpcSuccess = { ok: true; value: unknown }

function ok(value: unknown): RpcSuccess {
  return { ok: true, value }
}

function failure(message: string): RpcResult<unknown> {
  return { ok: false, error: { code: 'internal', message, details: {} } }
}

function badRequest(message: string, issues: z.core.$ZodIssue[]): RpcResult<unknown> {
  return { ok: false, error: { code: 'bad-request', message, details: { issues } } }
}

/** Register the `/omt` RPC channel (loopback authority: local GUI only). */
export function registerOmtRpc(ctx: Context, service: OmtService, recent?: RecentRegistry, changes?: ChangeHub, running?: RunningRegistry, prompt?: PromptRpcHost): void {
  const agents = (ctx as unknown as { agents?: AgentsLike<SessionAgent> }).agents
  const sessionLabelOf = (sessionId: string): string => {
    const cwd = agents?.get(sessionId)?.session.header.cwd
    const base = cwd === undefined ? undefined : cwd.split('/').filter(Boolean).pop()
    return base !== undefined ? `${base} 的会话` : sessionId.slice(0, 13)
  }
  const homeFor = (sessionId: string | undefined) =>
    service.homeFor(sessionId === undefined ? undefined : agents?.get(sessionId)?.session.header.cwd)
  const cwdOf = (sessionId: string | undefined) =>
    sessionId === undefined ? undefined : agents?.get(sessionId)?.session.header.cwd

  /**
   * Identity translation gate (TICKET-0123): a payload-supplied sessionId
   * must name a LIVE Cordis agent — a forged id must not steer home
   * resolution or executor attribution. An absent sessionId (global-home
   * fallback) and a registry-less context pass through unchanged.
   */
  const requireLiveSession = (sessionId: string | undefined): void => {
    if (sessionId === undefined || agents === undefined) return
    if (agents.get(sessionId) === undefined) {
      throw new OmtError(
        'FORBIDDEN',
        `sessionId does not match a live session: ${sessionId.slice(0, 13)}`,
        { kind: 'session' },
      )
    }
  }

  /**
   * Executor lineage view (TICKET-0066/0068): the RunningRegistry snapshot
   * wins while the ticket is still marked running; otherwise fall back to
   * the live session header (gone once the executor session is disposed).
   */
  const executorOf = (item: OmtRunItem) => {
    const sessionId = item.executor_session_id
    if (sessionId === undefined) return undefined
    const live = running?.get(item.node_id)
    if (live !== undefined && live.sessionId === sessionId) {
      return {
        sessionId,
        label: live.sessionLabel,
        ...(live.parentSessionId !== undefined ? { parentSessionId: live.parentSessionId } : {}),
        ...(live.isSubagent === true ? { isSubagent: true } : {}),
      }
    }
    const header = agents?.get(sessionId)?.session.header
    return { sessionId, label: sessionLabelOf(sessionId), ...lineageOfHeader(header) }
  }

  /** Run-detail item row: state/谱系/attempts/last_error/stalled + ticket join. */
  const runItemView = async (home: HomeRef, item: OmtRunItem) => {
    // Full ticket join (id/title/status/archived) — detail views carry only
    // the title, so fetch the node row for the rest (local IPC, cheap).
    const node = await service.getNodeIn(home, item.node_id)
    const executor = executorOf(item)
    return {
      node_id: item.node_id,
      position: item.position,
      state: item.state,
      attempts: item.attempts,
      ...(item.last_error !== undefined ? { last_error: item.last_error } : {}),
      ...(isRunItemStalled(item) ? { stalled: true } : {}),
      ...(item.started_at !== undefined ? { started_at: item.started_at } : {}),
      ...(item.finished_at !== undefined ? { finished_at: item.finished_at } : {}),
      ...(executor !== undefined ? { executor } : {}),
      ...(node !== undefined
        ? { node: { id: node.id, title: node.title, status: node.status, archived: node.archived } }
        : item.title !== undefined ? { node: { id: item.node_id, title: item.title } } : {}),
    }
  }

  const ancestorSummaries = async (home: HomeRef, firstParent: OmtNode | undefined): Promise<NodeSummary[]> => {
    const ancestors: NodeSummary[] = []
    let current = firstParent
    while (current !== undefined) {
      ancestors.unshift(summarize(current))
      current = await service.parentOfNodeIn(home, current.id)
    }
    return ancestors
  }

  ctx.connection.rpc.handle('/omt', async (endpoint, payload) => {
    try {
      // Identity gate FIRST: every sessionId-bearing endpoint funnels the
      // same check, so a forged session id is rejected before any home or
      // executor resolution happens (TICKET-0123).
      const rawSessionId = (payload as { sessionId?: unknown } | null)?.sessionId
      if (typeof rawSessionId === 'string') requireLiveSession(rawSessionId)
      switch (endpoint) {
        case 'tree': {
          const parsed = treePayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid tree payload', parsed.error.issues)
          const home = await homeFor(parsed.data.sessionId)
          return ok(await service.tree(home, parsed.data.rootId))
        }
        case 'search': {
          const parsed = searchPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid search payload', parsed.error.issues)
          const { query, limit } = parsed.data
          const home = await homeFor(parsed.data.sessionId)
          // Empty query backs the just-typed-'@' candidate list: latest nodes first.
          return ok(await service.search(home, query.trim(), limit))
        }
        case 'get': {
          const parsed = getPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid get payload', parsed.error.issues)
          const result = await service.showNode(parsed.data.id, cwdOf(parsed.data.sessionId))
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          const runningInfo = running?.get(parsed.data.id)
          // 所属 run 链接 (TICKET-0068): every non-terminal run holding
          // this ticket, with the item state (awaiting_confirmation 标识)
          // and a small progress summary.
          const runs = result.runs.map(link => ({
            id: link.runId,
            ...(link.title !== undefined ? { title: link.title } : {}),
            status: link.status,
            itemState: link.itemState,
            progress: progressFromCounts(link.progress),
          }))
          return ok({
            home: result.home.homeId,
            ...(runningInfo !== undefined ? { running: runningInfo } : {}),
            node: result.node,
            ...(result.parent != null ? { parent: summarize(result.parent) } : {}),
            children: result.children.map(summarize),
            body: result.body,
            ancestors: await ancestorSummaries(result.home, result.parent),
            runs,
          })
        }
        case 'update': {
          const parsed = updatePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid update payload', parsed.error.issues)
          const { title, status, archived, priority, append, body } = parsed.data
          if ([title, status, archived, priority, append, body].every(v => v === undefined)) {
            return badRequest('update requires at least one change (title/status/archived/priority/append/body)', [])
          }
          // Passive observation (TICKET-0061) rides on the daemon update;
          // the session is recorded adapter-side for hook attribution.
          const { node } = await service.updateNode(
            { id: parsed.data.id, title, status, archived, priority, append, body },
            { cwd: cwdOf(parsed.data.sessionId), sessionId: parsed.data.sessionId },
          )
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          // Manual status changes never START a running mark — execution is
          // claimed only by the execute endpoint and model tool calls
          // (TICKET-0028). Done/blocked/skipped/archive always clear it.
          if (endsExecution(status, archived)) running?.stop(parsed.data.id)
          return ok(summarize(node))
        }
        case 'create': {
          const parsed = createPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid create payload', parsed.error.issues)
          const cwd = cwdOf(parsed.data.sessionId)
          const home = parsed.data.parentId !== undefined
            ? (await service.resolveNodeHome(parsed.data.parentId, cwd)).home
            : parsed.data.scope !== undefined
              ? await service.homeForScope(cwd, parsed.data.scope)
              : await service.homeFor(cwd)
          const node = await service.createNode(home, {
            type: parsed.data.type,
            title: parsed.data.title,
            parentId: parsed.data.parentId,
            body: parsed.data.body,
          })
          recent?.touch(parsed.data.sessionId, node.id)
          return ok(summarize(node))
        }
        case 'execute': {
          const parsed = executePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid execute payload', parsed.error.issues)
          // Executing un-archives nothing and starts work: in_progress + running mark.
          // Passive observation (TICKET-0061) rides on the daemon update: the
          // execute button dispatches the pending item of every active run
          // holding this ticket, with this session as the recorded executor.
          const { node } = await service.executeNode(parsed.data.id, {
            cwd: cwdOf(parsed.data.sessionId),
            sessionId: parsed.data.sessionId,
          })
          running?.start(
            parsed.data.id,
            parsed.data.sessionId,
            sessionLabelOf(parsed.data.sessionId),
            // Executor lineage snapshot (TICKET-0066) from the session header.
            lineageOfHeader(agents?.get(parsed.data.sessionId)?.session.header),
          )
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          return ok(summarize(node))
        }
        case 'recent': {
          const parsed = recentPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid recent payload', parsed.error.issues)
          const sessionId = parsed.data.sessionId
          const cwd = cwdOf(sessionId)
          const summaries: NodeSummary[] = []
          for (const id of (recent !== undefined ? await recent.resolve(sessionId) : [])) {
            try {
              const shown = await service.showNode(id, cwd)
              summaries.push(summarize(shown.node))
            } catch {
              /* missing node: skip silently (pre-daemon behavior) */
            }
          }
          return ok(summaries)
        }
        case 'skills': {
          const parsed = skillsPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid skills payload', parsed.error.issues)
          const settings = prompt?.getSettings() ?? { extraPrompt: '', boundSkillNames: [] }
          const catalog = prompt === undefined ? [] : await prompt.listCatalog(parsed.data.sessionId)
          return ok({
            extraPrompt: settings.extraPrompt,
            skills: describeBoundCatalog(catalog, settings.boundSkillNames),
          })
        }
        case 'reindex': {
          const parsed = reindexPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid reindex payload', parsed.error.issues)
          const home = await homeFor(parsed.data.sessionId)
          const result = await service.reindex(home)
          changes?.bump(home.homeId)
          return ok(result)
        }
        case 'run-list': {
          const parsed = runListPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid run-list payload', parsed.error.issues)
          const home = await homeFor(parsed.data.sessionId)
          // Active + interrupted + history in one list; the client groups by
          // the active/history flags (TICKET-0068 layout).
          const summaries = await service.listRunSummaries(home)
          const runs = []
          for (const { run, progress } of summaries) {
            const snapshot = await service.fetchRun(home, run.id)
            runs.push({ ...runSummary(run, snapshot.items), progress: progressFromCounts(progress) })
          }
          return ok({ runs })
        }
        case 'run-show': {
          const parsed = runShowPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-show payload', parsed.error.issues)
          const home = await service.resolveRunHome(parsed.data.id, cwdOf(parsed.data.sessionId))
          // One membership fetch feeds both the summary and the item views.
          const snapshot = await service.fetchRun(home, parsed.data.id)
          return ok({
            run: { ...runSummary(snapshot.run, snapshot.items), config: snapshot.run.config },
            items: await Promise.all(snapshot.items.map(item => runItemView(home, item))),
          })
        }
        case 'run-control': {
          const parsed = runControlPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-control payload', parsed.error.issues)
          const { id, action, nodeId } = parsed.data
          const home = await service.resolveRunHome(id, cwdOf(parsed.data.sessionId))
          if ((action === 'retry' || action === 'remove') && nodeId === undefined) {
            return badRequest(`run-control ${action} requires nodeId`, [])
          }
          const result = await service.controlRun(home, id, action, nodeId)
          if (action === 'start') {
            // 开始执行 (TICKET-0068): wake the operating session with a
            // followup pointing at the claim loop. Best-effort — a dead
            // session never fails the start.
            const agent = parsed.data.sessionId === undefined ? undefined : agents?.get(parsed.data.sessionId)
            if (agent?.followup !== undefined) {
              const snapshot = await service.fetchRun(home, id)
              try {
                agent.followup(pluginUserMessage(
                  `用户从 UI 触发了 run ${id} 的开始执行（共 ${snapshot.items.length} 个成员）。`
                  + `请调用 omt_run_claim（id="${id}"）认领下一个待执行项并执行；`
                  + '每项完成后用 omt_run_report 报告结果（done/failed/blocked/skipped）。',
                ))
              } catch (error) {
                console.warn('[omt] run-control start: followup injection failed', error)
              }
            }
          }
          // remove emits no run event envelope on some paths, so it bumps
          // explicitly; every other transition reaches the hub through the
          // daemon event bridge (TICKET-0071).
          if (action === 'remove') changes?.bump(home.homeId, { id, kind: 'run' })
          const fresh = await service.fetchRun(home, id)
          const controlledItem = result.item ?? fresh.items.find(candidate => candidate.node_id === nodeId)
          return ok({
            run: runSummary(fresh.run, fresh.items),
            ...(controlledItem !== undefined ? { item: await runItemView(home, controlledItem) } : {}),
          })
        }
        case 'run-create': {
          const parsed = runCreatePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-create payload', parsed.error.issues)
          const cwd = cwdOf(parsed.data.sessionId)
          // Single-home rule (TICKET-0067, same as omt_run_create): every
          // root must resolve to one home; the run lands there.
          let baseline: HomeRef | undefined
          for (const rootId of parsed.data.nodeIds) {
            const owner = (await service.resolveNodeHome(rootId, cwd)).home
            baseline ??= owner
            if (owner.homeId !== baseline.homeId) {
              throw new OmtError('INVALID_INPUT',
                `run 成员必须同属一个 OMT home（${rootId} 属于 ${owner.homeId}，与 ${baseline.homeId} 不同）`)
            }
          }
          baseline ??= await homeFor(parsed.data.sessionId)
          const trees = (await Promise.all(parsed.data.nodeIds.map(rootId => service.tree(baseline as HomeRef, rootId)))).flat()
          const collected = collectRunMembers(running, trees, () => false)
          // 一键直建 (TICKET-0067): default config; members enter pending on
          // this daemon build (no add-members RPC → no running-state joins).
          const created = await service.createRun(baseline, {
            ...(parsed.data.title !== undefined ? { title: parsed.data.title } : {}),
            nodeIds: collected.members.map(member => member.nodeId),
          })
          return ok({
            run: runSummary(created.run, created.items),
            added: collected.members.map(member => member.nodeId),
            addedRunning: [] as string[],
            skippedDone: collected.skippedDone,
            skippedArchived: collected.skippedArchived,
          })
        }
        case 'run-add': {
          const parsed = runAddPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-add payload', parsed.error.issues)
          // This daemon build exposes no add-members RPC: refuse with an
          // actionable problem instead of silently degrading membership.
          throw new OmtError('INVALID_INPUT',
            `当前 omt-daemon 版本不支持向既有 run ${parsed.data.id} 追加成员（协议缺口）；请改用 omt_run_create 另建包含这些成员的 run`)
        }
        case 'run-confirm': {
          const parsed = runConfirmPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-confirm payload', parsed.error.issues)
          const { id, nodeId, decision } = parsed.data
          const home = await service.resolveRunHome(id, cwdOf(parsed.data.sessionId))
          const current = (await service.fetchRun(home, id)).items.find(item => item.node_id === nodeId)
          if (current === undefined) throw new OmtError('NOT_FOUND', `run ${id} has no item for node: ${nodeId}`)
          if (current.state !== 'awaiting_confirmation') {
            throw new OmtError('CONFLICT', `item ${nodeId} is ${current.state}; only awaiting_confirmation items can be confirmed/rejected`)
          }
          let item: OmtRunItem
          if (decision === 'confirm') {
            // 确认完成 (TICKET-0070): an explicit done report — item done +
            // ticket done (reported bypasses the trust gate by design).
            item = (await service.reportItem(home, id, nodeId, 'done')).item
            running?.stop(nodeId)
          } else {
            // 打回: reopen the ticket (open over an awaiting_confirmation
            // item is the TICKET-0064 rejection path). The observation
            // interrupt lands the item AND replays other active runs holding
            // the ticket (decision 1 cross-run broadcast).
            await service.updateNode({ id: nodeId, status: 'open' }, { cwd: cwdOf(parsed.data.sessionId) })
            item = (await service.fetchRun(home, id)).items.find(entry => entry.node_id === nodeId) as OmtRunItem
          }
          // The item transition already reached the hub through the event bridge.
          const fresh = await service.fetchRun(home, id)
          return ok({ run: runSummary(fresh.run, fresh.items), item: await runItemView(home, item) })
        }
        case 'filters-get': {
          const parsed = filtersGetPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid filters-get payload', parsed.error.issues)
          const home = await homeFor(parsed.data.sessionId)
          return ok(coerceSavedFilters(await service.filtersGetDsh(home)))
        }
        case 'filters-set': {
          const parsed = filtersSetPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid filters-set payload', parsed.error.issues)
          const home = await homeFor(parsed.data.sessionId)
          // Merge the partial patch onto the saved bag so the client can
          // send single-field updates; the merged result is fully validated
          // (zod failures fold to the INVALID_INPUT problem, like pre-U7a).
          const current = coerceSavedFilters(await service.filtersGet(home, FILTERS_KEY))
          const merged = savedFiltersSchema.safeParse({ ...current, ...(parsed.data.filters as Record<string, unknown>) })
          if (!merged.success) {
            throw new OmtError('INVALID_INPUT', `invalid saved-filters payload: ${merged.error.issues.map(issue => `${issue.path.join('.')}: ${issue.message}`).join('; ')}`)
          }
          return ok(coerceSavedFilters(await service.filtersSet(home, FILTERS_KEY, merged.data)))
        }
        default:
          return badRequest(`unknown endpoint: ${endpoint}`, [])
      }
    } catch (error) {
      if (error instanceof OmtError) return failure(`${error.code}: ${error.message}`)
      throw error
    }
  })
}

/**
 * DSH filters bag key (U4/R3): surface-prefixed per the bag scoping contract;
 * legacy bare 'ui' bags migrate on read via filtersGetDsh (fallback +
 * write-through), so this stays the only key the DSH surface ever writes.
 */
const FILTERS_KEY: string = DSH_FILTERS_KEY
