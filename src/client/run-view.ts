/**
 * Run-view derivations (STORY-0013): pure helpers shared by the RunsView
 * component and the tests — list grouping (TICKET-0068 layout), run-level
 * control availability, and item-level action eligibility. Host rules
 * mirrored: src/host/core.ts (startRun/pauseRun/resumeRun/cancelRun/
 * retryItem/removeRunItem) — keep the two sides aligned.
 */
import type { RunItemState, RunSummary } from './store.ts'

/**
 * List grouping (TICKET-0068): the main list holds everything that is NOT
 * history — active runs AND interrupted runs (interrupted needs human
 * review and must stay visible). Terminal runs fold into the 历史 group.
 */
export function groupRuns(runs: readonly RunSummary[]): { main: RunSummary[]; history: RunSummary[] } {
  const main: RunSummary[] = []
  const history: RunSummary[] = []
  for (const entry of runs) {
    if (entry.history) history.push(entry)
    else main.push(entry)
  }
  return { main, history }
}

export type RunControlAction = 'start' | 'pause' | 'resume' | 'cancel'

/**
 * Run-level controls by status (host CONFLICT rules): start only from
 * pending; pause only while running; resume from paused/interrupted;
 * cancel from every non-terminal status.
 */
export function runControlActions(run: Pick<RunSummary, 'status'>): readonly RunControlAction[] {
  switch (run.status) {
    case 'pending':
      return ['start', 'cancel']
    case 'running':
      return ['pause', 'cancel']
    case 'paused':
    case 'interrupted':
      return ['resume', 'cancel']
    default:
      return []
  }
}

type ItemLike = { readonly state: RunItemState; readonly stalled?: boolean }

/**
 * Row-level retry (TICKET-0068): failed, interrupted (含打回产生的), and
 * stalled pending items (nudge budget exhausted) reset back to pending.
 * blocked/skipped items go back through a human ticket-status change +
 * replay, not retry.
 */
export function canRetryItem(item: ItemLike): boolean {
  if (item.state === 'failed' || item.state === 'interrupted') return true
  return item.state === 'pending' && item.stalled === true
}

/**
 * Row-level remove (TICKET-0068): the 一键加入 undo path. In-flight items
 * (running / awaiting_confirmation) cannot be removed host-side.
 */
export function canRemoveItem(item: ItemLike): boolean {
  return item.state !== 'running' && item.state !== 'awaiting_confirmation'
}

/** 确认入口 (TICKET-0070): only awaiting_confirmation items confirm/reject. */
export function canConfirmItem(item: ItemLike): boolean {
  return item.state === 'awaiting_confirmation'
}
