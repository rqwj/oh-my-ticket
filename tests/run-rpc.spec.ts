/**
 * Run RPC endpoint tests (STORY-0013 host side / TICKET-0071): the UI-facing
 * `/omt` run endpoints — run-list/run-show/run-control/run-create/run-add/
 * run-confirm — plus the run membership projection on `get`. Runs against a
 * real OmtCore through the pool, exactly like rpc.spec.ts.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { bridgeRunEvents, ChangeHub, type OmtChangeEvent } from '../src/host/changes.ts'
import { OmtCore } from '../src/host/core.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { registerOmtRpc } from '../src/host/rpc.ts'
import { RunningRegistry } from '../src/host/running.ts'
import { NUDGE_BUDGET } from '../src/host/types.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

type Handler = (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>

const SESSION = 'session-ui-1'
const WS_SESSION = 'session-ui-ws'

let home: string
let core: OmtCore
let pool: OmtCorePool
let hub: ChangeHub
let running: RunningRegistry
let handler: Handler
let events: OmtChangeEvent[]
let followups: { sessionId: string; message: any }[]
let agentCwd: Map<string, string>

/** The pool's own core (opened in beforeEach so the janitor never demotes fixtures). */
async function pcore(): Promise<OmtCore> {
  return pool.coreForHome(home)
}

/** epic → story → n tickets through the fixture core; returns [story, tickets]. */
async function storyFixture(count: number) {
  const epic = await core.create({ type: 'epic', title: '界面' })
  const story = await core.create({ type: 'story', title: '运行面板', parentId: epic.id })
  const tickets = []
  for (let index = 0; index < count; index += 1) {
    tickets.push(await core.create({ type: 'ticket', title: `子任务${index + 1}`, parentId: story.id }))
  }
  return { story, tickets }
}

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-run-rpc-test-'))
  hub = new ChangeHub()
  running = new RunningRegistry()
  events = []
  followups = []
  agentCwd = new Map([[SESSION, '/nonexistent-omt-run-rpc-ws']])
  hub.subscribe(event => events.push(event))
  const stubCtx = {
    connection: {
      rpc: {
        handle(_channel: string, h: Handler, _options: { authority: string }) {
          handler = h
        },
      },
    },
    agents: {
      get(id: string) {
        const cwd = agentCwd.get(id)
        if (cwd === undefined) return undefined
        return {
          session: { header: { cwd } },
          followup(message: unknown) {
            followups.push({ sessionId: id, message })
          },
        }
      },
    },
  }
  // Production wiring (src/index.ts): every opened core bridges its run
  // events into the hub, so run/item transitions bump without explicit RPC
  // bumps.
  pool = new OmtCorePool(home, { onCoreOpened: opened => { bridgeRunEvents(opened, hub) } })
  registerOmtRpc(stubCtx as never, pool, undefined, hub, running)
  // Single opener per home (U2b owner lock): the fixture core IS the pool's
  // cached core. Open it up front: its startup janitor assumes no live
  // sessions, so fixtures with running items must be created afterwards.
  core = await pcore()
})

afterEach(async () => {
  await pool.closeAll()
  await rm(home, { recursive: true, force: true })
})

