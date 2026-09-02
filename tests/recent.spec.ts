/**
 * Turn-tail related-ticket tests: RecentRegistry ordering/dedup/cap, the
 * /omt recent endpoint (U7a: against a REAL omt-daemon), and the
 * controller's related-store flows.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { RecentRegistry } from '../src/host/recent.ts'
import { registerOmtRpc } from '../src/host/rpc.ts'
import type { OmtService } from '../src/host/service.ts'
import { OmtController } from '../src/client/controller.ts'
import type { RpcResultLike } from '../src/client/trigger/source.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

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
    // Delegate side: a plain map standing in for daemon-owned storage.
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

    // "Restart": a fresh registry over the same storage.
    const second = new RecentRegistry()
    second.attachPersistence(persistence)
    expect(second.list('s1')).toEqual([]) // memory empty until resolved
    expect(await second.resolve('s1')).toEqual(['B', 'A'])
    expect(second.list('s1')).toEqual(['B', 'A']) // now cached in memory
  })

  it('persistence rides ui/recent-get|set on the real runtime (U7a)', async () => {
    // REWRITTEN for U7a: was a Map stand-in for the meta table; now the
    // delegate is wired to the daemon's recent-get/set and a fresh registry
    // (same service) reloads across the "restart".
    const fixture = await createRuntimeFixture({ label: 'recent-persist' })
    try {
      const makeRegistry = (): RecentRegistry => {
        const registry = new RecentRegistry()
        registry.attachPersistence({
          load: sessionId => fixture.service.recentGet(sessionId),
          save: async (sessionId, ids) => {
            await fixture.service.recentSet(sessionId, ids)
          },
        })
        return registry
      }
      const first = makeRegistry()
      first.touch('s1', 'EPIC-0001')
      first.touch('s1', 'EPIC-0002')
      await new Promise(resolve => setTimeout(resolve, 5)) // let fire-and-forget save land
      const second = makeRegistry()
      expect(await second.resolve('s1')).toEqual(['EPIC-0002', 'EPIC-0001'])
    } finally {
      await fixture.stop()
    }
  })
})

describe('/omt recent endpoint', () => {
  let fixture: RuntimeFixture
  let handler: (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>
  let registry: RecentRegistry
  let epic1: string
  let epic2: string

  beforeEach(async () => {
    fixture = await createRuntimeFixture({ label: 'recent' })
    const service: OmtService = fixture.service
    registry = new RecentRegistry()
    registerOmtRpc({ connection: { rpc: { handle: (_c: string, h: any) => { handler = h } } } } as never, service, registry)
    // Daemon allocates ids; capture them instead of hardcoding EPIC-000x.
    epic1 = (await service.createNode(fixture.globalHome, { type: 'epic', title: '用户体系' })).id
    epic2 = (await service.createNode(fixture.globalHome, { type: 'epic', title: '通知系统' })).id
  })

  afterEach(async () => {
    await fixture.stop()
  })

  it('returns summaries most-recent-first, skipping missing nodes', async () => {
    registry.touch('s1', epic1)
    registry.touch('s1', 'EPIC-9999') // missing
    registry.touch('s1', epic2)
    const result = await handler('recent', { sessionId: 's1' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.map((n: any) => n.id)).toEqual([epic2, epic1])
  })

  it('get/update handlers touch the registry', async () => {
    await handler('get', { sessionId: 's1', id: epic1 }, new AbortController().signal)
    await handler('update', { sessionId: 's1', id: epic2, status: 'done' }, new AbortController().signal)
    expect(registry.list('s1')).toEqual([epic2, epic1])
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
