/**
 * TurnTickets: the conversation.chat.turnTail chain entry — after a turn
 * closes, list the session's related tickets (referenced via @, touched by
 * omt_* tools or UI actions). Clicking one opens it in the details panel.
 * Renders nothing when the session has no related tickets.
 */
import { useEffect } from 'react'
import type { NodeSummary } from '../store.ts'
import type { Selector } from './Drawer.tsx'
import { STATUS_KEY, type Translate } from '../locales.ts'
import { PriorityIcon } from './PriorityIcon.tsx'
import css from './TurnTickets.module.css'

export interface TurnTicketsProps {
  /** Framework session-scope prop — the session OWNING this render. */
  readonly sessionId?: string
  readonly useRelated: Selector<Record<string, readonly NodeSummary[]>>
  readonly select: (id: string, sessionId?: string) => void
  readonly refreshRelated: (sessionId: string) => void
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

const TYPE_BADGE: Record<NodeSummary['type'], string> = {
  epic: 'E',
  story: 'S',
  substory: 'SS',
  ticket: 'T',
  subticket: 'ST',
}

export function TurnTickets(props: TurnTicketsProps) {
  const t = props.t
  const sessionId = props.sessionId
  const related = props.useRelated(snapshot => (sessionId === undefined ? undefined : snapshot[sessionId]))
  const refreshRelated = props.refreshRelated
  useEffect(() => {
    if (sessionId !== undefined) void refreshRelated(sessionId)
  }, [sessionId, refreshRelated])

  if (sessionId === undefined || related === undefined || related.length === 0) return null
  return (
    <div className={css.wrap} aria-label={t('turn.aria')}>
      <span className={css.label}>{t('turn.label')}</span>
      {related.map(node => (
        <button
          key={node.id}
          type="button"
          className={css.item}
          title={t('node.titleWithStatus', { id: node.id, title: node.title, status: t(STATUS_KEY[node.status]) })}
          onClick={() => props.select(node.id, sessionId)}
        >
          <span className={`omt-badge omt-type-${node.type}`}>{TYPE_BADGE[node.type]}</span>
          <PriorityIcon priority={node.priority} t={t} />
          <span className={css.title}>{node.title}</span>
          <span className={`omt-dot ${node.archived ? 'omt-status-archived' : `omt-status-${node.status}`}`} />
        </button>
      ))}
    </div>
  )
}
