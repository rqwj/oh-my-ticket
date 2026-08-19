/**
 * OmtCore: orchestrates the SQLite index and the Markdown file tree so every
 * mutation stays dual-write consistent. Rules enforced here: hierarchy
 * (HIERARCHY), id allocation, stable directory names, managed children
 * blocks, and full reindex from disk (files are the content authority).
 */
import { readFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { OmtFiles } from './files.ts'
import {
  defaultBody,
  parseNodeFile,
  renderChildrenBlock,
  replaceChildrenBlock,
  serializeNodeFile,
  stripChildrenBlock,
} from './markdown.ts'
import { OmtStore } from './store.ts'
import {
  DEFAULT_RUN_CONFIG,
  HIERARCHY,
  ID_PATTERN,
  OmtError,
  RUN_ITEM_FINAL_STATES,
  RUN_ITEM_FAILURE_STATES,
  isNodeType,
  isRunItemState,
  isStatus,
  type NodeFrontmatter,
  type NodeType,
  type OmtEdge,
  type OmtNode,
  type OmtRun,
  type OmtRunItem,
  type OmtTreeNode,
  type RunConfig,
  type RunItemState,
  type RunStatus,
  type Status,
} from './types.ts'

export interface CreateInput {
  readonly type: NodeType
  readonly title: string
  readonly parentId?: string
  readonly body?: string
  readonly priority?: number
  /** Pre-allocated id (pool-wide uniqueness); omit to use the local counter. */
  readonly id?: string
}

export interface UpdateInput {
  readonly id: string
  readonly title?: string
  readonly status?: Status
  /** Archive (true) or restore (false) — the only change an archived node accepts. */
  readonly archived?: boolean
  readonly priority?: number
  /** Replace the whole user body. */
  readonly body?: string
  /** Append a progress note to the user body (ignored when body is set). */
  readonly append?: string
}

export interface ShowResult {
  readonly node: OmtNode
  readonly body: string
  readonly parent?: OmtNode
  readonly children: OmtNode[]
}

export interface ReindexResult {
  readonly nodes: number
  readonly edges: number
  readonly skipped: number
}

export interface CreateRunInput {
  /** Optional human label (multi-run pickers); falls back to the id. */
  readonly title?: string
  /** Config overrides; missing keys take DEFAULT_RUN_CONFIG. */
  readonly config?: Partial<RunConfig>
  /** Member node ids in execution order (snapshot; duplicates rejected). */
  readonly nodeIds: readonly string[]
}

export interface TransitionItemOptions {
  /** Session executing the item (recorded when entering running). */
  readonly executorSessionId?: string
  /** Failure detail kept in last_error (also across retries). */
  readonly error?: string
}

/**
 * Legal run transitions. `interrupted` is not an absolute terminal state:
 * resume takes it back to running (EPIC-0003 decision 7).
 * `completed_with_failures` can only be left via a row-level retry, which
 * reopens the run to running (only `canceled` explicitly forbids retry).
 */
const RUN_TRANSITIONS: Readonly<Record<RunStatus, readonly RunStatus[]>> = {
  pending: ['running', 'canceled'],
  running: ['paused', 'canceled', 'completed', 'completed_with_failures', 'interrupted'],
  paused: ['running', 'canceled', 'completed', 'completed_with_failures', 'interrupted'],
  interrupted: ['running', 'canceled'],
  completed: [],
  completed_with_failures: ['running'],
  canceled: [],
}

/**
 * Legal direct item transitions. Terminal-ish states (done/failed/blocked/
 * skipped/interrupted) have no direct exits — they move only through the
 * dedicated retry (failed/interrupted/stalled pending) and replay
 * (done/blocked/skipped) paths. Pending additionally allows done/blocked:
 * passive observation (TICKET-0061) must map a ticket directly set to those
 * statuses onto a not-yet-dispatched item, or the run would wedge.
 */
const ITEM_TRANSITIONS: Readonly<Record<RunItemState, readonly RunItemState[]>> = {
  pending: ['running', 'done', 'blocked', 'skipped'],
  running: ['done', 'failed', 'blocked', 'skipped', 'awaiting_confirmation', 'interrupted'],
  awaiting_confirmation: ['done', 'failed', 'blocked', 'skipped', 'running'],
  done: [],
  failed: [],
  blocked: [],
  skipped: [],
  interrupted: [],
}

/** Explicit report vocabulary (TICKET-0059): the only legal outcomes. */
export const RUN_REPORT_OUTCOMES = ['done', 'failed', 'blocked', 'skipped'] as const
export type RunReportOutcome = (typeof RUN_REPORT_OUTCOMES)[number]

/** Result of an explicit report: the transitioned item plus the node as-is. */
export interface ReportResult {
  readonly item: OmtRunItem
  readonly node: OmtNode
}

/** A node change observed through the normal update paths (TICKET-0061). */
export interface ObservedNodeChange {
  readonly status?: Status
  readonly archived?: boolean
}

const DB_FILE = 'omt.db'

export interface OpenOptions {
  /**
   * Sessions known to be alive at open time, consulted by the startup
   * janitor. Default (and safe) assumption after a fresh process start:
   * none — every running item whose executor is not in this set is demoted
   * to interrupted. Callers that can enumerate live DSH sessions (after the
   * session registry has warmed up) should pass them here.
   */
  readonly activeSessionIds?: readonly string[]
}

export interface JanitorResult {
  readonly interruptedRuns: string[]
  readonly interruptedItems: OmtRunItem[]
}

export class OmtCore {
  private constructor(
    readonly home: string,
    private readonly store: OmtStore,
    private readonly files: OmtFiles,
  ) {}

  /**
   * Open the OMT home: create directories, open the database, and reindex
   * when the database is fresh but markdown files already exist on disk.
   * Finally run the startup janitor (decision 12): running runs/items left
   * over from a previous process have no live executor here and are demoted
   * to interrupted (resumable via resume + row-level retry).
   */
  static async open(home: string, options: OpenOptions = {}): Promise<OmtCore> {
    const files = new OmtFiles(home)
    await files.ensureDirs()
    const store = await OmtStore.open(join(home, DB_FILE))
    const core = new OmtCore(home, store, files)
    if (store.schemaVersion === undefined) {
      const existing = await files.listNodeFiles()
      if (existing.length > 0) {
        await core.reindex()
      } else {
        store.markSchemaVersion()
      }
    }
    const active = new Set(options.activeSessionIds ?? [])
    core.janitorSweep(sessionId => active.has(sessionId))
    return core
  }

  close(): void {
    this.store.close()
  }

  // ── create ───────────────────────────────────────────────────────────

  async create(input: CreateInput): Promise<OmtNode> {
    const title = input.title.trim()
    if (title === '') throw new OmtError('INVALID_INPUT', 'title must not be empty')
    if (!isNodeType(input.type)) throw new OmtError('INVALID_INPUT', `unknown node type: ${String(input.type)}`)

    let parent: OmtNode | undefined
    if (input.parentId === undefined) {
      if (input.type !== 'epic') {
        throw new OmtError('INVALID_HIERARCHY', `${input.type} requires a parent; only epic can be created at root`)
      }
    } else {
      parent = this.requireNode(input.parentId)
      if (!HIERARCHY[parent.type].includes(input.type)) {
        throw new OmtError('INVALID_HIERARCHY', `${parent.type} cannot contain ${input.type}`)
      }
    }

    const id = input.id ?? this.store.nextId(input.type)
    if (this.store.getNode(id) !== undefined) throw new OmtError('CONFLICT', `duplicate node id: ${id}`)
    const now = new Date().toISOString()
    const path = this.files.pathFor(input.type, id, title, parent?.path)
    const node: OmtNode = {
      id,
      type: input.type,
      title,
      status: 'open',
      archived: false,
      priority: input.priority ?? 0,
      path,
      created_at: now,
      updated_at: now,
    }

    const body = input.body ?? defaultBody(input.type)
    const frontmatter = this.frontmatterOf(node, parent?.id)
    const emptyChildren = renderChildrenBlock([], child => this.childDirName(child))
    await this.files.writeNode(path, serializeNodeFile(frontmatter, replaceChildrenBlock(body, emptyChildren)))

    this.store.insertNode(node)
    if (parent !== undefined) {
      this.store.insertEdge(parent.id, id, this.store.childrenOf(parent.id).length)
    }
    this.store.indexNode(id, title, stripChildrenBlock(body))
    if (parent !== undefined) await this.refreshChildrenBlock(parent.id)
    return node
  }

  // ── update ───────────────────────────────────────────────────────────

  async update(input: UpdateInput): Promise<OmtNode> {
    const node = this.requireNode(input.id)
    // Archived nodes are sealed: the only accepted change is restoring.
    if (node.archived && input.archived !== false) {
      const touchesContent = input.title !== undefined || input.status !== undefined
        || input.priority !== undefined || input.body !== undefined || input.append !== undefined
      if (touchesContent || input.archived === true) {
        throw new OmtError('INVALID_INPUT', `${input.id} 已归档，请先恢复（archived: false）再做修改`)
      }
    }
    const now = new Date().toISOString()
    const file = await this.files.readNode(node.path)
    const parent = this.store.parentOf(node.id)

    const patch: { title?: string; status?: Status; archived?: boolean; priority?: number; updated_at?: string } = { updated_at: now }
    if (input.title !== undefined) {
      const title = input.title.trim()
      if (title === '') throw new OmtError('INVALID_INPUT', 'title must not be empty')
      patch.title = title
    }
    if (input.status !== undefined) {
      if (!isStatus(input.status)) throw new OmtError('INVALID_INPUT', `unknown status: ${String(input.status)}`)
      patch.status = input.status
    }
    if (input.priority !== undefined) patch.priority = input.priority
    if (input.archived !== undefined) patch.archived = input.archived

    let body = file.body
    if (input.body !== undefined) {
      body = input.body
    } else if (input.append !== undefined) {
      body = `${body.replace(/\s+$/, '')}\n\n${input.append}\n`
    }

    this.store.updateNode(node.id, patch)
    const updated: OmtNode = { ...node, ...patch } as OmtNode
    await this.files.writeNode(node.path, serializeNodeFile(this.frontmatterOf(updated, parent?.id), body))
    this.store.indexNode(node.id, updated.title, stripChildrenBlock(body))

    // A title change shows up in the parent's managed children list.
    if (patch.title !== undefined && parent !== undefined) await this.refreshChildrenBlock(parent.id)
    return updated
  }

  // ── move ─────────────────────────────────────────────────────────────

  async move(id: string, newParentId: string): Promise<OmtNode> {
    const node = this.requireNode(id)
    const newParent = this.requireNode(newParentId)
    if (id === newParentId) throw new OmtError('INVALID_HIERARCHY', 'a node cannot be its own parent')
    if (!HIERARCHY[newParent.type].includes(node.type)) {
      throw new OmtError('INVALID_HIERARCHY', `${newParent.type} cannot contain ${node.type}`)
    }
    for (const ancestorId of this.ancestorIds(newParentId)) {
      if (ancestorId === id) throw new OmtError('INVALID_HIERARCHY', 'cannot move a node under its own descendant')
    }

    const oldParent = this.store.parentOf(id)
    const oldPath = node.path
    const newPath = this.files.pathFor(node.type, node.id, node.title, newParent.path)
    if (oldPath === newPath) throw new OmtError('CONFLICT', 'node is already at the target location')

    const now = new Date().toISOString()
    await this.files.moveDir(oldPath, newPath)

    // The move relocates the whole subtree: rewrite the stored path prefix
    // for the node and every descendant.
    const oldDir = dirname(oldPath)
    const newDir = dirname(newPath)
    const subtree = [node, ...this.descendantsOf(id)]
    for (const member of subtree) {
      const relocated = member.path.replace(oldDir, newDir)
      this.store.updateNode(member.id, { path: relocated, ...(member.id === id ? { updated_at: now } : {}) })
    }

    if (oldParent !== undefined) this.store.deleteEdge(oldParent.id, id)
    this.store.insertEdge(newParentId, id, this.store.childrenOf(newParentId).length)

    const moved = this.requireNode(id)
    const file = await this.files.readNode(moved.path)
    await this.files.writeNode(moved.path, serializeNodeFile(this.frontmatterOf(moved, newParentId), file.body))

    if (oldParent !== undefined) await this.refreshChildrenBlock(oldParent.id)
    await this.refreshChildrenBlock(newParentId)
    return moved
  }

  /** Direct metadata probe (pool ownership checks). */
  getNode(id: string): OmtNode | undefined {
    return this.store.getNode(id)
  }

  /** Session recent-ticket list (meta-backed, most-recent-first). */
  getSessionRecent(sessionId: string): string[] | undefined {
    const raw = this.store.getMeta(`recent:${sessionId}`)
    if (raw === undefined) return undefined
    try {
      const parsed: unknown = JSON.parse(raw)
      return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === 'string') : undefined
    } catch {
      return undefined
    }
  }

  setSessionRecent(sessionId: string, ids: readonly string[]): void {
    this.store.setMeta(`recent:${sessionId}`, JSON.stringify(ids))
  }

  /** Counter passthroughs for pool-wide unique id allocation. */
  counterValue(type: NodeType): number {
    return this.store.counterValue(type)
  }

  setCounter(type: NodeType, value: number): void {
    this.store.setCounter(type, value)
  }

  // ── queries ──────────────────────────────────────────────────────────

  list(filter: { type?: NodeType; status?: Status; query?: string } = {}): OmtNode[] {
    if (filter.query !== undefined && filter.query.trim() !== '') {
      const ids = this.store.search(filter.query)
      return ids.map(id => this.store.getNode(id)).filter((node): node is OmtNode => node !== undefined)
    }
    return this.store.listNodes({ type: filter.type, status: filter.status })
  }

  async show(id: string): Promise<ShowResult> {
    const node = this.requireNode(id)
    const file = await this.files.readNode(node.path)
    return {
      node,
      body: file.body,
      parent: this.store.parentOf(id),
      children: this.store.childrenOf(id),
    }
  }

  /** Assemble the forest (epics as roots), children ordered by edge ord. */
  tree(rootId?: string): OmtTreeNode[] {
    const nodes = this.store.allNodes()
    const byId = new Map(nodes.map(node => [node.id, { ...node, children: [] as OmtTreeNode[] }]))
    const roots: OmtTreeNode[] = []
    for (const edge of this.store.allEdges()) {
      const parent = byId.get(edge.parent_id)
      const child = byId.get(edge.child_id)
      if (parent !== undefined && child !== undefined) parent.children.push(child)
    }
    for (const node of byId.values()) {
      if (this.store.parentOf(node.id) === undefined) roots.push(node)
    }
    if (rootId !== undefined) {
      const root = byId.get(rootId)
      if (root === undefined) throw new OmtError('NOT_FOUND', `unknown node: ${rootId}`)
      return [root]
    }
    return roots.sort((a, b) => a.id.localeCompare(b.id))
  }

  // ── reindex ──────────────────────────────────────────────────────────

  /**
   * Rebuild the SQLite index from the markdown files on disk. Frontmatter
   * `parent` fields define the edges; managed children blocks are
   * regenerated afterwards so both stores agree again.
   */
  async reindex(): Promise<ReindexResult> {
    const paths = await this.files.listNodeFiles()
    const nodes: OmtNode[] = []
    const edges: OmtEdge[] = []
    const bodies = new Map<string, { title: string; body: string }>()
    const seen = new Set<string>()
    const pendingParent: { childId: string; parentId: string }[] = []
    let skipped = 0

    for (const path of paths) {
      let file
      try {
        file = parseNodeFile(await readFile(this.files.abs(path), 'utf8'))
      } catch {
        skipped += 1
        continue
      }
      const attrs = file.attrs
      if (typeof attrs.id !== 'string' || ID_PATTERN.exec(attrs.id) === null || !isNodeType(attrs.type) || seen.has(attrs.id)) {
        skipped += 1
        continue
      }
      seen.add(attrs.id)
      const now = new Date().toISOString()
      const node: OmtNode = {
        id: attrs.id,
        type: attrs.type,
        title: typeof attrs.title === 'string' && attrs.title.trim() !== '' ? attrs.title : attrs.id,
        status: isStatus(attrs.status) ? attrs.status : 'open',
        archived: attrs.archived === true,
        priority: typeof attrs.priority === 'number' ? attrs.priority : 0,
        path,
        created_at: typeof attrs.created_at === 'string' ? attrs.created_at : now,
        updated_at: typeof attrs.updated_at === 'string' ? attrs.updated_at : now,
      }
      nodes.push(node)
      bodies.set(node.id, { title: node.title, body: stripChildrenBlock(file.body) })
      if (typeof attrs.parent === 'string' && attrs.parent !== '') {
        pendingParent.push({ childId: node.id, parentId: attrs.parent })
      }
    }

    // Edges second pass: only keep parent references that resolve, and only
    // when the link is hierarchy-legal (files may have been hand-edited).
    const byId = new Map(nodes.map(node => [node.id, node]))
    const ordCounter = new Map<string, number>()
    for (const { childId, parentId } of pendingParent) {
      const child = byId.get(childId)
      const parent = byId.get(parentId)
      if (child === undefined || parent === undefined || !HIERARCHY[parent.type].includes(child.type)) {
        skipped += 1
        continue
      }
      const ord = ordCounter.get(parentId) ?? 0
      ordCounter.set(parentId, ord + 1)
      edges.push({ parent_id: parentId, child_id: childId, ord })
    }

    this.store.rebuild(nodes, edges, bodies)

    // Normalize the managed children blocks on disk from the rebuilt edges.
    for (const node of nodes) {
      const children = this.store.childrenOf(node.id)
      const file = await this.files.readNode(node.path)
      const body = replaceChildrenBlock(file.body, renderChildrenBlock(children, child => this.childDirName(child)))
      const parentId = edges.find(edge => edge.child_id === node.id)?.parent_id
      await this.files.writeNode(node.path, serializeNodeFile(this.frontmatterOf(node, parentId), body))
    }

    return { nodes: nodes.length, edges: edges.length, skipped }
  }

  // ── runs (EPIC-0003) ─────────────────────────────────────────────────
  // Runs are DB-only: no markdown dual-write. Membership is validated
  // against this home's nodes, which also enforces the single-home rule
  // (nodes from another home do not resolve here).

  async createRun(input: CreateRunInput): Promise<OmtRun> {
    const seen = new Set<string>()
    for (const nodeId of input.nodeIds) {
      if (seen.has(nodeId)) throw new OmtError('INVALID_INPUT', `duplicate run member: ${nodeId}`)
      seen.add(nodeId)
      this.requireNode(nodeId)
    }
    const config: RunConfig = { ...DEFAULT_RUN_CONFIG, ...input.config }
    if (!Number.isInteger(config.concurrency) || config.concurrency < 1) {
      throw new OmtError('INVALID_INPUT', `concurrency must be a positive integer, got ${String(config.concurrency)}`)
    }
    const run: OmtRun = {
      id: this.store.nextRunId(),
      ...(input.title !== undefined && input.title.trim() !== '' ? { title: input.title.trim() } : {}),
      status: 'pending',
      config,
      created_at: new Date().toISOString(),
    }
    this.store.insertRun(run)
    input.nodeIds.forEach((nodeId, position) => {
      this.store.insertRunItem({ run_id: run.id, node_id: nodeId, position, state: 'pending', attempts: 0, nudge_count: 0 })
    })
    return run
  }

  getRun(id: string): OmtRun | undefined {
    return this.store.getRun(id)
  }

  listRuns(filter: { status?: RunStatus } = {}): OmtRun[] {
    return this.store.listRuns(filter)
  }

  getRunItem(runId: string, nodeId: string): OmtRunItem | undefined {
    return this.store.getRunItem(runId, nodeId)
  }

  runItems(runId: string): OmtRunItem[] {
    this.requireRun(runId)
    return this.store.listRunItems(runId)
  }

  /** start: pending → running; an empty run derives completed immediately. */
  async startRun(id: string): Promise<OmtRun> {
    const run = this.requireRun(id)
    if (run.status !== 'pending') throw new OmtError('CONFLICT', `only a pending run can start (${id} is ${run.status})`)
    this.setRunStatus(id, 'running')
    this.deriveRunTerminal(id)
    return this.requireRun(id)
  }

  /** pause: running → paused; only dispatch stops, running items keep being observed. */
  async pauseRun(id: string): Promise<OmtRun> {
    const run = this.requireRun(id)
    if (run.status !== 'running') throw new OmtError('CONFLICT', `only a running run can be paused (${id} is ${run.status})`)
    this.setRunStatus(id, 'paused')
    return this.requireRun(id)
  }

  /** resume: paused|interrupted → running; interrupted items are NOT auto-reset (row-level retry only). */
  async resumeRun(id: string): Promise<OmtRun> {
    const run = this.requireRun(id)
    if (run.status !== 'paused' && run.status !== 'interrupted') {
      throw new OmtError('CONFLICT', `only a paused or interrupted run can resume (${id} is ${run.status})`)
    }
    this.setRunStatus(id, 'running')
    return this.requireRun(id)
  }

  /** cancel: pending/running/paused/interrupted → canceled; items freeze in place, tickets untouched. */
  async cancelRun(id: string): Promise<OmtRun> {
    this.setRunStatus(id, 'canceled')
    return this.requireRun(id)
  }

  /**
   * Direct item transition with run-level gating: a running run allows all
   * legal item moves; a paused run only lets in-flight (running /
   * awaiting_confirmation) items advance — no new dispatch. stop-on-failure
   * and terminal derivation run after final-state transitions.
   */
  async transitionItem(runId: string, nodeId: string, to: RunItemState, options: TransitionItemOptions = {}): Promise<OmtRunItem> {
    if (!isRunItemState(to)) throw new OmtError('INVALID_INPUT', `unknown run item state: ${String(to)}`)
    const run = this.requireRun(runId)
    const item = this.store.getRunItem(runId, nodeId)
    if (item === undefined) throw new OmtError('NOT_FOUND', `run ${runId} has no item for node: ${nodeId}`)

    if (run.status === 'paused') {
      if (item.state !== 'running' && item.state !== 'awaiting_confirmation') {
        throw new OmtError('CONFLICT', `run ${runId} is paused; dispatch is stopped (item ${nodeId} is ${item.state})`)
      }
    } else if (run.status !== 'running') {
      throw new OmtError('CONFLICT', `run ${runId} is ${run.status}; items are frozen`)
    }
    if (!ITEM_TRANSITIONS[item.state].includes(to)) {
      throw new OmtError('CONFLICT', `illegal item transition for ${nodeId}: ${item.state} → ${to}`)
    }

    const now = new Date().toISOString()
    this.store.updateRunItem(runId, nodeId, {
      state: to,
      ...(to === 'running' ? { started_at: item.started_at ?? now, ...(options.executorSessionId !== undefined ? { executor_session_id: options.executorSessionId } : {}) } : {}),
      ...(RUN_ITEM_FINAL_STATES.includes(to) ? { finished_at: now } : {}),
      ...(to === 'failed' && options.error !== undefined ? { last_error: options.error } : {}),
    })

    // stop-on-failure: only `failed` triggers (blocked/skipped never do).
    if (to === 'failed' && run.config.stopOnFailure && run.status === 'running') {
      this.setRunStatus(runId, 'paused')
    }
    if (RUN_ITEM_FINAL_STATES.includes(to)) this.deriveRunTerminal(runId)
    return this.store.getRunItem(runId, nodeId) as OmtRunItem
  }

  /**
   * Retry (decision 10): reset a failed / interrupted / stalled-pending item
   * back to pending in place — attempts+1, last_error kept, nudge budget
   * cleared (the new attempt gets a fresh one), executor/timestamps cleared.
   * Retrying inside a completed_with_failures run reopens it to running;
   * canceled (and fully completed) runs accept no retry (TICKET-0055 #6).
   */
  async retryItem(runId: string, nodeId: string): Promise<OmtRunItem> {
    const run = this.requireRun(runId)
    if (run.status === 'canceled' || run.status === 'completed') {
      throw new OmtError('CONFLICT', `run ${runId} is ${run.status}; retry is unavailable`)
    }
    const item = this.store.getRunItem(runId, nodeId)
    if (item === undefined) throw new OmtError('NOT_FOUND', `run ${runId} has no item for node: ${nodeId}`)
    if (item.state !== 'failed' && item.state !== 'interrupted' && item.state !== 'pending') {
      throw new OmtError('CONFLICT', `only failed/interrupted/stalled-pending items can retry (${nodeId} is ${item.state})`)
    }
    this.store.updateRunItem(runId, nodeId, {
      state: 'pending',
      attempts: item.attempts + 1,
      executor_session_id: null,
      nudged_at: null,
      nudge_count: 0,
      started_at: null,
      finished_at: null,
    })
    // The run has dispatchable work again: leave the terminal state.
    if (run.status === 'completed_with_failures') this.setRunStatus(runId, 'running')
    return this.store.getRunItem(runId, nodeId) as OmtRunItem
  }

  /**
   * Replay (decision 11): a member ticket went done/blocked/skipped → open
   * while the run was in progress — its item falls back to pending, keeping
   * position and attempt history. Only on in-progress runs.
   */
  async replayItem(runId: string, nodeId: string): Promise<OmtRunItem> {
    const run = this.requireRun(runId)
    if (run.status !== 'pending' && run.status !== 'running' && run.status !== 'paused' && run.status !== 'interrupted') {
      throw new OmtError('CONFLICT', `run ${runId} is ${run.status}; replay requires an in-progress run`)
    }
    const item = this.store.getRunItem(runId, nodeId)
    if (item === undefined) throw new OmtError('NOT_FOUND', `run ${runId} has no item for node: ${nodeId}`)
    if (item.state !== 'done' && item.state !== 'blocked' && item.state !== 'skipped') {
      throw new OmtError('CONFLICT', `only done/blocked/skipped items can replay (${nodeId} is ${item.state})`)
    }
    this.store.updateRunItem(runId, nodeId, {
      state: 'pending',
      executor_session_id: null,
      started_at: null,
      finished_at: null,
    })
    return this.store.getRunItem(runId, nodeId) as OmtRunItem
  }

  /**
   * Claim (TICKET-0058): atomically take the next pending item of a running
   * run — one immediate transaction flips it to running and binds the
   * executor session, so concurrent claimers never share an item. Returns
   * undefined when the queue is empty (an explicit signal, not an error).
   * Claimed ownership wins over passive observation (decision 14).
   */
  async claimRunItem(runId: string, executorSessionId: string): Promise<OmtRunItem | undefined> {
    const run = this.requireRun(runId)
    if (executorSessionId.trim() === '') {
      throw new OmtError('INVALID_INPUT', 'claim requires an executor session id')
    }
    if (run.status === 'pending') {
      throw new OmtError('CONFLICT', `run ${runId} has not started; start it before claiming`)
    }
    if (run.status !== 'running') {
      throw new OmtError('CONFLICT', `run ${runId} is ${run.status}; only a running run dispatches claims`)
    }
    return this.store.claimNextRunItem(runId, executorSessionId, new Date().toISOString())
  }

  /**
   * Continuation candidates (TICKET-0062): for every RUNNING run with
   * autoContinue on where `sessionId` is the executor — it owns at least one
   * item (any state; ownership survives completion) — the next pending item
   * in position order. Paused runs stop dispatch AND nudges (decision 9);
   * stalled pending items (nudge budget exhausted) stay in the result so the
   * hook can recognize them instead of silently skipping ahead.
   */
  continuationCandidates(sessionId: string): { run: OmtRun; item: OmtRunItem }[] {
    const candidates: { run: OmtRun; item: OmtRunItem }[] = []
    for (const run of this.store.listRunsByStatus(['running'])) {
      if (!run.config.autoContinue) continue
      const items = this.store.listRunItems(run.id)
      if (!items.some(item => item.executor_session_id === sessionId)) continue
      const next = items.find(item => item.state === 'pending')
      if (next !== undefined) candidates.push({ run, item: next })
    }
    return candidates
  }

  /**
   * Record one continuation nudge on an item (TICKET-0062): nudge_count+1
   * with nudged_at stamped. Pure bookkeeping — budget/backoff policy lives
   * in the idle hook; retryItem clears both fields for a fresh budget.
   */
  recordItemNudge(runId: string, nodeId: string, at: string = new Date().toISOString()): OmtRunItem {
    this.requireRun(runId)
    const item = this.store.getRunItem(runId, nodeId)
    if (item === undefined) throw new OmtError('NOT_FOUND', `run ${runId} has no item for node: ${nodeId}`)
    this.store.updateRunItem(runId, nodeId, { nudged_at: at, nudge_count: item.nudge_count + 1 })
    return this.store.getRunItem(runId, nodeId) as OmtRunItem
  }

  /**
   * Item-level removal (omt_run_control remove): drops the membership row
   * only — the ticket node is never touched. In-flight items (running /
   * awaiting_confirmation) cannot be removed; let them settle or cancel the
   * run. Afterwards the run may derive its terminal state (last pending
   * item removed with the rest final).
   */
  async removeRunItem(runId: string, nodeId: string): Promise<void> {
    this.requireRun(runId)
    const item = this.store.getRunItem(runId, nodeId)
    if (item === undefined) throw new OmtError('NOT_FOUND', `run ${runId} has no item for node: ${nodeId}`)
    if (item.state === 'running' || item.state === 'awaiting_confirmation') {
      throw new OmtError('CONFLICT', `item ${nodeId} is ${item.state} (in-flight); it cannot be removed`)
    }
    this.store.deleteRunItem(runId, nodeId)
    this.deriveRunTerminal(runId)
  }

  /**
   * Explicit report (TICKET-0059): the model's legal vocabulary for how an
   * execution ended. done/blocked/skipped double-write the ticket status and
   * the item; failed touches only the item (the node enum has no failed —
   * the ticket stays in_progress) and keeps the note in last_error. Any
   * note is appended to the ticket's progress record. stop-on-failure and
   * terminal derivation ride on transitionItem.
   */
  async reportRunItem(runId: string, nodeId: string, outcome: RunReportOutcome, note?: string): Promise<ReportResult> {
    if (!RUN_REPORT_OUTCOMES.includes(outcome)) {
      throw new OmtError('INVALID_INPUT', `unknown report outcome: ${String(outcome)} (done/failed/blocked/skipped)`)
    }
    this.requireRun(runId)
    const node = this.requireNode(nodeId)
    if (node.archived) {
      throw new OmtError('CONFLICT', `${nodeId} 已归档，无法接受 report`)
    }
    const item = this.store.getRunItem(runId, nodeId)
    if (item === undefined) throw new OmtError('NOT_FOUND', `run ${runId} has no item for node: ${nodeId}`)
    if (item.state !== 'running' && item.state !== 'awaiting_confirmation') {
      throw new OmtError('CONFLICT', `only in-flight items can report (${nodeId} is ${item.state})`)
    }

    if (note !== undefined && note.trim() !== '') {
      await this.update({ id: nodeId, append: note })
    }
    const transitioned = await this.transitionItem(runId, nodeId, outcome, {
      ...(outcome === 'failed' && note !== undefined && note.trim() !== '' ? { error: note } : {}),
    })
    if (outcome !== 'failed') {
      await this.update({ id: nodeId, status: outcome })
    }
    return { item: transitioned, node: this.requireNode(nodeId) }
  }

  /**
   * Passive observation (TICKET-0061): a node status/archive change seen on
   * the ordinary update paths advances the matching items of every ACTIVE
   * run (running | paused) in this home — the cross-run broadcast of
   * decision 1. Mapping:
   *   in_progress → pending item dispatches to running (running runs only —
   *     pause stops dispatch, decision 9), recording the observing session
   *   done/blocked/skipped → item to the same state (pending items included:
   *     direct sets must not wedge a run)
   *   archived → item skipped
   *   open over a done/blocked/skipped item → replay back to pending
   *     (decision 11)
   * Claim priority (decision 14): an already-claimed item keeps its
   * executor_session_id — observation advances state but never rebinds
   * ownership. Returns the items actually advanced (hook reuse).
   */
  async observeNodeStatus(nodeId: string, change: ObservedNodeChange, executorSessionId?: string): Promise<OmtRunItem[]> {
    const advanced: OmtRunItem[] = []
    for (const item of this.store.runItemsForNode(nodeId, ['running', 'paused'])) {
      const run = this.requireRun(item.run_id)
      const inFlight = item.state === 'running' || item.state === 'awaiting_confirmation'

      if (change.archived === true) {
        if (inFlight || (item.state === 'pending' && run.status === 'running')) {
          advanced.push(await this.transitionItem(item.run_id, nodeId, 'skipped'))
        }
        continue
      }
      switch (change.status) {
        case 'in_progress':
          // Dispatch only — a claimed (already running) item is left alone,
          // so its executor attribution is never overwritten.
          if (item.state === 'pending' && run.status === 'running') {
            advanced.push(await this.transitionItem(item.run_id, nodeId, 'running', { executorSessionId }))
          }
          break
        case 'done':
        case 'blocked':
        case 'skipped':
          if (inFlight || (item.state === 'pending' && run.status === 'running')) {
            advanced.push(await this.transitionItem(item.run_id, nodeId, change.status))
          }
          break
        case 'open':
          if (item.state === 'done' || item.state === 'blocked' || item.state === 'skipped') {
            advanced.push(await this.replayItem(item.run_id, nodeId))
          }
          break
        default:
          break
      }
    }
    return advanced
  }


  /**
   * Startup janitor (decision 12): demote orphaned in-flight work. Item
   * level — every running item (in running or paused runs) whose executor
   * session is not active → interrupted. Run level — a running run with no
   * actively-executed item left either derives its terminal state (when the
   * demotion finished the last item) or falls to interrupted, which stays
   * resumable. Paused runs keep their status (human-controlled already).
   */
  janitorSweep(isSessionActive: (sessionId: string) => boolean = () => false): JanitorResult {
    const interruptedItems: OmtRunItem[] = []
    const interruptedRuns: string[] = []
    const now = new Date().toISOString()
    const candidates = this.store.listRunsByStatus(['running', 'paused'])

    for (const run of candidates) {
      for (const item of this.store.listRunItems(run.id)) {
        if (item.state !== 'running') continue
        if (item.executor_session_id !== undefined && isSessionActive(item.executor_session_id)) continue
        this.store.updateRunItem(run.id, item.node_id, { state: 'interrupted', finished_at: now })
        interruptedItems.push(this.store.getRunItem(run.id, item.node_id) as OmtRunItem)
      }
    }

    for (const run of candidates) {
      if (run.status !== 'running') continue
      const items = this.store.listRunItems(run.id)
      const hasLiveExecution = items.some(
        item => item.state === 'running' && item.executor_session_id !== undefined && isSessionActive(item.executor_session_id),
      )
      if (hasLiveExecution) continue
      if (items.length > 0 && items.every(item => RUN_ITEM_FINAL_STATES.includes(item.state))) {
        // The demotion finished the last in-flight item: derive instead.
        this.deriveRunTerminal(run.id)
      } else {
        this.setRunStatus(run.id, 'interrupted')
        interruptedRuns.push(run.id)
      }
    }
    return { interruptedRuns, interruptedItems }
  }

  // ── internals ────────────────────────────────────────────────────────

  private requireRun(id: string): OmtRun {
    const run = this.store.getRun(id)
    if (run === undefined) throw new OmtError('NOT_FOUND', `unknown run: ${id}`)
    return run
  }

  /** Validated run status change; finished_at tracks absolute terminal states. */
  private setRunStatus(id: string, to: RunStatus): void {
    const run = this.requireRun(id)
    if (!RUN_TRANSITIONS[run.status].includes(to)) {
      throw new OmtError('CONFLICT', `illegal run transition for ${id}: ${run.status} → ${to}`)
    }
    const now = new Date().toISOString()
    this.store.updateRun(id, {
      status: to,
      ...(to === 'running' ? { finished_at: null } : {}),
      ...(to === 'completed' || to === 'completed_with_failures' || to === 'canceled' || to === 'interrupted' ? { finished_at: now } : {}),
    })
  }

  /**
   * Terminal derivation (decision 7): once every item is final — all
   * done/skipped → completed; any failed/interrupted (or blocked, which is
   * likewise not a success) → completed_with_failures. Runs have no failed.
   */
  private deriveRunTerminal(id: string): void {
    const run = this.requireRun(id)
    if (run.status !== 'running' && run.status !== 'paused') return
    const items = this.store.listRunItems(id)
    if (items.some(item => !RUN_ITEM_FINAL_STATES.includes(item.state))) return
    const failed = items.some(item => RUN_ITEM_FAILURE_STATES.includes(item.state))
    this.setRunStatus(id, failed ? 'completed_with_failures' : 'completed')
  }

  private requireNode(id: string): OmtNode {
    const node = this.store.getNode(id)
    if (node === undefined) throw new OmtError('NOT_FOUND', `unknown node: ${id}`)
    return node
  }

  private childDirName(child: OmtNode): string {
    return this.files.nodeDirName(child.id, child.title)
  }

  private frontmatterOf(node: OmtNode, parentId?: string): NodeFrontmatter {
    return {
      id: node.id,
      type: node.type,
      title: node.title,
      status: node.status,
      ...(node.archived ? { archived: true } : {}),
      priority: node.priority,
      ...(parentId !== undefined ? { parent: parentId } : {}),
      created_at: node.created_at,
      updated_at: node.updated_at,
    }
  }

  /** Re-render one node's managed children block from current edges. */
  private async refreshChildrenBlock(id: string): Promise<void> {
    const node = this.requireNode(id)
    const children = this.store.childrenOf(id)
    const file = await this.files.readNode(node.path)
    const body = replaceChildrenBlock(file.body, renderChildrenBlock(children, child => this.childDirName(child)))
    await this.files.writeNode(node.path, serializeNodeFile(this.frontmatterOf(node, this.store.parentOf(id)?.id), body))
  }

  private ancestorIds(id: string): string[] {
    const chain: string[] = []
    let current = this.store.parentOf(id)
    while (current !== undefined) {
      chain.push(current.id)
      current = this.store.parentOf(current.id)
    }
    return chain
  }

  private descendantsOf(id: string): OmtNode[] {
    const result: OmtNode[] = []
    const queue = [id]
    while (queue.length > 0) {
      const current = queue.shift() as string
      for (const child of this.store.childrenOf(current)) {
        result.push(child)
        queue.push(child.id)
      }
    }
    return result
  }
}
