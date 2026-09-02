/**
 * Run RPC endpoint tests (STORY-0013 host side / TICKET-0071): the UI-facing
 * `/omt` run endpoints — run-list/run-show/run-control/run-create/run-add/
 * run-confirm — plus the run membership projection on `get`. U7a: runs
 * against a REAL omt-daemon through the runtime fixture.
 *
 * Documented rewrites (protocol gaps vs the pre-U7a core):
 *  - run-create no longer snapshots in_progress executors into running items
 *    (no add-members RPC → all members join pending; addedRunning is always
 *    empty — U7b open item).
 *  - run-add has NO daemon RPC: the endpoint now refuses with an actionable
 *    problem instead of appending members.
 *  - The interrupted fixtures use the daemon's own observation-interrupt
 *    path (claim then bare-done gating + reopen) instead of janitorSweep.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { registerOmtRpc } from '../src/host/rpc.ts'
import type { ChangeHub, OmtChangeEvent, OmtService } from '../src/host/service.ts'
import { RunningRegistry } from '../src/host/running.ts'
import { NUDGE_BUDGET } from '../src/host/types.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

type Handler = (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>

const SESSION = 'session-ui-1'
const WS_SESSION = 'session-ui-ws'

let fixture: RuntimeFixture
let service: OmtService
let hub: ChangeHub
let running: RunningRegistry
let handler: Handler
let events: OmtChangeEvent[]
let followups: { sessionId: string; message: any }[]
let agentCwd: Map<string, string>

/** epic → story → n tickets via the service; returns [story, tickets]. */
async function storyFixture(count: number) {
  const epic = await service.createNode(fixture.globalHome, { type: 'epic', title: '界面' })
  const story = await service.createNode(fixture.globalHome, { type: 'story', title: '运行面板', parentId: epic.id })
  const tickets = []
  for (let index = 0; index < count; index += 1) {
    tickets.push(await service.createNode(fixture.globalHome, { type: 'ticket', title: `子任务${index + 1}`, parentId: story.id }))
  }
  return { story, tickets }
}

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'run-rpc' })
  service = fixture.service
  hub = service.hub
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
  // Production wiring (src/index.ts): run/item transitions reach the hub
  // through the service's daemon-event bridge, so no explicit bumps here.
  registerOmtRpc(stubCtx as never, service, undefined, hub, running)
})

afterEach(async () => {
  await fixture.stop()
})

