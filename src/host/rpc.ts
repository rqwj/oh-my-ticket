/**
 * OMT RPC channel: exposes read endpoints to the browser half through the
 * generic Connection RPC transport (`POST /omt/<endpoint>`). External
 * plugins cannot rely on Typert's monorepo-built strict descriptors, so
 * payloads are validated here with zod on both ends.
 *
 * Endpoints (mutations bump the ChangeHub so SSE clients refetch):
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
 *   run-add     { id, nodeIds }                    → 加入既有 run（只收集 ticket，去重/跳过/home 校验）
 *   run-confirm { id, nodeId, decision }           → awaiting_confirmation 确认/打回
 *   filters-get { sessionId? }                     → SavedFilters（缺失/损坏回退默认）
 *   filters-set { sessionId?, filters }            → 校验合并后持久化到 <home>/ui-filters.json
 */
import { z } from 'zod'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-client-connection'
import type { RpcResult } from '@deepseek-ai/dsh-host-apiproxy/api'
import type { AgentsLike } from './agents-like.ts'
import type { OmtCore } from './core.ts'
import { pluginUserMessage } from './messages.ts'
import type { OmtCorePool } from './pool.ts'
import type { ChangeHub } from './changes.ts'
import type { RecentRegistry } from './recent.ts'
import { endsExecution, lineageOfHeader, type RunningRegistry } from './running.ts'
import {
  isRunActive,
  isRunHistory,
  isRunItemStalled,
  isRunMemberNodeType,
  OmtError,
  RUN_ITEM_STATES,
  STATUSES,
  type OmtNode,
  type OmtRun,
  type OmtRunItem,
  type OmtTreeNode,
  type RunItemState,
} from './types.ts'

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
  status: z.enum(STATUSES).optional(),
  archived: z.boolean().optional(),
  priority: z.number().int().optional(),
  append: z.string().min(1).optional(),
  ...sessionField,
}).strict()
const reindexPayloadSchema = z.object({ ...sessionField }).strict()
const recentPayloadSchema = z.object({ sessionId: z.string().min(1) }).strict()
const executePayloadSchema = z.object({ id: z.string().min(1), sessionId: z.string().min(1) }).strict()

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

