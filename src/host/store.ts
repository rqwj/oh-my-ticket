/**
 * SQLite layer (`node:sqlite`, lazy-imported to keep host startup quiet):
 * `nodes` metadata + `edges` relations + `meta` counters/version + a
 * `nodes_search` content mirror for LIKE search. SQLite is the query/index
 * authority; the Markdown files remain the content authority (see reindex).
 *
 * Search note: FTS5's default unicode61 tokenizer does not segment CJK runs
 * (a whole Chinese phrase becomes one token, defeating prefix queries), so
 * content search uses parameterized LIKE over the mirror table instead —
 * predictable for CJK and more than fast enough at local-ticket scale.
 */
import type { DatabaseSync } from 'node:sqlite'
import {
  ID_PATTERN,
  TYPE_PREFIX,
  type NodeType,
  type OmtEdge,
  type OmtNode,
  type OmtRun,
  type OmtRunItem,
  type RunConfig,
  type RunItemState,
  type RunStatus,
  type Status,
  isRunItemState,
  isRunStatus,
} from './types.ts'

const SCHEMA_VERSION = '3'

const DDL = `
CREATE TABLE IF NOT EXISTS nodes (
  id         TEXT PRIMARY KEY,
  type       TEXT NOT NULL,
  title      TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'open',
  archived   INTEGER NOT NULL DEFAULT 0,
  priority   INTEGER NOT NULL DEFAULT 0,
  path       TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS edges (
  parent_id  TEXT NOT NULL REFERENCES nodes(id),
  child_id   TEXT NOT NULL REFERENCES nodes(id),
  ord        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (parent_id, child_id)
);
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS nodes_search (
  id    TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  body  TEXT NOT NULL
);
`

interface NodeRow {
  id: string
  type: string
  title: string
  status: string
  archived: number
  priority: number
  path: string
  created_at: string
  updated_at: string
}

function rowToNode(row: NodeRow): OmtNode {
  return { ...row, type: row.type as OmtNode['type'], status: row.status as Status, archived: row.archived === 1 }
}

interface RunRow {
  id: string
  title: string | null
  status: string
  config: string
  created_at: string
  finished_at: string | null
}

interface RunItemRow {
  run_id: string
  node_id: string
  position: number
  state: string
  executor_session_id: string | null
  attempts: number
  last_error: string | null
  nudged_at: string | null
  nudge_count: number
  started_at: string | null
  finished_at: string | null
}

function rowToRun(row: RunRow): OmtRun {
  const parsed: unknown = JSON.parse(row.config)
  return {
    id: row.id,
    ...(row.title === null ? {} : { title: row.title }),
    status: row.status as RunStatus,
    config: parsed as RunConfig,
    created_at: row.created_at,
    ...(row.finished_at === null ? {} : { finished_at: row.finished_at }),
  }
}

function rowToRunItem(row: RunItemRow): OmtRunItem {
  return {
    run_id: row.run_id,
    node_id: row.node_id,
    position: row.position,
    state: row.state as RunItemState,
    ...(row.executor_session_id === null ? {} : { executor_session_id: row.executor_session_id }),
    attempts: row.attempts,
    ...(row.last_error === null ? {} : { last_error: row.last_error }),
    ...(row.nudged_at === null ? {} : { nudged_at: row.nudged_at }),
    nudge_count: row.nudge_count,
    ...(row.started_at === null ? {} : { started_at: row.started_at }),
    ...(row.finished_at === null ? {} : { finished_at: row.finished_at }),
  }
}

/** v1 → v2: archive becomes its own column; prior lifecycle status is unrecoverable (safe default 'open'). */
const MIGRATION_V2 = `
ALTER TABLE nodes ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
UPDATE nodes SET archived = 1, status = 'open' WHERE status = 'archived';
`

/**
 * v2 → v3: run tables (EPIC-0003). Purely additive: no existing data is
 * touched. Runs live only in SQLite (no markdown files), and `rebuild`
 * (reindex) explicitly never touches these two tables.
 */
