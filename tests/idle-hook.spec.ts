/**
 * Idle hook tests (TICKET-0062): on cordis `agent/status` → idle the hook
 * injects (1) a once-per-session-per-ticket 未收尾提醒 for tickets still
 * marked running, and (2) a bounded 续跑 nudge for the next pending item of
 * a running, autoContinue run the session executes. Covers timing/content,
 * paused and autoContinue=false gating, exponential backoff, budget
 * exhaustion → stalled marker, retry budget reset, and loop prevention.
 * U7a: runs against a REAL omt-daemon through the runtime fixture.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { registerOmtIdleHook } from '../src/host/idle-hook.ts'
import { RunningRegistry } from '../src/host/running.ts'
import type { HomeRef, OmtService } from '../src/host/service.ts'
import { isRunItemStalled, NUDGE_BUDGET, type OmtRun, type OmtRunItem, type RunConfig } from '../src/host/types.ts'
import { requireItem, ticketFixture } from './mocks/fixtures.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

const T0 = Date.parse('2026-08-19T00:00:00.000Z')
const BACKOFF_BASE_MS = 1000

interface FakeAgent {
  id: string
  status: 'idle' | 'running'
  session: { header: { cwd?: string } }
  followup(message: unknown): void
}

interface Scheduled {
  fn: () => void
  ms: number
  cleared: boolean
}

let fixture: RuntimeFixture
let service: OmtService
let globalHome: HomeRef
let running: RunningRegistry
let nowMs: number
let scheduled: Scheduled[]
let messages: unknown[]
let statusListeners: ((payload: { agent: FakeAgent; status: 'idle' | 'running' }) => void)[]
/** Disposers captured from the hook's ctx.effect generator (dispose path). */
let disposers: (() => void)[]

function fakeAgent(id: string): FakeAgent {
  return {
    id,
    status: 'idle',
    session: { header: {} },
    followup(message: unknown) {
      messages.push(message)
    },
  }
}

function emitIdle(agent: FakeAgent): void {
  for (const listener of statusListeners) listener({ agent, status: 'idle' })
}

/**
 * Real-time settle for one idle event: the handler performs several async
 * daemon round-trips, so a microtask drain is NOT enough — overlapping
 * handlers would decide from stale snapshots. Local daemon calls are
 * sub-millisecond; 50ms is ample headroom.
 */
const settle = (): Promise<void> => new Promise(resolve => setTimeout(resolve, 50))

/**
 * The hook's idle handler performs several async daemon round-trips; poll
 * until the expectation holds (or fail after the budget).
 */
async function waitFor<T>(probe: () => T | Promise<T>, timeoutMs = 4000): Promise<T> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const value = await probe()
    if (value) return value
    if (Date.now() > deadline) return value // let the caller's expect fail
    await settle()
  }
}

function fireLastTimer(): void {
  const entry = scheduled.filter(candidate => !candidate.cleared).pop()
  expect(entry, 'expected a scheduled backoff timer').toBeDefined()
  // A fired timer is spent (the hook drops it from its own registry too).
  ;(entry as Scheduled).cleared = true
  ;(entry as Scheduled).fn()
}

function texts(): string[] {
  return messages.map(message => (message as { content: { text: string }[] }).content.map(block => block.text).join('\n'))
}

async function messageCount(): Promise<number> {
  return waitFor(async () => {
    const resolved = await texts()
    return resolved.length > 0 ? resolved.length : undefined
  }).then(count => count ?? 0)
}

/** Poll until at least `min` messages have landed (timer paths are async). */
async function waitForCount(min: number): Promise<void> {
  await waitFor(async () => ((await texts()).length >= min ? true : undefined))
}

/** Two-ticket run, started, first item claimed+reported done by `sessionId`. */
async function runFixture(sessionId: string, config?: Partial<RunConfig>): Promise<{ run: OmtRun; ticketIds: string[] }> {
  const tickets = await ticketFixture(service, globalHome, 2)
  const ticketIds = tickets.map(ticket => ticket.id)
  const created = await service.createRun(globalHome, {
    nodeIds: ticketIds,
    ...(config !== undefined ? { config } : {}),
  })
  await service.controlRun(globalHome, created.run.id, 'start')
  await service.claimItem(globalHome, created.run.id, sessionId)
  await service.reportItem(globalHome, created.run.id, ticketIds[0]!, 'done')
  return { run: created.run, ticketIds }
}

async function itemOf(runId: string, nodeId: string): Promise<OmtRunItem> {
  return requireItem(service, globalHome, runId, nodeId)
}

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'idle-hook' })
  service = fixture.service
  globalHome = fixture.globalHome
  running = new RunningRegistry()
  nowMs = T0
  scheduled = []
  messages = []
  statusListeners = []
  disposers = []
  const stubCtx = {
    on: (event: string, listener: any) => {
      if (event === 'agent/status') statusListeners.push(listener)
    },
    // cordis effect lifecycle: run the generator, keep the yielded disposers.
    effect: (body: () => Generator<() => void, void, unknown>) => {
      for (const disposer of body()) disposers.push(disposer)
    },
  }
  registerOmtIdleHook(stubCtx as never, service, running, {
    backoffBaseMs: BACKOFF_BASE_MS,
    now: () => nowMs,
    setTimer: (fn, ms) => {
      const entry: Scheduled = { fn, ms, cleared: false }
      scheduled.push(entry)
      return entry
    },
    clearTimer: handle => {
      ;(handle as Scheduled).cleared = true
    },
  })
})

