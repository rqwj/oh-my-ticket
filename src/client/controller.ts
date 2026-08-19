/**
 * OMT client controller: owns the snapshot stores and every async flow
 * (tree fetch, doc selection, mutations). Registered components subscribe
 * through the inject hooks compartment; the details-panel shadow is attached
 * by index.ts through attachDetailsShadow (dynamic register/dispose).
 */
import { createSnapshotStore, type SnapshotStore } from '@deepseek-ai/dsh-client-runtime/client'
import type { RpcCaller } from './trigger/source.ts'
import type { FloatPos, FloatSize } from './float-geometry.ts'
import type { ActiveInfo, DocData, DocState, NodeSummary, OmtTreeNode, TreeState } from './store.ts'

/** ctx.layout face (ui-layout service). */
export interface LayoutLike {
  openDetails(): void
  closeDetails(): void
}

/**
 * Overlay panel presentation (STORY-0006): the same TicketPanel renders as
 * the left drawer or as the floating window. The third presentation — the
 * conversation.view OMT tab — is always available and needs no mode state.
 */
export type PanelMode = 'drawer' | 'float'

type RpcValue = { ok: true; value: unknown } | { ok: false; error: { message: string } }

function errorMessage(result: RpcValue): string {
  return result.ok ? '' : result.error.message
}

export class OmtController {
  readonly drawerOpen: SnapshotStore<boolean> = createSnapshotStore(false)
  /** Drawer width in px (persisted; drag-clamped 240–480). */
  readonly drawerWidth: SnapshotStore<number> = createSnapshotStore(320, { persist: { name: 'omt-drawer-width' } })
  /** Overlay presentation while drawerOpen (persisted; STORY-0006). */
  readonly panelMode: SnapshotStore<PanelMode> = createSnapshotStore('drawer', { persist: { name: 'omt-panel-mode' } })
  /** Floating-window top-left in px (persisted; re-clamped per viewport at render). */
  readonly floatPos: SnapshotStore<FloatPos> = createSnapshotStore({ x: 96, y: 96 }, { persist: { name: 'omt-float-pos' } })
  /** Floating-window size in px (persisted; re-clamped per viewport at render). */
  readonly floatSize: SnapshotStore<FloatSize> = createSnapshotStore({ w: 380, h: 520 }, { persist: { name: 'omt-float-size' } })
  /** Collapsed node ids (persisted; absent = expanded). */
  readonly collapsed: SnapshotStore<Record<string, boolean>> = createSnapshotStore({}, { persist: { name: 'omt-collapsed' } })
  readonly tree: SnapshotStore<TreeState> = createSnapshotStore({ status: 'idle' })
  readonly doc: SnapshotStore<DocState> = createSnapshotStore({ status: 'idle' })
  readonly active: SnapshotStore<ActiveInfo | undefined> = createSnapshotStore(undefined)
  /** Per-session related tickets backing the turn-tail list. */
  readonly related: SnapshotStore<Record<string, readonly NodeSummary[]>> = createSnapshotStore({})
  /** id → summary cache (reference bar needs live title/status by bare ref). */
  readonly summaries: SnapshotStore<Record<string, NodeSummary>> = createSnapshotStore({})
  /** Session currently on stage (reported by session-scope components). */
  currentSessionId: string | undefined

  private detailsOff: (() => void) | undefined
  private shadowFactory: (() => () => void) | undefined
  private yieldListener: (() => void) | undefined
  private events: EventSource | undefined
  private refreshTimer: ReturnType<typeof setTimeout> | undefined

  constructor(
    private readonly rpc: RpcCaller,
    private readonly layout: LayoutLike,
  ) {}

  /** index.ts wires the dynamic details-panel shadow registration. */
  attachDetailsShadow(factory: () => () => void): void {
    this.shadowFactory = factory
  }