describe('run-list', () => {
  it('returns summaries with progress, stalled count, and active/history flags', async () => {
    const { tickets } = await storyFixture(3)
    const created = await service.createRun(fixture.globalHome, { title: '批次A', nodeIds: tickets.map(t => t.id) })
    await service.controlRun(fixture.globalHome, created.run.id, 'start')
    await service.claimItem(fixture.globalHome, created.run.id, SESSION)
    await service.reportItem(fixture.globalHome, created.run.id, tickets[0]!.id, 'done')
    for (let i = 0; i < NUDGE_BUDGET; i += 1) await service.recordItemNudge(fixture.globalHome, created.run.id, tickets[1]!.id, new Date().toISOString())
    // REWRITTEN for U7a: the daemon refuses empty-member runs (minItems 1),
    // so the history entry uses a real (canceled) member instead.
    const canceled = await service.createRun(fixture.globalHome, { nodeIds: [tickets[2]!.id] })
    await service.controlRun(fixture.globalHome, canceled.run.id, 'cancel')

    const result = await handler('run-list', { sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    const first = result.value.runs.find((r: any) => r.id === created.run.id)
    expect(first.title).toBe('批次A')
    expect(first.status).toBe('running')
    expect(first.active).toBe(true)
    expect(first.history).toBe(false)
    expect(first.progress.total).toBe(3)
    expect(first.progress.done).toBe(1)
    expect(first.progress.pending).toBe(2)
    expect(first.stalled).toBe(1)
    expect(first.created_at).toBeDefined()
    const second = result.value.runs.find((r: any) => r.id === canceled.run.id)
    expect(second.history).toBe(true)
    expect(second.active).toBe(false)
  })

  it('keeps observation-interrupted work out of the picker; terminal derivation seals the run', async () => {
    // REWRITTEN for U7a (protocol reality, documented): was pc.janitorSweep
    // (() => false) producing a run stuck at `interrupted`. The daemon has
    // no sweep RPC and its startup janitor keeps unexpired-lease items
    // running across restarts (LEASE_TTL_MS = 15min, hardcoded), so the
    // `interrupted` RUN status is unreachable through the adapter in test
    // time — it stays daemon-janitor/corpus-owned (U7b open item).
    // The reachable equivalent via the observation interrupt: claim, bare
    // done gates to awaiting_confirmation, reopen demotes the ITEM to
    // interrupted; the single-member run then derives
    // completed_with_failures (history group, not the picker).
    const { tickets } = await storyFixture(1)
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    await service.controlRun(fixture.globalHome, created.run.id, 'start')
    await service.claimItem(fixture.globalHome, created.run.id, 'sess-dead')
    await service.updateNode({ id: tickets[0]!.id, status: 'done' }, { sessionId: 'sess-dead' })
    await service.updateNode({ id: tickets[0]!.id, status: 'open' }, {})

    const snapshot = await service.fetchRun(fixture.globalHome, created.run.id)
    expect(snapshot.items[0]?.state).toBe('interrupted')
    expect(snapshot.run.status).toBe('completed_with_failures')

    const result = await handler('run-list', {}, new AbortController().signal)
    const entry = result.value.runs.find((r: any) => r.id === created.run.id)
    expect(entry.active).toBe(false) // not addable / not in the picker
    expect(entry.history).toBe(true)
  })
})

describe('run-show', () => {
  it('returns config and items with node info and executor lineage', async () => {
    const { tickets } = await storyFixture(2)
    const created = await service.createRun(fixture.globalHome, { nodeIds: tickets.map(t => t.id), config: { stopOnFailure: true } })
    await service.controlRun(fixture.globalHome, created.run.id, 'start')
    await service.claimItem(fixture.globalHome, created.run.id, SESSION)
    running.start(tickets[0]!.id, SESSION, '面板会话', { parentSessionId: 'parent-1', isSubagent: true })

    const result = await handler('run-show', { id: created.run.id }, new AbortController().signal)
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
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    for (let i = 0; i < NUDGE_BUDGET; i += 1) await service.recordItemNudge(fixture.globalHome, created.run.id, tickets[0]!.id, new Date().toISOString())
    const result = await handler('run-show', { id: created.run.id }, new AbortController().signal)
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
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })

    const result = await handler('run-control', { id: created.run.id, action: 'start', sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.run.status).toBe('running')
    const bump = events.find(event => event.run?.id === created.run.id)
    expect(bump?.run).toMatchObject({ id: created.run.id, kind: 'run' })
    expect(followups).toHaveLength(1)
    expect(followups[0]!.sessionId).toBe(SESSION)
    const text = followups[0]!.message.content[0].text as string
    expect(text).toContain(created.run.id)
    expect(text).toContain('omt_run_claim')
  })

  it('pause/resume/cancel forward to the daemon and bump', async () => {
    const { tickets } = await storyFixture(1)
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    const signal = new AbortController().signal
    await handler('run-control', { id: created.run.id, action: 'start' }, signal)
    expect((await handler('run-control', { id: created.run.id, action: 'pause' }, signal)).value.run.status).toBe('paused')
    expect((await handler('run-control', { id: created.run.id, action: 'resume' }, signal)).value.run.status).toBe('running')
    expect((await handler('run-control', { id: created.run.id, action: 'cancel' }, signal)).value.run.status).toBe('canceled')
    expect(events.filter(event => event.run?.id === created.run.id).length).toBeGreaterThanOrEqual(4)
    // No sessionId on start: no injection, but the start still succeeds.
    expect(followups).toHaveLength(0)
  })

  it('retry resets a failed item; remove drops a pending item', async () => {
    const { tickets } = await storyFixture(2)
    const created = await service.createRun(fixture.globalHome, { nodeIds: tickets.map(t => t.id) })
    await service.controlRun(fixture.globalHome, created.run.id, 'start')
    await service.claimItem(fixture.globalHome, created.run.id, SESSION)
    await service.reportItem(fixture.globalHome, created.run.id, tickets[0]!.id, 'failed', '炸了')
    const signal = new AbortController().signal

    const retried = await handler('run-control', { id: created.run.id, action: 'retry', nodeId: tickets[0]!.id }, signal)
    expect(retried.ok).toBe(true)
    expect(retried.value.item).toMatchObject({ node_id: tickets[0]!.id, state: 'pending', attempts: 1, last_error: '炸了' })

    const removed = await handler('run-control', { id: created.run.id, action: 'remove', nodeId: tickets[1]!.id }, signal)
    expect(removed.ok).toBe(true)
    // REWRITTEN for U7a: membership readback goes through the daemon detail
    // view instead of core.runItems.
    const snapshot = await service.fetchRun(fixture.globalHome, created.run.id)
    expect(snapshot.items.map(item => item.node_id)).toEqual([tickets[0]!.id])
  })

  it('requires nodeId for retry/remove and rejects unknown actions', async () => {
    const { tickets } = await storyFixture(1)
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    const signal = new AbortController().signal
    const missing = await handler('run-control', { id: created.run.id, action: 'retry' }, signal)
    expect(missing.ok).toBe(false)
    expect(missing.error.code).toBe('bad-request')
    const unknown = await handler('run-control', { id: created.run.id, action: 'explode' }, signal)
    expect(unknown.ok).toBe(false)
    expect(unknown.error.code).toBe('bad-request')
  })
})

describe('run-create', () => {
  it('collects the subtree and skips done/archived members', async () => {
    // REWRITTEN for U7a: in_progress members join as PENDING on this daemon
    // build (no per-member state override), so addedRunning is always [] and
    // the executor-snapshot assertions are dropped (U7b open item).
    const { story, tickets } = await storyFixture(3)
    const [open, done, inProgress] = tickets
    await service.updateNode({ id: done!.id, status: 'done' }, {})
    await service.updateNode({ id: inProgress!.id, status: 'in_progress' }, {})
    const archived = await service.createNode(fixture.globalHome, { type: 'ticket', title: '已归档', parentId: story.id })
    await service.updateNode({ id: archived.id, archived: true }, {})

    const result = await handler('run-create', { nodeIds: [story.id], title: '面板批次', sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.run.title).toBe('面板批次')
    expect(result.value.run.status).toBe('pending')
    expect(result.value.run.config).toBeUndefined() // list-style summary; config via run-show
    expect(result.value.added).toEqual([open!.id, inProgress!.id])
    expect(result.value.addedRunning).toEqual([])
    expect(result.value.skippedDone).toBe(1)
    expect(result.value.skippedArchived).toBe(1)

    const snapshot = await service.fetchRun(fixture.globalHome, result.value.run.id)
    expect(snapshot.items.map(item => [item.node_id, item.state])).toEqual([
      [open!.id, 'pending'],
      [inProgress!.id, 'pending'],
    ])
    expect(events.some(event => event.run?.id === result.value.run.id)).toBe(true)
  })

  it('treats epic/story/substory as context and collects only ticket/subticket work', async () => {
    const epic = await service.createNode(fixture.globalHome, { type: 'epic', title: '发布背景' })
    const story = await service.createNode(fixture.globalHome, { type: 'story', title: '执行范围', parentId: epic.id })
    const substory = await service.createNode(fixture.globalHome, { type: 'substory', title: '补充背景', parentId: story.id })
    const nestedTicket = await service.createNode(fixture.globalHome, { type: 'ticket', title: '嵌套任务', parentId: substory.id })
    const ticket = await service.createNode(fixture.globalHome, { type: 'ticket', title: '直接任务', parentId: story.id })
    const subticket = await service.createNode(fixture.globalHome, { type: 'subticket', title: '细分任务', parentId: ticket.id })

    const result = await handler('run-create', { nodeIds: [epic.id], sessionId: SESSION }, new AbortController().signal)

    expect(result.ok).toBe(true)
    expect(result.value.added).toEqual([nestedTicket.id, ticket.id, subticket.id])
    const snapshot = await service.fetchRun(fixture.globalHome, result.value.run.id)
    expect(snapshot.items.map(item => item.node_id)).toEqual([
      nestedTicket.id,
      ticket.id,
      subticket.id,
    ])
  })

  it('an in_progress ticket WITHOUT a running mark joins as pending (re-dispatch)', async () => {
    const { story, tickets } = await storyFixture(2)
    const [open, inProgress] = tickets
    await service.updateNode({ id: inProgress!.id, status: 'in_progress' }, {})
    // 没有 running.start：无活跃标记的 in_progress 不是真实执行中，加入后
    // 应置 pending 让 run 重新派发。（U7a：有标记也一样置 pending —— 见上）
    const result = await handler('run-create', { nodeIds: [story.id], sessionId: SESSION }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.added).toEqual([open!.id, inProgress!.id])
    expect(result.value.addedRunning).toEqual([])

    const snapshot = await service.fetchRun(fixture.globalHome, result.value.run.id)
    expect(snapshot.items.map(item => [item.node_id, item.state])).toEqual([
      [open!.id, 'pending'],
      [inProgress!.id, 'pending'],
    ])
  })

  it('rejects members from different homes', async () => {
    // Two homes on ONE daemon (workspace fixture): ids count per home, so
    // ownership resolution must split the members and refuse the mix.
    await fixture.stop()
    fixture = await createRuntimeFixture({ label: 'run-rpc-ws', workspace: true })
    service = fixture.service
    hub = service.hub
    events = []
    hub.subscribe(event => events.push(event))
    registerOmtRpc(({
      connection: { rpc: { handle: (_c: string, h: Handler) => { handler = h } } },
      agents: { get: (id: string) => (agentCwd.get(id) === undefined ? undefined : { session: { header: { cwd: agentCwd.get(id) } }, followup: (m: unknown) => followups.push({ sessionId: id, message: m }) }) },
    }) as never, service, undefined, hub, running)

    const globalStory = await storyFixture(1)
    const wsRoot = fixture.root + '/workspace'
    const wsEpic = await service.createNode(fixture.workspaceHome!, { type: 'epic', title: '本地占位' })
    const wsEpic2 = await service.createNode(fixture.workspaceHome!, { type: 'epic', title: '本地票' })
    agentCwd.set(WS_SESSION, wsRoot)
    const result = await handler(
      'run-create',
      { nodeIds: [wsEpic2.id, globalStory.tickets[0]!.id], sessionId: WS_SESSION },
      new AbortController().signal,
    )
    expect(result.ok).toBe(false)
    expect(result.error.message).toContain('home')
    void wsEpic
  })
})

// REWRITTEN for U7a: this daemon build has NO add-members RPC. The endpoint
// refuses with an actionable problem; membership stays untouched. (The old
// cases asserted append/dedup/skip semantics of the removed core flow.)
describe('run-add (refused: protocol gap)', () => {
  it('refuses with an actionable problem pointing at omt_run_create', async () => {
    const { tickets } = await storyFixture(1)
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    const signal = new AbortController().signal

    const result = await handler('run-add', { id: created.run.id, nodeIds: [tickets[0]!.id], sessionId: SESSION }, signal)
    expect(result.ok).toBe(false)
    expect(result.error.message).toContain('追加成员')
    expect(result.error.message).toContain('omt_run_create')

    const snapshot = await service.fetchRun(fixture.globalHome, created.run.id)
    expect(snapshot.items).toHaveLength(1) // unchanged
  })

  it('still validates its payload shape', async () => {
    const signal = new AbortController().signal
    const bad = await handler('run-add', { id: 'RUN-0001', nodeIds: 'TICKET-0001' }, signal)
    expect(bad.ok).toBe(false)
    expect(bad.error.code).toBe('bad-request')
  })
})

describe('run-confirm', () => {
  /** Drive one item into awaiting_confirmation through the trust gate. */
  async function awaitingFixture() {
    const { tickets } = await storyFixture(1)
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    await service.controlRun(fixture.globalHome, created.run.id, 'start')
    await service.claimItem(fixture.globalHome, created.run.id, SESSION)
    // Bare done by the executor session (no report) → awaiting_confirmation.
    await service.updateNode({ id: tickets[0]!.id, status: 'done' }, { cwd: undefined, sessionId: SESSION })
    const snapshot = await service.fetchRun(fixture.globalHome, created.run.id)
    expect(snapshot.items[0]?.state).toBe('awaiting_confirmation')
    return { runId: created.run.id, ticket: tickets[0]! }
  }

  it('confirm lands item done and ticket done, clearing the running mark', async () => {
    const { runId, ticket } = await awaitingFixture()
    running.start(ticket.id, SESSION, '面板会话', {})

    const result = await handler('run-confirm', { id: runId, nodeId: ticket.id, decision: 'confirm' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.item.state).toBe('done')
    expect((await service.getNodeIn(fixture.globalHome, ticket.id))?.status).toBe('done')
    expect(running.get(ticket.id)).toBeUndefined()
    expect(events.some(event => event.run?.id === runId)).toBe(true)
  })

  it('reject interrupts the item and reopens the ticket to open', async () => {
    const { runId, ticket } = await awaitingFixture()
    // 真实门控状态：ticket 已 done、item awaiting_confirmation（不重置）。
    expect((await service.getNodeIn(fixture.globalHome, ticket.id))?.status).toBe('done')

    const result = await handler('run-confirm', { id: runId, nodeId: ticket.id, decision: 'reject' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.item.state).toBe('interrupted')
    // 打回重开 ticket（而不是保持 in_progress）。
    expect((await service.getNodeIn(fixture.globalHome, ticket.id))?.status).toBe('open')
    const bump = events.find(event => event.run?.id === runId && event.run.kind === 'item')
    expect(bump?.run).toMatchObject({ id: runId, kind: 'item', nodeId: ticket.id })
  })

  it('reject replays the same ticket’s item in another active run (cross-run broadcast)', async () => {
    const { tickets } = await storyFixture(2)
    const [ticket, other] = tickets
    const runA = await service.createRun(fixture.globalHome, { nodeIds: [ticket!.id] })
    // runB needs a second pending member: a single-member run would derive
    // completed on the bare done below and leave the replay path.
    const runB = await service.createRun(fixture.globalHome, { nodeIds: [ticket!.id, other!.id] })
    await service.controlRun(fixture.globalHome, runA.run.id, 'start')
    await service.controlRun(fixture.globalHome, runB.run.id, 'start')
    await service.claimItem(fixture.globalHome, runA.run.id, SESSION)
    // Bare done by the executor: runA's item is gated to
    // awaiting_confirmation; runB's pending item lands done directly.
    await service.updateNode({ id: ticket!.id, status: 'done' }, { sessionId: SESSION })
    const snapA = await service.fetchRun(fixture.globalHome, runA.run.id)
    const snapB = await service.fetchRun(fixture.globalHome, runB.run.id)
    expect(snapA.items.find(i => i.node_id === ticket!.id)?.state).toBe('awaiting_confirmation')
    expect(snapB.items.find(i => i.node_id === ticket!.id)?.state).toBe('done')
    expect(snapB.run.status).toBe('running')

    const result = await handler('run-confirm', { id: runA.run.id, nodeId: ticket!.id, decision: 'reject' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.item.state).toBe('interrupted')
    expect((await service.getNodeIn(fixture.globalHome, ticket!.id))?.status).toBe('open')
    // 回放：另一活跃 run 中已落 done 的同票 item 回退 pending（重跑）。
    const afterB = await service.fetchRun(fixture.globalHome, runB.run.id)
    expect(afterB.items.find(i => i.node_id === ticket!.id)?.state).toBe('pending')
    expect(afterB.items.find(i => i.node_id === other!.id)?.state).toBe('pending')
    expect(afterB.run.status).toBe('running')
  })

  it('rejects items that are not awaiting_confirmation', async () => {
    const { tickets } = await storyFixture(1)
    const created = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    await service.controlRun(fixture.globalHome, created.run.id, 'start')
    const result = await handler('run-confirm', { id: created.run.id, nodeId: tickets[0]!.id, decision: 'confirm' }, new AbortController().signal)
    expect(result.ok).toBe(false)
    expect(result.error.message).toContain('awaiting_confirmation')
  })
})

describe('get run memberships (TICKET-0068 ticket detail run links)', () => {
  it('get includes the node’s non-terminal runs with item state and progress', async () => {
    const { tickets } = await storyFixture(1)
    const active = await service.createRun(fixture.globalHome, { title: '进行中的批次', nodeIds: [tickets[0]!.id] })
    const history = await service.createRun(fixture.globalHome, { nodeIds: [tickets[0]!.id] })
    await service.controlRun(fixture.globalHome, history.run.id, 'cancel')

    const result = await handler('get', { id: tickets[0]!.id }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.runs).toHaveLength(1)
    expect(result.value.runs[0]).toMatchObject({
      id: active.run.id,
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
