/**
 * Drawer: the left side-drawer shell (shell.overlay) around the shared
 * TicketPanel. Owns only drawer furniture — open gate, width preference,
 * narrow-viewport adaptation, the width drag handle; every tree interaction
 * lives in TicketPanel. Pure props component: reactive facts arrive as
 * renderer-bound use<Name> selector hooks, actions as plain callbacks.
 */
import { useEffect, useRef, useState } from 'react'
import type { PanelMode } from '../controller.ts'
import type { ActiveInfo, TreeState } from '../store.ts'
import type { SavedFilters } from '../saved-filters.ts'
import type { Translate } from '../locales.ts'
import { TicketPanel, type Selector } from './TicketPanel.tsx'
import type { RunBindings } from './RunsView.tsx'
import css from './Drawer.module.css'

// Selector moved to TicketPanel with the shared content; re-exported here so
// the older component imports keep resolving.
export type { Selector } from './TicketPanel.tsx'

/**
 * DrawerProps extends the run bindings flat (STORY-0013): the inject hooks
 * channel binds run stores/callbacks as top-level props; the shell forwards
 * itself as the panel's runView bindings object.
 */
export interface DrawerProps extends RunBindings {
  readonly useDrawerOpen: Selector<boolean>
  /** Panel-mode gate: the drawer yields to the floating window. */
  readonly usePanelMode: Selector<PanelMode>
  readonly useTree: Selector<TreeState>
  readonly useActive: Selector<ActiveInfo | undefined>
  /** Framework sessions hook (root scope); reads the current session id. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  readonly useSessions: (selector: (snapshot: any) => any) => any
  readonly toggleDrawer: (sessionId?: string) => void
  readonly refreshTree: (sessionId?: string) => void
  readonly reindex: (sessionId?: string) => void
  readonly loadFilters: (sessionId?: string) => Promise<SavedFilters>
  readonly saveFilters: (sessionId: string | undefined, filters: SavedFilters) => Promise<void>
  readonly select: (id: string, sessionId?: string) => void
  readonly archive: (id: string, sessionId?: string) => void
  readonly useDrawerWidth: Selector<number>
  readonly setDrawerWidth: (px: number) => void
  readonly useCollapsed: Selector<Record<string, boolean>>
  readonly toggleCollapsed: (id: string) => void
  /** Mode switch: re-present the open panel as a floating window. */
  readonly setPanelMode: (mode: PanelMode) => void
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

/**
 * Full-cover breakpoint: below it the drawer takes 100vw and the drag handle
 * is hidden. Mirrors the host CENTER_MIN (ui-layout columns.ts) — the width
 * below which the host itself starts conceding columns.
 */
const NARROW_BREAKPOINT = 640

/**
 * Rendered drawer width for one viewport: the stored preference capped at
 * the viewport (min(width, 100vw)); below NARROW_BREAKPOINT the drawer
 * covers the full width (and the caller retires the drag handle).
 */
export function effectiveDrawerWidth(width: number, viewport: number): number {
  return viewport < NARROW_BREAKPOINT ? viewport : Math.min(width, viewport)
}

/** Whether the drawer renders full-width (drag handle retired). */
export function isNarrowViewport(viewport: number): boolean {
  return viewport < NARROW_BREAKPOINT
}

/**
 * Drawer width drag handle (the AppFrame pattern): pointer capture on the
 * handle keeps events flowing even off-element, rAF-throttled deltas
 * against the drag-start origin.
 */
function DrawerDragHandle({ width, setDrawerWidth, t }: { width: number; setDrawerWidth: (px: number) => void; t: Translate }) {
  const [dragging, setDragging] = useState(false)
  const origin = useRef(0)
  const base = useRef(0)
  const latest = useRef(0)
  const frame = useRef<number | null>(null)
  const applyRef = useRef(setDrawerWidth)
  applyRef.current = setDrawerWidth

  useEffect(() => () => {
    if (frame.current !== null) cancelAnimationFrame(frame.current)
  }, [])

  const report = (): void => {
    applyRef.current(base.current + (latest.current - origin.current))
  }

  return (
    <div
      className={css.dragHandle}
      title={t('drawer.dragHandle')}
      data-dragging={dragging || undefined}
      onPointerDown={(event) => {
        event.preventDefault()
        event.currentTarget.setPointerCapture(event.pointerId)
        origin.current = event.clientX
        latest.current = event.clientX
        base.current = width
        setDragging(true)
      }}
      onPointerMove={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return
        latest.current = event.clientX
        frame.current ??= requestAnimationFrame(() => {
          frame.current = null
          report()
        })
      }}
      onPointerUp={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return
        event.currentTarget.releasePointerCapture(event.pointerId)
        if (frame.current !== null) {
          cancelAnimationFrame(frame.current)
          frame.current = null
        }
        report()
        setDragging(false)
      }}
    />
  )
}

export function Drawer(props: DrawerProps) {
  const t = props.t
  const open = props.useDrawerOpen(snapshot => snapshot)
  const mode = props.usePanelMode(snapshot => snapshot)
  // The tree follows the currently viewed session's workspace home.
  const current = props.useSessions((snapshot: { current?: string }) => snapshot.current) as string | undefined
  const width = props.useDrawerWidth(snapshot => snapshot)
  // Narrow-viewport adaptation (TICKET-0033): the stored width preference is
  // capped at the viewport (min(width, 100vw)); below NARROW_BREAKPOINT the
  // drawer covers the full width and the drag handle is retired (nothing to
  // resize — and one less touch target fighting the tree).
  const [viewport, setViewport] = useState(() => window.innerWidth)
  useEffect(() => {
    const onResize = (): void => setViewport(window.innerWidth)
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [])
  if (!open || mode !== 'drawer') return null
  const narrow = isNarrowViewport(viewport)
  const effectiveWidth = effectiveDrawerWidth(width, viewport)

  return (
    <aside
      className={css.drawer}
      aria-label={t('drawer.aria')}
      style={{ width: effectiveWidth }}
    >
      <TicketPanel
        useTree={props.useTree}
        useActive={props.useActive}
        useCollapsed={props.useCollapsed}
        toggleCollapsed={props.toggleCollapsed}
        refreshTree={props.refreshTree}
        reindex={props.reindex}
        loadFilters={props.loadFilters}
        saveFilters={props.saveFilters}
        select={props.select}
        archive={props.archive}
        runView={props}
        sessionId={current}
        headerActions={
          <button
            type="button"
            className={css.headerButton}
            onClick={() => props.setPanelMode('float')}
            title={t('panel.toFloat')}
          >
            ⧉
          </button>
        }
        onClose={() => props.toggleDrawer(current)}
        closeTitle={t('drawer.collapse')}
        t={t}
      />
      {!narrow && <DrawerDragHandle width={effectiveWidth} setDrawerWidth={props.setDrawerWidth} t={t} />}
    </aside>
  )
}
