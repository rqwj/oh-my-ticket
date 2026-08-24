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
import { bridgeRunEvents, ChangeHub, type OmtChangeEvent } from '../src/host/changes.ts'
import { OmtCore, type OmtRunEvent } from '../src/host/core.ts'
import { OmtStore } from '../src/host/store.ts'
import { isRunItemStalled, NUDGE_BUDGET, type OmtRun, type OmtRunItem, type RunConfig } from '../src/host/types.ts'
import { expectProblem, requireItem, ticketFixture } from './mocks/fixtures.ts'

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
    await expectProblem(core.createRun({ nodeIds: ['TICKET-9999'] }), 'NOT_FOUND', { kind: 'node', id: 'TICKET-9999' })
    await expectProblem(core.createRun({ nodeIds: [ticket!.id, ticket!.id] }), 'DUPLICATE_MEMBER', { nodeId: ticket!.id })
    await expectProblem(core.createRun({ nodeIds: [], config: { concurrency: 0 } }), 'INVALID_CONCURRENCY', { value: 0 })
    await expectProblem(Promise.resolve().then(() => core.runItems('RUN-9999')), 'NOT_FOUND', { kind: 'run', id: 'RUN-9999' })
  })

  it('rejects archived members up front (they could never accept a report)', async () => {
    const [a, b] = await fixture(2)
    await core.update({ id: a!.id, archived: true })
    await expectProblem(core.createRun({ nodeIds: [a!.id, b!.id] }), 'ARCHIVED_READONLY', { nodeId: a!.id, operation: 'run-membership' })
    // Restoring the node unblocks the run creation.
    await core.update({ id: a!.id, archived: false })
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    expect(run.status).toBe('pending')
  })

  it('rejects epic/story/substory containers as run members', async () => {
    const epic = await core.create({ type: 'epic', title: '背景' })
    const story = await core.create({ type: 'story', title: '范围', parentId: epic.id })
    const substory = await core.create({ type: 'substory', title: '补充背景', parentId: story.id })

    await expectProblem(core.createRun({ nodeIds: [epic.id] }), 'INVALID_INPUT', { rule: 'member-type', nodeId: epic.id, nodeType: 'epic' })
    await expectProblem(core.createRun({ nodeIds: [story.id] }), 'INVALID_INPUT', { rule: 'member-type', nodeType: 'story' })

    const run = await core.createRun({ nodeIds: [] })
    await expectProblem(core.addRunMembers(run.id, [{ nodeId: substory.id }]), 'INVALID_INPUT', { rule: 'member-type', nodeType: 'substory' })
  })
})

describe('legacy container run members', () => {
  it('skips persisted containers during claim/report without mutating their nodes', async () => {
    const epic = await core.create({ type: 'epic', title: '旧 Epic 背景' })
    const story = await core.create({ type: 'story', title: '旧 Story 背景', parentId: epic.id })
    const ticket = await core.create({ type: 'ticket', title: '实际任务', parentId: story.id })
    const storyBodyBefore = (await core.show(story.id)).body
    await core.update({ id: story.id, archived: true })
    const now = new Date().toISOString()
    const store = await OmtStore.open(join(home, 'omt.db'))
    const runId = store.nextRunId()
    store.insertRun({
      id: runId,
      status: 'running',
      config: { stopOnFailure: false, autoContinue: true, autoVerify: false, concurrency: 1 },
      created_at: now,
    })
    store.insertRunItem({ run_id: runId, node_id: epic.id, position: 0, state: 'pending', attempts: 0, nudge_count: 0 })
    store.insertRunItem({
      run_id: runId,
      node_id: story.id,
      position: 1,
      state: 'running',
      executor_session_id: 'legacy-session',
      attempts: 0,
      nudge_count: 0,
      started_at: now,
    })
    store.insertRunItem({ run_id: runId, node_id: ticket.id, position: 2, state: 'pending', attempts: 0, nudge_count: 0 })
    store.close()

    const claimed = await core.claimRunItem(runId, 'new-session')
    expect(claimed?.node_id).toBe(ticket.id)
    expect(core.getRunItem(runId, epic.id)?.state).toBe('skipped')

    const reported = await core.reportRunItem(runId, story.id, 'done', '不应写入容器')
    expect(reported.item.state).toBe('skipped')
    expect(reported.node).toMatchObject({ status: 'open', archived: true })
    expect((await core.show(story.id)).body).toBe(storyBodyBefore)
  })
})

