/**
 * Archive-dimension tests (TICKET-0023): status preserved under archiving,
 * read-only enforcement on archived nodes, restore, and the v1→v2 migration.
 */
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { expectProblem } from './mocks/fixtures.ts'

let home: string
let core: OmtCore

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-archived-test-'))
  core = await OmtCore.open(home)
})

afterEach(async () => {
  core.close()
  await rm(home, { recursive: true, force: true })
})

it('archiving preserves the lifecycle status and writes the frontmatter flag', async () => {
  const epic = await core.create({ type: 'epic', title: '用户体系' })
  await core.update({ id: epic.id, status: 'in_progress' })
  const archived = await core.update({ id: epic.id, archived: true })

  expect(archived.archived).toBe(true)
  expect(archived.status).toBe('in_progress') // preserved, not overwritten

  const file = await readFile(join(home, epic.path), 'utf8')
  expect(file).toContain('archived: true')
  expect(file).toContain('status: in_progress')
})

it('archived nodes reject content changes; restore unlocks them', async () => {
  const epic = await core.create({ type: 'epic', title: '用户体系' })
  await core.update({ id: epic.id, archived: true })

  await expectProblem(core.update({ id: epic.id, status: 'done' }), 'ARCHIVED_READONLY', { nodeId: epic.id, operation: 'update' })
  await expectProblem(core.update({ id: epic.id, append: 'x' }), 'ARCHIVED_READONLY', { nodeId: epic.id, operation: 'update' })
  await expectProblem(core.update({ id: epic.id, title: '改名' }), 'ARCHIVED_READONLY', { nodeId: epic.id, operation: 'update' })

  const restored = await core.update({ id: epic.id, archived: false })
  expect(restored.archived).toBe(false)
  await core.update({ id: epic.id, status: 'done' }) // works again
  expect(core.getNode(epic.id)?.status).toBe('done')
})

it('reindex round-trips the archived flag through frontmatter', async () => {
  const epic = await core.create({ type: 'epic', title: '用户体系' })
  await core.update({ id: epic.id, status: 'done', archived: true })
  await core.reindex()
  const node = core.getNode(epic.id)
  expect(node?.archived).toBe(true)
  expect(node?.status).toBe('done')
})

it('migrates a v1 database (status=archived → archived column)', async () => {
  core.close()

  // Build a v1-shape database by hand: no archived column, one archived row.
  const { DatabaseSync } = await import('node:sqlite')
  const dbPath = join(home, 'omt.db')
  await rm(dbPath, { force: true })
  const db = new DatabaseSync(dbPath)
  db.exec(`
    CREATE TABLE nodes (id TEXT PRIMARY KEY, type TEXT NOT NULL, title TEXT NOT NULL,
      status TEXT NOT NULL DEFAULT 'open', priority INTEGER NOT NULL DEFAULT 0,
      path TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
    CREATE TABLE edges (parent_id TEXT NOT NULL, child_id TEXT NOT NULL, ord INTEGER NOT NULL DEFAULT 0,
      PRIMARY KEY (parent_id, child_id));
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE nodes_search (id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL);
    INSERT INTO meta (key, value) VALUES ('schema_version', '1');
    INSERT INTO nodes VALUES ('EPIC-0001', 'epic', '旧项目', 'archived', 0, 'tickets/x/epic.md', 't', 't');
  `)
  db.close()

  core = await OmtCore.open(home)
  const node = core.getNode('EPIC-0001')
  expect(node?.archived).toBe(true)
  expect(node?.status).toBe('open') // safe default; prior status unrecoverable
})
