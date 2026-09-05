import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

describe('TicketPanel hook order', () => {
  it('subscribes to collapsed state before the tree-status branch', () => {
    const source = readFileSync(new URL('../src/client/components/TicketPanel.tsx', import.meta.url), 'utf8')
    const start = source.indexOf('export function TicketPanel')
    const treeBranch = source.indexOf("tree.status === 'idle'", start)
    const hookCall = source.indexOf('useCollapsed', start)

    expect(start).toBeGreaterThan(-1)
    expect(treeBranch).toBeGreaterThan(start)
    expect(hookCall).toBeGreaterThan(start)
    expect(hookCall).toBeLessThan(treeBranch)
  })
})