afterEach(async () => {
  await fixture.stop()
})

describe('未收尾提醒', () => {
  it('reminds once when a running ticket is still open at idle', async () => {
    running.start('TICKET-0042', 's1', 'demo 的会话')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    expect((await messageCount()) >= 1).toBe(true)
    expect(texts()[0]).toContain('TICKET-0042')
    expect(texts()[0]).toContain('收尾')

    // The reminder followup itself drives a turn that idles again: no repeat.
    emitIdle(agent)
    await settle()
    expect(texts()).toHaveLength(1)
  })

  it('scopes reminders to the idling session', async () => {
    running.start('TICKET-0042', 's1', 'demo 的会话')
    emitIdle(fakeAgent('s2'))
    await settle()
    expect(texts()).toHaveLength(0)

    running.start('TICKET-0043', 's2', 'demo 的会话')
    emitIdle(fakeAgent('s2'))
    expect(await messageCount()).toBe(1)
    expect(texts()[0]).toContain('TICKET-0043')
    expect(texts()[0]).not.toContain('TICKET-0042')
  })

  it('stops reminding once the ticket is no longer running', async () => {
    running.start('TICKET-0042', 's1', 'demo 的会话')
    running.stop('TICKET-0042')
    emitIdle(fakeAgent('s1'))
    await settle()
    expect(texts()).toHaveLength(0)
  })
})

describe('run 续跑 nudge', () => {
  it('nudges the next pending item of a running run the session executes', async () => {
    const { run, ticketIds } = await runFixture('s1')
    emitIdle(fakeAgent('s1'))

    expect(await messageCount()).toBe(1)
    expect(texts()[0]).toContain('继续下一项')
    expect(texts()[0]).toContain(ticketIds[1])
    const item = await itemOf(run.id, ticketIds[1])
    expect(item.nudge_count).toBe(1)
    expect(item.nudged_at).toBe(new Date(T0).toISOString())
  })

  it('merges reminder and nudge into a single followup', async () => {
    const { ticketIds } = await runFixture('s1')
    running.start(ticketIds[1], 's1', 'demo 的会话')
    emitIdle(fakeAgent('s1'))

    expect(await messageCount()).toBe(1)
    expect(texts()[0]).toContain('仍标记为执行中')
    expect(texts()[0]).toContain('继续下一项')
  })

  it('does not nudge a paused run (decision 9)', async () => {
    const { run } = await runFixture('s1')
    await service.controlRun(globalHome, run.id, 'pause')
    emitIdle(fakeAgent('s1'))
    await settle()
    expect(texts()).toHaveLength(0)
  })

  it('autoContinue=false reminds but never continues', async () => {
    const { ticketIds } = await runFixture('s1', { autoContinue: false })
    running.start(ticketIds[1], 's1', 'demo 的会话')
    emitIdle(fakeAgent('s1'))

    expect(await messageCount()).toBe(1)
    expect(texts()[0]).toContain('仍标记为执行中')
    expect(texts()[0]).not.toContain('继续下一项')
  })

  it('does not nudge a session that is not the run executor', async () => {
    await runFixture('s1')
    emitIdle(fakeAgent('s2'))
    await settle()
    expect(texts()).toHaveLength(0)
  })

  it('does not nudge when the run has no pending item left', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const claimed = await service.claimItem(globalHome, run.id, 's1')
    expect(claimed.item?.node_id).toBe(ticketIds[1])
    await service.reportItem(globalHome, run.id, ticketIds[1]!, 'done')
    emitIdle(fakeAgent('s1'))
    await settle()
    expect(texts()).toHaveLength(0)
  })
})