const MIGRATION_V3 = `
CREATE TABLE IF NOT EXISTS runs (
  id          TEXT PRIMARY KEY,
  title       TEXT,
  status      TEXT NOT NULL DEFAULT 'pending',
  config      TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  finished_at TEXT
);
CREATE TABLE IF NOT EXISTS run_items (
  run_id              TEXT NOT NULL REFERENCES runs(id),
  node_id             TEXT NOT NULL,
  position            INTEGER NOT NULL,
  state               TEXT NOT NULL DEFAULT 'pending',
  executor_session_id TEXT,
  attempts            INTEGER NOT NULL DEFAULT 0,
  last_error          TEXT,
  nudged_at           TEXT,
  nudge_count         INTEGER NOT NULL DEFAULT 0,
  started_at          TEXT,
  finished_at         TEXT,
  PRIMARY KEY (run_id, node_id)
);
`

export class OmtStore {
  private constructor(private readonly db: DatabaseSync) {}

  /** Open (or create) the database at `dbPath` and ensure the schema. */
  static async open(dbPath: string): Promise<OmtStore> {
    // Lazy import: node:sqlite still emits an ExperimentalWarning on Node 22,
    // so it is loaded on first use rather than at plugin load.
    const { DatabaseSync } = await import('node:sqlite')
    const db = new DatabaseSync(dbPath)
    db.exec('PRAGMA journal_mode = WAL')
    db.exec('PRAGMA foreign_keys = ON')
    db.exec(DDL)
    // v1→v2 migration: add the archived column to databases created before it existed.
    const columns = db.prepare('PRAGMA table_info(nodes)').all() as { name: string }[]
    if (!columns.some(column => column.name === 'archived')) {
      db.exec(MIGRATION_V2)
    }
    // v2→v3 migration: add the run tables to databases created before they existed.
    const tables = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all() as { name: string }[]
    if (!tables.some(table => table.name === 'runs')) {
      db.exec(MIGRATION_V3)
    }
    const store = new OmtStore(db)
    // Bump the version marker of pre-v3 databases (v1 databases carry no
    // marker at all; the core's first-open reindex/mark path handles those).
    const version = store.schemaVersion
    if (version !== undefined && Number(version) < Number(SCHEMA_VERSION)) {
      store.markSchemaVersion()
    }
    return store
  }

  close(): void {
    this.db.close()
  }

  // ── meta ─────────────────────────────────────────────────────────────

  getMeta(key: string): string | undefined {
    const row = this.db.prepare('SELECT value FROM meta WHERE key = ?').get(key) as { value: string } | undefined
    return row?.value
  }

  setMeta(key: string, value: string): void {
    this.db.prepare('INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value')
      .run(key, value)
  }

  get schemaVersion(): string | undefined {
    return this.getMeta('schema_version')
  }

  markSchemaVersion(): void {
    this.setMeta('schema_version', SCHEMA_VERSION)
  }

  /** Current counter value for a type (last allocated number; 0 = none). */
  counterValue(type: NodeType): number {
    return Number(this.getMeta(`counter_${TYPE_PREFIX[type]}`) ?? '0')
  }

  /** Move a type's counter to an explicit value (pool-wide id sync). */
  setCounter(type: NodeType, value: number): void {
    this.setMeta(`counter_${TYPE_PREFIX[type]}`, String(value))
  }

  /** Allocate the next id for a type (`EPIC-0001`, counters independent). */
  nextId(type: NodeType): string {
    const prefix = TYPE_PREFIX[type]
    const key = `counter_${prefix}`
    const next = Number(this.getMeta(key) ?? '0') + 1
    this.setMeta(key, String(next))
    return `${prefix}-${String(next).padStart(4, '0')}`
  }

  /** After a reindex, move counters past every id seen on disk. */
  resetCounters(ids: readonly string[]): void {
    for (const id of ids) {
      const match = ID_PATTERN.exec(id)
      if (match === null) continue
      const key = `counter_${match[1]}`
      const current = Number(this.getMeta(key) ?? '0')
      const seen = Number(match[2])
      if (seen > current) this.setMeta(key, String(seen))
    }
  }