  /**
   * index.ts wires the dynamic conversation.view tab registration
   * (TICKET-0040). The OMT tab hides while the floating window owns the
   * ticket list: disposing the entry drops it from the view ring, and the
   * shell's resolveActiveView falls back to Chat for any session staged on
   * it — no shell change, no explicit setView.
   */
  attachViewTab(factory: () => () => void): void {
    this.viewTabFactory = factory
    const sync = (): void => this.syncViewTab()
    this.drawerOpen.subscribe(sync)
    this.panelMode.subscribe(sync)
    this.syncViewTab()
  }

  private viewTabOff: (() => void) | undefined
  private viewTabFactory: (() => () => void) | undefined

  private syncViewTab(): void {
    const floatActive = this.drawerOpen.getSnapshot() && this.panelMode.getSnapshot() === 'float'
    if (floatActive && this.viewTabOff !== undefined) {
      this.viewTabOff()
      this.viewTabOff = undefined
    } else if (!floatActive && this.viewTabOff === undefined && this.viewTabFactory !== undefined) {
      this.viewTabOff = this.viewTabFactory()
    }
  }

  /**
   * Subscribe to host change pushes (SSE /omt/events). Any mutation —
   * model tool call or UI action — refreshes the tree, the related list,
   * and the open doc. Debounced: a burst of creates lands one refresh.
   */
  connectEvents = (): void => {
    if (this.events !== undefined || typeof EventSource === 'undefined') return
    this.events = new EventSource('/omt/events')
    this.events.onmessage = () => this.scheduleRefresh()
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer !== undefined) clearTimeout(this.refreshTimer)
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = undefined
      const sessionId = this.currentSessionId
      void this.refreshTree(sessionId).catch(() => {})
      if (sessionId !== undefined) void this.refreshRelated(sessionId).catch(() => {})
      const active = this.active.getSnapshot()
      if (active !== undefined && !this.bodyEditing) void this.select(active.id, sessionId).catch(() => {})
    }, 300)
  }

  /** Merge summaries into the id cache. */
  noteSummaries = (summaries: readonly NodeSummary[]): void => {
    if (summaries.length === 0) return
    this.summaries.update(draft => {
      for (const summary of summaries) draft[summary.id] = summary
    })
  }

  /** Fetch any uncached summaries (reference bar display data). */
  ensureSummaries = async (sessionId: string, ids: readonly string[]): Promise<void> => {
    const missing = ids.filter(id => this.summaries.getSnapshot()[id] === undefined)
    for (const id of missing) {
      const result = await this.rpc.call('/omt', 'get', { sessionId, id })
      if (result.ok) {
        const { node } = result.value as { node: NodeSummary }
        this.noteSummaries([{ id: node.id, type: node.type, title: node.title, status: node.status, archived: node.archived, priority: node.priority }])
      }
    }
  }

  /** Session-scope components report the staged session through here. */
  noteSession = (sessionId: string | undefined): void => {
    this.currentSessionId = sessionId
  }

  /** Merge summaries into a session's related list (dedup, front = recent). */
  noteRelated = (sessionId: string, summaries: readonly NodeSummary[]): void => {
    if (summaries.length === 0) return
    this.related.update(draft => {
      const existing = draft[sessionId] ?? []
      const incomingIds = new Set(summaries.map(item => item.id))
      // Incoming order wins (re-referenced ids move to the front), then the
      // remaining previous entries keep their relative order.
      const rest = existing.filter(item => !incomingIds.has(item.id))
      draft[sessionId] = [...summaries, ...rest].slice(0, 10)
    })
  }

  relatedOf = (sessionId: string | undefined): readonly NodeSummary[] => {
    if (sessionId === undefined) return []
    return this.related.getSnapshot()[sessionId] ?? []
  }

  private readonly relatedFetchedAt = new Map<string, number>()

  /** Pull the host-side recent list (tool calls + codec reads) into the store. */
  refreshRelated = async (sessionId: string): Promise<void> => {
    // Every turn tail mounts a fetcher; collapse bursts per session.
    const last = this.relatedFetchedAt.get(sessionId) ?? 0
    if (Date.now() - last < 2000) return
    this.relatedFetchedAt.set(sessionId, Date.now())
    const result = await this.rpc.call('/omt', 'recent', { sessionId })
    if (result.ok) this.noteRelated(sessionId, result.value as readonly NodeSummary[])
  }

  setDrawerWidth = (px: number): void => {
    this.drawerWidth.set(Math.min(480, Math.max(240, Math.round(px))))
  }

  /**
   * Switch the overlay presentation. The open fact (drawerOpen) is
   * untouched: switching modes mid-session re-presents the SAME panel —
   * tree, filters, and scroll — in the other shell.
   */
  setPanelMode = (mode: PanelMode): void => {
    this.panelMode.set(mode)
  }

  /** Store a float drag result (already viewport-clamped by the shell). */
  setFloatPos = (pos: FloatPos): void => {
    this.floatPos.set({ x: Math.round(pos.x), y: Math.round(pos.y) })
  }

  /** Store a float resize result (already viewport-clamped by the shell). */
  setFloatSize = (size: FloatSize): void => {
    this.floatSize.set({ w: Math.round(size.w), h: Math.round(size.h) })
  }

  /** Open the overlay panel in the current mode (tab "pop out" seat). */
  openPanel = (sessionId?: string): void => {
    if (this.drawerOpen.getSnapshot()) return
    this.drawerOpen.set(true)
    void this.refreshTree(sessionId)
  }

  expandIds = (ids: readonly string[]): void => {
    this.collapsed.update(draft => {
      for (const id of ids) delete draft[id]
    })
  }

  toggleCollapsed = (id: string): void => {
    this.collapsed.update(draft => {
      draft[id] = draft[id] !== true
    })
  }

  toggleDrawer = (sessionId?: string): void => {
    const next = !this.drawerOpen.getSnapshot()
    this.drawerOpen.set(next)
    if (next) void this.refreshTree(sessionId)
  }

  refreshTree = async (sessionId?: string): Promise<void> => {
    this.tree.set({ status: 'loading' })
    const result = await this.rpc.call('/omt', 'tree', sessionId === undefined ? {} : { sessionId })
    if (result.ok) {
      this.tree.set({ status: 'ready', forest: result.value as readonly OmtTreeNode[] })
    } else {
      this.tree.set({ status: 'error', message: errorMessage(result) })
    }
    if (sessionId !== undefined) void this.refreshRelated(sessionId).catch(() => {})
  }

  reindex = async (sessionId?: string): Promise<void> => {
    await this.rpc.call('/omt', 'reindex', sessionId === undefined ? {} : { sessionId })
    await this.refreshTree(sessionId)
    const active = this.active.getSnapshot()
    if (active !== undefined) await this.select(active.id, sessionId)
  }

  /** Select a node: load its doc, pin it active, shadow the details panel. */
  select = async (id: string, sessionId?: string): Promise<void> => {
    this.doc.set({ status: 'loading', id })
    const result = await this.rpc.call('/omt', 'get', { sessionId, id })
    if (!result.ok) {
      this.doc.set({ status: 'error', id, message: errorMessage(result) })
      return
    }
    const data = result.value as DocData
    this.doc.set({ status: 'ready', data })
    this.active.set({ id: data.node.id, title: data.node.title, status: data.node.status, priority: data.node.priority })
    if (sessionId !== undefined) {
      this.noteRelated(sessionId, [{ id: data.node.id, type: data.node.type, title: data.node.title, status: data.node.status, archived: data.node.archived, priority: data.node.priority }])
    }
    this.ensureDetailsShadow()
    this.layout.openDetails()
  }

  /** Forget a node everywhere (NOT_FOUND cleanup): active pin, related list, open doc. */
  forget = (id: string, sessionId?: string): void => {
    if (this.active.getSnapshot()?.id === id) this.active.set(undefined)
    if (sessionId !== undefined) {
      this.related.update(draft => {
        const list = draft[sessionId]
        if (list !== undefined) draft[sessionId] = list.filter(item => item.id !== id)
      })
    }
    if (this.doc.getSnapshot().status !== 'idle') this.closeDoc()
  }

  clearActive = (): void => {
    this.active.set(undefined)
    this.closeDoc()
  }

  closeDoc = (): void => {
    this.releaseShadow()
    this.doc.set({ status: 'idle' })
    // Closing the doc closes the details column too — the stock tool-details
    // panel must not linger in a column the user thinks they just closed.
    this.layout.closeDetails()
  }

  /**
   * One-way yield (TICKET-0017, option 2): while our doc shadows the details
   * slot, a click on any tool-call row (data-chat-call-id) disposes the
   * shadow so the stock panel shows the selected call — the column STAYS
   * open. The active strip / tree re-open the doc in one click.
   */
  private yieldDoc(): void {
    this.releaseShadow()
    this.doc.set({ status: 'idle' })
  }

  private releaseShadow(): void {
    if (this.detailsOff !== undefined) {
      this.detailsOff()
      this.detailsOff = undefined
    }
    if (this.yieldListener !== undefined) {
      this.yieldListener()
      this.yieldListener = undefined
    }
  }

  /** Execute button: status + running mark server-side, then SSE refreshes us. */
  executeTicket = async (id: string, sessionId?: string): Promise<void> => {
    if (sessionId === undefined) return
    await this.rpc.call('/omt', 'execute', { sessionId, id })
    await this.afterMutation(id, sessionId)
  }

  rename = async (id: string, title: string, sessionId?: string): Promise<void> => {
    const trimmed = title.trim()
    if (trimmed === '') return
    await this.rpc.call('/omt', 'update', { sessionId, id, title: trimmed })
    await this.afterMutation(id, sessionId)
  }

  setPriority = async (id: string, priority: number, sessionId?: string): Promise<void> => {
    await this.rpc.call('/omt', 'update', { sessionId, id, priority })
    await this.afterMutation(id, sessionId)
  }

  setArchived = async (id: string, archived: boolean, sessionId?: string): Promise<void> => {
    await this.rpc.call('/omt', 'update', { sessionId, id, archived })
    await this.afterMutation(id, sessionId)
  }

  setStatus = async (id: string, status: ActiveInfo['status'], sessionId?: string): Promise<void> => {
    await this.rpc.call('/omt', 'update', { sessionId, id, status })
    await this.afterMutation(id, sessionId)
  }

  private bodyEditing = false

  setBodyEditing = (editing: boolean): void => {
    this.bodyEditing = editing
  }

  saveBody = async (id: string, body: string, sessionId?: string): Promise<void> => {
    await this.rpc.call('/omt', 'update', { sessionId, id, body })
    this.bodyEditing = false
    await this.afterMutation(id, sessionId)
  }

  createNode = async (
    input: { type: OmtTreeNode['type']; title: string; parentId?: string; body?: string },
    sessionId?: string,
  ): Promise<string | undefined> => {
    const result = await this.rpc.call('/omt', 'create', { sessionId, ...input })
    if (!result.ok) return undefined
    const created = result.value as { id: string }
    await this.refreshTree(sessionId)
    await this.select(created.id, sessionId)
    return created.id
  }

  appendNote = async (id: string, text: string, sessionId?: string): Promise<void> => {
    const trimmed = text.trim()
    if (trimmed === '') return
    await this.rpc.call('/omt', 'update', { sessionId, id, append: trimmed })
    await this.afterMutation(id, sessionId)
  }

  private ensureDetailsShadow(): void {
    if (this.detailsOff === undefined && this.shadowFactory !== undefined) {
      this.detailsOff = this.shadowFactory()
      if (typeof document === 'undefined') return
      // Capture-phase: yield before the tool row's own click handler selects
      // the call, so the unshadowed stock panel renders it immediately.
      const onClick = (event: MouseEvent): void => {
        if ((event.target as HTMLElement | null)?.closest?.('[data-chat-call-id]') !== null
          && (event.target as HTMLElement | null)?.closest?.('[data-chat-call-id]') !== undefined) {
          this.yieldDoc()
        }
      }
      document.addEventListener('click', onClick, true)
      this.yieldListener = () => document.removeEventListener('click', onClick, true)
    }
  }

  private async afterMutation(id: string, sessionId?: string): Promise<void> {
    await this.refreshTree(sessionId)
    if (this.active.getSnapshot()?.id === id) await this.select(id, sessionId)
  }
}
