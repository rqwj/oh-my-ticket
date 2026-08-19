/**
 * Disposed-hook tests (TICKET-0063, tier 2): on cordis `agent/disposed` —
 * subagent executor with a live parent → ONE followup to the parent (wakes
 * an idle parent) carrying the unfinished items and the subagent's final
 * report summary; parent gone → items demoted to interrupted. A disposed
 * main session with unfinished run work → run interrupted (resume path).
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { registerOmtDisposedHook } from '../src/host/disposed-hook.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { RunningRegistry } from '../src/host/running.ts'
import type { OmtRun } from '../src/host/types.ts'
import { requireItem, ticketFixture } from './mocks/fixtures.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

interface FakeAgent {
  id: string
  session: { header: { cwd?: string; origin?: 'subagent'; parentSession?: string }; events?: unknown[] }
  followup(message: unknown): void
}

let home: string
let pool: OmtCorePool
let core: OmtCore
let running: RunningRegistry
let liveAgents: Map<string, FakeAgent>
let disposedListeners: ((payload: { agent: FakeAgent }) => void)[]
let parentMessages: unknown[]

function makeAgent(id: string, header: FakeAgent['session']['header'] = {}, events: unknown[] = []): FakeAgent {
  return {
    id,
    session: { header, events },
    followup(message: unknown) {
      parentMessages.push(message)
    },
  }
}

function emitDisposed(agent: FakeAgent): void {
  liveAgents.delete(agent.id)
  for (const listener of disposedListeners) listener({ agent })
}

/** The hook's handler is async (pool resolution) — drain microtasks. */
function flush(): Promise<void> {
  return new Promise<void>(resolve => setImmediate(resolve))
}

function texts(): string[] {
  return parentMessages.map(message => (message as { content: { text: string }[] }).content.map(block => block.text).join('\n'))
}

/** Two-ticket started run; first item claimed by `sessionId`. */
async function runningItemFixture(sessionId: string): Promise<{ run: OmtRun; ticketIds: string[] }> {
  const tickets = await ticketFixture(core, 2)
  const run = await core.createRun({ nodeIds: tickets.map(ticket => ticket.id) })
  await core.startRun(run.id)
  await core.claimRunItem(run.id, sessionId)
  return { run, ticketIds: tickets.map(ticket => ticket.id) }
}

const ASSISTANT_EVENTS = [
  { type: 'user/message', content: [{ type: 'text', text: '做 TICKET-0003' }] },
  { type: 'assistant/message', message: { content: [{ type: 'text', text: '最终报告：已完成大半，卡在验收脚本。' }] } },
]

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-disposed-hook-'))
  pool = new OmtCorePool(home)
  core = await pool.coreFor(undefined)
  running = new RunningRegistry()
  liveAgents = new Map()
  disposedListeners = []
  parentMessages = []
  const stubCtx = {
    on: (event: string, listener: any) => {
      if (event === 'agent/disposed') disposedListeners.push(listener)
    },
    agents: {
      get: (id: string) => liveAgents.get(id),
      list: () => [...liveAgents.values()],
    },
  }
  registerOmtDisposedHook(stubCtx as never, pool, running)
})

afterEach(async () => {
  await pool.closeAll()
  await rm(home, { recursive: true, force: true })
})

describe('subagent executor', () => {
  it('notifies the live parent with unfinished items and the final-report summary', async () => {
    const { run, ticketIds } = await runningItemFixture('child-1')
    const parent = makeAgent('parent-1')
    liveAgents.set('parent-1', parent)
    const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' }, ASSISTANT_EVENTS)
    liveAgents.set('child-1', child)

    emitDisposed(child)
    await flush()

    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain(run.id)
    expect(texts()[0]).toContain(ticketIds[0])
    expect(texts()[0]).toContain('最终报告：已完成大半，卡在验收脚本。')
    // The parent now owns the follow-up: the item is left running.
    expect(requireItem(core, run.id, ticketIds[0]!).state).toBe('running')
    expect(core.getRun(run.id)?.status).toBe('running')
  })

  it('demotes the item to interrupted when the parent is gone too', async () => {
    const { run, ticketIds } = await runningItemFixture('child-1')
    const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' }, ASSISTANT_EVENTS)
    liveAgents.set('child-1', child)

    emitDisposed(child)
    await flush()

    expect(texts()).toHaveLength(0)
    expect(requireItem(core, run.id, ticketIds[0]!).state).toBe('interrupted')
    expect(core.getRun(run.id)?.status).toBe('interrupted')
  })

  it('does nothing when the subagent owns no run items', async () => {
    const { run, ticketIds } = await runningItemFixture('someone-else')
    liveAgents.set('someone-else', makeAgent('someone-else'))
    const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' })
    liveAgents.set('child-1', child)

    emitDisposed(child)
    await flush()

    expect(texts()).toHaveLength(0)
    expect(requireItem(core, run.id, ticketIds[0]!).state).toBe('running')
    expect(core.getRun(run.id)?.status).toBe('running')
  })

  it('contains a throwing parent followup (warn, item left running)', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const { run, ticketIds } = await runningItemFixture('child-1')
      const parent = makeAgent('parent-1')
      parent.followup = () => {
        throw new Error('channel gone')
      }
      liveAgents.set('parent-1', parent)
      const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' })
      liveAgents.set('child-1', child)

      emitDisposed(child)
      await flush()

      expect(warn).toHaveBeenCalled()
      expect(requireItem(core, run.id, ticketIds[0]!).state).toBe('running')
    } finally {
      warn.mockRestore()
    }
  })
})

describe('main-session executor', () => {
  it('interrupts the run when the session ends with unfinished items', async () => {
    const { run, ticketIds } = await runningItemFixture('s1')
    running.start(ticketIds[0]!, 's1', 'demo 的会话')
    const main = makeAgent('s1')
    liveAgents.set('s1', main)

    emitDisposed(main)
    await flush()

    expect(texts()).toHaveLength(0)
    expect(requireItem(core, run.id, ticketIds[0]!).state).toBe('interrupted')
    expect(core.getRun(run.id)?.status).toBe('interrupted')
    // The dead session's running mark is cleaned up.
    expect(running.get(ticketIds[0]!)).toBeUndefined()
  })

  it('leaves runs alone when the disposed session has no involvement', async () => {
    const { run, ticketIds } = await runningItemFixture('s1')
    liveAgents.set('s1', makeAgent('s1'))
    const bystander = makeAgent('s2')
    liveAgents.set('s2', bystander)

    emitDisposed(bystander)
    await flush()

    expect(requireItem(core, run.id, ticketIds[0]!).state).toBe('running')
    expect(core.getRun(run.id)?.status).toBe('running')
  })

  it('derives the terminal state when the disposed session finishes no work but everything else is final', async () => {
    const tickets = await ticketFixture(core, 1)
    const run = await core.createRun({ nodeIds: [tickets[0]!.id] })
    await core.startRun(run.id)
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, tickets[0]!.id, 'done')
    expect(core.getRun(run.id)?.status).toBe('completed')

    const main = makeAgent('s1')
    liveAgents.set('s1', main)
    emitDisposed(main)
    await flush()
    // Already terminal: the sweep must not resurrect or disturb it.
    expect(core.getRun(run.id)?.status).toBe('completed')
  })
})