describe('run-list', () => {
  it('returns summaries with progress, stalled count, and active/history flags', async () => {
    const { tickets } = await storyFixture(3)
    const pc = await pcore()
    const run = await pc.createRun({ title: '批次A', nodeIds: tickets.map(t => t.id) })
    await pc.startRun(run.id)
    await pc.transitionItem(run.id, tickets[0]!.id, 'running', { executorSessionId: SESSION })
    await pc.transitionItem(run.id, tickets[0]!.id, 'done')
    for (let i = 0; i < NUDGE_BUDGET; i += 1) pc.recordItemNudge(run.id, tickets[1]!.id)
    const canceled = await pc.createRun({ nodeIds: [] })
    await pc.cancelRun(canceled.id)

    const result = await handler('run-list', { sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    const [first, second] = result.value.runs
    expect(first.id).toBe(run.id)
    expect(first.title).toBe('批次A')
    expect(first.status).toBe('running')
    expect(first.active).toBe(true)
    expect(first.history).toBe(false)
    expect(first.progress.total).toBe(3)
    expect(first.progress.done).toBe(1)
    expect(first.progress.pending).toBe(2)
    expect(first.stalled).toBe(1)
    expect(first.created_at).toBeDefined()
    expect(second.id).toBe(canceled.id)
    expect(second.history).toBe(true)
    expect(second.active).toBe(false)
  })

  it('keeps interrupted runs out of both the picker set and the history group', async () => {
    const { tickets } = await storyFixture(2)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: tickets.map(t => t.id) })
    await pc.startRun(run.id)
    await pc.transitionItem(run.id, tickets[0]!.id, 'running', { executorSessionId: 'sess-dead' })
    pc.janitorSweep(() => false)
    expect(pc.getRun(run.id)?.status).toBe('interrupted')

    const result = await handler('run-list', {}, new AbortController().signal)
    const entry = result.value.runs.find((r: any) => r.id === run.id)
    expect(entry.status).toBe('interrupted')
    expect(entry.active).toBe(false) // not addable / not in the picker
    expect(entry.history).toBe(false) // stays in the main list (TICKET-0068)
  })
})

describe('run-show', () => {
  it('returns config and items with node info and executor lineage', async () => {
    const { tickets } = await storyFixture(2)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: tickets.map(t => t.id), config: { stopOnFailure: true } })
    await pc.startRun(run.id)
    await pc.transitionItem(run.id, tickets[0]!.id, 'running', { executorSessionId: SESSION })
    running.start(tickets[0]!.id, SESSION, '面板会话', { parentSessionId: 'parent-1', isSubagent: true })

    const result = await handler('run-show', { id: run.id }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.run.config).toEqual({ stopOnFailure: true, autoContinue: true, autoVerify: false, concurrency: 1 })
    const [first, second] = result.value.items
    expect(first.node_id).toBe(tickets[0]!.id)
    expect(first.state).toBe('running')
    expect(first.node).toMatchObject({ id: tickets[0]!.id, title: '子任务1', status: 'open', archived: false })
    expect(first.executor).toEqual({ sessionId: SESSION, label: '面板会话', parentSessionId: 'parent-1', isSubagent: true })
    expect(second.state).toBe('pending')
    expect(second.executor).toBeUndefined()
    expect(second.stalled).toBeUndefined()
  })

  it('marks stalled pending items', async () => {
    const { tickets } = await storyFixture(1)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [tickets[0]!.id] })
    for (let i = 0; i < NUDGE_BUDGET; i += 1) pc.recordItemNudge(run.id, tickets[0]!.id)
    const result = await handler('run-show', { id: run.id }, new AbortController().signal)
    expect(result.value.items[0].stalled).toBe(true)
  })

  it('folds unknown run ids into an error result', async () => {
    const result = await handler('run-show', { id: 'RUN-9999' }, new AbortController().signal)
    expect(result.ok).toBe(false)
    expect(result.error.message).toContain('NOT_FOUND')
  })
})

