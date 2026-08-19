/**
 * Run core tests (EPIC-0003 / STORY-0010): run + item state machines,
 * terminal derivation, and the six boundary semantics (stop-on-failure,
 * pause, retry, replay, resume, cancel), plus the startup janitor.
 * Runs are DB-only; nodes exist so membership validation has real targets.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { OmtStore } from '../src/host/store.ts'
import { OmtError } from '../src/host/types.ts'
import { ticketFixture } from './mocks/fixtures.ts'

let home: string
let core: OmtCore

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-run-core-'))
  core = await OmtCore.open(home)
})

afterEach(async () => {
  core.close()
  await rm(home, { recursive: true, force: true })
})

/** Standard fixture: epic → story → `count` tickets (shared helper). */
function fixture(count = 3) {
  return ticketFixture(core, count)
}

describe('createRun', () => {
  it('creates a run with pending items ordered by position', async () => {
    const tickets = await fixture()
    const run = await core.createRun({ title: '第一批次', nodeIds: tickets.map(t => t.id) })

    expect(run.id).toBe('RUN-0001')
    expect(run.status).toBe('pending')
    expect(run.config).toEqual({ stopOnFailure: false, autoContinue: true, autoVerify: false, concurrency: 1 })

    const items = core.runItems(run.id)
    expect(items.map(item => item.node_id)).toEqual(tickets.map(t => t.id))
    expect(items.map(item => item.position)).toEqual([0, 1, 2])
    expect(items.every(item => item.state === 'pending')).toBe(true)
    expect(items.every(item => item.attempts === 0 && item.nudge_count === 0)).toBe(true)
  })

  it('rejects unknown members, duplicates, and unknown run ids', async () => {
    const [ticket] = await fixture(1)
    await expect(core.createRun({ nodeIds: ['TICKET-9999'] })).rejects.toThrow(OmtError)
    await expect(core.createRun({ nodeIds: [ticket.id, ticket.id] })).rejects.toThrow(/duplicate/i)
    await expect(core.createRun({ nodeIds: [], config: { concurrency: 0 } })).rejects.toThrow(/concurrency/)
    expect(() => core.runItems('RUN-9999')).toThrow(OmtError)
  })
})

