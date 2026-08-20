/**
 * Run notifier (TICKET-0065 / EPIC-0003 decision 5, tier 3): subscribes to
 * core run events (one attach per core, via the pool's onCoreOpened) and
 * closes the notification loop towards the executor sessions:
 *
 *  1. item 完成逐个通知 — done/failed/blocked/skipped → inject 一条进度
 *     （「RUN-0003 进度 7/12：TICKET-0041 done」）。inject 不唤醒 idle
 *     会话：item 进度属低优信息，执行会话 running 时即时可见。
 *  2. paused 待决通知 — stop-on-failure 把 run 转 paused 时 → followup
 *     （唤醒）注入失败项 + last_error + resume/cancel/retry 选项。识别点
 *     是 failed item 事件携带的 post-change run 快照（status==='paused'
 *     且 stopOnFailure 开启）：人工 pause 没有 failed item 事件，不通知。
 *  3. awaiting_confirmation 待确认提示 → inject（item 标识 + 确认/打回
 *     指引）；默认 autoVerify=false 下没有它 run 会静默卡壳。
 *  4. run 终态总结 — completed/completed_with_failures/canceled →
 *     followup（唤醒）注入各项计数 + 失败项 last_error；**interrupted
 *     终态不注入**（执行会话往往已销毁），走 UI 核对入口（TICKET-0068）。
 *
 * 合并/去重：同一 tick 内发往同一会话的多条通知合并为一条消息（wake
 * 优先——含 followup 类通知时整批走 followup），避免「最后一项完成 +
 * run 终态总结」「failed + paused 待决」这类同源事件连续注入。与 idle
 * nudge 的时序协调：nudge 只在 idle 事件触发、paused run 从不 nudge、
 * 待确认项不是 pending 不会被 nudge，两个钩子天然不撞车。
 *
 * 执行会话已销毁或通道抛错都被包容（warn），绝不抛出。
 */
import type { Context } from '@deepseek-ai/cordis'
import type { AgentsLike } from './agents-like.ts'
import type { OmtCore, OmtRunEvent } from './core.ts'
import { pluginUserMessage } from './messages.ts'
import { isRunHistory, RUN_ITEM_FINAL_STATES, type OmtRun, type OmtRunItem, type RunItemState } from './types.ts'

/** Structural agent face for delivery. */
interface NotifyTarget {
  readonly id: string
  followup(message: unknown): void
  inject(message: unknown): void
}

export interface OmtRunNotifier {
  /** Attach to one freshly opened core (pool onCoreOpened callback). */
  attach(core: OmtCore): void
}

/** Item states that earn a per-item completion notification. */
const NOTIFIED_ITEM_STATES: readonly RunItemState[] = ['done', 'failed', 'blocked', 'skipped']

function progressLine(core: OmtCore, run: OmtRun, item: OmtRunItem): string {
  const counts = core.runItemStateCounts(run.id)
  const total = counts.reduce((sum, entry) => sum + entry.count, 0)
  const finished = counts
    .filter(entry => (RUN_ITEM_FINAL_STATES as readonly string[]).includes(entry.state))
    .reduce((sum, entry) => sum + entry.count, 0)
  return `${run.id} 进度 ${finished}/${total}：${item.node_id} ${item.state}`
}

function pausedLines(run: OmtRun, item: OmtRunItem): string[] {
  return [
    `run ${run.id} 因失败暂停（stop-on-failure）：${item.node_id} failed`
      + (item.last_error !== undefined ? `（${item.last_error}）` : ''),
    '请决策：omt_run_control resume 续跑（跳过失败项）/ retry 重试失败项 / cancel 取消整个 run。',
  ]
}

function awaitingLine(run: OmtRun, item: OmtRunItem): string {
  return `${run.id} / ${item.node_id} 进入待确认（awaiting_confirmation）：ticket 未经 omt_run_report 直接落 done。`
    + '如确认完成，请用 omt_run_report 补报（outcome=done）；人工可在 run 详情确认（ticket 落 done）或打回（item 转 interrupted，ticket 重开为 open，可 retry 重跑）。'
}