describe('run-control', () => {
  it('start starts the run, bumps with a run hint, and followups the session to claim', async () => {
    const { tickets } = await storyFixture(1)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [tickets[0]!.id] })

    const result = await handler('run-control', { id: run.id, action: 'start', sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.run.status).toBe('running')
    const bump = events.find(event => event.run?.id === run.id)
    expect(bump?.run).toMatchObject({ id: run.id, kind: 'run' })
    expect(followups).toHaveLength(1)
    expect(followups[0]!.sessionId).toBe(SESSION)
    const text = followups[0]!.message.content[0].text as string
    expect(text).toContain(run.id)
    expect(text).toContain('omt_run_claim')
  })

  it('pause/resume/cancel forward to core and bump', async () => {
    const { tickets } = await storyFixture(1)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [tickets[0]!.id] })
    const signal = new AbortController().signal
    await handler('run-control', { id: run.id, action: 'start' }, signal)
    expect((await handler('run-control', { id: run.id, action: 'pause' }, signal)).value.run.status).toBe('paused')
    expect((await handler('run-control', { id: run.id, action: 'resume' }, signal)).value.run.status).toBe('running')
    expect((await handler('run-control', { id: run.id, action: 'cancel' }, signal)).value.run.status).toBe('canceled')
    expect(events.filter(event => event.run?.id === run.id).length).toBeGreaterThanOrEqual(4)
    // No sessionId on start: no injection, but the start still succeeds.
    expect(followups).toHaveLength(0)
  })

  it('retry resets a failed item; remove drops a pending item', async () => {
    const { tickets } = await storyFixture(2)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: tickets.map(t => t.id) })
    await pc.startRun(run.id)
    await pc.transitionItem(run.id, tickets[0]!.id, 'running', { executorSessionId: SESSION })
    await pc.transitionItem(run.id, tickets[0]!.id, 'failed', { error: '炸了' })
    const signal = new AbortController().signal

    const retried = await handler('run-control', { id: run.id, action: 'retry', nodeId: tickets[0]!.id }, signal)
    expect(retried.ok).toBe(true)
    expect(retried.value.item).toMatchObject({ node_id: tickets[0]!.id, state: 'pending', attempts: 1, last_error: '炸了' })

    const removed = await handler('run-control', { id: run.id, action: 'remove', nodeId: tickets[1]!.id }, signal)
    expect(removed.ok).toBe(true)
    expect(pc.runItems(run.id).map(item => item.node_id)).toEqual([tickets[0]!.id])
  })

  it('requires nodeId for retry/remove and rejects unknown actions', async () => {
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [] })
    const signal = new AbortController().signal
    const missing = await handler('run-control', { id: run.id, action: 'retry' }, signal)
    expect(missing.ok).toBe(false)
    expect(missing.error.code).toBe('bad-request')
    const unknown = await handler('run-control', { id: run.id, action: 'explode' }, signal)
    expect(unknown.ok).toBe(false)
    expect(unknown.error.code).toBe('bad-request')
  })
})

