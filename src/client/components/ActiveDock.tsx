/**
 * ActiveDock: the composer-dock strip showing the currently active ticket
 * (selection doubles as activation). Clears via the × button; clicking the
 * strip reopens the doc in the details panel.
 */
import type { ActiveInfo } from '../store.ts'
import type { Selector } from './Drawer.tsx'
import { STATUS_KEY, type Translate } from '../locales.ts'
import { PriorityIcon } from './PriorityIcon.tsx'
import css from './ActiveDock.module.css'

export interface ActiveDockProps {
  /** Framework session-scope prop; routes RPC to the workspace home. */
  readonly sessionId?: string
  readonly useActive: Selector<ActiveInfo | undefined>
  readonly select: (id: string, sessionId?: string, scope?: 'workspace' | 'global') => void
  readonly clearActive: () => void
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

export function ActiveDock(props: ActiveDockProps) {
  const t = props.t
  const active = props.useActive(snapshot => snapshot)
  if (active === undefined) return null
  return (
    <div className={css.strip}>
      <button type="button" className={css.target} onClick={() => props.select(active.id, props.sessionId, active.scope)} title={t('dock.openInPanel')}>
        <span className={css.pin}>◈</span>
        <span className={css.id}>{active.id}</span>
        <PriorityIcon priority={active.priority} t={t} />
        <span className={css.title}>{active.title}</span>
        <span className={css.status}>{t(STATUS_KEY[active.status])}</span>
      </button>
      <button type="button" className={css.clear} onClick={props.clearActive} title={t('dock.clear')}>
        ×
      </button>
    </div>
  )
}
