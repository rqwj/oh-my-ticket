/**
 * Disposed-hook tests (TICKET-0063, tier 2): on cordis `agent/disposed` —
 * subagent executor with a live parent → ONE followup to the parent (wakes
 * an idle parent) carrying the unfinished items and the subagent's final
 * report summary.
 *
 * U7a rewrites (documented deviation): the pre-daemon janitor sweep demoted
 * dead sessions' running items to interrupted IN-PROCESS. This daemon build
 * owns lease expiry and exposes no mid-flight demotion RPC, so the adapter's
 * sweep is notify-only: outstanding items stay `running` until the
 * daemon-side lease path or a human retry settles them, and the hook emits a
 * once-per-process degradation warning instead. The old "demoted to
 * interrupted" assertions below are rewritten to pin the AS-BUILT behavior.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { registerOmtDisposedHook } from '../src/host/disposed-hook.ts'
import { RunningRegistry } from '../src/host/running.ts'
import type { HomeRef, OmtService } from '../src/host/service.ts'
import type { OmtRun } from '../src/host/types.ts'
import { requireItem, ticketFixture } from './mocks/fixtures.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

interface FakeAgent {
  id: string
  session: { header: { cwd?: string; origin?: 'subagent'; parentSession?: string }; events?: unknown[] }
  followup(message: unknown): void
}

let fixture: RuntimeFixture
let service: OmtService
let globalHome: HomeRef
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

/**
 * Real-time settle: the handler performs several async daemon round-trips;
 * local calls are sub-millisecond so this is ample headroom.
 */
const settle = (): Promise<void> => new Promise(resolve => setTimeout(resolve, 50))

function texts(): string[] {
  return parentMessages.map(message => (message as { content: { text: string }[] }).content.map(block => block.text).join('\n'))
}

/** Two-ticket started run; first item claimed by `sessionId`. */
async function runningItemFixture(sessionId: string): Promise<{ run: OmtRun; ticketIds: string[] }> {
  const tickets = await ticketFixture(service, globalHome, 2)
  const created = await service.createRun(globalHome, { nodeIds: tickets.map(ticket => ticket.id) })
  await service.controlRun(globalHome, created.run.id, 'start')
  await service.claimItem(globalHome, created.run.id, sessionId)
  return { run: created.run, ticketIds: tickets.map(ticket => ticket.id) }
}

const ASSISTANT_EVENTS = [
  { type: 'user/message', content: [{ type: 'text', text: '做 TICKET-0003' }] },
  { type: 'assistant/message', message: { content: [{ type: 'text', text: '最终报告：已完成大半，卡在验收脚本。' }] } },
]

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'disposed-hook' })
  service = fixture.service
  globalHome = fixture.globalHome
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
  registerOmtDisposedHook(stubCtx as never, service, running)
})

afterEach(async () => {
  await fixture.stop()
})

describe('subagent executor', () => {
  it('notifies the live parent with unfinished items and the final-report summary', async () => {
    const { run, ticketIds } = await runningItemFixture('child-1')
    const parent = makeAgent('parent-1')
    liveAgents.set('parent-1', parent)
    const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' }, ASSISTANT_EVENTS)
    liveAgents.set('child-1', child)

    emitDisposed(child)
    await settle()

    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain(run.id)
    expect(texts()[0]).toContain(ticketIds[0])
    expect(texts()[0]).toContain('最终报告：已完成大半，卡在验收脚本。')
    // The parent now owns the follow-up: the item is left running.
    expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
    expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('running')
  })

  it('KEEPS the item running when the parent is gone too (degraded sweep warns)', async () => {
    // REWRITTEN for U7a: pre-daemon this demoted the item to interrupted;
    // there is no mid-flight demotion RPC any more — the hook warns once
    // and leaves settlement to the daemon lease path / manual retry.
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const { run, ticketIds } = await runningItemFixture('child-1')
      const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' }, ASSISTANT_EVENTS)
      liveAgents.set('child-1', child)

      emitDisposed(child)
      await settle()

      expect(texts()).toHaveLength(0)
      expect(warn).toHaveBeenCalledWith(expect.stringContaining('janitor sweep unavailable'), undefined)
      expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
      expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('running')
    } finally {
      warn.mockRestore()
    }
  })

  it('does nothing when the subagent owns no run items', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const { run, ticketIds } = await runningItemFixture('someone-else')
      liveAgents.set('someone-else', makeAgent('someone-else'))
      const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' })
      liveAgents.set('child-1', child)

      emitDisposed(child)
      await settle()

      expect(texts()).toHaveLength(0)
      // No involvement → no degraded sweep over unrelated runs either.
      expect(warn).not.toHaveBeenCalledWith(expect.stringContaining('janitor sweep unavailable'), undefined)
      expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
      expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('running')
    } finally {
      warn.mockRestore()
    }
  })

  it('sends NO followup and NO sweep when the subagent owns only terminal items (parent alive)', async () => {
    const { run, ticketIds } = await runningItemFixture('child-1')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'done', '收工')
    const parent = makeAgent('parent-1')
    liveAgents.set('parent-1', parent)
    const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' }, ASSISTANT_EVENTS)
    liveAgents.set('child-1', child)

    emitDisposed(child)
    await settle()

    // 仅含终态项：无需父会话接管，也不触发 janitor sweep。
    expect(texts()).toHaveLength(0)
    expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('done')
    expect((await requireItem(service, globalHome, run.id, ticketIds[1]!)).state).toBe('pending')
    expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('running')
  })

  it('falls back to （不可得） when the disposed subagent has no usable event stream', async () => {
    const { run, ticketIds } = await runningItemFixture('child-1')
    const parent = makeAgent('parent-1')
    liveAgents.set('parent-1', parent)
    // 空事件流：最终报告摘要不存在的兜底分支。
    const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' })
    liveAgents.set('child-1', child)

    emitDisposed(child)
    await settle()

    expect(texts()).toHaveLength(1)
    expect(texts()[0]).toContain(ticketIds[0])
    expect(texts()[0]).toContain('（不可得）')
    expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
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
      await settle()

      expect(warn).toHaveBeenCalled()
      expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
    } finally {
      warn.mockRestore()
    }
  })

  it('keeps the handed-off child items running when the parent disposes afterwards (degraded sweep warns)', async () => {
    // REWRITTEN for U7a: the handoff registration still routes the later
    // parent disposal into the sweep's liveness predicate, but the sweep
    // itself is notify-only now (no demotion RPC).
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const { run, ticketIds } = await runningItemFixture('child-1')
      const parent = makeAgent('parent-1')
      liveAgents.set('parent-1', parent)
      const child = makeAgent('child-1', { origin: 'subagent', parentSession: 'parent-1' }, ASSISTANT_EVENTS)
      liveAgents.set('child-1', child)

      // 子先销毁：交接给存活的父会话，item 保持 running。
      emitDisposed(child)
      await settle()
      expect(texts()).toHaveLength(1)
      expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')

      // 父后销毁：交接登记让它落入降级 sweep —— 只告警（无降级 RPC）。
      emitDisposed(parent)
      await settle()
      expect(warn).toHaveBeenCalledWith(expect.stringContaining('janitor sweep unavailable'), undefined)
      expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
      expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('running')
    } finally {
      warn.mockRestore()
    }
  })
})

