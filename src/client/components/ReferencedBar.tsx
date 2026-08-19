/**
 * ReferencedBar: the input-dock strip listing tickets currently referenced
 * (via @) in the draft, with FULL titles — the textarea chip cell is a
 * fixed-width non-interactive overlay, so this bar is where referenced
 * tickets are fully visible and clickable (TICKET-0005). The list derives
 * from the owner-supplied occurrence table, so deleting a chip or
 * submitting the draft updates it automatically.
 */
import { useEffect } from 'react'
import type { NodeSummary } from '../store.ts'
import type { Selector } from './Drawer.tsx'
import { STATUS_KEY, type Translate } from '../locales.ts'
import { PriorityIcon } from './PriorityIcon.tsx'
import css from './ReferencedBar.module.css'

/** Structural subset of the InputZone owner's input snapshot. */
interface InputSnapshotLike {
  readonly occurrences?: readonly { source: string; ref: string; label: string }[]
}

export interface ReferencedBarProps {
  /** Owner share (InputZone): input carries the live occurrence table. */
  readonly input?: InputSnapshotLike
  /** Framework session-scope prop. */
  readonly sessionId?: string
  readonly useSummaries: Selector<Record<string, NodeSummary>>
  readonly ensureSummaries: (sessionId: string, ids: readonly string[]) => void
  readonly select: (id: string, sessionId?: string) => void
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

/** Dedup ticket refs from the occurrence table (order preserved). */
export function ticketRefs(input: InputSnapshotLike | undefined): string[] {
  const refs: string[] = []
  for (const occurrence of input?.occurrences ?? []) {
    if (occurrence.source === 'ticket' && !refs.includes(occurrence.ref)) refs.push(occurrence.ref)
  }
  return refs
}

export function ReferencedBar(props: ReferencedBarProps) {
  const t = props.t
  const refs = ticketRefs(props.input)
  const summaries = props.useSummaries(snapshot => snapshot)
  const sessionId = props.sessionId
  const ensureSummaries = props.ensureSummaries
  const missingKey = refs.filter(id => summaries[id] === undefined).join(',')

  useEffect(() => {
    if (sessionId !== undefined && missingKey !== '') {
      void ensureSummaries(sessionId, missingKey.split(','))
    }
  }, [sessionId, missingKey, ensureSummaries])

  if (refs.length === 0) return null
  return (
    <div className={css.strip} aria-label={t('refs.aria')}>
      <span className={css.label}>{t('refs.label')}</span>
      {refs.map(ref => {
        const summary = summaries[ref]
        return (
          <button
            key={ref}
            type="button"
            className={css.item}
            title={summary !== undefined
              ? t('node.titleWithStatus', { id: summary.id, title: summary.title, status: t(STATUS_KEY[summary.status]) })
              : ref}
            onClick={() => props.select(ref, sessionId)}
          >
            {summary !== undefined && (
              <span className={`omt-badge omt-type-${summary.type}`}>{TYPE_BADGE[summary.type]}</span>
            )}
            {summary !== undefined && <PriorityIcon priority={summary.priority} t={t} />}
            <span className={css.title}>{summary !== undefined ? `${summary.id} ${summary.title}` : ref}</span>
            {summary !== undefined && (
              <span className={`omt-dot ${summary.archived ? 'omt-status-archived' : `omt-status-${summary.status}`}`} />
            )}
          </button>
        )
      })}
    </div>
  )
}
