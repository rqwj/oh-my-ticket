/**
 * Disposed hook (TICKET-0063 / EPIC-0003 decision 5, tier 2): subscribes to
 * the cordis `agent/disposed` event and settles run work whose executor
 * session just ended.
 *
 *  1. executor 是 subagent（session header origin==='subagent' 且
 *     parentSession 可读）：名下仍有 running item → 向父会话 **followup**
 *     （唤醒 idle 驱动；inject 不唤醒 idle 会话）注入兜底通知，含未完成的
 *     run/item 清单与 subagent 最终报告摘要（best-effort：取会话事件流里
 *     最后一条 assistant 文本）。父会话也不在 → 走 janitor 降级：
 *     item → interrupted。
 *  2. executor 是主会话：名下有未完项的 running run → janitor 降级（item →
 *     interrupted，无存活执行者的 running run → interrupted，等 resume +
 *     逐项 retry）。
 *
 * 降级复用 core.janitorSweep（以存活会话清单为谓词），与启动 janitor
 * （decision 12）语义一致；已终态的 run 不受影响。死会话的
 * RunningRegistry 标记一并清理。所有注入失败都被包容（warn），绝不抛出。
 */
import type { Context } from '@deepseek-ai/cordis'
import type { AgentsLike } from './agents-like.ts'
import { safeFollowup, type FollowupTargetLike } from './messages.ts'
import type { OmtCorePool } from './pool.ts'
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
export function registerOmtDisposedHook(ctx: Context, pool: OmtCorePool, running: RunningRegistry): void {
  const agents = (ctx as unknown as { agents?: AgentsLike<FollowupTarget> }).agents

  const warn = (message: string, error: unknown): void => {
    console.warn(`[omt] disposed-hook: ${message}`, error)
  }

  const followup = (agent: FollowupTarget, text: string): void => {
    safeFollowup(agent, text, warn)
  }

  const onDisposed = async (agent: DisposedAgentLike): Promise<void> => {
    const header = agent.session.header
    const core = await pool.coreFor(header.cwd)
    const owned = core.executorItems(agent.id)
    // A session with no run involvement changes nothing — and must not
    // trigger a sweep over unrelated runs.
    if (owned.length === 0) return

    // The dead session's running marks are stale by definition.
    for (const { id } of running.forSession(agent.id)) running.stop(id)

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
        }
        return
      }
      // 父会话也不在：落入下方的 janitor 降级。
    }

    // 主会话（或无父 subagent）：名下 running run 有未完项才降级；
    // 已全终态的介入不触发 sweep。
    const involved = owned.some(({ run, item }) => run.status === 'running' && !RUN_ITEM_FINAL_STATES.includes(item.state))
    if (!involved) return
    const live = new Set((agents?.list() ?? []).map(candidate => candidate.id))
    // Defensive: the disposed agent must read as dead even if the registry
    // still carried it when the event fired.
    live.delete(agent.id)
    core.janitorSweep(sessionId => live.has(sessionId))
  }

  const events = ctx as unknown as DisposedEvents
  events.on('agent/disposed', ({ agent }) => {
    void onDisposed(agent).catch((error: unknown) => warn(`disposed handling failed for agent "${agent.id}"`, error))
  })
}
