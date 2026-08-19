/** Priority visualization tests (scheme A). */
import { describe, expect, it } from 'vitest'
import { priorityLabel, priorityMeta, priorityOptionLabel } from '../src/client/priority.ts'
import { zh, type Translate } from '../src/client/locales.ts'

/** Dictionary-backed t over zh (mirrors the shell's {param} interpolation). */
const t: Translate = (key, params) =>
  zh[key].replace(/\{(\w+)\}/g, (_match, name: string) => String(params?.[name] ?? ''))

describe('priorityMeta', () => {
  it('P0 renders nothing', () => {
    expect(priorityMeta(0)).toBeUndefined()
  })

  it('escalates bars and colors P1→P3 (colors read the shared tokens)', () => {
    expect(priorityMeta(1)).toMatchObject({ icon: '▂', color: 'var(--omt-priority-p1)', labelKey: 'priority.p1' })
    expect(priorityMeta(2)).toMatchObject({ icon: '▂▅', color: 'var(--omt-priority-p2)', labelKey: 'priority.p2' })
    expect(priorityMeta(3)).toMatchObject({ icon: '▂▅▇', color: 'var(--omt-priority-p3)', labelKey: 'priority.p3' })
  })
})

describe('priorityLabel / priorityOptionLabel', () => {
  it('translates labels through t', () => {
    expect(priorityLabel(t, 0)).toBe('普通')
    expect(priorityLabel(t, 3)).toBe('紧急')
    expect(priorityOptionLabel(t, 0)).toBe('P0 普通')
    expect(priorityOptionLabel(t, 3)).toBe('▂▅▇ P3 紧急')
  })
})
