/** Relative-time util tests. */
import { describe, expect, it } from 'vitest'
import { formatRelative } from '../src/client/relative-time.ts'
import { zh, type Translate } from '../src/client/locales.ts'

/** Dictionary-backed t over zh (mirrors the shell's {param} interpolation). */
const t: Translate = (key, params) =>
  zh[key].replace(/\{(\w+)\}/g, (_match, name: string) => String(params?.[name] ?? ''))

const NOW = Date.parse('2026-08-18T12:00:00.000Z')

describe('formatRelative', () => {
  it('covers justNow/minutes/hours/days/date buckets', () => {
    expect(formatRelative(t, '2026-08-18T11:59:40.000Z', NOW)).toBe('刚刚')
    expect(formatRelative(t, '2026-08-18T11:30:00.000Z', NOW)).toBe('30 分钟前')
    expect(formatRelative(t, '2026-08-18T09:00:00.000Z', NOW)).toBe('3 小时前')
    expect(formatRelative(t, '2026-08-15T12:00:00.000Z', NOW)).toBe('3 天前')
    expect(formatRelative(t, '2026-01-01T00:00:00.000Z', NOW)).toContain('2026')
  })

  it('passes through unparsable input', () => {
    expect(formatRelative(t, 'not-a-date', NOW)).toBe('not-a-date')
  })
})
