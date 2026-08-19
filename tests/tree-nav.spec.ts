import { describe, expect, it } from 'vitest'
import { ancestorIdsOf, flattenVisible, navigateVisible, type VisibleRow } from '../src/client/tree-nav.ts'
import type { OmtTreeNode } from '../src/client/store.ts'

function node(id: string, children: OmtTreeNode[] = []): OmtTreeNode {
  return {
    id, type: 'ticket', title: id, status: 'open', archived: false, priority: 0,
    path: id, created_at: '', updated_at: '', children,
  }
}

const forest = [node('EPIC-1', [node('STORY-1', [node('TICKET-1')]), node('STORY-2')])]

describe('flattenVisible', () => {
  it('skips children of collapsed nodes', () => {
    const all = flattenVisible(forest, {})
    expect(all.map(row => row.id)).toEqual(['EPIC-1', 'STORY-1', 'TICKET-1', 'STORY-2'])
    const closed = flattenVisible(forest, { 'STORY-1': true })
    expect(closed.map(row => row.id)).toEqual(['EPIC-1', 'STORY-1', 'STORY-2'])
  })
})

describe('navigateVisible', () => {
  const rows: VisibleRow[] = flattenVisible(forest, {})
  it('moves down and up within visible rows', () => {
    expect(navigateVisible(rows, 'EPIC-1', 'ArrowDown').focusId).toBe('STORY-1')
    expect(navigateVisible(rows, 'STORY-1', 'ArrowUp').focusId).toBe('EPIC-1')
  })
  it('expands a collapsed parent on right and enters first child when open', () => {
    const collapsed = flattenVisible(forest, { 'STORY-1': true })
    expect(navigateVisible(collapsed, 'STORY-1', 'ArrowRight')).toEqual({ focusId: 'STORY-1', expandId: 'STORY-1' })
    expect(navigateVisible(rows, 'STORY-1', 'ArrowRight').focusId).toBe('TICKET-1')
  })
  it('collapses or climbs to parent on left', () => {
    expect(navigateVisible(rows, 'STORY-1', 'ArrowLeft')).toEqual({ focusId: 'STORY-1', collapseId: 'STORY-1' })
    const leaf = flattenVisible(forest, { 'STORY-1': true })
    expect(navigateVisible(leaf, 'STORY-1', 'ArrowLeft').focusId).toBe('EPIC-1')
  })
  it('opens on enter and jumps home/end', () => {
    expect(navigateVisible(rows, 'TICKET-1', 'Enter').openId).toBe('TICKET-1')
    expect(navigateVisible(rows, 'TICKET-1', 'Home').focusId).toBe('EPIC-1')
    expect(navigateVisible(rows, 'EPIC-1', 'End').focusId).toBe('STORY-2')
  })
})

describe('ancestorIdsOf', () => {
  it('returns the path from root to the node parent', () => {
    expect(ancestorIdsOf(forest, 'TICKET-1')).toEqual(['EPIC-1', 'STORY-1'])
    expect(ancestorIdsOf(forest, 'EPIC-1')).toEqual([])
  })
})
