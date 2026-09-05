/** Keyboard navigation over the visible (filtered, expanded) tree rows. */
import type { OmtTreeNode } from './store.ts'

export interface VisibleRow {
  readonly id: string
  readonly parentId: string | undefined
  readonly hasChildren: boolean
  readonly collapsed: boolean
}

export function flattenVisible(
  forest: readonly OmtTreeNode[],
  collapsed: Readonly<Record<string, boolean>>,
  parentId?: string,
): VisibleRow[] {
  const rows: VisibleRow[] = []
  for (const node of forest) {
    const isCollapsed = collapsed[node.id] === true
    rows.push({
      id: node.id,
      parentId,
      hasChildren: node.children.length > 0,
      collapsed: isCollapsed,
    })
    if (!isCollapsed && node.children.length > 0) {
      rows.push(...flattenVisible(node.children, collapsed, node.id))
    }
  }
  return rows
}

export interface NavResult {
  readonly focusId: string | undefined
  readonly expandId?: string
  readonly collapseId?: string
  readonly openId?: string
}

export function navigateVisible(
  rows: readonly VisibleRow[],
  focusId: string | undefined,
  key: string,
): NavResult {
  if (rows.length === 0) return { focusId }
  const index = focusId === undefined ? -1 : rows.findIndex(row => row.id === focusId)
  const current = index >= 0 ? rows[index] : undefined
  if (key === 'Home') return { focusId: rows[0]!.id }
  if (key === 'End') return { focusId: rows[rows.length - 1]!.id }
  if (key === 'ArrowDown') return { focusId: rows[Math.min(rows.length - 1, Math.max(0, index) + (index < 0 ? 0 : 1))]!.id }
  if (key === 'ArrowUp') return { focusId: rows[Math.max(0, (index < 0 ? 0 : index) - 1)]!.id }
  if (key === 'Enter' && current !== undefined) return { focusId: current.id, openId: current.id }
  if (key === 'ArrowRight' && current !== undefined) {
    if (current.hasChildren && current.collapsed) return { focusId: current.id, expandId: current.id }
    const next = rows[index + 1]
    if (next !== undefined && next.parentId === current.id) return { focusId: next.id }
    return { focusId: current.id }
  }
  if (key === 'ArrowLeft' && current !== undefined) {
    if (current.hasChildren && !current.collapsed) return { focusId: current.id, collapseId: current.id }
    if (current.parentId !== undefined) return { focusId: current.parentId }
    return { focusId: current.id }
  }
  return { focusId }
}

export const CHILD_TYPES: Readonly<Record<OmtTreeNode['type'], readonly OmtTreeNode['type'][]>> = {
  epic: ['story'],
  story: ['substory', 'ticket'],
  substory: ['ticket'],
  ticket: ['subticket'],
  subticket: [],
}

export function ancestorIdsOf(forest: readonly OmtTreeNode[], id: string): string[] {
  const walk = (nodes: readonly OmtTreeNode[], trail: string[]): string[] | undefined => {
    for (const node of nodes) {
      if (node.id === id) return trail
      const found = walk(node.children, [...trail, node.id])
      if (found !== undefined) return found
    }
    return undefined
  }
  return walk(forest, []) ?? []
}