describe('run state machine', () => {
  it('walks pending → running → paused → running and rejects illegal moves', async () => {
    const tickets = await fixture()
    const run = await core.createRun({ nodeIds: tickets.map(t => t.id) })

    // pending: only start/cancel are legal.
    await expectProblem(core.pauseRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'pending' })
    await expectProblem(core.resumeRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'pending' })

    await core.startRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('running')
    await expectProblem(core.startRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'running' })

    await core.pauseRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('paused')
    await expectProblem(core.pauseRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'paused' })

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

    await expectProblem(core.startRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'completed' })
    await expectProblem(core.pauseRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'completed' })
    await expectProblem(core.resumeRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'completed' })
    await expectProblem(core.cancelRun(run.id), 'CONFLICT', { rule: 'run-transition', from: 'completed', to: 'canceled' })
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
    await expectProblem(core.transitionItem(run.id, a!.id, 'failed'), 'CONFLICT', { rule: 'item-transition', from: 'pending', to: 'failed' })
    await expectProblem(core.transitionItem(run.id, a!.id, 'awaiting_confirmation'), 'CONFLICT', { rule: 'item-transition', from: 'pending' })
    await expectProblem(core.transitionItem(run.id, a!.id, 'interrupted'), 'CONFLICT', { rule: 'item-transition', from: 'pending' })
    await expectProblem(core.transitionItem(run.id, a!.id, 'bogus' as never), 'INVALID_INPUT', { field: 'to', value: 'bogus' })
    await expectProblem(core.transitionItem(run.id, 'TICKET-9999', 'running'), 'NOT_FOUND', { kind: 'run-item' })

    // done is final for direct transitions (replay is a dedicated method).
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    await expectProblem(core.transitionItem(run.id, a!.id, 'failed'), 'CONFLICT', { rule: 'item-transition', from: 'done' })

    // Items are frozen once the run is terminal: no more dispatch.
    await core.transitionItem(run.id, b!.id, 'running')
    await core.transitionItem(run.id, b!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed')
    await expectProblem(core.transitionItem(run.id, a!.id, 'running'), 'CONFLICT', { rule: 'items-frozen', runStatus: 'completed' })
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

  it('removing the last pending item after the rest finished derives completed', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.claimRunItem(run.id, 'sess-1')
    await core.reportRunItem(run.id, a!.id, 'done')
    // One final item, one still pending: the run is not derivable yet.
    expect(core.getRun(run.id)?.status).toBe('running')

    // Dropping the remaining pending item leaves only final states behind.
    await core.removeRunItem(run.id, b!.id)
    const finished = core.getRun(run.id)
    expect(finished?.status).toBe('completed')
    expect(finished?.finished_at).toBeDefined()
    expect(core.getRunItem(run.id, b!.id)).toBeUndefined()
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
    await expectProblem(core.transitionItem(run.id, b!.id, 'running'), 'CONFLICT', { rule: 'dispatch-paused', runStatus: 'paused', itemState: 'pending' })
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
    await expectProblem(core.retryItem(run.id, a!.id), 'CONFLICT', { rule: 'retry-state-gate', itemState: 'done' })
  })

  it('retry rejects a completed run (full success has no reopen path)', async () => {
    const [a] = await fixture(1)
    const run = await core.createRun({ nodeIds: [a!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed')

    // Unlike completed_with_failures, completed accepts no row-level retry.
    await expectProblem(core.retryItem(run.id, a!.id), 'CONFLICT', { rule: 'retry-run-gate', runStatus: 'completed' })
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
    await expectProblem(core.replayItem(run.id, a!.id), 'CONFLICT', { rule: 'replay-state-gate', itemState: 'running' })
  })

  it('replay clears the nudge budget: a replayed item is no longer stalled', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    // Exhaust the continuation-nudge budget on the pending item (TICKET-0062).
    for (let count = 0; count < NUDGE_BUDGET; count += 1) core.recordItemNudge(run.id, a!.id)
    expect(isRunItemStalled(requireItem(core, run.id, a!.id))).toBe(true)

    // done → open replays the item; like retry, it gets a fresh budget.
    await core.update({ id: a!.id, status: 'done' })
    await core.update({ id: a!.id, status: 'open' })
    const replayed = requireItem(core, run.id, a!.id)
    expect(replayed.state).toBe('pending')
    expect(replayed.nudge_count).toBe(0)
    expect(replayed.nudged_at).toBeUndefined()
    expect(isRunItemStalled(replayed)).toBe(false)
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
    // The status flip itself dispatches the item (passive observation rides
    // on core.update now — no separate transitionItem needed).
    await core.update({ id: a!.id, status: 'in_progress' })
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('running')

    await core.cancelRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('canceled')
    // Items freeze in place — no state rewrite, no interrupted downgrade.
    expect(core.getRunItem(run.id, a!.id)?.state).toBe('running')
    expect(core.getRunItem(run.id, b!.id)?.state).toBe('pending')
    // The ticket node status is untouched by the cancel.
    expect(core.getNode(a!.id)?.status).toBe('in_progress')
    expect(core.getNode(b!.id)?.status).toBe('open')

    // A canceled run accepts nothing: no retry, no resume, no item moves.
    await expectProblem(core.retryItem(run.id, a!.id), 'CONFLICT', { rule: 'retry-run-gate', runStatus: 'canceled' })
    await expectProblem(core.resumeRun(run.id), 'CONFLICT', { rule: 'run-status-gate', current: 'canceled' })
    await expectProblem(core.transitionItem(run.id, a!.id, 'done'), 'CONFLICT', { rule: 'items-frozen', runStatus: 'canceled' })
  })
})