describe('main-session executor', () => {
  it('warms the degraded sweep when the session ends with unfinished items (items stay running)', async () => {
    // REWRITTEN for U7a: was interrupt-the-run; now notify-only.
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const { run, ticketIds } = await runningItemFixture('s1')
      running.start(ticketIds[0]!, 's1', 'demo 的会话')
      const main = makeAgent('s1')
      liveAgents.set('s1', main)

      emitDisposed(main)
      await settle()

      expect(texts()).toHaveLength(0)
      expect(warn).toHaveBeenCalledWith(expect.stringContaining('janitor sweep unavailable'), undefined)
      expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
      expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('running')
      // The dead session's running mark IS cleaned up (unchanged rule).
      expect(running.get(ticketIds[0]!)).toBeUndefined()
    } finally {
      warn.mockRestore()
    }
  })

  it('covers PAUSED runs in the degraded sweep without touching their state', async () => {
    // REWRITTEN for U7a: was demote-to-interrupted; paused runs stay paused
    // (人工控制中), items stay running until lease/retry settles them.
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const { run, ticketIds } = await runningItemFixture('s1')
      await service.controlRun(globalHome, run.id, 'pause')
      const main = makeAgent('s1')
      liveAgents.set('s1', main)

      emitDisposed(main)
      await settle()

      expect(warn).toHaveBeenCalledWith(expect.stringContaining('janitor sweep unavailable'), undefined)
      expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
      expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('paused')
    } finally {
      warn.mockRestore()
    }
  })

  it('clears the running marks of a disposed session with NO run involvement', async () => {
    // 无 run 项的会话：执行中标记也必须清理（不能因早退而泄漏）。
    running.start('TICKET-0001', 's1', 'demo 的会话', {}, 'home-a')
    running.start('TICKET-0001', 's2', 'other 的会话', {}, 'home-b')
    running.start('TICKET-0001', 's1', 'demo 的会话', {}, 'home-c')
    const main = makeAgent('s1')
    liveAgents.set('s1', main)

    emitDisposed(main)
    await settle()

    expect(running.forSession('s1')).toEqual([])
    expect(running.get('TICKET-0001', 'home-b')?.sessionId).toBe('s2')
  })

  it('leaves runs alone when the disposed session has no involvement', async () => {
    const { run, ticketIds } = await runningItemFixture('s1')
    liveAgents.set('s1', makeAgent('s1'))
    const bystander = makeAgent('s2')
    liveAgents.set('s2', bystander)

    emitDisposed(bystander)
    await settle()

    expect((await requireItem(service, globalHome, run.id, ticketIds[0]!)).state).toBe('running')
    expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('running')
  })

  it('derives the terminal state when the disposed session finishes no work but everything else is final', async () => {
    const tickets = await ticketFixture(service, globalHome, 1)
    const created = await service.createRun(globalHome, { nodeIds: [tickets[0]!.id] })
    await service.controlRun(globalHome, created.run.id, 'start')
    await service.claimItem(globalHome, created.run.id, 's1')
    await service.reportItem(globalHome, created.run.id, tickets[0]!.id, 'done')
    expect((await service.fetchRun(globalHome, created.run.id)).run.status).toBe('completed')

    const main = makeAgent('s1')
    liveAgents.set('s1', main)
    emitDisposed(main)
    await settle()
    // Already terminal: the sweep must not resurrect or disturb it.
    expect((await service.fetchRun(globalHome, created.run.id)).run.status).toBe('completed')
  })
})
