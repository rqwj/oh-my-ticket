/**
 * Tree filtering for the drawer (TICKET-0004): keyword match over id/title
 * plus archived visibility. A node survives when it matches itself or any
 * descendant does — ancestors of matches stay visible to preserve context.
 */
import type { OmtTreeNode } from './store.ts'

export interface TreeFilter {
  readonly query: string
  readonly showArchived: boolean
  /** Active type whitelist; empty/undefined = no type filtering. */
  readonly types?: readonly string[]
  /** Active status whitelist; empty/undefined = no status filtering. */
  readonly statuses?: readonly string[]
  /** Active priority whitelist; empty/undefined = no priority filtering. */
  readonly priorities?: readonly number[]
}

function matches(node: OmtTreeNode, filter: TreeFilter): boolean {
  if (filter.types !== undefined && filter.types.length > 0 && !filter.types.includes(node.type)) return false
  if (filter.statuses !== undefined && filter.statuses.length > 0 && !filter.statuses.includes(node.status)) return false
  if (filter.priorities !== undefined && filter.priorities.length > 0 && !filter.priorities.includes(node.priority)) return false
  const needle = filter.query.trim().toLowerCase()
  if (needle === '') return true
  return node.id.toLowerCase().includes(needle) || node.title.toLowerCase().includes(needle)
}

function filterNode(node: OmtTreeNode, filter: TreeFilter): OmtTreeNode | undefined {
  if (!filter.showArchived && node.archived) return undefined
  const children = node.children
    .map(child => filterNode(child, filter))
    .filter((child): child is OmtTreeNode => child !== undefined)
  if (matches(node, filter) || children.length > 0) {
    return { ...node, children }
  }
  return undefined
}

/** Filter the forest; returns matching nodes with their matching subtrees. */
export function filterForest(forest: readonly OmtTreeNode[], filter: TreeFilter): OmtTreeNode[] {
  return forest
    .map(node => filterNode(node, filter))
    .filter((node): node is OmtTreeNode => node !== undefined)
}

export type TreeSortOrder = 'none' | 'priority-desc' | 'priority-asc'

/** Sort every sibling group by priority (secondary key: id); 'none' keeps storage order. */
export function sortForest(nodes: readonly OmtTreeNode[], order: TreeSortOrder): OmtTreeNode[] {
  const sortedChildren = nodes.map(node => ({ ...node, children: sortForest(node.children, order) }))
  if (order === 'none') return sortedChildren
  const sign = order === 'priority-desc' ? -1 : 1
  return [...sortedChildren].sort((a, b) => sign * (a.priority - b.priority) || a.id.localeCompare(b.id))
}