describe('run-create', () => {
  it('collects the subtree, skips done/archived, and snapshots in_progress executors', async () => {
    const { story, tickets } = await storyFixture(3)
    const [open, done, inProgress] = tickets
    await core.update({ id: done!.id, status: 'done' })
    await core.update({ id: inProgress!.id, status: 'in_progress' })
    running.start(inProgress!.id, SESSION, '面板会话', {})
    const pc = await pcore()
    const archived = await core.create({ type: 'ticket', title: '已归档', parentId: story.id })
    await core.update({ id: archived.id, archived: true })

    const result = await handler('run-create', { nodeIds: [story.id], title: '面板批次', sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.run.title).toBe('面板批次')
    expect(result.value.run.status).toBe('pending')
    expect(result.value.run.config).toBeUndefined() // list-style summary; config via run-show
    expect(result.value.added).toEqual([open!.id, inProgress!.id])
    expect(result.value.addedRunning).toEqual([inProgress!.id])
    expect(result.value.skippedDone).toBe(1)
    expect(result.value.skippedArchived).toBe(1)

    const items = pc.runItems(result.value.run.id)
    expect(items.map(item => [item.node_id, item.state])).toEqual([
      [open!.id, 'pending'],
      [inProgress!.id, 'running'],
    ])
    expect(items[1]!.executor_session_id).toBe(SESSION)
    expect(items[1]!.started_at).toBeDefined()
    expect(events.some(event => event.run?.id === result.value.run.id)).toBe(true)
  })

  it('treats epic/story/substory as context and collects only ticket/subticket work', async () => {
    const epic = await core.create({ type: 'epic', title: '发布背景' })
    const story = await core.create({ type: 'story', title: '执行范围', parentId: epic.id })
    const substory = await core.create({ type: 'substory', title: '补充背景', parentId: story.id })
    const nestedTicket = await core.create({ type: 'ticket', title: '嵌套任务', parentId: substory.id })
    const ticket = await core.create({ type: 'ticket', title: '直接任务', parentId: story.id })
    const subticket = await core.create({ type: 'subticket', title: '细分任务', parentId: ticket.id })
    const pc = await pcore()

    const result = await handler('run-create', { nodeIds: [epic.id], sessionId: SESSION }, new AbortController().signal)

    expect(result.ok).toBe(true)
    expect(result.value.added).toEqual([nestedTicket.id, ticket.id, subticket.id])
    expect(pc.runItems(result.value.run.id).map(item => item.node_id)).toEqual([
      nestedTicket.id,
      ticket.id,
      subticket.id,
    ])
  })

  it('an in_progress ticket WITHOUT a running mark joins as pending (re-dispatch)', async () => {
    const { story, tickets } = await storyFixture(2)
    const [open, inProgress] = tickets
    await core.update({ id: inProgress!.id, status: 'in_progress' })
    // 没有 running.start：RunningRegistry 无活跃标记的 in_progress 不是
    // 真实执行中，加入后应置 pending 让 run 重新派发。
    const pc = await pcore()

    const result = await handler('run-create', { nodeIds: [story.id], sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.added).toEqual([open!.id, inProgress!.id])
    expect(result.value.addedRunning).toEqual([])

    const items = pc.runItems(result.value.run.id)
    expect(items.map(item => [item.node_id, item.state])).toEqual([
      [open!.id, 'pending'],
      [inProgress!.id, 'pending'],
    ])
  })

  it('rejects members from different homes', async () => {
    const { tickets } = await storyFixture(2)
    const wsDir = await mkdtemp(join(tmpdir(), 'omt-run-rpc-ws-'))
    const wsCore = await OmtCore.open(join(wsDir, '.omt'))
    try {
      // Ids count per home: EPIC-0002 exists ONLY in the workspace home
      // (the global fixture holds EPIC-0001), so ownership resolves there.
      await wsCore.create({ type: 'epic', title: '本地占位' })
      const local = await wsCore.create({ type: 'epic', title: '本地票' })
      agentCwd.set(WS_SESSION, wsDir)
      const result = await handler(
        'run-create',
        { nodeIds: [local.id, tickets[0]!.id], sessionId: WS_SESSION },
        new AbortController().signal,
      )
      expect(result.ok).toBe(false)
      expect(result.error.message).toContain('home')
    } finally {
      wsCore.close()
      await rm(wsDir, { recursive: true, force: true })
    }
  })
})

describe('run-add', () => {
  it('appends subtree members with dedup, skip counts, and in_progress snapshots', async () => {
    const { story, tickets } = await storyFixture(3)
    const [member, done, inProgress] = tickets
    await core.update({ id: done!.id, status: 'done' })
    await core.update({ id: inProgress!.id, status: 'in_progress' })
    running.start(inProgress!.id, SESSION, '面板会话', {})
    const archived = await core.create({ type: 'ticket', title: '已归档', parentId: story.id })
    await core.update({ id: archived.id, archived: true })
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [member!.id] })

    const result = await handler('run-add', { id: run.id, nodeIds: [story.id], sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.added).toEqual([inProgress!.id])
    expect(result.value.addedRunning).toEqual([inProgress!.id])
    expect(result.value.duplicates).toEqual([member!.id])
    expect(result.value.skippedDone).toBe(1)
    expect(result.value.skippedArchived).toBe(1)

    const items = pc.runItems(run.id)
    expect(items.map(item => [item.node_id, item.position, item.state])).toEqual([
      [member!.id, 0, 'pending'],
      [inProgress!.id, 1, 'running'],
    ])
    expect(items[1]!.executor_session_id).toBe(SESSION)
    expect(events.some(event => event.run?.id === run.id)).toBe(true)
  })

  it('rejects terminal and interrupted runs with guidance', async () => {
    const { tickets } = await storyFixture(3)
    const pc = await pcore()
    const canceled = await pc.createRun({ nodeIds: [tickets[0]!.id] })
    await pc.cancelRun(canceled.id)
    const signal = new AbortController().signal
    const terminalResult = await handler('run-add', { id: canceled.id, nodeIds: [tickets[1]!.id] }, signal)
    expect(terminalResult.ok).toBe(false)
    expect(terminalResult.error.message).toMatch(/终态|另建/)

    const interrupted = await pc.createRun({ nodeIds: [tickets[0]!.id, tickets[1]!.id] })
    await pc.startRun(interrupted.id)
    await pc.transitionItem(interrupted.id, tickets[0]!.id, 'running', { executorSessionId: 'sess-dead' })
    pc.janitorSweep(() => false)
    const interruptedResult = await handler('run-add', { id: interrupted.id, nodeIds: [tickets[2]!.id] }, signal)
    expect(interruptedResult.ok).toBe(false)
    expect(interruptedResult.error.message).toContain('resume')
  })

  it('rejects cross-home members and unknown nodes', async () => {
    await storyFixture(1)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [] })
    const wsDir = await mkdtemp(join(tmpdir(), 'omt-run-rpc-ws-'))
    const wsCore = await OmtCore.open(join(wsDir, '.omt'))
    try {
      // EPIC-0002 exists only in the workspace home (global holds EPIC-0001).
      await wsCore.create({ type: 'epic', title: '本地占位' })
      const local = await wsCore.create({ type: 'epic', title: '本地票' })
      agentCwd.set(WS_SESSION, wsDir)
      const signal = new AbortController().signal
      const crossHome = await handler('run-add', { id: run.id, nodeIds: [local.id], sessionId: WS_SESSION }, signal)
      expect(crossHome.ok).toBe(false)
      expect(crossHome.error.message).toContain('home')
      const unknown = await handler('run-add', { id: run.id, nodeIds: ['TICKET-9999'] }, signal)
      expect(unknown.ok).toBe(false)
      expect(unknown.error.message).toContain('NOT_FOUND')
      expect(pc.runItems(run.id)).toEqual([]) // still empty
    } finally {
      wsCore.close()
      await rm(wsDir, { recursive: true, force: true })
    }
  })
})