  // ── nodes ────────────────────────────────────────────────────────────

  insertNode(node: OmtNode): void {
    this.db.prepare(
      'INSERT INTO nodes (id, type, title, status, archived, priority, path, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)',
    ).run(node.id, node.type, node.title, node.status, node.archived ? 1 : 0, node.priority, node.path, node.created_at, node.updated_at)
  }

  updateNode(id: string, patch: { title?: string; status?: Status; archived?: boolean; priority?: number; path?: string; updated_at?: string }): void {
    const entries = Object.entries(patch)
      .filter(([, value]) => value !== undefined)
      .map(([key, value]) => [key, key === 'archived' ? (value === true ? 1 : 0) : value] as [string, string | number])
    if (entries.length === 0) return
    const assignments = entries.map(([key]) => `${key} = ?`).join(', ')
    this.db.prepare(`UPDATE nodes SET ${assignments} WHERE id = ?`)
      .run(...entries.map(([, value]) => value), id)
  }

  getNode(id: string): OmtNode | undefined {
    const row = this.db.prepare('SELECT * FROM nodes WHERE id = ?').get(id) as NodeRow | undefined
    return row === undefined ? undefined : rowToNode(row)
  }

  listNodes(filter: { type?: NodeType; status?: Status } = {}): OmtNode[] {
    const conditions: string[] = []
    const params: string[] = []
    if (filter.type !== undefined) {
      conditions.push('type = ?')
      params.push(filter.type)
    }
    if (filter.status !== undefined) {
      conditions.push('status = ?')
      params.push(filter.status)
    }
    const where = conditions.length > 0 ? ` WHERE ${conditions.join(' AND ')}` : ''
    const rows = this.db.prepare(`SELECT * FROM nodes${where} ORDER BY id`).all(...params) as unknown as NodeRow[]
    return rows.map(rowToNode)
  }

  allNodes(): OmtNode[] {
    return this.listNodes()
  }

  // ── edges ────────────────────────────────────────────────────────────

  insertEdge(parentId: string, childId: string, ord: number): void {
    this.db.prepare('INSERT INTO edges (parent_id, child_id, ord) VALUES (?, ?, ?)').run(parentId, childId, ord)
  }

  deleteEdge(parentId: string, childId: string): void {
    this.db.prepare('DELETE FROM edges WHERE parent_id = ? AND child_id = ?').run(parentId, childId)
  }

  childrenOf(parentId: string): OmtNode[] {
    const rows = this.db.prepare(
      'SELECT n.* FROM edges e JOIN nodes n ON n.id = e.child_id WHERE e.parent_id = ? ORDER BY e.ord, e.child_id',
    ).all(parentId) as unknown as NodeRow[]
    return rows.map(rowToNode)
  }

  parentOf(childId: string): OmtNode | undefined {
    const row = this.db.prepare(
      'SELECT n.* FROM edges e JOIN nodes n ON n.id = e.parent_id WHERE e.child_id = ?',
    ).get(childId) as NodeRow | undefined
    return row === undefined ? undefined : rowToNode(row)
  }

  allEdges(): OmtEdge[] {
    return this.db.prepare('SELECT parent_id, child_id, ord FROM edges ORDER BY parent_id, ord').all() as unknown as OmtEdge[]
  }

  // ── content search ───────────────────────────────────────────────────

  /** Upsert one node's searchable content mirror. */
  indexNode(id: string, title: string, body: string): void {
    this.db.prepare(
      'INSERT INTO nodes_search (id, title, body) VALUES (?, ?, ?) ON CONFLICT(id) DO UPDATE SET title = excluded.title, body = excluded.body',
    ).run(id, title, body)
  }

