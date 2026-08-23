/**
 * FloatWindow: the floating-window shell (shell.overlay) around the shared
 * TicketPanel (STORY-0006). A fixed-position, header-draggable, corner-
 * resizable panel. Drag furniture follows the DrawerDragHandle pattern —
 * pointer capture on the drag surface, rAF-throttled deltas against the
 * drag-start origin — and every geometry write is viewport-clamped through
 * float-geometry before it reaches the persisted stores.
 */
import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'
import type { PanelMode } from '../controller.ts'
import { clampFloatPos, clampFloatSize, type FloatPos, type FloatSize } from '../float-geometry.ts'
import type { ActiveInfo, TreeState } from '../store.ts'
import type { Translate } from '../locales.ts'
import { TicketPanel, type HeaderDragHandlers, type Selector } from './TicketPanel.tsx'
import type { RunBindings } from './RunsView.tsx'
import type { SavedFilters } from '../saved-filters.ts'
import css from './FloatWindow.module.css'

export interface FloatWindowProps extends RunBindings {
  readonly useDrawerOpen: Selector<boolean>
  readonly usePanelMode: Selector<PanelMode>
  readonly useFloatPos: Selector<FloatPos>
  readonly useFloatSize: Selector<FloatSize>
  readonly setFloatPos: (pos: FloatPos) => void
  readonly setFloatSize: (size: FloatSize) => void
  readonly useTree: Selector<TreeState>
  readonly useActive: Selector<ActiveInfo | undefined>
  readonly useCollapsed: Selector<Record<string, boolean>>
  readonly toggleCollapsed: (id: string) => void
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
  readonly createNode: import('./TicketPanel.tsx').TicketPanelProps['createNode']
  readonly expandIds: (ids: readonly string[]) => void
  /** Mode switch: re-present the open panel as the left drawer. */
  readonly setPanelMode: (mode: PanelMode) => void
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

/** Shared pointer-capture drag state: origin pointer + base value + latest. */
interface DragState {
  readonly pointerId: number
  readonly originX: number
  readonly originY: number
  readonly baseX: number
  readonly baseY: number
  latestX: number
  latestY: number
}

export function FloatWindow(props: FloatWindowProps) {
  const t = props.t
  const open = props.useDrawerOpen(snapshot => snapshot)
  const mode = props.usePanelMode(snapshot => snapshot)
  const storedPos = props.useFloatPos(snapshot => snapshot)
  const storedSize = props.useFloatSize(snapshot => snapshot)
  const current = props.useSessions((snapshot: { current?: string }) => snapshot.current) as string | undefined

  // Geometry is re-clamped against the LIVE viewport on every render: a
  // persisted preference must never strand the window off a resized screen.
  const [viewport, setViewport] = useState(() => ({ w: window.innerWidth, h: window.innerHeight }))
  useEffect(() => {
    const onResize = (): void => setViewport({ w: window.innerWidth, h: window.innerHeight })
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [])
  const size = clampFloatSize(storedSize, viewport.w, viewport.h)
  const pos = clampFloatPos(storedPos, viewport.w, viewport.h)

  const moveDrag = useRef<DragState | null>(null)
  const resizeDrag = useRef<DragState | null>(null)
  const frame = useRef<number | null>(null)
  const viewportRef = useRef(viewport)
  viewportRef.current = viewport
  const sizeRef = useRef(size)
  sizeRef.current = size
  const setFloatPosRef = useRef(props.setFloatPos)
  setFloatPosRef.current = props.setFloatPos
  const setFloatSizeRef = useRef(props.setFloatSize)
  setFloatSizeRef.current = props.setFloatSize

  useEffect(() => () => {
    if (frame.current !== null) cancelAnimationFrame(frame.current)
  }, [])

  const schedule = (report: () => void): void => {
    frame.current ??= requestAnimationFrame(() => {
      frame.current = null
      report()
    })
  }
  const cancelScheduled = (): void => {
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current)
      frame.current = null
    }
  }

