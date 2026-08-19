/**
 * Trigger source tests: the '@' ticket source against a stub RPC caller —
 * candidate mapping, pick → ReferenceInsert, lexicon caching, and codec
 * serialization (including failure blocking the send).
 */
import { describe, expect, it } from 'vitest'
import {
  createTicketSource,
  OMT_REF_SOURCE,
  type RpcResultLike,
} from '../src/client/trigger/source.ts'
import { zh, type Translate } from '../src/client/locales.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

/** Dictionary-backed t over zh (mirrors the shell's {param} interpolation). */
const t: Translate = (key, params) =>
  zh[key].replace(/\{(\w+)\}/g, (_match, name: string) => String(params?.[name] ?? ''))

const SESSION = { sessionId: 'session-test' }

function stubRpc(routes: Record<string, RpcResultLike | ((payload: any) => RpcResultLike)>) {
  const calls: { endpoint: string; payload: any }[] = []
  return {
    calls,
    async call(_channel: string, endpoint: string, payload: any): Promise<RpcResultLike> {
      calls.push({ endpoint, payload })
      const route = routes[endpoint]
      return typeof route === 'function' ? route(payload) : route ?? { ok: false, error: { message: 'no route' } }
    },
  }
}

const SEARCH_OK: RpcResultLike = {
  ok: true,
  value: [
    { id: 'STORY-0001', type: 'story', title: '登录', status: 'open' },
    { id: 'TICKET-0001', type: 'ticket', title: '登录接口', status: 'in_progress' },
  ],
}

const GET_OK: RpcResultLike = {
  ok: true,
  value: {
    node: { id: 'TICKET-0001', type: 'ticket', title: '登录接口', status: 'in_progress', path: 'tickets/x/ticket.md' },
    parent: { id: 'STORY-0001', type: 'story', title: '登录', status: 'open' },
    children: [{ id: 'SUBTICKET-0001', type: 'subticket', title: '参数校验', status: 'open' }],
    body: '## 描述\n\n支持 OAuth 授权码模式\n\n<!-- omt:children -->\n## 子节点\n\n- x\n<!-- /omt:children -->\n',
  },
}

it('candidates maps search results and caches the lexicon', async () => {
  const rpc = stubRpc({ search: SEARCH_OK })
  const source = createTicketSource(rpc, undefined, t)

  const candidates = await source.candidates(SESSION, { query: '登录', position: 'inline', signal: new AbortController().signal })
  expect(rpc.calls[0]).toEqual({ endpoint: 'search', payload: { sessionId: 'session-test', query: '登录', limit: 20 } })
  expect(candidates).toHaveLength(2)
  expect(candidates[1]).toEqual({ name: 'TICKET-0001', description: '[ticket · in_progress] 登录接口', icon: '🔵' })
  expect(source.lexicon?.(SESSION)).toEqual(['STORY-0001', 'TICKET-0001'])
})

it('candidates exclude archived, sink done, and color status icons', async () => {
  const rpc = stubRpc({
    search: {
      ok: true,
      value: [
        { id: 'T-1', type: 'ticket', title: '已完成的事', status: 'done', archived: false },
        { id: 'T-2', type: 'ticket', title: '进行中的事', status: 'in_progress', archived: false },
        { id: 'T-3', type: 'ticket', title: '归档的事', status: 'done', archived: true },
        { id: 'T-4', type: 'ticket', title: '未开始的事', status: 'open', archived: false },
      ],
    } as RpcResultLike,
  })
  const source = createTicketSource(rpc, undefined, t)
  const candidates = await source.candidates(SESSION, { query: '事', position: 'inline', signal: new AbortController().signal })
  // Archived (T-3) is never offered; done (T-1) sinks to the end.
  expect(candidates.map(c => c.name)).toEqual(['T-2', 'T-4', 'T-1'])
  expect(candidates.map(c => c.icon)).toEqual(['🔵', '⚪', '🟢'])
})

it('candidates degrades to an empty list on RPC failure', async () => {
  const rpc = stubRpc({ search: { ok: false, error: { message: 'boom' } } })
  const source = createTicketSource(rpc, undefined, t)
  const candidates = await source.candidates(SESSION, { query: 'x', position: 'inline', signal: new AbortController().signal })
  expect(candidates).toEqual([])
})

it('onPick returns a ReferenceInsert for the picked node', () => {
  const source = createTicketSource(stubRpc({}), undefined, t)
  const outcome = source.onPick({
    candidate: { name: 'TICKET-0001' },
    session: SESSION,
    position: 'inline',
    via: 'menu',
    span: { start: 0, end: 6, draftRev: 1 },
  })
  expect(outcome).toEqual({
    insert: {
      source: OMT_REF_SOURCE,
      ref: 'TICKET-0001',
      label: 'TICKET-0001',
      clipboardText: '@TICKET-0001',
    },
  })
})

it('onPick labels the chip with id + title once candidates warmed the map', async () => {
  const rpc = stubRpc({ search: SEARCH_OK })
  const source = createTicketSource(rpc, undefined, t)
  await source.candidates(SESSION, { query: '', position: 'inline', signal: new AbortController().signal })

  const outcome = source.onPick({
    candidate: { name: 'TICKET-0001' },
    session: SESSION,
    position: 'inline',
    via: 'menu',
    span: { start: 0, end: 6, draftRev: 1 },
  })
  // Hover tooltip carries the full ticket name.
  expect(outcome).toMatchObject({ insert: { label: 'TICKET-0001 登录接口' } })
})

it('codec serializes a reference into model-readable ticket content', async () => {
  const rpc = stubRpc({ get: GET_OK })
  const source = createTicketSource(rpc, undefined, t)
  const text = await source.codec!.serialize('TICKET-0001', new AbortController().signal)

  expect(text).toContain('<omt-ticket id="TICKET-0001" type="ticket" status="in_progress" title="登录接口">')
  expect(text).toContain('父节点: STORY-0001 登录')
  expect(text).toContain('子节点: SUBTICKET-0001 参数校验')
  expect(text).toContain('支持 OAuth 授权码模式')
  expect(text).not.toContain('omt:children') // managed block stays out
  expect(text).toContain('</omt-ticket>')
  expect(source.codec!.clipboardText('TICKET-0001')).toBe('@TICKET-0001')
})

it('codec throws on RPC failure (blocks the send per contract)', async () => {
  const rpc = stubRpc({ get: { ok: false, error: { message: 'NOT_FOUND: unknown node' } } })
  const source = createTicketSource(rpc, undefined, t)
  await expect(source.codec!.serialize('TICKET-9999', new AbortController().signal))
    .rejects.toThrow(/无法序列化 ticket 引用/)
})

it('warm populates the lexicon without blocking', async () => {
  const rpc = stubRpc({ search: SEARCH_OK })
  const source = createTicketSource(rpc, undefined, t)
  expect(source.lexicon?.(SESSION)).toBeUndefined()
  source.warm?.(SESSION)
  await new Promise(resolve => setTimeout(resolve, 0))
  expect(source.lexicon?.(SESSION)).toEqual(['STORY-0001', 'TICKET-0001'])
})
