/**
 * Run schema (v3) store tests: table creation, RUN id counter, v2→v3
 * migration losslessness, and reindex (rebuild) protection for runs/run_items.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtStore } from '../src/host/store.ts'
import type { OmtRun, OmtRunItem } from '../src/host/types.ts'

let home: string
let dbPath: string
let store: OmtStore

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-run-store-'))
  dbPath = join(home, 'omt.db')
  store = await OmtStore.open(dbPath)
})

afterEach(async () => {
  store.close()
  await rm(home, { recursive: true, force: true })
})

function makeRun(id: string, patch: Partial<OmtRun> = {}): OmtRun {
  return {
    id,
    status: 'pending',
    config: { stopOnFailure: false, autoContinue: true, autoVerify: false, concurrency: 1 },
    created_at: new Date().toISOString(),
    ...patch,
  }
}

function makeItem(runId: string, nodeId: string, position: number, patch: Partial<OmtRunItem> = {}): OmtRunItem {
  return {
    run_id: runId,
    node_id: nodeId,
    position,
    state: 'pending',
    attempts: 0,
    nudge_count: 0,
    ...patch,
  }
}

describe('schema v3', () => {
  it('creates runs/run_items tables on a fresh database', () => {
    const run = makeRun('RUN-0001', { title: '批量执行' })
    store.insertRun(run)
    store.insertRunItem(makeItem('RUN-0001', 'TICKET-0001', 0))
    store.insertRunItem(makeItem('RUN-0001', 'TICKET-0002', 1))

    const loaded = store.getRun('RUN-0001')
    expect(loaded).toEqual(run)
    expect(store.listRunItems('RUN-0001').map(item => item.node_id)).toEqual(['TICKET-0001', 'TICKET-0002'])
  })

  it('allocates sequential RUN ids from the meta counter', () => {
    expect(store.nextRunId()).toBe('RUN-0001')
    expect(store.nextRunId()).toBe('RUN-0002')
  })

  it('round-trips nullable fields and config JSON', () => {
    const run = makeRun('RUN-0001', {
      status: 'running',
      config: { stopOnFailure: true, autoContinue: false, autoVerify: true, concurrency: 3 },
    })
    store.insertRun(run)
    store.insertRunItem(makeItem('RUN-0001', 'TICKET-0001', 0, {
      state: 'failed',
      executor_session_id: 'sess-1',
      attempts: 2,
      last_error: 'boom',
      nudged_at: '2026-08-19T00:00:00.000Z',
      nudge_count: 2,
      started_at: '2026-08-19T00:00:00.000Z',
      finished_at: '2026-08-19T01:00:00.000Z',
    }))

    const loadedRun = store.getRun('RUN-0001')
    expect(loadedRun?.config).toEqual({ stopOnFailure: true, autoContinue: false, autoVerify: true, concurrency: 3 })
    const item = store.getRunItem('RUN-0001', 'TICKET-0001')
    expect(item).toMatchObject({
      state: 'failed',
      executor_session_id: 'sess-1',
      attempts: 2,
      last_error: 'boom',
      nudge_count: 2,
    })
    expect(store.getRunItem('RUN-0001', 'TICKET-0002')).toBeUndefined()
  })
})

describe('migration v2 → v3', () => {
  it('migrates a v2 database losslessly on open', async () => {
    // Hand-build a v2 database: v1 DDL + v2 archived column, no runs tables.
    store.close()
    await rm(dbPath, { force: true })
    const { DatabaseSync } = await import('node:sqlite')
    const legacy = new DatabaseSync(dbPath)
    legacy.exec(`
      CREATE TABLE nodes (
        id TEXT PRIMARY KEY, type TEXT NOT NULL, title TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'open', archived INTEGER NOT NULL DEFAULT 0,
        priority INTEGER NOT NULL DEFAULT 0, path TEXT NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL
      );
      CREATE TABLE edges (
        parent_id TEXT NOT NULL, child_id TEXT NOT NULL, ord INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (parent_id, child_id)
      );
      CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE nodes_search (id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL);
    `)
    legacy.prepare(
      'INSERT INTO nodes (id, type, title, status, archived, priority, path, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)',
    ).run('TICKET-0001', 'ticket', '旧库票据', 'done', 0, 1, 'tickets/x/ticket.md', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')
    legacy.prepare('INSERT INTO meta (key, value) VALUES (?, ?)').run('schema_version', '2')
    legacy.prepare('INSERT INTO meta (key, value) VALUES (?, ?)').run('counter_TICKET', '7')
    legacy.close()

    store = await OmtStore.open(dbPath)

    // Existing data is untouched.
    expect(store.getNode('TICKET-0001')).toMatchObject({ title: '旧库票据', status: 'done', priority: 1 })
    expect(store.counterValue('ticket')).toBe(7)
    // Version marker bumped; run tables usable.
    expect(store.schemaVersion).toBe('3')
    store.insertRun(makeRun(store.nextRunId()))
    expect(store.getRun('RUN-0001')).toBeDefined()
  })
})

describe('reindex protection', () => {
  it('rebuild only replaces nodes/edges/nodes_search and keeps runs/run_items', () => {
    const now = new Date().toISOString()
    store.insertNode({
      id: 'TICKET-0001', type: 'ticket', title: '旧', status: 'open',
      archived: false, priority: 0, path: 'tickets/old/ticket.md', created_at: now, updated_at: now,
    })
    const run = makeRun(store.nextRunId(), { status: 'running' })
    store.insertRun(run)
    store.insertRunItem(makeItem(run.id, 'TICKET-0001', 0, { state: 'running', executor_session_id: 'sess-1' }))

    // Rebuild the index from a completely different node set.
    store.rebuild([{
      id: 'TICKET-0009', type: 'ticket', title: '新', status: 'open',
      archived: false, priority: 0, path: 'tickets/new/ticket.md', created_at: now, updated_at: now,
    }], [], new Map())

    expect(store.getNode('TICKET-0001')).toBeUndefined()
    expect(store.getNode('TICKET-0009')).toBeDefined()
    // Run data survived the rebuild, including the running item.
    expect(store.getRun('RUN-0001')).toMatchObject({ status: 'running' })
    expect(store.getRunItem('RUN-0001', 'TICKET-0001')).toMatchObject({ state: 'running', executor_session_id: 'sess-1' })
    // The RUN counter is not a node counter: reindex must not reset it.
    expect(store.nextRunId()).toBe('RUN-0002')
  })
})