function summaryLines(run: OmtRun, items: readonly OmtRunItem[]): string[] {
  // Same shape/order as a GROUP BY state scan (alphabetical by state).
  const counts = new Map<RunItemState, number>()
  for (const item of items) counts.set(item.state, (counts.get(item.state) ?? 0) + 1)
  const parts = [...counts.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([state, count]) => `${state} ×${count}`).join(' · ')
  const lines = [`run ${run.id} 已结束（${run.status}）：${parts === '' ? '无成员' : parts}`]
  for (const item of items) {
    if (item.state === 'failed' && item.last_error !== undefined) {
      lines.push(`- 失败：${item.node_id} — ${item.last_error}`)
    }
  }
  return lines
}

/** Distinct executor sessions of a run's items (terminal-summary targets). */
function executorSessions(items: readonly OmtRunItem[]): string[] {
  const sessions = new Set<string>()
  for (const item of items) {
    if (item.executor_session_id !== undefined) sessions.add(item.executor_session_id)
  }
  return [...sessions]
}

/** Create the notifier. Delivery batches per session per tick (wake wins). */
export function createOmtRunNotifier(ctx: Context): OmtRunNotifier {
  const agents = (ctx as unknown as { agents?: AgentsLike<NotifyTarget> }).agents

  const warn = (messageText: string, error: unknown): void => {
    console.warn(`[omt] notify-hook: ${messageText}`, error)
  }

  /** Per-session pending batch; flushed on one microtask. */
  interface Batch { lines: string[]; wake: boolean }
  const batches = new Map<string, Batch>()
  let flushScheduled = false

  const deliver = (sessionId: string, batch: Batch): void => {
    const agent = agents?.get(sessionId)
    if (agent === undefined) return // 执行会话已销毁：防御性跳过。
    const payload = pluginUserMessage(batch.lines.join('\n'))
    try {
      if (batch.wake) agent.followup(payload)
      else agent.inject(payload)
    } catch (error: unknown) {
      warn(`could not deliver notification to session "${sessionId}"`, error)
    }
  }

  const flush = (): void => {
    flushScheduled = false
    const pending = [...batches.entries()]
    batches.clear()
    for (const [sessionId, batch] of pending) deliver(sessionId, batch)
  }

  const enqueue = (sessionId: string | undefined, lines: string[], wake: boolean): void => {
    if (sessionId === undefined) return
    const batch = batches.get(sessionId) ?? { lines: [], wake: false }
    batch.lines.push(...lines)
    batch.wake = batch.wake || wake
    batches.set(sessionId, batch)
    if (!flushScheduled) {
      flushScheduled = true
      queueMicrotask(flush)
    }
  }

  const onEvent = (core: OmtCore, event: OmtRunEvent): void => {
    if (event.kind === 'item') {
      const item = event.item
      if (item === undefined || item.state === event.fromItemState) return
      if ((NOTIFIED_ITEM_STATES as readonly string[]).includes(item.state)) {
        const wake = item.state === 'failed' && event.run.status === 'paused' && event.run.config.stopOnFailure
        const lines = [progressLine(core, event.run, item)]
        if (wake) lines.push(...pausedLines(event.run, item))
        enqueue(item.executor_session_id, lines, wake)
      } else if (item.state === 'awaiting_confirmation') {
        enqueue(item.executor_session_id, [awaitingLine(event.run, item)], false)
      }
      return
    }
    // Run events: terminal summary only; interrupted 终态不注入.
    if (event.run.status === event.fromRunStatus) return
    if (isRunHistory(event.run.status)) {
      // One membership scan feeds both the summary lines and the
      // executor-session targets.
      const items = core.runItems(event.run.id)
      const lines = summaryLines(event.run, items)
      for (const sessionId of executorSessions(items)) {
        enqueue(sessionId, lines, true)
      }
    }
  }

  return {
    attach(core: OmtCore): void {
      core.onRunEvent(event => onEvent(core, event))
    },
  }
}
