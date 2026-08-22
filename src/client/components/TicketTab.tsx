/**
 * TicketTab: the OMT conversation.view entry — the third ticket-list
 * presentation (STORY-0006), a tab beside Chat | Trajectory rendered by the
 * session body. Session scope: the framework supplies sessionId directly as
 * a standard prop, so the tree follows THIS view's session with no
 * side-channel attribution. Pure props component like every other shell.
 */
import type { PanelMode } from '../controller.ts'
import type { ActiveInfo, TreeState } from '../store.ts'
import type { SavedFilters } from '../saved-filters.ts'
import type { Translate } from '../locales.ts'
import { TicketPanel, type Selector } from './TicketPanel.tsx'
import type { RunBindings } from './RunsView.tsx'
import css from './TicketTab.module.css'

export interface TicketTabProps extends RunBindings {
  /** Framework standard prop (conversation.view owner): this view's session. */
  readonly sessionId: string
  readonly useTree: Selector<TreeState>
  readonly useActive: Selector<ActiveInfo | undefined>
  readonly useCollapsed: Selector<Record<string, boolean>>
  readonly toggleCollapsed: (id: string) => void
  readonly refreshTree: (sessionId?: string) => void
  readonly reindex: (sessionId?: string) => void
  readonly loadFilters: (sessionId?: string) => Promise<SavedFilters>
  readonly saveFilters: (sessionId: string | undefined, filters: SavedFilters) => Promise<void>
  readonly select: (id: string, sessionId?: string) => void
  readonly archive: (id: string, sessionId?: string) => void
  /** Pop-out seats: switch the overlay to float mode and open it. */
  readonly setPanelMode: (mode: PanelMode) => void
  readonly openPanel: (sessionId?: string) => void
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

export function TicketTab(props: TicketTabProps) {
  const t = props.t
  const sessionId = props.sessionId
  return (
    <div className={css.tab} aria-label={t('tab.aria')}>
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
        sessionId={sessionId}
        headerActions={
          <button
            type="button"
            className={css.headerButton}
            onClick={() => {
              props.setPanelMode('float')
              props.openPanel(sessionId)
            }}
            title={t('panel.popOut')}
          >
            ⧉
          </button>
        }
        t={t}
      />
    </div>
  )
}