function summarize(node: OmtNode): NodeSummary {
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

/** Counts-based progress (cheap SQL aggregate for the get endpoint's run links). */
function runProgress(core: OmtCore, runId: string): RunProgress {
  const progress = emptyProgress()
  for (const { state, count } of core.runItemStateCounts(runId)) {
    progress.total += count
    progress[state] = count
  }
  return progress
}

/**
 * Single-scan summary: progress and the stalled count both derive from one
 * runItems fetch (run-list calls this per run — no N+1 double scan).
 */
function runSummary(core: OmtCore, run: OmtRun, items: readonly OmtRunItem[] = core.runItems(run.id)): RunSummaryValue {
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

/** One collectable run member: pending by default, running only for in_progress tickets with a live running mark. */
interface CollectedMember {
  readonly nodeId: string
  readonly state: 'pending' | 'running'
  readonly executorSessionId?: string
}

interface MemberCollection {
  readonly members: CollectedMember[]
  readonly skippedDone: number
  readonly skippedArchived: number
}

/**
 * 加入-run collection (TICKET-0067): walk each root + its whole subtree in
 * DFS pre-order, but collect only executable ticket/subticket nodes. Epic,
 * story, and substory nodes provide context and scope; they never become run
 * items. done/archived executable nodes are skipped and counted (their
 * children are still visited); in_progress nodes join as running ONLY when
 * the RunningRegistry holds a live mark for them (executor snapshot) — an
 * unmarked in_progress ticket is re-dispatched as pending. Overlapping roots
 * dedupe via `seen`.
 */
function collectRunMembers(core: OmtCore, running: RunningRegistry | undefined, rootIds: readonly string[]): MemberCollection {
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
        } else if (node.status === 'in_progress') {
          const info = running?.get(node.id)
          if (info !== undefined) {
            members.push({ nodeId: node.id, state: 'running', executorSessionId: info.sessionId })
          } else {
            members.push({ nodeId: node.id, state: 'pending' })
          }
        } else {
          members.push({ nodeId: node.id, state: 'pending' })
        }
      }
    }
    for (const child of node.children) visit(child)
  }
  for (const rootId of rootIds) {
    for (const root of core.tree(rootId)) visit(root)
  }
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
export function registerOmtRpc(ctx: Context, pool: OmtCorePool, recent?: RecentRegistry, changes?: ChangeHub, running?: RunningRegistry): void {
  const agents = (ctx as unknown as { agents?: AgentsLike<SessionAgent> }).agents
  const sessionLabelOf = (sessionId: string): string => {
    const cwd = agents?.get(sessionId)?.session.header.cwd
    const base = cwd === undefined ? undefined : cwd.split('/').filter(Boolean).pop()
    return base !== undefined ? `${base} 的会话` : sessionId.slice(0, 13)
  }
  const coreFor = (sessionId: string | undefined) =>
    pool.coreFor(sessionId === undefined ? undefined : agents?.get(sessionId)?.session.header.cwd)
  const cwdOf = (sessionId: string | undefined) =>
    sessionId === undefined ? undefined : agents?.get(sessionId)?.session.header.cwd

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
  const runItemView = (core: OmtCore, item: OmtRunItem) => {
    const node = core.getNode(item.node_id)
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
      ...(node !== undefined ? { node: { id: node.id, title: node.title, status: node.status, archived: node.archived } } : {}),
    }
  }

  /**
   * 加入-run home rule (TICKET-0067, same as omt_run_create): every root
   * must exist and resolve to `baselineHome`. For run-create the baseline
   * is the first root's home; for run-add it is the run's home.
   */
  const assertSameHome = async (baselineHome: string | undefined, rootIds: readonly string[], cwd: string | undefined, runId?: string): Promise<string> => {
    let home = baselineHome
    for (const { rootId, core: owner } of await pool.ownerCores(rootIds, cwd)) {
      if (owner.getNode(rootId) === undefined) throw new OmtError('NOT_FOUND', `unknown node: ${rootId}`)
      home ??= owner.home
      if (owner.home !== home) {
        throw new OmtError('INVALID_INPUT', runId === undefined
          ? `run 成员必须同属一个 OMT home（${rootId} 属于 ${owner.home}，与 ${home} 不同）`
          : `跨 home 加入被拒绝：${rootId} 属于 ${owner.home}，而 run ${runId} 属于 ${home}；请另建 run`)
      }
    }
    return home as string
  }
  ctx.connection.rpc.handle('/omt', async (endpoint, payload) => {
    try {
      switch (endpoint) {
        case 'tree': {
          const parsed = treePayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid tree payload', parsed.error.issues)
          const core = await coreFor(parsed.data.sessionId)
          const forest: OmtTreeNode[] = core.tree(parsed.data.rootId)
          return ok(forest)
        }
        case 'search': {
          const parsed = searchPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid search payload', parsed.error.issues)
          const { query, limit } = parsed.data
          const core = await coreFor(parsed.data.sessionId)
          // Empty query backs the just-typed-'@' candidate list: latest nodes first.
          const nodes = query.trim() === '' ? core.list({}).slice(-limit).reverse() : core.list({ query }).slice(0, limit)
          return ok(nodes.map(summarize))
        }
        case 'get': {
          const parsed = getPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid get payload', parsed.error.issues)
          const core = await pool.coreForNode(parsed.data.id, cwdOf(parsed.data.sessionId))
          const result = await core.show(parsed.data.id)
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          const runningInfo = running?.get(parsed.data.id)
          return ok({
            home: core.home,
            ...(runningInfo !== undefined ? { running: runningInfo } : {}),
            node: result.node,
            ...(result.parent !== undefined ? { parent: summarize(result.parent) } : {}),
            children: result.children.map(summarize),
            body: result.body,
            // 所属 run 链接 (TICKET-0068): every non-terminal run holding
            // this ticket, with the item state (awaiting_confirmation 标识)
            // and a small progress summary.
            runs: core.runsOfNode(parsed.data.id).map(({ run, item }) => ({
              id: run.id,
              ...(run.title !== undefined ? { title: run.title } : {}),
              status: run.status,
              itemState: item.state,
              progress: runProgress(core, run.id),
            })),
          })
        }
        case 'update': {
          const parsed = updatePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid update payload', parsed.error.issues)
          const { title, status, archived, priority, append } = parsed.data
          if ([title, status, archived, priority, append].every(v => v === undefined)) {
            return badRequest('update requires at least one change (title/status/archived/priority/append)', [])
          }
          const cwd = cwdOf(parsed.data.sessionId)
          const core = await pool.coreForNode(parsed.data.id, cwd)
          // Passive observation (TICKET-0061) rides on core.update; the
          // session becomes the executor of any item this change dispatches.
          const node = await core.update({ id: parsed.data.id, title, status, archived, priority, append, executorSessionId: parsed.data.sessionId })
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          // Manual status changes never START a running mark — execution is
          // claimed only by the execute endpoint and model tool calls
          // (TICKET-0028). Done/blocked/skipped/archive always clear it.
          if (endsExecution(status, archived)) running?.stop(parsed.data.id)
          changes?.bump(core.home)
          return ok(summarize(node))
        }
        case 'execute': {
          const parsed = executePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid execute payload', parsed.error.issues)
          const cwd = cwdOf(parsed.data.sessionId)
          const core = await pool.coreForNode(parsed.data.id, cwd)
          // Executing un-archives nothing and starts work: in_progress + running mark.
          // Passive observation (TICKET-0061) rides on core.update: the
          // execute button dispatches the pending item of every active run
          // holding this ticket, with this session as the executor.
          const node = await core.update({ id: parsed.data.id, status: 'in_progress', executorSessionId: parsed.data.sessionId })
          running?.start(
            parsed.data.id,
            parsed.data.sessionId,
            sessionLabelOf(parsed.data.sessionId),
            // Executor lineage snapshot (TICKET-0066) from the session header.
            lineageOfHeader(agents?.get(parsed.data.sessionId)?.session.header),
          )
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          changes?.bump(core.home)
          return ok(summarize(node))
        }
        case 'recent': {
          const parsed = recentPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid recent payload', parsed.error.issues)
          const sessionId = parsed.data.sessionId
          const cwd = cwdOf(sessionId)
          const summaries: NodeSummary[] = []
          for (const id of (recent !== undefined ? await recent.resolve(sessionId) : [])) {
            const owner = await pool.coreForNode(id, cwd)
            const node = owner.getNode(id)
            if (node !== undefined) summaries.push(summarize(node))
          }
          return ok(summaries)
        }
        case 'reindex': {
          const parsed = reindexPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid reindex payload', parsed.error.issues)
          const core = await coreFor(parsed.data.sessionId)
          const result = await core.reindex()
          changes?.bump(core.home)
          return ok(result)
        }
        case 'run-list': {
          const parsed = runListPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid run-list payload', parsed.error.issues)
          const core = await coreFor(parsed.data.sessionId)
          // Active + interrupted + history in one list; the client groups by
          // the active/history flags (TICKET-0068 layout).
          return ok({ runs: core.listRuns({}).map(run => runSummary(core, run)) })
        }
        case 'run-show': {
          const parsed = runShowPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-show payload', parsed.error.issues)
          const core = await pool.coreForRun(parsed.data.id, cwdOf(parsed.data.sessionId))
          const run = core.requireRun(parsed.data.id)
          // One membership scan feeds both the summary and the item views.
          const items = core.runItems(run.id)
          return ok({
            run: { ...runSummary(core, run, items), config: run.config },
            items: items.map(item => runItemView(core, item)),
          })
        }
        case 'run-control': {
          const parsed = runControlPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-control payload', parsed.error.issues)
          const { id, action, nodeId } = parsed.data
          const core = await pool.coreForRun(id, cwdOf(parsed.data.sessionId))
          let item: OmtRunItem | undefined
          switch (action) {
            case 'start': {
              await core.startRun(id)
              // 开始执行 (TICKET-0068): wake the operating session with a
              // followup pointing at the claim loop. Best-effort — a dead
              // session never fails the start.
              const agent = parsed.data.sessionId === undefined ? undefined : agents?.get(parsed.data.sessionId)
              if (agent?.followup !== undefined) {
                const count = core.runItemStateCounts(id).reduce((sum, entry) => sum + entry.count, 0)
                try {
                  agent.followup(pluginUserMessage(
                    `用户从 UI 触发了 run ${id} 的开始执行（共 ${count} 个成员）。`
                    + `请调用 omt_run_claim（id="${id}"）认领下一个待执行项并执行；`
                    + '每项完成后用 omt_run_report 报告结果（done/failed/blocked/skipped）。',
                  ))
                } catch (error) {
                  console.warn('[omt] run-control start: followup injection failed', error)
                }
              }
              break
            }
            case 'pause':
              await core.pauseRun(id)
              break
            case 'resume':
              await core.resumeRun(id)
              break
            case 'cancel':
              await core.cancelRun(id)
              break
            case 'retry':
              if (nodeId === undefined) return badRequest('run-control retry requires nodeId', [])
              item = await core.retryItem(id, nodeId)
              break
            case 'remove':
              if (nodeId === undefined) return badRequest('run-control remove requires nodeId', [])
              await core.removeRunItem(id, nodeId)
              break
          }
          // removeRunItem emits no run event, so it bumps explicitly; every
          // other action's transition already reached the hub through
          // bridgeRunEvents (TICKET-0071).
          if (action === 'remove') changes?.bump(core.home, { id, kind: 'run' })
          return ok({
            run: runSummary(core, core.requireRun(id)),
            ...(item !== undefined ? { item: runItemView(core, item) } : {}),
          })
        }
        case 'run-create': {
          const parsed = runCreatePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-create payload', parsed.error.issues)
          const cwd = cwdOf(parsed.data.sessionId)
          // 一键直建 (TICKET-0067): default config; advanced config stays
          // with the model-side omt_run_create.
          const home = await assertSameHome(undefined, parsed.data.nodeIds, cwd)
          const core = await pool.coreForHome(home)
          const collected = collectRunMembers(core, running, parsed.data.nodeIds)
          const run = await core.createRun({
            ...(parsed.data.title !== undefined ? { title: parsed.data.title } : {}),
            nodeIds: [],
          })
          const { added } = await core.addRunMembers(run.id, collected.members)
          changes?.bump(core.home, { id: run.id, kind: 'run' })
          return ok({
            run: runSummary(core, core.requireRun(run.id)),
            added: added.map(entry => entry.node_id),
            addedRunning: added.filter(entry => entry.state === 'running').map(entry => entry.node_id),
            skippedDone: collected.skippedDone,
            skippedArchived: collected.skippedArchived,
          })
        }
        case 'run-add': {
          const parsed = runAddPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-add payload', parsed.error.issues)
          const cwd = cwdOf(parsed.data.sessionId)
          const core = await pool.coreForRun(parsed.data.id, cwd)
          const run = core.requireRun(parsed.data.id)
          await assertSameHome(core.home, parsed.data.nodeIds, cwd, run.id)
          const collected = collectRunMembers(core, running, parsed.data.nodeIds)
          const { added, duplicates } = await core.addRunMembers(run.id, collected.members)
          changes?.bump(core.home, { id: run.id, kind: 'run' })
          return ok({
            run: runSummary(core, core.requireRun(run.id)),
            added: added.map(entry => entry.node_id),
            addedRunning: added.filter(entry => entry.state === 'running').map(entry => entry.node_id),
            duplicates,
            skippedDone: collected.skippedDone,
            skippedArchived: collected.skippedArchived,
          })
        }
        case 'run-confirm': {
          const parsed = runConfirmPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid run-confirm payload', parsed.error.issues)
          const { id, nodeId, decision } = parsed.data
          const core = await pool.coreForRun(id, cwdOf(parsed.data.sessionId))
          core.requireRun(id)
          const current = core.getRunItem(id, nodeId)
          if (current === undefined) throw new OmtError('NOT_FOUND', `run ${id} has no item for node: ${nodeId}`)
          if (current.state !== 'awaiting_confirmation') {
            throw new OmtError('CONFLICT', `item ${nodeId} is ${current.state}; only awaiting_confirmation items can be confirmed/rejected`)
          }
          let item: OmtRunItem
          if (decision === 'confirm') {
            // 确认完成 (TICKET-0070): an explicit done report — item done +
            // ticket done (reported bypasses the trust gate by design).
            item = (await core.reportRunItem(id, nodeId, 'done')).item
            running?.stop(nodeId)
          } else {
            // 打回: reopen the ticket (open over an awaiting_confirmation
            // item is the TICKET-0064 rejection path). The observation
            // interrupt lands the item AND replays other active runs holding
            // the ticket (decision 1 cross-run broadcast) — a bare
            // transitionItem would leave those members falsely done.
            await core.update({ id: nodeId, status: 'open' })
            item = core.getRunItem(id, nodeId) as OmtRunItem
          }
          // The item transition already reached the hub through bridgeRunEvents.
          return ok({ run: runSummary(core, core.requireRun(id)), item: runItemView(core, item) })
        }
        case 'filters-get': {
          const parsed = filtersGetPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid filters-get payload', parsed.error.issues)
          const core = await coreFor(parsed.data.sessionId)
          return ok(await core.savedFilters())
        }
        case 'filters-set': {
          const parsed = filtersSetPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid filters-set payload', parsed.error.issues)
          const core = await coreFor(parsed.data.sessionId)
          // Merge the partial patch onto the saved bag so the client can
          // send single-field updates; the merged result is fully validated.
          const current = await core.savedFilters()
          return ok(await core.saveSavedFilters({ ...current, ...parsed.data.filters }))
        }
        default:
          return badRequest(`unknown endpoint: ${endpoint}`, [])
      }
    } catch (error) {
      if (error instanceof OmtError) return failure(`${error.code}: ${error.message}`)
      throw error
    }
  }, { authority: 'loopback' })
}
