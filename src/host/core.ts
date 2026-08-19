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
  HIERARCHY,
  ID_PATTERN,
  isNodeType,
  isStatus,
  OmtError,
  type NodeFrontmatter,
  type NodeType,
  type OmtEdge,
  type OmtNode,
  type OmtTreeNode,
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

const DB_FILE = 'omt.db'

export class OmtCore {
  private constructor(
    readonly home: string,
    private readonly store: OmtStore,
    private readonly files: OmtFiles,
  ) {}

  /**
   * Open the OMT home: create directories, open the database, and reindex
   * when the database is fresh but markdown files already exist on disk.
   */
  static async open(home: string): Promise<OmtCore> {
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

  /** Ancestors from root to the direct parent (empty for a root). */
  ancestors(id: string): OmtNode[] {
    this.requireNode(id)
    const chain: OmtNode[] = []
    let current = this.store.parentOf(id)
    while (current !== undefined) {
      chain.unshift(current)
      current = this.store.parentOf(current.id)
    }
    return chain
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

  // ── internals ────────────────────────────────────────────────────────

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
