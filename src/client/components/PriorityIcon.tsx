/**
 * PriorityIcon: the signal-bar glyph for P1+ tickets (P0 renders null).
 * Shared by the drawer rows, doc panel, turn tail, reference bar, dock.
 */
import { priorityMeta, priorityLabel } from '../priority.ts'
import type { Translate } from '../locales.ts'

export function PriorityIcon({ priority, t }: { priority: number; t: Translate }) {
  const meta = priorityMeta(priority)
  if (meta === undefined) return null
  return (
    <span
      style={{ color: meta.color, fontWeight: 700, fontSize: '0.9em', flex: 'none' }}
      title={t('priority.iconTitle', { priority, label: priorityLabel(t, priority) })}
    >
      {meta.icon}
    </span>
  )
}