  /**
   * Content search over titles and bodies: every whitespace-separated token
   * must appear somewhere (AND), title hits rank before body-only hits.
   */
  search(query: string, limit = 20): string[] {
    const tokens = query.trim().split(/\s+/).filter(t => t !== '')
    if (tokens.length === 0) return []
    const escape = (token: string): string => `%${token.replace(/[\\%_]/g, ch => `\\${ch}`)}%`
    const conditions = tokens.map(() => "(title LIKE ? ESCAPE '\\' OR body LIKE ? ESCAPE '\\')").join(' AND ')
    const params = tokens.flatMap(token => [escape(token), escape(token)])
    const rows = this.db.prepare(
      `SELECT id, title FROM nodes_search WHERE ${conditions}`,
    ).all(...params) as unknown as { id: string; title: string }[]
    return rows
      .sort((a, b) => {
        const aTitle = tokens.every(token => a.title.includes(token))
        const bTitle = tokens.every(token => b.title.includes(token))
        if (aTitle !== bTitle) return aTitle ? -1 : 1
        return a.id.localeCompare(b.id)
      })
      .slice(0, limit)
      .map(row => row.id)
  }

  // ── runs ─────────────────────────────────────────────────────────────

  /** Allocate the next run id (`RUN-0001`, counter shared via meta). */
  nextRunId(): string {
    const key = 'counter_RUN'
    const next = Number(this.getMeta(key) ?? '0') + 1
    this.setMeta(key, String(next))
    return `RUN-${String(next).padStart(4, '0')}`
  }

  insertRun(run: OmtRun): void {
    this.db.prepare(
      'INSERT INTO runs (id, title, status, config, created_at, finished_at) VALUES (?, ?, ?, ?, ?, ?)',
    ).run(run.id, run.title ?? null, run.status, JSON.stringify(run.config), run.created_at, run.finished_at ?? null)
  }

  updateRun(id: string, patch: { title?: string | null; status?: RunStatus; finished_at?: string | null }): void {
    const entries = Object.entries(patch).filter(([, value]) => value !== undefined)
    if (entries.length === 0) return
    const assignments = entries.map(([key]) => `${key} = ?`).join(', ')
    this.db.prepare(`UPDATE runs SET ${assignments} WHERE id = ?`)
      .run(...entries.map(([, value]) => value as string | null), id)
  }

  getRun(id: string): OmtRun | undefined {
    const row = this.db.prepare('SELECT * FROM runs WHERE id = ?').get(id) as RunRow | undefined
    return row === undefined ? undefined : rowToRun(row)
  }

  listRuns(filter: { status?: RunStatus } = {}): OmtRun[] {
    const where = filter.status === undefined ? '' : ' WHERE status = ?'
    const params = filter.status === undefined ? [] : [filter.status]
    const rows = this.db.prepare(`SELECT * FROM runs${where} ORDER BY id`).all(...params) as unknown as RunRow[]
    return rows.map(rowToRun)
  }

  // ── run items ────────────────────────────────────────────────────────