describe('run state machine', () => {
  it('walks pending → running → paused → running and rejects illegal moves', async () => {
    const tickets = await fixture()
    const run = await core.createRun({ nodeIds: tickets.map(t => t.id) })

    // pending: only start/cancel are legal.
    await expect(core.pauseRun(run.id)).rejects.toThrow(/paused/)
    await expect(core.resumeRun(run.id)).rejects.toThrow()

    await core.startRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('running')
    await expect(core.startRun(run.id)).rejects.toThrow(/running/)

    await core.pauseRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('paused')
    await expect(core.pauseRun(run.id)).rejects.toThrow(/paused/)

    await core.resumeRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('running')
  })

  it('rejects every transition out of absolute terminal states', async () => {
    const tickets = await fixture(1)
    const run = await core.createRun({ nodeIds: tickets.map(t => t.id) })
    await core.startRun(run.id)
    await core.transitionItem(run.id, tickets[0]!.id, 'running')
    await core.transitionItem(run.id, tickets[0]!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed')

    await expect(core.startRun(run.id)).rejects.toThrow(OmtError)
    await expect(core.pauseRun(run.id)).rejects.toThrow(OmtError)
    await expect(core.resumeRun(run.id)).rejects.toThrow(OmtError)
    await expect(core.cancelRun(run.id)).rejects.toThrow(OmtError)
  })

  it('starts an empty run straight into completed (vacuous success)', async () => {
    const run = await core.createRun({ nodeIds: [] })
    await core.startRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('completed')
  })
})

describe('item state machine', () => {
  it('walks pending → running → done with executor and timestamps', async () => {
    const [ticket] = await fixture(1)
    const run = await core.createRun({ nodeIds: [ticket.id] })
    await core.startRun(run.id)

    const item = await core.transitionItem(run.id, ticket.id, 'running', { executorSessionId: 'sess-1' })
    expect(item.state).toBe('running')
    expect(item.executor_session_id).toBe('sess-1')
    expect(item.started_at).toBeDefined()

    const done = await core.transitionItem(run.id, ticket.id, 'done')
    expect(done.state).toBe('done')
    expect(done.finished_at).toBeDefined()
  })

  it('rejects illegal item transitions and unknown states', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)

    // pending can go to running (dispatch) or done/blocked/skipped (passive
    // observation direct sets, TICKET-0061) — but not to executor-side states.
    await expect(core.transitionItem(run.id, a!.id, 'failed')).rejects.toThrow(/pending/)
    await expect(core.transitionItem(run.id, a!.id, 'awaiting_confirmation')).rejects.toThrow(/pending/)
    await expect(core.transitionItem(run.id, a!.id, 'interrupted')).rejects.toThrow(/pending/)
    await expect(core.transitionItem(run.id, a!.id, 'bogus' as never)).rejects.toThrow(/unknown/i)
    await expect(core.transitionItem(run.id, 'TICKET-9999', 'running')).rejects.toThrow(OmtError)

    // done is final for direct transitions (replay is a dedicated method).
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    await expect(core.transitionItem(run.id, a!.id, 'failed')).rejects.toThrow(/done/)

    // Items are frozen once the run is terminal: no more dispatch.
    await core.transitionItem(run.id, b!.id, 'running')
    await core.transitionItem(run.id, b!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed')
    await expect(core.transitionItem(run.id, a!.id, 'running')).rejects.toThrow(/completed/)
  })

  it('supports running → awaiting_confirmation → done', async () => {
    const [ticket] = await fixture(1)
    const run = await core.createRun({ nodeIds: [ticket.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, ticket.id, 'running', { executorSessionId: 'sess-1' })
    const awaiting = await core.transitionItem(run.id, ticket.id, 'awaiting_confirmation')
    expect(awaiting.state).toBe('awaiting_confirmation')
    // awaiting_confirmation is not final: the run stays running.
    expect(core.getRun(run.id)?.status).toBe('running')
    await core.transitionItem(run.id, ticket.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed')
  })
})

describe('terminal derivation', () => {
  it('all done/skipped → completed', async () => {
    const [a, b, c] = await fixture()
    const run = await core.createRun({ nodeIds: [a!.id, b!.id, c!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    await core.transitionItem(run.id, c!.id, 'skipped')
    expect(core.getRun(run.id)?.status).toBe('running')
    await core.transitionItem(run.id, b!.id, 'running')
    await core.transitionItem(run.id, b!.id, 'done')

    const finished = core.getRun(run.id)
    expect(finished?.status).toBe('completed')
    expect(finished?.finished_at).toBeDefined()
  })

  it('a failed item → completed_with_failures', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'failed', { error: '编译失败' })
    expect(core.getRunItem(run.id, a!.id)?.last_error).toBe('编译失败')
    await core.transitionItem(run.id, b!.id, 'running')
    await core.transitionItem(run.id, b!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')
  })

  it('an interrupted item counts as failure → completed_with_failures', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-dead' })
    await core.transitionItem(run.id, a!.id, 'interrupted')
    await core.transitionItem(run.id, b!.id, 'running')
    await core.transitionItem(run.id, b!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')
  })
})

describe('boundary semantics (TICKET-0055)', () => {
  it('stop-on-failure: only failed pauses the run; blocked/skipped do not', async () => {
    const [a, b, c, d] = await fixture(4)
    const run = await core.createRun({ config: { stopOnFailure: true }, nodeIds: [a!.id, b!.id, c!.id, d!.id] })
    await core.startRun(run.id)

    // blocked does not trigger.
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'blocked')
    expect(core.getRun(run.id)?.status).toBe('running')

    // skipped does not trigger.
    await core.transitionItem(run.id, b!.id, 'skipped')
    expect(core.getRun(run.id)?.status).toBe('running')

    // failed triggers: run pauses, pending items stay pending.
    await core.transitionItem(run.id, c!.id, 'running')
    await core.transitionItem(run.id, c!.id, 'failed', { error: '炸了' })
    expect(core.getRun(run.id)?.status).toBe('paused')
    expect(core.getRunItem(run.id, d!.id)?.state).toBe('pending')
  })

  it('pause stops dispatch but in-flight items keep advancing', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-1' })
    await core.pauseRun(run.id)

    // Dispatch of a pending item is stopped…
    await expect(core.transitionItem(run.id, b!.id, 'running')).rejects.toThrow(/paused/)
    // …but the in-flight item can still be observed to completion, and the
    // run can still derive its terminal state from paused.
    await core.transitionItem(run.id, a!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('paused')
  })

  it('retry resets failed/interrupted/stalled items in place and clears the nudge budget', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-1' })
    await core.transitionItem(run.id, a!.id, 'failed', { error: '编译失败' })

    // Simulate idle-hook nudge budget consumption (TICKET-0062 bookkeeping).
    const store = await OmtStore.open(join(home, 'omt.db'))
    store.updateRunItem(run.id, a!.id, { nudge_count: 3, nudged_at: '2026-08-19T00:00:00.000Z' })
    store.close()

    const retried = await core.retryItem(run.id, a!.id)
    expect(retried.state).toBe('pending')
    expect(retried.attempts).toBe(1)
    expect(retried.last_error).toBe('编译失败') // error history is kept
    expect(retried.nudge_count).toBe(0) // new attempt gets a fresh nudge budget
    expect(retried.nudged_at).toBeUndefined()
    expect(retried.executor_session_id).toBeUndefined()
    expect(retried.started_at).toBeUndefined()
    expect(retried.finished_at).toBeUndefined()

    // Stalled pending items are retryable too (attempts still increment).
    const stalled = await core.retryItem(run.id, b!.id)
    expect(stalled.state).toBe('pending')
    expect(stalled.attempts).toBe(1)

    // done items are not retryable.
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    await expect(core.retryItem(run.id, a!.id)).rejects.toThrow(/done/)
  })

  it('replay returns done/blocked/skipped items to pending, keeping position', async () => {
    const [a, b, c, d] = await fixture(4)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id, c!.id, d!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    await core.transitionItem(run.id, b!.id, 'running')
    await core.transitionItem(run.id, b!.id, 'blocked')
    await core.transitionItem(run.id, c!.id, 'skipped')
    // d stays pending, so the run is still in progress while replaying.
    expect(core.getRun(run.id)?.status).toBe('running')

    // The ticket is reopened (done → open): the item replays to pending.
    const replayed = await core.replayItem(run.id, a!.id)
    expect(replayed.state).toBe('pending')
    expect(replayed.position).toBe(0)
    expect(replayed.executor_session_id).toBeUndefined()
    expect(replayed.finished_at).toBeUndefined()

    await expect(core.replayItem(run.id, b!.id)).resolves.toMatchObject({ state: 'pending', position: 1 })
    await expect(core.replayItem(run.id, c!.id)).resolves.toMatchObject({ state: 'pending', position: 2 })
    // running/pending items have nothing to replay.
    await core.transitionItem(run.id, a!.id, 'running')
    await expect(core.replayItem(run.id, a!.id)).rejects.toThrow(/running/)
  })

  it('resume after stop-on-failure skips the failed item and continues dispatch', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ config: { stopOnFailure: true }, nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'failed')
    expect(core.getRun(run.id)?.status).toBe('paused')

    await core.resumeRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('running')
    // The failed item is not auto-reset; the pending item dispatches.
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('failed')
    await core.transitionItem(run.id, b!.id, 'running')
    await core.transitionItem(run.id, b!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')
  })

  it('cancel freezes items in place and never touches the ticket nodes', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.update({ id: a!.id, status: 'in_progress' })
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-1' })

    await core.cancelRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('canceled')
    // Items freeze in place — no state rewrite, no interrupted downgrade.
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('running')
    expect(core.getRunItem(run.id, b!.id)?.state).toBe('pending')
    // The ticket node status is untouched by the cancel.
    expect(core.getNode(a!.id)?.status).toBe('in_progress')
    expect(core.getNode(b!.id)?.status).toBe('open')

    // A canceled run accepts nothing: no retry, no resume, no item moves.
    await expect(core.retryItem(run.id, a!.id)).rejects.toThrow(/canceled/)
    await expect(core.resumeRun(run.id)).rejects.toThrow(/canceled/)
    await expect(core.transitionItem(run.id, a!.id, 'done')).rejects.toThrow(/canceled/)
  })
})

describe('startup janitor (TICKET-0056)', () => {
  it('demotes crash residue (running run/items) to interrupted on reopen', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-dead' })

    // Simulate a crash: close without settling and reopen the home.
    core.close()
    core = await OmtCore.open(home)

    const reopened = core.getRun(run.id)
    expect(reopened?.status).toBe('interrupted')
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('interrupted')
    expect(core.getRunItem(run.id, b!.id)?.state).toBe('pending')

    // interrupted → resume → running: only pending items dispatch again;
    // the interrupted item is NOT auto-reset (needs a row-level retry).
    await core.resumeRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('running')
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('interrupted')
    await core.transitionItem(run.id, b!.id, 'running', { executorSessionId: 'sess-2' })
    await core.transitionItem(run.id, b!.id, 'done')
    // All items final (a interrupted counts as failure) → derived.
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')

    // Retry reopens a completed_with_failures run back to running.
    const retried = await core.retryItem(run.id, a!.id)
    expect(retried.state).toBe('pending')
    expect(retried.attempts).toBe(1)
    expect(core.getRun(run.id)?.status).toBe('running')
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed')
  })

  it('keeps items whose executor session is still active', async () => {
    const [a, b, c] = await fixture()
    const run = await core.createRun({ nodeIds: [a!.id, b!.id, c!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-alive' })
    await core.transitionItem(run.id, b!.id, 'running', { executorSessionId: 'sess-dead' })

    core.close()
    core = await OmtCore.open(home, { activeSessionIds: ['sess-alive'] })

    // The live executor's item and the run itself survive the sweep.
    expect(core.getRun(run.id)?.status).toBe('running')
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('running')
    expect(core.getRunItem(run.id, b!.id)?.state).toBe('interrupted')
    expect(core.getRunItem(run.id, c!.id)?.state).toBe('pending')
  })

  it('paused runs stay paused; only their orphaned running items are demoted', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-dead' })
    await core.pauseRun(run.id)

    core.close()
    core = await OmtCore.open(home)

    expect(core.getRun(run.id)?.status).toBe('paused')
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('interrupted')
    expect(core.getRunItem(run.id, b!.id)?.state).toBe('pending')
  })

  it('derives the terminal state when the demotion finishes the last item', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    await core.transitionItem(run.id, b!.id, 'running', { executorSessionId: 'sess-dead' })

    core.close()
    core = await OmtCore.open(home)

    // interrupted item counts as failure; nothing left to interrupt at run level.
    expect(core.getRunItem(run.id, b!.id)?.state).toBe('interrupted')
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')
  })

  it('reindex never touches run data', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-1' })
    await core.transitionItem(run.id, a!.id, 'done')

    const result = await core.reindex()
    expect(result.nodes).toBeGreaterThan(0)

    expect(core.getRun(run.id)?.status).toBe('running')
    expect(core.runItems(run.id).map(item => [item.node_id, item.state])).toEqual([
      [a!.id, 'done'],
      [b!.id, 'pending'],
    ])
    // The RUN counter survives: the next run continues the sequence.
    const next = await core.createRun({ nodeIds: [a!.id] })
    expect(next.id).toBe('RUN-0002')
  })
})
