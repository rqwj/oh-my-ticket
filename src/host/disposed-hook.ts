/**
 * Disposed hook (TICKET-0063 / EPIC-0003 decision 5, tier 2): subscribes to
 * the cordis `agent/disposed` event and settles run work whose executor
 * session just ended.
 *
 *  1. executor 是 subagent（session header origin==='subagent' 且
 *     parentSession 可读）：名下仍有 running item → 向父会话 **followup**
 *     （唤醒 idle 驱动；inject 不唤醒 idle 会话）注入兜底通知，含未完成的
 *     run/item 清单与 subagent 最终报告摘要（best-effort：取会话事件流里
 *     最后一条 assistant 文本）。父会话也不在 → 走降级路径（见下）。
 *  2. executor 是主会话：名下有未完项的 running/paused run → 同一降级路径。
 *
 * U7a degradation note: the pre-daemon janitor sweep demoted dead sessions'
 * running items to interrupted IN-PROCESS. This daemon build owns lease
 * expiry (claims carry a bounded lease; the daemon's startup janitor demotes
 * on open) and exposes no mid-flight demotion RPC, so the adapter's sweep is
 * notify-only: outstanding items stay `running` until the daemon-side lease
 * path or a human retry settles them. The liveness probe below still runs —
 * it decides whether the parent handoff or the degraded warning fires.
 *
 * 死会话的 RunningRegistry 标记总是清理（无论是否参与 run）。所有注入失败都
 * 被包容（warn），绝不抛出。finalReport 提取保持 DSH-side（cordis-only，
 * 会话事件流不经 daemon）。
 */
import type { Context } from '@deepseek-ai/cordis'
import type { AgentsLike } from './agents-like.ts'
import { safeFollowup, type FollowupTargetLike } from './messages.ts'
import type { OmtService } from './service.ts'
import type { RunningRegistry } from './running.ts'
import { RUN_ITEM_FINAL_STATES } from './types.ts'

/** Structural face of the agent carried by `agent/disposed` payloads. */
interface DisposedAgentLike {
  readonly id: string
  readonly session: {
    readonly header: { cwd?: string; origin?: 'subagent'; parentSession?: string }
    /** Live event log (best-effort final-report extraction). */
    readonly events?: readonly unknown[]
  }
}

type FollowupTarget = FollowupTargetLike

interface DisposedEvents {
  on(event: 'agent/disposed', listener: (payload: { agent: DisposedAgentLike }) => void): unknown
}

/** Final-report summary cap (one followup line, not a transcript). */
const SUMMARY_LIMIT = 500

/**
 * Best-effort final report: the last assistant text in the disposed
 * session's event log, truncated. Undefined when the log is unavailable.
 */
function finalReportOf(agent: DisposedAgentLike): string | undefined {
  const events = agent.session.events
  if (!Array.isArray(events)) return undefined
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index] as { type?: string; message?: { content?: unknown } } | undefined
    if (event?.type !== 'assistant/message') continue
    const content = event.message?.content
    if (!Array.isArray(content)) continue
    const text = content
      .map(block => ((block as { type?: string; text?: string }).type === 'text' ? (block as { text?: string }).text : undefined))
      .filter((part): part is string => typeof part === 'string' && part.trim() !== '')
      .join('\n')
      .trim()
    if (text !== '') return text.length > SUMMARY_LIMIT ? `${text.slice(0, SUMMARY_LIMIT)}…` : text
  }
  return undefined
}

/** Register the disposed hook. */
export function registerOmtDisposedHook(ctx: Context, service: OmtService, running: RunningRegistry): void {
  const agents = (ctx as unknown as { agents?: AgentsLike<FollowupTarget> }).agents

  const warn = (message: string, error: unknown): void => {
    console.warn(`[omt] disposed-hook: ${message}`, error)
  }

  const followup = (agent: FollowupTarget, text: string): void => {
    safeFollowup(agent, text, warn)
  }

  /** One degraded-sweep warning per process (the condition is structural). */
  let warnedSweepDegraded = false

  /**
   * Outstanding subagent→parent handoffs (TICKET-0063): parent session ids
   * that took over a disposed subagent's unfinished items. When such a
   * parent later disposes, the early return must not fire — the degraded
   * sweep's liveness predicate covers the dead child's orphaned items.
   * In-memory only: after a process restart the set is empty, which is
   * acceptable — the daemon-side lease/startup janitor covers that window.
   */
  const pendingHandoffs = new Set<string>()

  const onDisposed = async (agent: DisposedAgentLike): Promise<void> => {
    const header = agent.session.header
    // The dead session's running marks are stale by definition — always
    // clean them, regardless of run involvement.
    for (const { id } of running.forSession(agent.id)) running.stop(id)

    const owned = await service.executorItems(agent.id, header.cwd)
    // A session with no run involvement and no outstanding handoff changes
    // nothing — and must not trigger a sweep over unrelated runs.
    const handedOff = pendingHandoffs.delete(agent.id)
    if (owned.length === 0 && !handedOff) return

    const runningItems = owned.filter(({ item }) => item.state === 'running')

    if (header.origin === 'subagent' && header.parentSession !== undefined) {
      const parent = agents?.get(header.parentSession)
      if (parent !== undefined) {
        // 父会话兜底：通知它接管（报告或 retry），item 保持 running 等它处理。
        if (runningItems.length > 0) {
          const lines = runningItems.map(({ run, item }) => `- ${run.id} / ${item.node_id}（running）`)
          const summary = finalReportOf(agent)
          followup(parent, [
            '你委派的 subagent 会话已结束，但其执行的 run 项未完成：',
            ...lines,
            `subagent 最终报告摘要：${summary ?? '（不可得）'}`,
            '请核对结果：已完成用 omt_run_report 报告；未完成用 omt_run_control retry 重置后重新认领执行。',
          ].join('\n'))
          // 交接登记：父会话之后销毁时落入降级 sweep（存活谓词把死掉
          // 的 child 的 item 降级），而不是因子会话已死、父会话名下无项
          // 而漏清。
          pendingHandoffs.add(header.parentSession)
        }
        return
      }
      // 父会话也不在：落入下方的降级处理。
    }

    // 主会话（或无父 subagent）：名下 running/paused run 有未完项（paused
    // run 与 executorItems 的候选口径一致），或有未了结的 subagent 交接，才
    // 进入降级处理；已全终态的介入不触发。
    const involved = handedOff || owned.some(({ run, item }) => (run.status === 'running' || run.status === 'paused') && !RUN_ITEM_FINAL_STATES.includes(item.state))
    if (!involved) return
    // Degraded sweep: this daemon build has no mid-flight demotion RPC —
    // items stay running until the daemon-side lease path settles them.
    if (!warnedSweepDegraded) {
      warnedSweepDegraded = true
      warn('janitor sweep unavailable over RPC; outstanding items await daemon lease expiry or manual retry', undefined)
    }
  }

  const events = ctx as unknown as DisposedEvents
  events.on('agent/disposed', ({ agent }) => {
    void onDisposed(agent).catch((error: unknown) => warn(`disposed handling failed for agent "${agent.id}"`, error))
  })
}
