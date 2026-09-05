/**
 * OMT client controller: owns the snapshot stores and every async flow
 * (tree fetch, doc selection, mutations). Registered components subscribe
 * through the inject hooks compartment; the details-panel shadow is attached
 * by index.ts through attachDetailsShadow (dynamic register/dispose).
 */
import { createSnapshotStore, type SnapshotStore } from '@deepseek-ai/dsh-client-store'
import type { RpcCaller } from './trigger/source.ts'
import type { FloatPos, FloatSize } from './float-geometry.ts'
import type { RunControlCommand } from './run-view.ts'
import { DEFAULT_SAVED_FILTERS, type SavedFilters } from './saved-filters.ts'
import type {
  ActiveInfo,
  DocData,
  DocState,
  NodeSummary,
  Notice,
  OmtRunChangeHint,
  OmtTreeNode,
  PanelSection,
  RunDetailData,
  RunDetailState,
  RunItemView,
  RunListState,
  RunPickerState,
  RunSummary,
  TreeState,
} from './store.ts'

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
  /** Run list snapshot backing the Runs 区块 (TICKET-0068). */
  readonly runs: SnapshotStore<RunListState> = createSnapshotStore({ status: 'idle' })
  /** Open run detail (run-show) snapshot. */
  readonly runDetail: SnapshotStore<RunDetailState> = createSnapshotStore({ status: 'idle' })
  /** Panel section: the ticket tree vs the peer Runs 区块. */
  readonly panelSection: SnapshotStore<PanelSection> = createSnapshotStore('tickets')
  /** Join-run picker (TICKET-0067): set while the user picks a target run. */
  readonly runPicker: SnapshotStore<RunPickerState | undefined> = createSnapshotStore(undefined)
  /** Transient result notice (join counts / host errors); auto-clears. */
  readonly notice: SnapshotStore<Notice | undefined> = createSnapshotStore(undefined)
  /** Session currently on stage (reported by session-scope components). */
  currentSessionId: string | undefined

  private noticeTimer: ReturnType<typeof setTimeout> | undefined

  private detailsOff: (() => void) | undefined
  private shadowFactory: (() => () => void) | undefined
  private yieldListener: (() => void) | undefined
  private events: EventSource | undefined
  private refreshTimer: ReturnType<typeof setTimeout> | undefined
  private selectRevision = 0
  /** Run-hint ids accumulated across one debounce window (latest-wins #4). */
  private refreshRunHints = new Set<string>()

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
   * the open doc, and the run views. Events caused by a run/item
   * transition carry a `run` hint (TICKET-0071, additive JSON): the open
   * run detail reloads only when the hint names it. Debounced: a burst of
   * creates lands one refresh.
   */
  connectEvents = (): void => {
    if (this.events !== undefined || typeof EventSource === 'undefined') return
    this.events = new EventSource('/omt/events')
    this.events.onmessage = (message: MessageEvent<string>) => {
      let hint: OmtRunChangeHint | undefined
      try {
        const parsed = JSON.parse(message.data) as { run?: OmtRunChangeHint }
        hint = parsed.run
      } catch {
        hint = undefined
      }
      this.scheduleRefresh(hint)
    }
  }

  private scheduleRefresh(runHint?: OmtRunChangeHint): void {
    // Accumulate hints across the whole debounce window: a later hint-less
    // message must not drop an earlier run hint (#4 latest-wins 丢 hint).
    if (runHint !== undefined) this.refreshRunHints.add(runHint.id)
    if (this.refreshTimer !== undefined) clearTimeout(this.refreshTimer)
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = undefined
      const runHints = this.refreshRunHints
      this.refreshRunHints = new Set()
      const sessionId = this.currentSessionId
      void this.refreshTree(sessionId).catch(() => {})
      if (sessionId !== undefined) void this.refreshRelated(sessionId).catch(() => {})
      const active = this.active.getSnapshot()
      if (active !== undefined && !this.bodyEditing) void this.select(active.id, sessionId).catch(() => {})
      // Run views (TICKET-0071): the list refreshes when the window saw a
      // hint or the Runs 区块 is on screen (showRuns fetches on entry, so a
      // tree-only bump with the tickets section staged skips the round
      // trip); the open detail reloads only when a hint names it.
      if (runHints.size > 0 || this.panelSection.getSnapshot() === 'runs') {
        void this.refreshRuns(sessionId).catch(() => {})
      }
      const detail = this.runDetail.getSnapshot()
      if (detail.status === 'ready' && runHints.has(detail.id)) {
        void this.openRun(detail.id, sessionId).catch(() => {})
      }
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

  /**
   * Saved tree filters for the session's workspace home (STORY-0023).
   * Failures degrade to defaults — preference state never blocks the panel.
   */
  loadFilters = async (sessionId?: string): Promise<SavedFilters> => {
    const result = await this.rpc.call('/omt', 'filters-get', sessionId === undefined ? {} : { sessionId })
    if (!result.ok) return { ...DEFAULT_SAVED_FILTERS }
    return { ...DEFAULT_SAVED_FILTERS, ...(result.value as Partial<SavedFilters>) }
  }

  /** Persist a full filter bag; fire-and-forget callers may ignore errors. */
  saveFilters = async (sessionId: string | undefined, filters: SavedFilters): Promise<void> => {
    await this.rpc.call('/omt', 'filters-set', { sessionId, filters })
  }


  /** Select a node: load its doc, pin it active, shadow the details panel. */
  select = async (id: string, sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> => {
    const revision = ++this.selectRevision
    this.doc.set({ status: 'loading', id })
    const result = await this.rpc.call('/omt', 'get', { sessionId, id, ...(scope !== undefined ? { scope } : {}) })
    if (revision !== this.selectRevision) return
    if (!result.ok) {
      this.doc.set({ status: 'error', id, message: errorMessage(result) })
      return
    }
    const data = result.value as DocData
    this.doc.set({ status: 'ready', data })
    this.active.set({ id: data.node.id, title: data.node.title, status: data.node.status, priority: data.node.priority, scope: data.scope })
    if (sessionId !== undefined && data.scope !== 'global') {
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
    this.selectRevision += 1
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

  rename = async (id: string, title: string, sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> => {
    const trimmed = title.trim()
    if (trimmed === '') return
    await this.rpc.call('/omt', 'update', { sessionId, id, title: trimmed, ...(scope !== undefined ? { scope } : {}) })
    await this.afterMutation(id, sessionId, scope)
  }

  setPriority = async (id: string, priority: number, sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> => {
    await this.rpc.call('/omt', 'update', { sessionId, id, priority, ...(scope !== undefined ? { scope } : {}) })
    await this.afterMutation(id, sessionId, scope)
  }

  setArchived = async (id: string, archived: boolean, sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> => {
    await this.rpc.call('/omt', 'update', { sessionId, id, archived, ...(scope !== undefined ? { scope } : {}) })
    await this.afterMutation(id, sessionId, scope)
  }

  setStatus = async (id: string, status: ActiveInfo['status'], sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> => {
    await this.rpc.call('/omt', 'update', { sessionId, id, status, ...(scope !== undefined ? { scope } : {}) })
    await this.afterMutation(id, sessionId, scope)
  }

  private bodyEditing = false

  setBodyEditing = (editing: boolean): void => {
    this.bodyEditing = editing
  }

  saveBody = async (id: string, body: string, expectedRevision: number | undefined, sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> => {
    const result = await this.rpc.call('/omt', 'update', {
      sessionId,
      id,
      body,
      ...(expectedRevision !== undefined ? { expectedRevision } : {}),
      ...(scope !== undefined ? { scope } : {}),
    })
    if (!result.ok) throw new Error(result.error.message)
    await this.afterMutation(id, sessionId, scope)
  }

  createNode = async (
    input: { type: OmtTreeNode['type']; title: string; parentId?: string; body?: string; scope?: 'workspace' | 'global' },
    sessionId?: string,
  ): Promise<string | undefined> => {
    const result = await this.rpc.call('/omt', 'create', { sessionId, ...input })
    if (!result.ok) return undefined
    const created = result.value as { id: string }
    return created.id
  }

  appendNote = async (id: string, text: string, sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> => {
    const trimmed = text.trim()
    if (trimmed === '') return
    await this.rpc.call('/omt', 'update', { sessionId, id, append: trimmed, ...(scope !== undefined ? { scope } : {}) })
    await this.afterMutation(id, sessionId, scope)
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

  private async afterMutation(id: string, sessionId?: string, scope?: 'workspace' | 'global'): Promise<void> {
    await this.refreshTree(sessionId)
    const active = this.active.getSnapshot()
    if (active?.id === id && (scope === undefined || active.scope === scope)) await this.select(id, sessionId, scope)
  }

  // ── run flows (STORY-0013) ─────────────────────────────────────────────

  /** Show a transient notice; auto-clears after a few seconds. */
  private setNotice(notice: Notice): void {
    if (this.noticeTimer !== undefined) clearTimeout(this.noticeTimer)
    this.notice.set(notice)
    this.noticeTimer = setTimeout(() => {
      this.noticeTimer = undefined
      this.notice.set(undefined)
    }, 6000)
  }

  /** Switch the panel to the Runs 区块 and fetch the list. */
  showRuns = (sessionId?: string): void => {
    this.panelSection.set('runs')
    void this.refreshRuns(sessionId).catch(() => {})
  }

  /** Switch the panel back to the ticket tree. */
  showTickets = (): void => {
    this.panelSection.set('tickets')
  }

  /** Fetch the run list. A ready list stays on screen while refetching. */
  refreshRuns = async (sessionId?: string): Promise<void> => {
    if (this.runs.getSnapshot().status !== 'ready') this.runs.set({ status: 'loading' })
    const result = await this.rpc.call('/omt', 'run-list', sessionId === undefined ? {} : { sessionId })
    if (result.ok) {
      this.runs.set({ status: 'ready', runs: (result.value as { runs: readonly RunSummary[] }).runs })
    } else {
      this.runs.set({ status: 'error', message: errorMessage(result) })
    }
  }

  /** Open a run detail (run-show); the previous detail stays while loading. */
  openRun = async (id: string, sessionId?: string): Promise<void> => {
    const current = this.runDetail.getSnapshot()
    if (current.status !== 'ready' || current.id !== id) this.runDetail.set({ status: 'loading', id })
    const result = await this.rpc.call('/omt', 'run-show', { id, sessionId })
    if (result.ok) {
      this.runDetail.set({ status: 'ready', id, data: result.value as RunDetailData })
    } else {
      this.runDetail.set({ status: 'error', id, message: errorMessage(result) })
    }
  }

  /** Back from the run detail to the run list. */
  closeRunDetail = (): void => {
    this.runDetail.set({ status: 'idle' })
  }

  /** Deep-link (doc panel run links, TICKET-0068): open the panel on the run. */
  showRunInPanel = async (id: string, sessionId?: string): Promise<void> => {
    this.drawerOpen.set(true)
    this.showRuns(sessionId)
    await this.openRun(id, sessionId)
  }

  /**
   * Apply a run-mutation response's fresh payload directly to the stores
   * (the host response carries the post-mutation run/item; the SSE hint
   * covers other clients). A loaded runs list is patched in place; an open
   * matching detail merges the summary (config preserved, run-control /
   * run-confirm responses carry none) and the single changed item when
   * present.
   */
  private applyRunMutation(id: string, value: { run?: RunSummary; item?: RunItemView }): void {
    const run = value.run
    if (run === undefined) return
    const list = this.runs.getSnapshot()
    if (list.status === 'ready') {
      this.runs.set({
        status: 'ready',
        runs: list.runs.some(entry => entry.id === id)
          ? list.runs.map(entry => (entry.id === id ? run : entry))
          : [...list.runs, run],
      })
    }
    const detail = this.runDetail.getSnapshot()
    if (detail.status === 'ready' && detail.id === id) {
      const item = value.item
      this.runDetail.set({
        status: 'ready',
        id,
        data: {
          run: { ...run, config: detail.data.run.config },
          items: item === undefined
            ? detail.data.items
            : detail.data.items.map(entry => (entry.node_id === item.node_id ? item : entry)),
        },
      })
    }
  }

  /** Run-level / row-level control (start/pause/resume/cancel/retry/remove). */
  runControl = async (id: string, action: RunControlCommand, nodeId?: string, sessionId?: string): Promise<void> => {
    const result = await this.rpc.call('/omt', 'run-control', { id, action, ...(nodeId !== undefined ? { nodeId } : {}), sessionId })
    if (!result.ok) {
      this.setNotice({ kind: 'error', text: errorMessage(result) })
      return
    }
    this.applyRunMutation(id, result.value as { run?: RunSummary; item?: RunItemView })
  }

  /** 确认完成 / 打回 (TICKET-0070): the awaiting_confirmation decision. */
  runConfirm = async (id: string, nodeId: string, decision: 'confirm' | 'reject', sessionId?: string): Promise<void> => {
    const result = await this.rpc.call('/omt', 'run-confirm', { id, nodeId, decision, sessionId })
    if (!result.ok) {
      this.setNotice({ kind: 'error', text: errorMessage(result) })
      return
    }
    this.applyRunMutation(id, result.value as { run?: RunSummary; item?: RunItemView })
    // confirm → ticket done; reject → the 待确认 badge clears. Reload the
    // ticket doc when it is the one on stage.
    if (this.active.getSnapshot()?.id === nodeId) await this.select(nodeId, sessionId)
  }

  /** 加入 run reentry guard (#7): set before the first await, cleared in finally. */
  private joining = false

  /**
   * 加入 run (TICKET-0067): collect executable ticket/subticket nodes from
   * the selected node's subtree host-side; hierarchy containers stay context.
   * Zero active runs → 一键默认配置直建; exactly one → direct join;
   * several → the picker opens (non-terminal runs only — interrupted is
   * neither active nor history and accepts no new members). Host errors
   * (跨 home 拒绝) surface as an error notice.
   *
   * A fast double click must not create two duplicate runs: the first
   * joinRun sets `joining` before its first await and a concurrent call
   * returns immediately (guard-first, like pickRun).
   */
  joinRun = async (nodeId: string, sessionId?: string): Promise<void> => {
    if (this.joining) return
    this.joining = true
    try {
      await this.joinRunInner(nodeId, sessionId)
    } finally {
      this.joining = false
    }
  }

  private async joinRunInner(nodeId: string, sessionId?: string): Promise<void> {
    const result = await this.rpc.call('/omt', 'run-list', sessionId === undefined ? {} : { sessionId })
    if (!result.ok) {
      this.setNotice({ kind: 'error', text: errorMessage(result) })
      return
    }
    const runs = (result.value as { runs: readonly RunSummary[] }).runs
    this.runs.set({ status: 'ready', runs })
    const active = runs.filter(entry => entry.active)
    if (active.length === 0) {
      const created = await this.rpc.call('/omt', 'run-create', { nodeIds: [nodeId], sessionId })
      if (!created.ok) {
        this.setNotice({ kind: 'error', text: errorMessage(created) })
        return
      }
      const value = created.value as { run: RunSummary; added: readonly string[]; addedRunning: readonly string[]; skippedDone: number; skippedArchived: number }
      this.setNotice({
        kind: 'ok',
        key: 'run.noticeCreated',
        params: {
          run: value.run.title ?? value.run.id,
          added: value.added.length,
          running: value.addedRunning.length,
          skippedDone: value.skippedDone,
          skippedArchived: value.skippedArchived,
        },
      })
      this.applyRunMutation(value.run.id, value)
      return
    }
    if (active.length === 1 && active[0] !== undefined) {
      await this.runAddTo(active[0].id, nodeId, sessionId)
      return
    }
    this.runPicker.set({ nodeId, options: active })
  }

  /** Picker choice: join the selected run. */
  pickRun = async (runId: string, sessionId?: string): Promise<void> => {
    const picker = this.runPicker.getSnapshot()
    this.runPicker.set(undefined)
    if (picker === undefined) return
    await this.runAddTo(runId, picker.nodeId, sessionId)
  }

  cancelRunPicker = (): void => {
    this.runPicker.set(undefined)
  }

  /** run-add + result notice (加入数 / 跳过 done·archived / 重复数). */
  private async runAddTo(runId: string, nodeId: string, sessionId?: string): Promise<void> {
    const result = await this.rpc.call('/omt', 'run-add', { id: runId, nodeIds: [nodeId], sessionId })
    if (!result.ok) {
      this.setNotice({ kind: 'error', text: errorMessage(result) })
      return
    }
    const value = result.value as {
      run: RunSummary
      added: readonly string[]
      addedRunning: readonly string[]
      duplicates: readonly string[]
      skippedDone: number
      skippedArchived: number
    }
    this.setNotice({
      kind: 'ok',
      key: 'run.noticeAdded',
      params: {
        run: value.run.title ?? value.run.id,
        added: value.added.length,
        running: value.addedRunning.length,
        duplicates: value.duplicates.length,
        skippedDone: value.skippedDone,
        skippedArchived: value.skippedArchived,
      },
    })
    this.applyRunMutation(runId, value)
  }
}