describe('run-confirm', () => {
  /** Drive one item into awaiting_confirmation through the trust gate. */
  async function awaitingFixture() {
    const { tickets } = await storyFixture(1)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [tickets[0]!.id] })
    await pc.startRun(run.id)
    await pc.transitionItem(run.id, tickets[0]!.id, 'running', { executorSessionId: SESSION })
    // Bare done by the executor session (no report) → awaiting_confirmation.
    await pc.update({ id: tickets[0]!.id, status: 'done', executorSessionId: SESSION })
    expect(pc.getRunItem(run.id, tickets[0]!.id)?.state).toBe('awaiting_confirmation')
    return { pc, run, ticket: tickets[0]! }
  }

  it('confirm lands item done and ticket done, clearing the running mark', async () => {
    const { pc, run, ticket } = await awaitingFixture()
    // The gated bare update re-opened nothing; set the ticket back to the
    // in-progress execution state a real confirmation flow sees.
    await pc.update({ id: ticket.id, status: 'in_progress', executorSessionId: SESSION })
    running.start(ticket.id, SESSION, '面板会话', {})

    const result = await handler('run-confirm', { id: run.id, nodeId: ticket.id, decision: 'confirm' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.item.state).toBe('done')
    expect(pc.getNode(ticket.id)?.status).toBe('done')
    expect(running.get(ticket.id)).toBeUndefined()
    expect(events.some(event => event.run?.id === run.id)).toBe(true)
  })

  it('reject interrupts the item and reopens the ticket to open', async () => {
    const { pc, run, ticket } = await awaitingFixture()
    // 真实门控状态：ticket 已 done、item awaiting_confirmation（不重置）。
    expect(pc.getNode(ticket.id)?.status).toBe('done')

    const result = await handler('run-confirm', { id: run.id, nodeId: ticket.id, decision: 'reject' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.item.state).toBe('interrupted')
    // 打回重开 ticket（而不是保持 in_progress）。
    expect(pc.getNode(ticket.id)?.status).toBe('open')
    const bump = events.find(event => event.run?.id === run.id && event.run.kind === 'item')
    expect(bump?.run).toMatchObject({ id: run.id, kind: 'item', nodeId: ticket.id })
  })

  it('reject replays the same ticket’s item in another active run (cross-run broadcast)', async () => {
    const { tickets } = await storyFixture(2)
    const [ticket, other] = tickets
    const pc = await pcore()
    const runA = await pc.createRun({ nodeIds: [ticket!.id] })
    // runB needs a second pending member: a single-member run would derive
    // completed on the bare done below and leave the replay path.
    const runB = await pc.createRun({ nodeIds: [ticket!.id, other!.id] })
    await pc.startRun(runA.id)
    await pc.startRun(runB.id)
    await pc.transitionItem(runA.id, ticket!.id, 'running', { executorSessionId: SESSION })
    // Bare done by the executor: runA's item is gated to
    // awaiting_confirmation; runB's pending item lands done directly.
    await pc.update({ id: ticket!.id, status: 'done', executorSessionId: SESSION })
    expect(pc.getRunItem(runA.id, ticket!.id)?.state).toBe('awaiting_confirmation')
    expect(pc.getRunItem(runB.id, ticket!.id)?.state).toBe('done')
    expect(pc.getRun(runB.id)?.status).toBe('running')

    const result = await handler('run-confirm', { id: runA.id, nodeId: ticket!.id, decision: 'reject' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.item.state).toBe('interrupted')
    expect(pc.getNode(ticket!.id)?.status).toBe('open')
    // 回放：另一活跃 run 中已落 done 的同票 item 回退 pending（重跑）。
    expect(pc.getRunItem(runB.id, ticket!.id)?.state).toBe('pending')
    expect(pc.getRunItem(runB.id, other!.id)?.state).toBe('pending')
    expect(pc.getRun(runB.id)?.status).toBe('running')
  })

  it('rejects items that are not awaiting_confirmation', async () => {
    const { tickets } = await storyFixture(1)
    const pc = await pcore()
    const run = await pc.createRun({ nodeIds: [tickets[0]!.id] })
    await pc.startRun(run.id)
    const result = await handler('run-confirm', { id: run.id, nodeId: tickets[0]!.id, decision: 'confirm' }, new AbortController().signal)
    expect(result.ok).toBe(false)
    expect(result.error.message).toContain('awaiting_confirmation')
  })
})

describe('get run memberships (TICKET-0068 ticket detail run links)', () => {
  it('get includes the node’s non-terminal runs with item state and progress', async () => {
    const { tickets } = await storyFixture(1)
    const pc = await pcore()
    const active = await pc.createRun({ title: '进行中的批次', nodeIds: [tickets[0]!.id] })
    const history = await pc.createRun({ nodeIds: [tickets[0]!.id] })
    await pc.cancelRun(history.id)

    const result = await handler('get', { id: tickets[0]!.id }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.runs).toHaveLength(1)
    expect(result.value.runs[0]).toMatchObject({
      id: active.id,
      title: '进行中的批次',
      status: 'pending',
      itemState: 'pending',
      progress: { total: 1, done: 0, failed: 0 },
    })
  })
})

describe('payload validation', () => {
  it('rejects invalid run payloads', async () => {
    const signal = new AbortController().signal
    for (const [endpoint, payload] of [
      ['run-show', {}],
      ['run-control', { id: 'RUN-0001' }],
      ['run-create', { nodeIds: [] }],
      ['run-add', { id: 'RUN-0001', nodeIds: 'TICKET-0001' }],
      ['run-confirm', { id: 'RUN-0001', nodeId: 'TICKET-0001', decision: 'maybe' }],
    ] as const) {
      const result = await handler(endpoint, payload, signal)
      expect(result.ok).toBe(false)
      expect(result.error.code).toBe('bad-request')
    }
  })
})