  // Header move drag: buttons/inputs inside the header keep their clicks.
  const headerDrag: HeaderDragHandlers = {
    onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.button !== 0) return
      if ((event.target as HTMLElement).closest('button, input, a') !== null) return
      event.preventDefault()
      event.currentTarget.setPointerCapture(event.pointerId)
      moveDrag.current = {
        pointerId: event.pointerId,
        originX: event.clientX,
        originY: event.clientY,
        baseX: pos.x,
        baseY: pos.y,
        latestX: event.clientX,
        latestY: event.clientY,
      }
    },
    onPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => {
      const drag = moveDrag.current
      if (drag === null || !event.currentTarget.hasPointerCapture(drag.pointerId)) return
      drag.latestX = event.clientX
      drag.latestY = event.clientY
      schedule(() => {
        setFloatPosRef.current(clampFloatPos(
          { x: drag.baseX + (drag.latestX - drag.originX), y: drag.baseY + (drag.latestY - drag.originY) },
          viewportRef.current.w,
          viewportRef.current.h,
        ))
      })
    },
    onPointerUp: (event: ReactPointerEvent<HTMLDivElement>) => {
      const drag = moveDrag.current
      if (drag === null || !event.currentTarget.hasPointerCapture(drag.pointerId)) return
      event.currentTarget.releasePointerCapture(drag.pointerId)
      cancelScheduled()
      setFloatPosRef.current(clampFloatPos(
        { x: drag.baseX + (drag.latestX - drag.originX), y: drag.baseY + (drag.latestY - drag.originY) },
        viewportRef.current.w,
        viewportRef.current.h,
      ))
      moveDrag.current = null
    },
  }

  if (!open || mode !== 'float') return null

  return (
    <div
      className={css.float}
      role="dialog"
      aria-label={t('float.aria')}
      style={{ left: pos.x, top: pos.y, width: size.w, height: size.h }}
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
        createNode={props.createNode}
        expandIds={props.expandIds}
        runView={props}
        sessionId={current}
        headerDrag={headerDrag}
        headerActions={
          <button
            type="button"
            className={css.headerButton}
            onClick={() => props.setPanelMode('drawer')}
            title={t('panel.toDrawer')}
          >
            ◧
          </button>
        }
        onClose={() => props.toggleDrawer(current)}
        closeTitle={t('drawer.collapse')}
        t={t}
      />
      <div
        className={css.resizeHandle}
        title={t('float.resize')}
        onPointerDown={(event) => {
          if (event.button !== 0) return
          event.preventDefault()
          event.currentTarget.setPointerCapture(event.pointerId)
          resizeDrag.current = {
            pointerId: event.pointerId,
            originX: event.clientX,
            originY: event.clientY,
            baseX: sizeRef.current.w,
            baseY: sizeRef.current.h,
            latestX: event.clientX,
            latestY: event.clientY,
          }
        }}
        onPointerMove={(event) => {
          const drag = resizeDrag.current
          if (drag === null || !event.currentTarget.hasPointerCapture(drag.pointerId)) return
          drag.latestX = event.clientX
          drag.latestY = event.clientY
          schedule(() => {
            setFloatSizeRef.current(clampFloatSize(
              { w: drag.baseX + (drag.latestX - drag.originX), h: drag.baseY + (drag.latestY - drag.originY) },
              viewportRef.current.w,
              viewportRef.current.h,
            ))
          })
        }}
        onPointerUp={(event) => {
          const drag = resizeDrag.current
          if (drag === null || !event.currentTarget.hasPointerCapture(drag.pointerId)) return
          event.currentTarget.releasePointerCapture(drag.pointerId)
          cancelScheduled()
          setFloatSizeRef.current(clampFloatSize(
            { w: drag.baseX + (drag.latestX - drag.originX), h: drag.baseY + (drag.latestY - drag.originY) },
            viewportRef.current.w,
            viewportRef.current.h,
          ))
          resizeDrag.current = null
        }}
      />
    </div>
  )
}