describe('archived members (wedge fixes)', () => {
  it('claim skips archived members and derives the terminal state when the queue drains', async () => {
    const [a, b, c] = await fixture(3)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    // Archive b while the run is still pending: no observation fires, so the
    // archived member sits in the queue until claim time.
    await core.update({ id: b!.id, archived: true })
    await core.startRun(run.id)

    const claimed = await core.claimRunItem(run.id, 'sess-1')
    expect(claimed?.node_id).toBe(a!.id)
    // The archived member was marked skipped inside the claim transaction.
    expect(core.getRunItem(run.id, b!.id)?.state).toBe('skipped')

    // A run whose only member is archived drains to a terminal state.
    const solo = await core.createRun({ nodeIds: [c!.id] })
    await core.update({ id: c!.id, archived: true })
    await core.startRun(solo.id)
    expect(await core.claimRunItem(solo.id, 'sess-1')).toBeUndefined()
    expect(core.getRunItem(solo.id, c!.id)?.state).toBe('skipped')
    expect(core.getRun(solo.id)?.status).toBe('completed')
  })

  it('removeRunItem force-removes an in-flight item whose node is archived (wedge recovery)', async () => {
    const [a] = await fixture(1)
    const run = await core.createRun({ nodeIds: [a!.id] })
    await core.startRun(run.id)
    await core.claimRunItem(run.id, 'sess-1')

    // In-flight items are not removable while the node is live…
    await expectProblem(core.removeRunItem(run.id, a!.id), 'CONFLICT', { rule: 'remove-in-flight', itemState: 'running' })

    // …but a node archived out of band (here: cancel freezes observation,
    // like a reindex of hand-edited files would) wedges the item — reports
    // reject archived nodes. Forced removal recovers without touching the
    // ticket or canceling anything further.
    await core.cancelRun(run.id)
    await core.update({ id: a!.id, archived: true })
    await core.removeRunItem(run.id, a!.id)
    expect(core.getRunItem(run.id, a!.id)).toBeUndefined()
    expect(core.getNode(a!.id)?.status).toBe('open')
  })
})

describe('claim events (run-event broadcast)', () => {
  it('claimRunItem emits a pending → running item event that bridges into an SSE bump', async () => {
    const [a] = await fixture(1)
    const run = await core.createRun({ nodeIds: [a!.id] })
    await core.startRun(run.id)
    const hub = new ChangeHub()
    const detach = bridgeRunEvents(core, hub)
    const bumps: OmtChangeEvent[] = []
    hub.subscribe(event => bumps.push(event))

    const claimed = await core.claimRunItem(run.id, 'sess-1')
    detach()

    expect(claimed?.state).toBe('running')
    expect(claimed?.executor_session_id).toBe('sess-1')
    const bump = bumps.find(event => event.run?.kind === 'item' && event.run.nodeId === a!.id)
    expect(bump?.run).toMatchObject({ id: run.id, kind: 'item', nodeId: a!.id })
  })

  it('claimRunItem surfaces archived-member skips as item events (pending → skipped)', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    // Archive b while the run is still pending: no observation fires, so the
    // archived member sits in the queue until claim time.
    await core.update({ id: b!.id, archived: true })
    await core.startRun(run.id)
    const seen: OmtRunEvent[] = []
    const detach = core.onRunEvent(event => seen.push(event))

    const claimed = await core.claimRunItem(run.id, 'sess-1')
    detach()

    expect(claimed?.node_id).toBe(a!.id)
    expect(
      seen
        .filter(event => event.kind === 'item')
        .map(event => [event.item?.node_id, event.fromItemState, event.item?.state]),
    ).toEqual([
      [b!.id, 'pending', 'skipped'],
      [a!.id, 'pending', 'running'],
    ])
  })
})

