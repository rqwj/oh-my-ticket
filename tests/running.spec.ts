/**
 * Running-state tests (TICKET-0025): registry start/stop, the execute
 * endpoint (in_progress + running mark + get carrying running info), and
 * stop-on-done.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { RecentRegistry } from '../src/host/recent.ts'
import { RunningRegistry } from '../src/host/running.ts'
import { registerOmtRpc } from '../src/host/rpc.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

describe('RunningRegistry', () => {
  it('starts, reads, and stops', () => {
    const registry = new RunningRegistry()
    registry.start('TICKET-0001', 's1', 'demo 的会话')
    expect(registry.get('TICKET-0001')).toMatchObject({ sessionId: 's1', sessionLabel: 'demo 的会话' })
    registry.stop('TICKET-0001')
    expect(registry.get('TICKET-0001')).toBeUndefined()
  })

  it('snapshots executor lineage for subagent sessions (TICKET-0066)', () => {
    const registry = new RunningRegistry()
    registry.start('TICKET-0001', 'child-1', 'demo 的会话', { parentSessionId: 'parent-1', isSubagent: true })
    expect(registry.get('TICKET-0001')).toMatchObject({
      sessionId: 'child-1',
      parentSessionId: 'parent-1',
      isSubagent: true,
    })
  })

  it('plain sessions carry an empty lineage', () => {
    const registry = new RunningRegistry()
    registry.start('TICKET-0001', 's1', 'demo 的会话')
    const info = registry.get('TICKET-0001')
    expect(info?.parentSessionId).toBeUndefined()
    expect(info?.isSubagent).toBeUndefined()
  })
})

describe('/omt execute endpoint', () => {
  let home: string
  let core: OmtCore
  let handler: (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>
  let running: RunningRegistry

  beforeEach(async () => {
    home = await mkdtemp(join(tmpdir(), 'omt-running-test-'))
    core = await OmtCore.open(home)
    running = new RunningRegistry()
    const pool = new OmtCorePool(home)
    registerOmtRpc({ connection: { rpc: { handle: (_c: string, h: any) => { handler = h } } } } as never, pool, new RecentRegistry(), undefined, running)
    await core.create({ type: 'epic', title: '用户体系', id: 'EPIC-0001' })
  })

  afterEach(async () => {
    core.close()
    await rm(home, { recursive: true, force: true })
  })

  it('execute sets in_progress and records the session; get reports it', async () => {
    const result = await handler('execute', { id: 'EPIC-0001', sessionId: 's1' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.status).toBe('in_progress')

    const detail = await handler('get', { id: 'EPIC-0001' }, new AbortController().signal)
    expect(detail.value.running).toMatchObject({ sessionId: 's1' })
  })

  it('manual in_progress via update does NOT mark running (TICKET-0028)', async () => {
    await handler('update', { id: 'EPIC-0001', status: 'in_progress', sessionId: 's1' }, new AbortController().signal)
    const detail = await handler('get', { id: 'EPIC-0001' }, new AbortController().signal)
    expect(detail.value.running).toBeUndefined()
  })

  it('done clears the running mark', async () => {
    await handler('execute', { id: 'EPIC-0001', sessionId: 's1' }, new AbortController().signal)
    await handler('update', { id: 'EPIC-0001', status: 'done' }, new AbortController().signal)
    const detail = await handler('get', { id: 'EPIC-0001' }, new AbortController().signal)
    expect(detail.value.running).toBeUndefined()
  })

  it('execute snapshots the session lineage into the running mark (TICKET-0066)', async () => {
    // Re-register with an agents registry whose session header carries the
    // subagent lineage (parentSession + origin).
    const extraPool = new OmtCorePool(home)
    const withAgents = {
      connection: { rpc: { handle: (_c: string, h: any) => { handler = h } } },
      agents: {
        get: (id: string) => (id === 'child-1'
          ? { session: { header: { cwd: undefined, parentSession: 'parent-1', origin: 'subagent' } } }
          : undefined),
      },
    }
    registerOmtRpc(withAgents as never, extraPool, new RecentRegistry(), undefined, running)

    const result = await handler('execute', { id: 'EPIC-0001', sessionId: 'child-1' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    const detail = await handler('get', { id: 'EPIC-0001' }, new AbortController().signal)
    expect(detail.value.running).toMatchObject({
      sessionId: 'child-1',
      parentSessionId: 'parent-1',
      isSubagent: true,
    })
    await extraPool.closeAll()
  })
})
