/**
 * RPC layer tests: the `/omt` channel handler against a REAL omt-daemon
 * (U7a) — endpoint payloads (zod-validated), result envelopes, and error
 * folding. Filter persistence assertions moved from the adapter-side
 * `ui-filters.json` file to daemon-owned storage (documented rewrite).
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { registerOmtRpc } from '../src/host/rpc.ts'
import type { OmtService } from '../src/host/service.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

type Handler = (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>

let fixture: RuntimeFixture
let service: OmtService
let handler: Handler
let capturedAuthority: string | undefined

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'rpc' })
  service = fixture.service
  const stubCtx = {
    connection: {
      rpc: {
        handle(_channel: string, h: Handler, options: { authority: string }) {
          handler = h
          capturedAuthority = options.authority
        },
      },
    },
  }
  registerOmtRpc(stubCtx as never, service)

  const epic = await service.createNode(fixture.globalHome, { type: 'epic', title: '用户体系' })
  const story = await service.createNode(fixture.globalHome, { type: 'story', title: '登录', parentId: epic.id })
  await service.createNode(fixture.globalHome, { type: 'ticket', title: '登录接口', parentId: story.id, body: '支持 OAuth 授权码模式' })
})

afterEach(async () => {
  await fixture.stop()
})

it('registers the channel with loopback authority', () => {
  expect(capturedAuthority).toBe('loopback')
})

it('tree returns the assembled forest', async () => {
  const result = await handler('tree', {}, new AbortController().signal)
  expect(result.ok).toBe(true)
  expect(result.value).toHaveLength(1)
  expect(result.value[0].children[0].children[0].id).toBe('TICKET-0001')
})

it('search returns summaries for keyword and empty queries', async () => {
  const signal = new AbortController().signal
  const hits = await handler('search', { query: '登录', limit: 20 }, signal)
  expect(hits.ok).toBe(true)
  expect(hits.value.map((n: any) => n.id)).toContain('STORY-0001')

  const all = await handler('search', { query: '', limit: 20 }, signal)
  expect(all.value).toHaveLength(3)
})

it('get returns node detail with parent and body', async () => {
  const result = await handler('get', { id: 'TICKET-0001' }, new AbortController().signal)
  expect(result.ok).toBe(true)
  expect(result.value.parent.id).toBe('STORY-0001')
  expect(result.value.body).toContain('OAuth')
})

it('get folds unknown ids into an error result', async () => {
  const result = await handler('get', { id: 'TICKET-9999' }, new AbortController().signal)
  expect(result.ok).toBe(false)
  expect(result.error.message).toContain('NOT_FOUND')
})

it('update accepts title and priority', async () => {
  const result = await handler('update', { id: 'TICKET-0001', title: '登录接口 v2', priority: 2 }, new AbortController().signal)
  expect(result.ok).toBe(true)
  expect(result.value.title).toBe('登录接口 v2')
  expect(result.value.priority).toBe(2)
})

it('rejects invalid payloads and unknown endpoints', async () => {
  const badPayload = await handler('get', { id: 42 }, new AbortController().signal)
  expect(badPayload.ok).toBe(false)
  expect(badPayload.error.code).toBe('bad-request')

  const unknown = await handler('nope', {}, new AbortController().signal)
  expect(unknown.ok).toBe(false)
  expect(unknown.error.code).toBe('bad-request')
})

// REWRITTEN for U7a: was persisted to <home>/ui-filters.json (disk read
// asserted); the bag now lives in daemon storage (ui/filters-get|set), so
// persistence is proven by a FRESH service over the same runtime seeing the
// saved values instead of a file read.
it('filters-get defaults and filters-set persists via the daemon (STORY-0023)', async () => {
  const signal = new AbortController().signal
  const fresh = await handler('filters-get', {}, signal)
  expect(fresh.ok).toBe(true)
  expect(fresh.value).toEqual({
    query: '', showArchived: false, types: [], statuses: [], priorities: [], showId: false, sortOrder: 'none',
  })

  const saved = await handler('filters-set', {
    filters: { query: '登录', statuses: ['open', 'in_progress'], sortOrder: 'priority-desc' },
  }, signal)
  expect(saved.ok).toBe(true)
  expect(saved.value).toEqual({
    query: '登录', showArchived: false, types: [], statuses: ['open', 'in_progress'], priorities: [], showId: false, sortOrder: 'priority-desc',
  })

  const reloaded = await handler('filters-get', {}, signal)
  expect(reloaded.value).toEqual(saved.value)

  // Persistence is server-side: a brand-new client on the same daemon
  // observes the saved bag (the old assertion read ui-filters.json). U4:
  // the DSH surface persists under its surface-prefixed key.
  const { OmtService, DSH_FILTERS_KEY } = await import('../src/host/service.ts')
  const freshService = new OmtService({ runtimeDir: fixture.runtimeDir, name: 'rpc-persist-probe' })
  try {
    await freshService.ready()
    const probe = await freshService.filtersGet(fixture.globalHome, DSH_FILTERS_KEY)
    expect(probe.query).toBe('登录')
    expect(probe.sortOrder).toBe('priority-desc')
  } finally {
    await freshService.close()
  }
})

// REWRITTEN for U7a: the corrupt-file case has no direct equivalent — bags
// are validated BEFORE they reach daemon storage. Degradation is still
// exercised by storing an out-of-contract bag through the RAW service
// (which skips validation) and asserting the RPC read coerces it back to
// defaults; unknown/invalid patches keep their INVALID_INPUT refusal.
it('filters-set rejects invalid values; out-of-contract stored bags degrade to defaults', async () => {
  const signal = new AbortController().signal
  const junk = await handler('filters-set', { filters: { sortOrder: 'sideways' } }, signal)
  expect(junk.ok).toBe(false)
  expect(junk.error.message).toContain('INVALID_INPUT')

  await service.filtersSet(fixture.globalHome, 'ui', { garbage: true } as never)
  const degraded = await handler('filters-get', {}, signal)
  expect(degraded.ok).toBe(true)
  expect(degraded.value.query).toBe('')
  expect(degraded.value.sortOrder).toBe('none')
})

// TICKET-0123 identity translation: a payload sessionId must name a LIVE
// Cordis agent before any home/executor resolution happens. The default
// beforeEach stub has no agents registry (lenient path); these tests
// re-register the channel against a stub that DOES carry one.
describe('sessionId identity gate', () => {
  const LIVE = 'sess-live-1'
  let gatedHandler: Handler

  beforeEach(() => {
    const agentsStub = {
      get(id: string) {
        return id === LIVE
          ? { session: { header: { cwd: fixture.root } } }
          : undefined
      },
      list() {
        return [{ id: LIVE }]
      },
    }
    const stubCtx = {
      agents: agentsStub,
      connection: {
        rpc: {
          handle(_channel: string, h: Handler) {
            gatedHandler = h
          },
        },
      },
    }
    registerOmtRpc(stubCtx as never, service)
  })

  it('rejects a forged sessionId with FORBIDDEN', async () => {
    const result = await gatedHandler(
      'update',
      { id: 'TICKET-0001', status: 'in_progress', sessionId: 'sess-forged' },
      new AbortController().signal,
    )
    expect(result.ok).toBe(false)
    expect(result.error.message).toContain('FORBIDDEN')
  })

  it('accepts a live sessionId and still resolves the workspace home', async () => {
    const result = await gatedHandler(
      'get',
      { id: 'TICKET-0001', sessionId: LIVE },
      new AbortController().signal,
    )
    expect(result.ok).toBe(true)
    expect(result.value.node.id).toBe('TICKET-0001')
  })

  it('keeps the global-home fallback for absent sessionIds', async () => {
    const result = await gatedHandler('tree', {}, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value).toHaveLength(1)
  })
})