describe('reportRunItem write order', () => {
  it('failed transitions the item before appending the note (no orphaned note on a stranded item)', async () => {
    const [a] = await fixture(1)
    const run = await core.createRun({ nodeIds: [a!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running', { executorSessionId: 'sess-1' })

    // Make the note-append fail (node file gone): the transition must
    // already have landed, matching the other outcomes' order.
    await rm(join(home, a!.path))
    await expectProblem(core.reportRunItem(run.id, a!.id, 'failed', '炸了'), 'IO')
    const item = requireItem(core, run.id, a!.id)
    expect(item.state).toBe('failed')
    expect(item.last_error).toBe('炸了')
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')
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

    // Resume flips back to running and the pending item dispatches again.
    await core.resumeRun(run.id)
    expect(core.getRun(run.id)?.status).toBe('running')
    await core.transitionItem(run.id, b!.id, 'running', { executorSessionId: 'sess-2' })
    await core.transitionItem(run.id, b!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')
  })

  it('derives the terminal state of a paused run when the demotion finishes its last in-flight item', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, a!.id, 'running')
    await core.transitionItem(run.id, a!.id, 'done')
    await core.transitionItem(run.id, b!.id, 'running', { executorSessionId: 'sess-dead' })
    await core.pauseRun(run.id)

    core.close()
    core = await OmtCore.open(home)

    expect(core.getRunItem(run.id, b!.id)?.state).toBe('interrupted')
    // Paused no longer skips terminal derivation: no silent stall after resume.
    expect(core.getRun(run.id)?.status).toBe('completed_with_failures')
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

describe('addRunMembers (TICKET-0067 host side)', () => {
  it('appends members after existing positions and reports duplicates', async () => {
    const [a, b, c] = await fixture(3)
    const run = await core.createRun({ nodeIds: [a!.id] })
    const result = await core.addRunMembers(run.id, [
      { nodeId: b!.id },
      { nodeId: a!.id }, // already a member
      { nodeId: b!.id }, // duplicate within the same call
      { nodeId: c!.id },
    ])
    expect(result.duplicates).toEqual([a!.id, b!.id])
    expect(result.added.map(item => [item.node_id, item.position, item.state])).toEqual([
      [b!.id, 1, 'pending'],
      [c!.id, 2, 'pending'],
    ])
    expect(core.runItems(run.id)).toHaveLength(3)
  })

  it('inserts in_progress members directly as running with an executor snapshot (no transition)', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id] })
    const { added } = await core.addRunMembers(run.id, [
      { nodeId: b!.id, state: 'running', executorSessionId: 'sess-ui' },
    ])
    const item = added[0]!
    expect(item.state).toBe('running')
    expect(item.executor_session_id).toBe('sess-ui')
    expect(item.started_at).toBeDefined()
    expect(item.attempts).toBe(0)
  })

  it('rejects terminal runs and interrupted runs (human review first)', async () => {
    const [a, b, c] = await fixture(3)
    const canceled = await core.createRun({ nodeIds: [a!.id] })
    await core.cancelRun(canceled.id)
    await expectProblem(core.addRunMembers(canceled.id, [{ nodeId: b!.id }]), 'CONFLICT', { rule: 'run-not-active', runStatus: 'canceled' })

    const interrupted = await core.createRun({ nodeIds: [a!.id, b!.id] })
    await core.startRun(interrupted.id)
    await core.transitionItem(interrupted.id, a!.id, 'running', { executorSessionId: 'sess-dead' })
    // Demote the only running item with pending work left: run → interrupted.
    core.janitorSweep(() => false)
    expect(core.getRun(interrupted.id)?.status).toBe('interrupted')
    await expectProblem(core.addRunMembers(interrupted.id, [{ nodeId: c!.id }]), 'CONFLICT', { rule: 'run-not-active', runStatus: 'interrupted' })
  })

  it('rejects archived and unknown members (unknown = foreign home)', async () => {
    const [a, b] = await fixture(2)
    const run = await core.createRun({ nodeIds: [a!.id] })
    await core.update({ id: b!.id, archived: true })
    await expectProblem(core.addRunMembers(run.id, [{ nodeId: b!.id }]), 'ARCHIVED_READONLY', { nodeId: b!.id, operation: 'run-membership' })
    await expectProblem(core.addRunMembers(run.id, [{ nodeId: 'TICKET-9999' }]), 'NOT_FOUND', { kind: 'node', id: 'TICKET-9999' })
    // A rejected batch member must not wedge the run: prior adds still stand.
    expect(core.runItems(run.id).map(item => item.node_id)).toEqual([a!.id])
  })
})

describe('runsOfNode (TICKET-0068 ticket detail run links)', () => {
  it('returns non-terminal runs holding the node, with the item state', async () => {
    const [a] = await fixture(1)
    const active = await core.createRun({ nodeIds: [a!.id] })
    const history = await core.createRun({ nodeIds: [a!.id] })
    await core.cancelRun(history.id)

    const memberships = core.runsOfNode(a!.id)
    expect(memberships.map(m => m.run.id)).toEqual([active.id])
    expect(memberships[0]!.item.state).toBe('pending')
    expect(core.runsOfNode('TICKET-9999')).toEqual([])
  })
})
