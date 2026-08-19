/**
 * Priority visualization (scheme A, confirmed): signal bars with escalating
 * color. P0 renders nothing — zero noise for the default case. Labels are
 * dictionary keys; callers translate through the injected `t` seat.
 */
import type { OmtKey, Translate } from './locales.ts'

export interface PriorityMeta {
  readonly icon: string
  readonly color: string
  readonly labelKey: OmtKey
}

const META: Record<number, PriorityMeta> = {
  1: { icon: '▂', color: 'var(--omt-priority-p1)', labelKey: 'priority.p1' },
  2: { icon: '▂▅', color: 'var(--omt-priority-p2)', labelKey: 'priority.p2' },
  3: { icon: '▂▅▇', color: 'var(--omt-priority-p3)', labelKey: 'priority.p3' },
}

/** Display meta for one priority value; undefined for P0. */
export function priorityMeta(priority: number): PriorityMeta | undefined {
  return META[priority]
}

/** Translated priority label (P0 included). */
export function priorityLabel(t: Translate, priority: number): string {
  return t(META[priority]?.labelKey ?? 'priority.p0')
}

/** Option label for the priority selector (icon + translated name). */
export function priorityOptionLabel(t: Translate, priority: number): string {
  const meta = META[priority]
  return meta === undefined ? `P0 ${t('priority.p0')}` : `${meta.icon} P${priority} ${t(meta.labelKey)}`
}
