/**
 * ToggleButton: the drawer on/off control, registered in both
 * conversation.session.header.actions and sidebar.footer.action.
 */
import { useEffect } from 'react'
import type { Selector } from './Drawer.tsx'
import type { Translate } from '../locales.ts'
import css from './ToggleButton.module.css'

export interface ToggleButtonProps {
  /** Present on session-scope seats (header actions); absent at root (sidebar footer). */
  readonly sessionId?: string
  readonly useDrawerOpen: Selector<boolean>
  readonly toggleDrawer: (sessionId?: string) => void
  /** Reports the staged session to the controller (turn-tail routing). */
  readonly noteSession?: (sessionId: string | undefined) => void
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

export function ToggleButton(props: ToggleButtonProps) {
  const open = props.useDrawerOpen(snapshot => snapshot)
  const noteSession = props.noteSession
  const sessionId = props.sessionId
  useEffect(() => {
    noteSession?.(sessionId)
  }, [noteSession, sessionId])
  return (
    <button
      type="button"
      className={css.toggle}
      data-open={open || undefined}
      onClick={() => props.toggleDrawer(props.sessionId)}
      title={open ? props.t('toggle.collapse') : props.t('toggle.expand')}
    >
      OMT
    </button>
  )
}
