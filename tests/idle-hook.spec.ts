/**
 * Idle hook tests (TICKET-0062): on cordis `agent/status` → idle the hook
 * injects (1) a once-per-session-per-ticket 未收尾提醒 for tickets still
 * marked running, and (2) a bounded 续跑 nudge for the next pending item of
 * a running, autoContinue run the session executes. Covers timing/content,
 * paused and autoContinue=false gating, exponential backoff, budget
 * exhaustion → stalled marker, retry budget reset, and loop prevention.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { registerOmtIdleHook } from '../src/host/idle-hook.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { RunningRegistry } from '../src/host/running.ts'
import { isRunItemStalled, NUDGE_BUDGET, type OmtRun, type OmtRunItem, type RunConfig } from '../src/host/types.ts'
import { requireItem, ticketFixture } from './mocks/fixtures.ts'

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

let home: string
let pool: OmtCorePool
let core: OmtCore
let running: RunningRegistry
let nowMs: number
let scheduled: Scheduled[]
let messages: unknown[]
let statusListeners: ((payload: { agent: FakeAgent; status: 'idle' | 'running' }) => void)[]

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

/** The hook's idle handler is async (pool resolution) — drain microtasks. */
function flush(): Promise<void> {
  return new Promise<void>(resolve => setImmediate(resolve))
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

/** Two-ticket run, started, first item claimed+reported done by `sessionId`. */
async function runFixture(sessionId: string, config?: Partial<RunConfig>): Promise<{ run: OmtRun; ticketIds: string[] }> {
  const tickets = await ticketFixture(core, 2)
  const run = await core.createRun({ nodeIds: tickets.map(ticket => ticket.id), ...(config !== undefined ? { config } : {}) })
  await core.startRun(run.id)
  await core.claimRunItem(run.id, sessionId)
  await core.reportRunItem(run.id, tickets[0]!.id, 'done')
  return { run, ticketIds: tickets.map(ticket => ticket.id) }
}

function itemOf(runId: string, nodeId: string): OmtRunItem {
  return requireItem(core, runId, nodeId)
}

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-idle-hook-'))
  pool = new OmtCorePool(home)
  core = await pool.coreFor(undefined)
  running = new RunningRegistry()
  nowMs = T0
  scheduled = []
  messages = []
  statusListeners = []
  const stubCtx = {
    on: (event: string, listener: any) => {
      if (event === 'agent/status') statusListeners.push(listener)
    },
  }
  registerOmtIdleHook(stubCtx as never, pool, running, {
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
  await pool.closeAll()
  await rm(home, { recursive: true, force: true })
})

describe('未收尾提醒', () => {
  it('reminds once when a running ticket is still open at idle', async () => {
    running.start('TICKET-0042', 's1', 'demo 的会话')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain('TICKET-0042')
    expect(texts()[0]).toContain('收尾')

    // The reminder followup itself drives a turn that idles again: no repeat.
    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(1)
  })

  it('scopes reminders to the idling session', async () => {
    running.start('TICKET-0042', 's1', 'demo 的会话')
    emitIdle(fakeAgent('s2'))
    await flush()
    expect(texts()).toHaveLength(0)

    running.start('TICKET-0043', 's2', 'demo 的会话')
    emitIdle(fakeAgent('s2'))
    await flush()
    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain('TICKET-0043')
    expect(texts()[0]).not.toContain('TICKET-0042')
  })

  it('stops reminding once the ticket is no longer running', async () => {
    running.start('TICKET-0042', 's1', 'demo 的会话')
    running.stop('TICKET-0042')
    emitIdle(fakeAgent('s1'))
    await flush()
    expect(texts()).toHaveLength(0)
  })
})

describe('run 续跑 nudge', () => {
  it('nudges the next pending item of a running run the session executes', async () => {
    const { run, ticketIds } = await runFixture('s1')
    emitIdle(fakeAgent('s1'))
    await flush()

    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain('继续下一项')
    expect(texts()[0]).toContain(ticketIds[1])
    const item = itemOf(run.id, ticketIds[1])
    expect(item.nudge_count).toBe(1)
    expect(item.nudged_at).toBe(new Date(T0).toISOString())
  })

  it('merges reminder and nudge into a single followup', async () => {
    const { ticketIds } = await runFixture('s1')
    running.start(ticketIds[1], 's1', 'demo 的会话')
    emitIdle(fakeAgent('s1'))
    await flush()

    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain('仍标记为执行中')
    expect(texts()[0]).toContain('继续下一项')
  })

  it('does not nudge a paused run (decision 9)', async () => {
    const { run } = await runFixture('s1')
    await core.pauseRun(run.id)
    emitIdle(fakeAgent('s1'))
    await flush()
    expect(texts()).toHaveLength(0)
  })

  it('autoContinue=false reminds but never continues', async () => {
    const { ticketIds } = await runFixture('s1', { autoContinue: false })
    running.start(ticketIds[1], 's1', 'demo 的会话')
    emitIdle(fakeAgent('s1'))
    await flush()

    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain('仍标记为执行中')
    expect(texts()[0]).not.toContain('继续下一项')
  })

  it('does not nudge a session that is not the run executor', async () => {
    await runFixture('s1')
    emitIdle(fakeAgent('s2'))
    await flush()
    expect(texts()).toHaveLength(0)
  })

  it('does not nudge when the run has no pending item left', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const claimed = await core.claimRunItem(run.id, 's1')
    expect(claimed?.node_id).toBe(ticketIds[1])
    await core.reportRunItem(run.id, ticketIds[1], 'done')
    emitIdle(fakeAgent('s1'))
    await flush()
    expect(texts()).toHaveLength(0)
  })
})