  insertRunItem(item: OmtRunItem): void {
    this.db.prepare(
      `INSERT INTO run_items (run_id, node_id, position, state, executor_session_id, attempts, last_error, nudged_at, nudge_count, started_at, finished_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    ).run(
      item.run_id, item.node_id, item.position, item.state, item.executor_session_id ?? null,
      item.attempts, item.last_error ?? null, item.nudged_at ?? null, item.nudge_count,
      item.started_at ?? null, item.finished_at ?? null,
    )
  }

  updateRunItem(runId: string, nodeId: string, patch: Partial<{
    state: RunItemState
    position: number
    executor_session_id: string | null
    attempts: number
    last_error: string | null
    nudged_at: string | null
    nudge_count: number
    started_at: string | null
    finished_at: string | null
  }>): void {
    const entries = Object.entries(patch).filter(([, value]) => value !== undefined)
    if (entries.length === 0) return
    const assignments = entries.map(([key]) => `${key} = ?`).join(', ')
    this.db.prepare(`UPDATE run_items SET ${assignments} WHERE run_id = ? AND node_id = ?`)
      .run(...entries.map(([, value]) => value as string | number | null), runId, nodeId)
  }

  getRunItem(runId: string, nodeId: string): OmtRunItem | undefined {
    const row = this.db.prepare('SELECT * FROM run_items WHERE run_id = ? AND node_id = ?').get(runId, nodeId) as RunItemRow | undefined
    return row === undefined ? undefined : rowToRunItem(row)
  }

  /** Item-level removal (omt_run_control remove): membership only, nodes untouched. */
  deleteRunItem(runId: string, nodeId: string): void {
    this.db.prepare('DELETE FROM run_items WHERE run_id = ? AND node_id = ?').run(runId, nodeId)
  }

  /**
   * Every item for one node across runs in the given statuses (passive
   * observation scans the active runs holding a ticket — EPIC-0003
   * decision 1 cross-run broadcast).
   */
  runItemsForNode(nodeId: string, runStatuses: readonly RunStatus[]): OmtRunItem[] {
    if (runStatuses.length === 0) return []
    const placeholders = runStatuses.map(() => '?').join(', ')
    const rows = this.db.prepare(
      `SELECT i.* FROM run_items i JOIN runs r ON r.id = i.run_id
       WHERE i.node_id = ? AND r.status IN (${placeholders}) ORDER BY i.run_id`,
    ).all(nodeId, ...runStatuses) as unknown as RunItemRow[]
    return rows.map(rowToRunItem)
  }

  /**
   * Atomic claim (TICKET-0058): select the next pending item and flip it to
   * running with the executor in ONE immediate transaction, so two
   * concurrent claimers can never receive the same item.
   */
  claimNextRunItem(runId: string, executorSessionId: string, now: string): OmtRunItem | undefined {
    this.db.exec('BEGIN IMMEDIATE')
    try {
      const row = this.db.prepare(
        "SELECT node_id FROM run_items WHERE run_id = ? AND state = 'pending' ORDER BY position, node_id LIMIT 1",
      ).get(runId) as { node_id: string } | undefined
      if (row === undefined) {
        this.db.exec('COMMIT')
        return undefined
      }
      this.db.prepare(
        "UPDATE run_items SET state = 'running', executor_session_id = ?, started_at = COALESCE(started_at, ?) WHERE run_id = ? AND node_id = ? AND state = 'pending'",
      ).run(executorSessionId, now, runId, row.node_id)
      this.db.exec('COMMIT')
      return this.getRunItem(runId, row.node_id)
    } catch (error) {
      this.db.exec('ROLLBACK')
      throw error
    }
  }

  listRunItems(runId: string): OmtRunItem[] {
    const rows = this.db.prepare('SELECT * FROM run_items WHERE run_id = ? ORDER BY position, node_id').all(runId) as unknown as RunItemRow[]
    return rows.map(rowToRunItem)
  }

  /** All runs whose status is one of `statuses` (janitor sweep). */
  listRunsByStatus(statuses: readonly RunStatus[]): OmtRun[] {
    const placeholders = statuses.map(() => '?').join(', ')
    const rows = this.db.prepare(`SELECT * FROM runs WHERE status IN (${placeholders}) ORDER BY id`).all(...statuses) as unknown as RunRow[]
    return rows.map(rowToRun)
  }

  // ── rebuild ──────────────────────────────────────────────────────────

  /**
   * Replace the whole index content (reindex): nodes, edges, search mirror.
   * Reindex protection (EPIC-0003 decision 13): `runs`/`run_items` are
   * deliberately NOT touched here — they have no on-disk representation and
   * a rebuild must never drop them.
   */
  rebuild(nodes: readonly OmtNode[], edges: readonly OmtEdge[], bodies: ReadonlyMap<string, { title: string; body: string }>): void {
    this.db.exec('BEGIN')
    try {
      this.db.exec('DELETE FROM edges')
      this.db.exec('DELETE FROM nodes')
      this.db.exec('DELETE FROM nodes_search')
      for (const node of nodes) this.insertNode(node)
      for (const edge of edges) this.insertEdge(edge.parent_id, edge.child_id, edge.ord)
      for (const [id, content] of bodies) this.indexNode(id, content.title, content.body)
      this.resetCounters(nodes.map(node => node.id))
      this.markSchemaVersion()
      this.db.exec('COMMIT')
    } catch (error) {
      this.db.exec('ROLLBACK')
      throw error
    }
  }
}
