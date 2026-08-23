/**
 * RPC layer tests: the `/omt` channel handler against a real OmtCore —
 * endpoint payloads (zod-validated), result envelopes, and error folding.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { registerOmtRpc } from '../src/host/rpc.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

type Handler = (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>

let home: string
let core: OmtCore
let handler: Handler
let capturedAuthority: string | undefined

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-rpc-test-'))
  core = await OmtCore.open(home)
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
  registerOmtRpc(stubCtx as never, new OmtCorePool(home))

  const epic = await core.create({ type: 'epic', title: '用户体系' })
  const story = await core.create({ type: 'story', title: '登录', parentId: epic.id })
  await core.create({ type: 'ticket', title: '登录接口', parentId: story.id, body: '支持 OAuth 授权码模式' })
})

afterEach(async () => {
  core.close()
  await rm(home, { recursive: true, force: true })
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

it('update accepts a full body replace', async () => {
  const result = await handler('update', { id: 'TICKET-0001', body: '全新正文' }, new AbortController().signal)
  expect(result.ok).toBe(true)
  const shown = await handler('get', { id: 'TICKET-0001' }, new AbortController().signal)
  expect(shown.value.body).toContain('全新正文')
  expect(shown.value.body).not.toContain('OAuth')
})

it('get returns ancestors from root to parent', async () => {
  const result = await handler('get', { id: 'TICKET-0001' }, new AbortController().signal)
  expect(result.value.ancestors.map((n: { id: string }) => n.id)).toEqual(['EPIC-0001', 'STORY-0001'])
  const root = await handler('get', { id: 'EPIC-0001' }, new AbortController().signal)
  expect(root.value.ancestors).toEqual([])
})

it('create adds a legal child and a root epic', async () => {
  const child = await handler('create', { type: 'ticket', title: '新票', parentId: 'STORY-0001' }, new AbortController().signal)
  expect(child.ok).toBe(true)
  expect(child.value.type).toBe('ticket')
  const epic = await handler('create', { type: 'epic', title: '新史诗' }, new AbortController().signal)
  expect(epic.ok).toBe(true)
  expect(epic.value.type).toBe('epic')
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

it('skills returns catalog rows plus missing binds', async () => {
  let skillsHandler: Handler | undefined
  const stubCtx = {
    connection: {
      rpc: {
        handle(_channel: string, h: Handler) {
          skillsHandler = h
        },
      },
    },
  }
  registerOmtRpc(stubCtx as never, new OmtCorePool(home), undefined, undefined, undefined, {
    getSettings: () => ({ extraPrompt: '拆票标题用中文', boundSkillNames: ['ce-plan', 'gone'] }),
    listCatalog: async () => [{ name: 'ce-plan', description: 'plan' }, { name: 'omt', description: 'omt' }],
  })
  const withSession = await skillsHandler!('skills', { sessionId: 's1' }, new AbortController().signal)
  expect(withSession.ok).toBe(true)
  const result = await skillsHandler!('skills', {}, new AbortController().signal)
  expect(result.ok).toBe(true)
  expect(result.value.extraPrompt).toBe('拆票标题用中文')
  expect(result.value.skills).toEqual([
    { name: 'ce-plan', description: 'plan', bound: true, missing: false },
    { name: 'omt', description: 'omt', bound: false, missing: false },
    { name: 'gone', description: '', bound: true, missing: true },
  ])
})

it('filters-get defaults and filters-set persists to <home>/ui-filters.json (STORY-0023)', async () => {
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

  const { readFile } = await import('node:fs/promises')
  const onDisk = JSON.parse(await readFile(join(home, 'ui-filters.json'), 'utf8'))
  expect(onDisk.query).toBe('登录')
  expect(onDisk.sortOrder).toBe('priority-desc')
})

it('filters-set rejects unknown fields and invalid values; corrupt files degrade to defaults', async () => {
  const signal = new AbortController().signal
  const junk = await handler('filters-set', { filters: { sortOrder: 'sideways' } }, signal)
  expect(junk.ok).toBe(false)
  expect(junk.error.message).toContain('INVALID_INPUT')

  const { writeFile } = await import('node:fs/promises')
  await writeFile(join(home, 'ui-filters.json'), '{not json', 'utf8')
  const degraded = await handler('filters-get', {}, signal)
  expect(degraded.ok).toBe(true)
  expect(degraded.value.query).toBe('')
  expect(degraded.value.sortOrder).toBe('none')
})
