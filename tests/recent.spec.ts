/**
 * Turn-tail related-ticket tests: RecentRegistry ordering/dedup/cap, the
 * /omt recent endpoint, and the controller's related-store flows.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { RecentRegistry } from '../src/host/recent.ts'
import { registerOmtRpc } from '../src/host/rpc.ts'
import { OmtController } from '../src/client/controller.ts'
import type { RpcResultLike } from '../src/client/trigger/source.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

describe('RecentRegistry', () => {
  it('dedups, orders most-recent-first, and caps at 10', () => {
    const registry = new RecentRegistry()
    registry.touch('s1', 'A')
    registry.touch('s1', 'B')
    registry.touch('s1', 'A') // re-touch moves to front
    expect(registry.list('s1')).toEqual(['A', 'B'])
    registry.touch('s2', 'X')
    expect(registry.list('s1')).toEqual(['A', 'B']) // sessions isolated
    for (let i = 0; i < 12; i++) registry.touch('s2', `N${i}`)
    expect(registry.list('s2')).toHaveLength(10)
    expect(registry.list('s2')[0]).toBe('N11')
  })

  it('ignores empty session ids', () => {
    const registry = new RecentRegistry()
    registry.touch(undefined, 'A')
    registry.touch('', 'B')
    expect(registry.list('')).toEqual([])
  })

  it('survives a simulated host restart via persistence', async () => {
    // Disk side: a plain map standing in for the meta table.
    const disk = new Map<string, string[]>()
    const persistence = {
      load: (sessionId: string) => Promise.resolve(disk.get(sessionId)),
      save: (sessionId: string, ids: readonly string[]) => {
        disk.set(sessionId, [...ids])
        return Promise.resolve()
      },
    }

    const first = new RecentRegistry()
    first.attachPersistence(persistence)
    first.touch('s1', 'A')
    first.touch('s1', 'B')
    await new Promise(resolve => setTimeout(resolve, 0)) // let fire-and-forget save land

    // "Restart": a fresh registry over the same disk.
    const second = new RecentRegistry()
    second.attachPersistence(persistence)
    expect(second.list('s1')).toEqual([]) // memory empty until resolved
    expect(await second.resolve('s1')).toEqual(['B', 'A'])
    expect(second.list('s1')).toEqual(['B', 'A']) // now cached in memory
  })
})

describe('/omt recent endpoint', () => {
  let home: string
  let core: OmtCore
  let handler: (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>
  let registry: RecentRegistry
  let pool: OmtCorePool

  beforeEach(async () => {
    home = await mkdtemp(join(tmpdir(), 'omt-recent-test-'))
    // Single opener per home (U2b owner lock): the seeding core IS the
    // pool's cached core.
    pool = new OmtCorePool(home)
    core = await pool.coreForHome(home)
    registry = new RecentRegistry()
    registerOmtRpc({ connection: { rpc: { handle: (_c: string, h: any) => { handler = h } } } } as never, pool, registry)
    await core.create({ type: 'epic', title: '用户体系', id: 'EPIC-0001' })
    await core.create({ type: 'epic', title: '通知系统', id: 'EPIC-0002' })
  })

  afterEach(async () => {
    await pool.closeAll()
    await rm(home, { recursive: true, force: true })
  })

  it('returns summaries most-recent-first, skipping missing nodes', async () => {
    registry.touch('s1', 'EPIC-0001')
    registry.touch('s1', 'EPIC-9999') // missing
    registry.touch('s1', 'EPIC-0002')
    const result = await handler('recent', { sessionId: 's1' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.map((n: any) => n.id)).toEqual(['EPIC-0002', 'EPIC-0001'])
  })

  it('get/update handlers touch the registry', async () => {
    await handler('get', { sessionId: 's1', id: 'EPIC-0001' }, new AbortController().signal)
    await handler('update', { sessionId: 's1', id: 'EPIC-0002', status: 'done' }, new AbortController().signal)
    expect(registry.list('s1')).toEqual(['EPIC-0002', 'EPIC-0001'])
  })
})

describe('controller related store', () => {
  let controller: OmtController

  beforeEach(() => {
    const rpc = { async call(): Promise<RpcResultLike> { return { ok: true, value: [] } } }
    controller = new OmtController(rpc, { openDetails: () => {}, closeDetails: () => {} })
  })

  const summary = (id: string) => ({ id, type: 'ticket' as const, title: `T${id}`, status: 'open' as const, archived: false, priority: 0 })

  it('noteRelated dedups and caps; relatedOf reads per session', () => {
    controller.noteRelated('s1', [summary('A'), summary('B')])
    controller.noteRelated('s1', [summary('A'), summary('C')])
    expect(controller.relatedOf('s1').map(n => n.id)).toEqual(['A', 'C', 'B'])
    expect(controller.relatedOf('s2')).toEqual([])
    expect(controller.relatedOf(undefined)).toEqual([])
  })

  it('refreshRelated throttles bursts per session', async () => {
    let fetches = 0
    const rpc = {
      async call(): Promise<RpcResultLike> {
        fetches += 1
        return { ok: true, value: [] }
      },
    }
    const c = new OmtController(rpc, { openDetails: () => {}, closeDetails: () => {} })
    await c.refreshRelated('s1')
    await c.refreshRelated('s1')
    await c.refreshRelated('s2')
    expect(fetches).toBe(2) // s1 burst collapsed, s2 independent
  })

  it('refreshRelated merges host recency into the store', async () => {
    const rpc = {
      async call(_c: string, endpoint: string): Promise<RpcResultLike> {
        if (endpoint === 'recent') return { ok: true, value: [summary('H1')] }
        return { ok: true, value: [] }
      },
    }
    const c = new OmtController(rpc, { openDetails: () => {}, closeDetails: () => {} })
    c.noteRelated('s1', [summary('L1')])
    await c.refreshRelated('s1')
    expect(c.relatedOf('s1').map(n => n.id)).toEqual(['H1', 'L1'])
  })
})
