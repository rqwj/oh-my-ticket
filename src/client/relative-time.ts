/**
 * Relative-time formatting for the doc panel, translated through the
 * injected `t` seat. The absolute-date fallback locale tag rides the
 * dictionary ('time.localeTag') so English sessions see en-US dates.
 */
import type { Translate } from './locales.ts'

export function formatRelative(t: Translate, iso: string, now = Date.now()): string {
  const then = Date.parse(iso)
  if (Number.isNaN(then)) return iso
  const diff = Math.max(0, now - then)
  const minutes = Math.floor(diff / 60000)
  if (minutes < 1) return t('time.justNow')
  if (minutes < 60) return t('time.minutesAgo', { count: minutes })
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return t('time.hoursAgo', { count: hours })
  const days = Math.floor(hours / 24)
  if (days < 30) return t('time.daysAgo', { count: days })
  return new Date(then).toLocaleDateString(t('time.localeTag'))
}
