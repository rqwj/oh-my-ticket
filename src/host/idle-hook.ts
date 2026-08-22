/**
 * Idle hook (TICKET-0062 / EPIC-0003 decision 5, tier 1): subscribes to the
 * cordis `agent/status` event and acts when an agent turns idle:
 *
 *  1. 未收尾提醒 — the session's RunningRegistry still lists running tickets
 *     → one followup reminding to omt_update 收尾/说明进度. Debounced once per
 *     (session, ticket) in memory (execution state is transient by nature, so
 *     the reminder log is too — a host restart simply re-allows reminders).
 *  2. run 续跑 nudge — the session executes a RUNNING, autoContinue run with
 *     a pending item → followup 「继续下一项」. Paused runs are never nudged
 *     (decision 9); autoContinue=false runs only get the reminder.
 *
 * Nudge budget: one pending item is nudged at most NUDGE_BUDGET times with
 * exponential backoff (interval = base × 2^(count-1), first nudge
 * immediate); bookkeeping rides run_items.nudged_at/nudge_count (durable, so
 * the budget survives host restarts). Exhaustion leaves the item pending and
 * reads as stalled (isRunItemStalled) — a human retries it via
 * omt_run_control retry, which clears the budget. The budget is also the
 * loop guard: a nudge-driven turn that idles without progress hits the
 * backoff, then the ceiling, then silence.
 *
 * Backoff timers are unref'd (never hold the process open) and cleared on
 * plugin dispose; a firing timer revalidates everything (agent still idle,
 * run still running, item still pending) before nudging.
 */
import type { Context } from '@deepseek-ai/cordis'
import type { OmtCore } from './core.ts'
import { safeFollowup } from './messages.ts'
import type { OmtCorePool } from './pool.ts'
import type { RunningRegistry } from './running.ts'
import { NUDGE_BUDGET, type OmtRun, type OmtRunItem } from './types.ts'

/** Default backoff base interval between continuation nudges. */
const DEFAULT_BACKOFF_BASE_MS = 30_000

export interface IdleHookOptions {
  /** Base backoff interval in ms (doubled per nudge). Default 30s. */
  readonly backoffBaseMs?: number
  /** Clock (ms epoch) — injectable for tests. */
  readonly now?: () => number
  /** Timer primitives — injectable for tests. */
  readonly setTimer?: (fn: () => void, ms: number) => unknown
  readonly clearTimer?: (handle: unknown) => void
}

/** Structural face of the agent carried by `agent/status` payloads. */
interface AgentLike {
  readonly id: string
  readonly status: 'idle' | 'running'
  readonly session: { header: { cwd?: string } }
  followup(message: unknown): void
}

interface AgentStatusEvents {
  on(event: 'agent/status', listener: (payload: { agent: AgentLike; status: 'idle' | 'running' }) => void): unknown
}

/** Budget/backoff verdict for one pending item at one point in time. */
export type NudgeDecision =
  | { readonly kind: 'due' }
  | { readonly kind: 'backoff'; readonly waitMs: number }
  | { readonly kind: 'stalled' }

/**
 * Pure nudge policy: first nudge is due immediately; nudge k+1 (k ≥ 1)
 * requires base × 2^(k-1) ms since the last one; at NUDGE_BUDGET the item
 * is stalled (no more nudges until retry resets the budget).
 */
function nudgeDecision(
  item: Pick<OmtRunItem, 'nudge_count' | 'nudged_at'>,
  nowMs: number,
  baseMs: number,
): NudgeDecision {
  if (item.nudge_count >= NUDGE_BUDGET) return { kind: 'stalled' }
  if (item.nudge_count === 0 || item.nudged_at === undefined) return { kind: 'due' }
  const interval = baseMs * 2 ** (item.nudge_count - 1)
  const waitMs = Date.parse(item.nudged_at) + interval - nowMs
  return waitMs <= 0 ? { kind: 'due' } : { kind: 'backoff', waitMs }
}

function reminderLine(ticketId: string): string {
  return `${ticketId} 仍标记为执行中（running）：请用 omt_update 收尾（done/blocked/skipped），或用 append 说明当前进度。`
}

function nudgeLine(run: OmtRun, item: OmtRunItem): string {
  return `run ${run.id} 有待执行项：请继续下一项 ${item.node_id}`
    + '（omt_run_claim 认领后执行，完成后用 omt_run_report 报告结果）。'
}

