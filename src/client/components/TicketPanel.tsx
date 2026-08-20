/**
 * TicketPanel: the shared OMT ticket-tree content — header actions, search,
 * filter/sort chips, and the tree itself — rendered by every presentation
 * shell (left drawer, floating window, conversation.view OMT tab). Pure
 * props component: reactive facts arrive as renderer-bound use<Name>
 * selector hooks (inject hooks compartment), actions as plain callbacks.
 * The owning shell supplies positioning, sizing, and drag furniture; the
 * panel fills whatever box it is given (flex column, min-height 0).
 */
import { useEffect, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from 'react'
import type { ActiveInfo, OmtTreeNode, TreeState } from '../store.ts'
import { filterForest, sortForest, type TreeFilter, type TreeSortOrder } from '../tree-filter.ts'
import { priorityMeta } from '../priority.ts'
import { STATUS_KEY, type Translate } from '../locales.ts'
import { PriorityIcon } from './PriorityIcon.tsx'
import { RunsView, type RunBindings } from './RunsView.tsx'
import { RunFlowExtras } from './RunPicker.tsx'
import css from './TicketPanel.module.css'

/** Renderer-bound selector hook (inject hooks compartment member). */
export type Selector<T> = <S>(selector: (snapshot: T) => S) => S

/** Pointer handlers a shell puts on the panel header (float move drag). */
export interface HeaderDragHandlers {
  readonly onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void
  readonly onPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void
  readonly onPointerUp: (event: ReactPointerEvent<HTMLDivElement>) => void
}

export interface TicketPanelProps {
  readonly useTree: Selector<TreeState>
  readonly useActive: Selector<ActiveInfo | undefined>
  readonly useCollapsed: Selector<Record<string, boolean>>
  readonly toggleCollapsed: (id: string) => void
  readonly refreshTree: (sessionId?: string) => void
  readonly reindex: (sessionId?: string) => void
  readonly select: (id: string, sessionId?: string) => void
  readonly archive: (id: string, sessionId?: string) => void
  /** Run 区块 bindings (STORY-0013): section nav, RunsView, picker, notice. */
  readonly runView: RunBindings
  /** Session whose workspace home the tree follows. */
  readonly sessionId: string | undefined
  /** Extra header buttons ahead of the close seat (mode switches). */
  readonly headerActions?: ReactNode
  /** Close affordance; shells without one (the tab) omit it. */
  readonly onClose?: () => void
  readonly closeTitle?: string
  /** Shell drag furniture applied to the header bar (floating window). */
  readonly headerDrag?: HeaderDragHandlers
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

const TYPE_BADGE: Record<OmtTreeNode['type'], string> = {
  epic: 'E',
  story: 'S',
  substory: 'SS',
  ticket: 'T',
  subticket: 'ST',
}

/** Dot class: archived (hollow) overrides the lifecycle color. */
function dotClass(node: { status: OmtTreeNode['status']; archived: boolean }): string {
  return node.archived ? 'omt-status-archived' : `omt-status-${node.status}`
}

function statusText(t: Translate, node: { status: OmtTreeNode['status']; archived: boolean }): string {
  return node.archived
    ? t('status.archivedWith', { status: t(STATUS_KEY[node.status]) })
    : t(STATUS_KEY[node.status])
}

interface TreeRowProps {
  readonly node: OmtTreeNode
  readonly depth: number
  readonly activeId: string | undefined
  readonly onSelect: (id: string) => void
  readonly onArchive: (id: string) => void
  readonly onJoinRun: (id: string) => void
  readonly useCollapsed: Selector<Record<string, boolean>>
  readonly onToggleCollapsed: (id: string) => void
  readonly showId: boolean
  readonly t: Translate
}

function TreeRow({ node, depth, activeId, onSelect, onArchive, onJoinRun, useCollapsed, onToggleCollapsed, showId, t }: TreeRowProps) {
  // Collapse state persists across sessions (shared store, TICKET-0011).
  const collapsed = useCollapsed(snapshot => snapshot[node.id] === true)
  const hasChildren = node.children.length > 0
  return (
    <div>
      <div
        className={`${css.row} ${node.id === activeId ? css.rowActive : ''}`}
        style={{ paddingLeft: 8 + depth * 14 }}
        onClick={() => onSelect(node.id)}
        title={t('node.titleWithStatus', { id: node.id, title: node.title, status: statusText(t, node) })}
      >
        <span
          className={`${css.caret} ${hasChildren ? '' : css.caretHidden}`}
          onClick={(event) => {
            event.stopPropagation()
            onToggleCollapsed(node.id)
          }}
        >
          {collapsed ? '▸' : '▾'}
        </span>
        <span className={`omt-badge omt-badge--lg omt-type-${node.type}`}>{TYPE_BADGE[node.type]}</span>
        <PriorityIcon priority={node.priority} t={t} />
        {showId && <span className={css.rowId}>{node.id}</span>}
        <span className={css.rowTitle}>{node.title}</span>
        {!node.archived && (
          <button
            type="button"
            className={css.archiveButton}
            title={t('run.joinTitle')}
            onClick={(event) => {
              event.stopPropagation()
              onJoinRun(node.id)
            }}
          >
            ▸▸
          </button>
        )}
        {!node.archived && (
          <button
            type="button"
            className={css.archiveButton}
            title={t('drawer.archiveTitle', { id: node.id })}
            onClick={(event) => {
              event.stopPropagation()
              onArchive(node.id)
            }}
          >
            ⛁
          </button>
        )}
        <span className={`omt-dot omt-dot--lg ${dotClass(node)}`} />
      </div>
      {!collapsed && node.children.map(child => (
        <TreeRow key={child.id} node={child} depth={depth + 1} activeId={activeId} onSelect={onSelect} onArchive={onArchive} onJoinRun={onJoinRun} useCollapsed={useCollapsed} onToggleCollapsed={onToggleCollapsed} showId={showId} t={t} />
      ))}
    </div>
  )
}

/** Two-stage reindex trigger: text button (distinct from the refresh icon), arms for 3s on first click. */
function ReindexButton({ onReindex, t }: { onReindex: () => void; t: Translate }) {
  const [armed, setArmed] = useState(false)
  useEffect(() => {
    if (!armed) return
    const timer = setTimeout(() => setArmed(false), 3000)
    return () => clearTimeout(timer)
  }, [armed])
  return (
    <button
      type="button"
      className={`${css.reindexButton} ${armed ? css.reindexArmed : ''}`}
      title={armed ? t('drawer.reindexArmedTitle') : t('drawer.reindexTitle')}
      onClick={() => {
        if (armed) {
          setArmed(false)
          onReindex()
        } else {
          setArmed(true)
        }
      }}
    >
      {armed ? t('drawer.reindexConfirm') : t('drawer.reindex')}
    </button>
  )
}

export function TicketPanel(props: TicketPanelProps) {
  const t = props.t
  const tree = props.useTree(snapshot => snapshot)
  const active = props.useActive(snapshot => snapshot)
  const section = props.runView.usePanelSection(snapshot => snapshot)
  // Viewing-only filter state (search keyword + archived + type/status chips).
  const [query, setQuery] = useState('')
  const [showArchived, setShowArchived] = useState(false)
  const [typeFilter, setTypeFilter] = useState<readonly string[]>([])
  const [statusFilter, setStatusFilter] = useState<readonly string[]>([])
  const [priorityFilter, setPriorityFilter] = useState<readonly number[]>([])
  const [showId, setShowId] = useState(false)
  const [sortOrder, setSortOrder] = useState<TreeSortOrder>('none')
  const toggleNum = (list: readonly number[], value: number): readonly number[] =>
    list.includes(value) ? list.filter(item => item !== value) : [...list, value]
  const toggleIn = (list: readonly string[], value: string): readonly string[] =>
    list.includes(value) ? list.filter(item => item !== value) : [...list, value]
  // The tree follows the session whose shell this panel lives in. The panel
  // mounts only while its shell is visible, so mount == (re)open.
  const sessionId = props.sessionId
  useEffect(() => {
    void props.refreshTree(sessionId)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId])

  const filter: TreeFilter = { query, showArchived, types: typeFilter, statuses: statusFilter, priorities: priorityFilter }

  return (
    <div className={css.root}>
      <div
        className={`${css.header} ${props.headerDrag !== undefined ? css.headerDraggable : ''}`}
        title={props.headerDrag !== undefined ? t('float.move') : undefined}
        {...(props.headerDrag ?? {})}
      >
        <span className={css.headerTitle}>OMT Tickets</span>
        <button type="button" className={css.headerButton} onClick={() => props.refreshTree(sessionId)} title={t('drawer.refresh')}>
          ↻
        </button>
        <ReindexButton onReindex={() => props.reindex(sessionId)} t={t} />
        {props.headerActions}
        {props.onClose !== undefined && (
          <button type="button" className={css.headerButton} onClick={props.onClose} title={props.closeTitle}>
            ×
          </button>
        )}
      </div>
      <div className={css.sectionNav}>
        <button
          type="button"
          className={`${css.sectionTab} ${section === 'tickets' ? css.sectionTabOn : ''}`}
          onClick={props.runView.showTickets}
        >
          {t('run.sectionTickets')}
        </button>
        <button
          type="button"
          className={`${css.sectionTab} ${section === 'runs' ? css.sectionTabOn : ''}`}
          onClick={() => props.runView.showRuns(sessionId)}
        >
          {t('run.sectionRuns')}
        </button>
      </div>
      {section === 'runs' ? (
        <RunsView bindings={props.runView} select={props.select} sessionId={sessionId} t={t} />
      ) : (
        <>
      <div className={css.toolbar}>
        <input
          type="search"
          className={css.search}
          placeholder={t('drawer.searchPlaceholder')}
          value={query}
          onChange={event => setQuery(event.target.value)}
        />
      </div>
      <div className={css.filters}>
        {(['epic', 'story', 'substory', 'ticket', 'subticket'] as const).map(type => (
          <button
            key={type}
            type="button"
            className={`${css.filterChip} omt-type-${type} ${typeFilter.includes(type) ? css.filterChipOn : ''}`}
            title={t('drawer.filterType', { type })}
            onClick={() => setTypeFilter(toggleIn(typeFilter, type))}
          >
            {TYPE_BADGE[type]}
          </button>
        ))}
        <span className={css.filterDivider} />
        {(['open', 'in_progress', 'done', 'blocked', 'skipped'] as const).map(status => (
          <button
            key={status}
            type="button"
            className={`${css.filterChip} ${statusFilter.includes(status) ? css.filterChipOn : ''}`}
            title={t('drawer.filterStatus', { status: t(STATUS_KEY[status]) })}
            onClick={() => setStatusFilter(toggleIn(statusFilter, status))}
          >
            <span className={`omt-dot omt-status-${status}`} />
            {t(STATUS_KEY[status])}
          </button>
        ))}
        <span className={css.filterDivider} />
        {[0, 1, 2, 3].map(p => (
          <button
            key={p}
            type="button"
            className={`${css.filterChip} ${priorityFilter.includes(p) ? css.filterChipOn : ''}`}
            title={t('drawer.filterPriority', { priority: p })}
            onClick={() => setPriorityFilter(toggleNum(priorityFilter, p))}
          >
            {p === 0 ? 'P0' : <span style={{ color: priorityMeta(p)?.color }}>{priorityMeta(p)?.icon} P{p}</span>}
          </button>
        ))}
        <span className={css.filterDivider} />
        <button
          type="button"
          className={`${css.filterChip} ${showArchived ? css.filterChipOn : ''}`}
          title={t('drawer.showArchivedTitle')}
          onClick={() => setShowArchived(!showArchived)}
        >
          {t('drawer.showArchived')}
        </button>
      </div>
      <div className={css.filters}>
        <button
          type="button"
          className={`${css.filterChip} ${showId ? css.filterChipOn : ''}`}
          title={t('drawer.showIdTitle')}
          onClick={() => setShowId(!showId)}
        >
          {t('drawer.showId')}
        </button>
        <span className={css.filterDivider} />
        {([['none', 'drawer.sortNone'], ['priority-desc', 'drawer.sortPriorityDesc'], ['priority-asc', 'drawer.sortPriorityAsc']] as const).map(([order, key]) => (
          <button
            key={order}
            type="button"
            className={`${css.filterChip} ${sortOrder === order ? css.filterChipOn : ''}`}
            title={t(key)}
            onClick={() => setSortOrder(order)}
          >
            {t(key)}
          </button>
        ))}
      </div>
      <div className={css.tree}>
        {tree.status === 'idle' || tree.status === 'loading' ? (
          <div className={css.placeholder}>{t('drawer.loading')}</div>
        ) : tree.status === 'error' ? (
          <div className={css.placeholder}>{t('drawer.loadFailed', { message: tree.message })}</div>
        ) : tree.forest.length === 0 ? (
          <div className={css.placeholder}>{t('drawer.empty')}</div>
        ) : (
          (() => {
            const visible = sortForest(filterForest(tree.forest, filter), sortOrder)
            return visible.length === 0 ? (
              <div className={css.placeholder}>{t('drawer.noMatch')}</div>
            ) : (
              visible.map(node => (
                <TreeRow
                  key={node.id}
                  node={node}
                  depth={0}
                  activeId={active?.id}
                  onSelect={id => props.select(id, sessionId)}
                  onArchive={id => props.archive(id, sessionId)}
                  onJoinRun={id => props.runView.joinRun(id, sessionId)}
                  useCollapsed={props.useCollapsed}
                  onToggleCollapsed={props.toggleCollapsed}
                  showId={showId}
                  t={t}
                />
              ))
            )
          })()
        )}
      </div>
        </>
      )}
      <RunFlowExtras
        useNotice={props.runView.useNotice}
        useRunPicker={props.runView.useRunPicker}
        pickRun={props.runView.pickRun}
        cancelRunPicker={props.runView.cancelRunPicker}
        sessionId={sessionId}
        t={t}
      />
    </div>
  )
}