describe('nudge 预算与退避', () => {
  it('backs off exponentially between nudges and stalls when the budget is exhausted', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    // Nudge 1: immediate on idle.
    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(1)
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(1)

    // Inside the backoff window (base=1000, first interval): no nudge, a
    // timer is armed for the remainder.
    nowMs = T0 + 400
    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(1)
    expect(scheduled.filter(entry => !entry.cleared)).toHaveLength(1)
    expect(scheduled[0]?.ms).toBe(BACKOFF_BASE_MS - 400)

    // A second idle inside the window still does not nudge.
    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(1)

    // Backoff elapsed → timer fires nudge 2.
    nowMs = T0 + 1000
    fireLastTimer()
    await flush()
    expect(texts()).toHaveLength(2)
    expect(texts()[1]).toContain('继续下一项')
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(2)

    // Next interval doubles: 2 * base. Idle right after nudge 2 rearms.
    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(2)
    const armed = scheduled.filter(entry => !entry.cleared).pop()
    expect(armed?.ms).toBe(BACKOFF_BASE_MS * 2)

    // Budget (NUDGE_BUDGET) reached with nudge 3.
    nowMs = T0 + 1000 + BACKOFF_BASE_MS * 2
    fireLastTimer()
    await flush()
    expect(texts()).toHaveLength(3)
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(NUDGE_BUDGET)

    // Budget exhausted: the item is stalled — no more nudges, no timers.
    nowMs = T0 + 1000_000
    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(3)
    expect(scheduled.filter(entry => !entry.cleared)).toHaveLength(0)
    expect(isRunItemStalled(itemOf(run.id, ticketIds[1]))).toBe(true)
  })

  it('does not nudge from a backoff timer while the agent is running', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    await flush()
    nowMs = T0 + 400
    emitIdle(agent)
    await flush()
    expect(scheduled.filter(entry => !entry.cleared)).toHaveLength(1)

    agent.status = 'running'
    nowMs = T0 + 1000
    fireLastTimer()
    await flush()
    expect(texts()).toHaveLength(1)
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(1)
  })

  it('a timer revalidates the item: claimed meanwhile → no nudge', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    await flush()
    nowMs = T0 + 400
    emitIdle(agent)
    await flush()

    // Another path claims the item before the backoff elapses.
    await core.claimRunItem(run.id, 's9')
    nowMs = T0 + 1000
    fireLastTimer()
    await flush()
    expect(texts()).toHaveLength(1)
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(1)
  })

  it('retry clears the nudge budget: a stalled item can be nudged again', async () => {
    const { run, ticketIds } = await runFixture('s1')
    const agent = fakeAgent('s1')

    emitIdle(agent)
    await flush()
    // Arm the backoff timer, then let it fire nudge 2.
    nowMs = T0 + 400
    emitIdle(agent)
    await flush()
    nowMs = T0 + 1000
    fireLastTimer()
    await flush()
    // Arm the doubled backoff, then let it fire nudge 3 (budget exhausted).
    emitIdle(agent)
    await flush()
    nowMs = T0 + 1000 + BACKOFF_BASE_MS * 2
    fireLastTimer()
    await flush()
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(NUDGE_BUDGET)
    expect(isRunItemStalled(itemOf(run.id, ticketIds[1]))).toBe(true)
    expect(texts()).toHaveLength(3)

    await core.retryItem(run.id, ticketIds[1])
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(0)
    expect(isRunItemStalled(itemOf(run.id, ticketIds[1]))).toBe(false)

    nowMs = T0 + 2000_000
    emitIdle(agent)
    await flush()
    expect(texts()).toHaveLength(4)
    expect(texts()[3]).toContain('继续下一项')
    expect(itemOf(run.id, ticketIds[1]).nudge_count).toBe(1)
  })
})