/** Register the idle hook. Timers are cleaned up via the cordis effect lifecycle. */
export function registerOmtIdleHook(ctx: Context, pool: OmtCorePool, running: RunningRegistry, options: IdleHookOptions = {}): void {
  const baseMs = options.backoffBaseMs ?? DEFAULT_BACKOFF_BASE_MS
  const now = options.now ?? (() => Date.now())
  const setTimer = options.setTimer ?? ((fn: () => void, ms: number) => setTimeout(fn, ms))
  const clearTimer = options.clearTimer ?? ((handle: unknown) => { clearTimeout(handle as Parameters<typeof clearTimeout>[0]) })

  /** Once-per-(session, ticket) reminder log (in-memory; see module doc). */
  const reminded = new Set<string>()
  /** Armed backoff timers, keyed `${runId}:${nodeId}` (latest wins). */
  const timers = new Map<string, unknown>()

  const warn = (message: string, error: unknown): void => {
    console.warn(`[omt] idle-hook: ${message}`, error)
  }

  const followup = (agent: AgentLike, text: string): void => {
    safeFollowup(agent, text, warn)
  }

  const recordNudge = (core: OmtCore, runId: string, nodeId: string): void => {
    core.recordItemNudge(runId, nodeId, new Date(now()).toISOString())
  }

  const armBackoff = (agent: AgentLike, runId: string, nodeId: string, waitMs: number): void => {
    const key = `${runId}:${nodeId}`
    const existing = timers.get(key)
    if (existing !== undefined) clearTimer(existing)
    const handle = setTimer(() => {
      timers.delete(key)
      void onBackoffTimer(agent, runId, nodeId).catch((error: unknown) => warn('backoff timer failed', error))
    }, waitMs)
    // Never keep the host process alive for a nudge.
    ;(handle as { unref?: () => void }).unref?.()
    timers.set(key, handle)
  }

  /**
   * Shared nudge-decision dispatch (idle event and backoff timer): arms the
   * backoff timer or records the nudge, returning the nudge line when one
   * is due. Delivery is the caller's choice (merged vs direct followup).
   */
  const applyNudgeDecision = (agent: AgentLike, core: OmtCore, run: OmtRun, item: OmtRunItem): string | undefined => {
    const decision = nudgeDecision(item, now(), baseMs)
    if (decision.kind === 'stalled') return undefined
    if (decision.kind === 'backoff') {
      armBackoff(agent, run.id, item.node_id, decision.waitMs)
      return undefined
    }
    recordNudge(core, run.id, item.node_id)
    return nudgeLine(run, item)
  }

  /** Timer path: revalidate everything before spending budget. */
  const onBackoffTimer = async (agent: AgentLike, runId: string, nodeId: string): Promise<void> => {
    // Not idle anymore → drop; the next idle event re-evaluates from scratch.
    if (agent.status !== 'idle') return
    const core = await pool.coreFor(agent.session.header.cwd)
    const run = core.getRun(runId)
    if (run === undefined || run.status !== 'running' || !run.config.autoContinue) return
    const item = core.getRunItem(runId, nodeId)
    if (item === undefined || item.state !== 'pending') return
    const text = applyNudgeDecision(agent, core, run, item)
    if (text !== undefined) followup(agent, text)
  }

  const onIdle = async (agent: AgentLike): Promise<void> => {
    const sessionId = agent.id
    const lines: string[] = []

    // 1. 未收尾提醒 — once per (session, ticket); keys are pruned once the
    // ticket leaves the running registry so the set stays bounded.
    const runningEntries = running.forSession(sessionId)
    const activeKeys = new Set(runningEntries.map(({ id }) => `${sessionId}:${id}`))
    for (const key of reminded) {
      if (key.startsWith(`${sessionId}:`) && !activeKeys.has(key)) reminded.delete(key)
    }
    for (const { id } of runningEntries) {
      const key = `${sessionId}:${id}`
      if (reminded.has(key)) continue
      reminded.add(key)
      lines.push(reminderLine(id))
    }

    // 2. run 续跑 nudge — running runs only (paused excluded inside
    // continuationCandidates), autoContinue honored, budget enforced.
    const core = await pool.coreFor(agent.session.header.cwd)
    for (const { run, item } of core.continuationCandidates(sessionId)) {
      const text = applyNudgeDecision(agent, core, run, item)
      if (text !== undefined) lines.push(text)
    }

    // One merged followup per idle event (TICKET-0065 owns further merging).
    if (lines.length > 0) followup(agent, lines.join('\n'))
  }

  const events = ctx as unknown as AgentStatusEvents
  events.on('agent/status', ({ agent, status }) => {
    if (status !== 'idle') return
    void onIdle(agent).catch((error: unknown) => warn(`idle handling failed for agent "${agent.id}"`, error))
  })

  // Dispose path: drop every armed backoff timer with the plugin fiber.
  const disposeTimers = (): void => {
    for (const handle of timers.values()) clearTimer(handle)
    timers.clear()
  }
  const withEffect = ctx as unknown as { effect?: (body: () => Generator<() => void, void, unknown>) => void }
  withEffect.effect?.call(ctx, function* () {
    yield disposeTimers
  })
}