describe('nudge 预算与退避', () => {
  it('backs off exponentially between nudges and stalls when the budget is exhausted', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    // Nudge 1: immediate on idle.
    emitIdle(agent)
    expect(await messageCount()).toBe(1)
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(1)

    // Inside the backoff window (base=1000, first interval): no nudge, a
    // timer is armed for the remainder.
    nowMs = T0 + 400
    emitIdle(agent)
    await waitFor(() => scheduled.filter(entry => !entry.cleared).length === 1)
    expect(texts()).toHaveLength(1)
    expect(scheduled[0]?.ms).toBe(BACKOFF_BASE_MS - 400)

    // A second idle inside the window still does not nudge.
    emitIdle(agent)
    await settle()
    expect(texts()).toHaveLength(1)

    // Backoff elapsed → timer fires nudge 2.
    nowMs = T0 + 1000
    fireLastTimer()
    await waitForCount(2)
    expect(texts()).toHaveLength(2)
    expect(texts()[1]).toContain('继续下一项')
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(2)

    // Next interval doubles: 2 * base. Idle right after nudge 2 rearms.
    emitIdle(agent)
    await waitFor(() => scheduled.filter(entry => !entry.cleared).length === 1)
    expect(texts()).toHaveLength(2)
    const armed = scheduled.filter(entry => !entry.cleared).pop()
    expect(armed?.ms).toBe(BACKOFF_BASE_MS * 2)

    // Budget (NUDGE_BUDGET) reached with nudge 3.
    nowMs = T0 + 1000 + BACKOFF_BASE_MS * 2
    fireLastTimer()
    await waitForCount(3)
    expect(texts()).toHaveLength(3)
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(NUDGE_BUDGET)

    // Budget exhausted: the item is stalled — no more nudges, no timers.
    nowMs = T0 + 1000_000
    emitIdle(agent)
    await settle()
    expect(texts()).toHaveLength(3)
    expect(scheduled.filter(entry => !entry.cleared)).toHaveLength(0)
    expect(isRunItemStalled(await itemOf(run.id, ticketIds[1]))).toBe(true)
  })

  it('does not nudge from a backoff timer while the agent is running', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    expect(await messageCount()).toBe(1)
    nowMs = T0 + 400
    emitIdle(agent)
    await waitFor(() => scheduled.filter(entry => !entry.cleared).length === 1)

    agent.status = 'running'
    nowMs = T0 + 1000
    fireLastTimer()
    await settle()
    expect(texts()).toHaveLength(1)
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(1)
  })

  it('a timer revalidates the item: claimed meanwhile → no nudge', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    expect(await messageCount()).toBe(1)
    nowMs = T0 + 400
    emitIdle(agent)
    await waitFor(() => scheduled.filter(entry => !entry.cleared).length === 1)

    // Another path claims the item before the backoff elapses.
    await service.claimItem(globalHome, run.id, 's9')
    nowMs = T0 + 1000
    fireLastTimer()
    await settle()
    expect(texts()).toHaveLength(1)
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(1)
  })

  it('retry clears the nudge budget: a stalled item can be nudged again', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    expect(await messageCount()).toBe(1)
    // Arm the backoff timer, then let it fire nudge 2.
    nowMs = T0 + 400
    emitIdle(agent)
    await waitFor(() => scheduled.filter(entry => !entry.cleared).length === 1)
    nowMs = T0 + 1000
    fireLastTimer()
    await waitForCount(2)
    expect(texts()).toHaveLength(2)
    // Arm the doubled backoff, then let it fire nudge 3 (budget exhausted).
    emitIdle(agent)
    await waitFor(() => scheduled.filter(entry => !entry.cleared).length === 1)
    nowMs = T0 + 1000 + BACKOFF_BASE_MS * 2
    fireLastTimer()
    await waitForCount(3)
    expect(texts()).toHaveLength(3)
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(NUDGE_BUDGET)
    expect(isRunItemStalled(await itemOf(run.id, ticketIds[1]))).toBe(true)

    await service.controlRun(globalHome, run.id, 'retry', ticketIds[1])
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(0)
    expect(isRunItemStalled(await itemOf(run.id, ticketIds[1]))).toBe(false)

    nowMs = T0 + 2000_000
    emitIdle(agent)
    await waitForCount(4)
    expect(texts()).toHaveLength(4)
    expect(texts()[3]).toContain('继续下一项')
    expect((await itemOf(run.id, ticketIds[1])).nudge_count).toBe(1)
  })
})

describe('ctx.effect 清理路径', () => {
  it('the yielded disposer clears every armed backoff timer', async () => {
    // Registration ran the effect generator exactly once.
    expect(disposers).toHaveLength(1)

    await runFixture('s1')
    const agent = fakeAgent('s1')

    // Nudge 1 on idle, then a second idle inside the backoff window arms a
    // timer for the remainder.
    emitIdle(agent)
    expect(await messageCount()).toBe(1)
    nowMs = T0 + 400
    emitIdle(agent)
    await waitFor(() => scheduled.filter(entry => !entry.cleared).length === 1)

    // Plugin dispose: the armed timer is dropped, nothing left behind.
    disposers[0]!()
    expect(scheduled.filter(entry => !entry.cleared)).toHaveLength(0)
  })
})

describe('错误路径', () => {
  it('a throwing followup is contained: no unhandled rejection, other reminders still land', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const broken = fakeAgent('s1')
      broken.followup = () => {
        throw new Error('channel gone')
      }
      running.start('TICKET-0042', 's1', 'demo 的会话')

      // The throw is caught inside the hook (logged as a warning) — the idle
      // handling promise still settles, so vitest sees no rejection.
      emitIdle(broken)
      await new Promise(resolve => setTimeout(resolve, 50))
      expect(warn).toHaveBeenCalled()
      expect(texts()).toHaveLength(0)

      // …and the failure never poisons delivery to other sessions.
      running.start('TICKET-0043', 's2', 'demo 的会话')
      emitIdle(fakeAgent('s2'))
      expect(await messageCount()).toBe(1)
      expect(texts()[0]).toContain('TICKET-0043')
      expect(texts()[0]).toContain('收尾')
    } finally {
      warn.mockRestore()
    }
  })
})
