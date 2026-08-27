/**
 * U10 domain types + store: ticket tree state over the daemon JSON-RPC.
 * Types mirror the shared protocol shapes (schema/common.schema.json —
 * node_type/status enums); the desktop defines them locally per KD4
 * (fresh surface; no cross-surface imports).
 */

export type NodeType = 'epic' | 'story' | 'substory' | 'ticket' | 'subticket'
export type NodeStatus = 'open' | 'in_progress' | 'done' | 'blocked' | 'skipped' | 'archived'

export interface OmtNode {
  id: string
  type: NodeType
  title: string
  status: NodeStatus
  priority: number
  parentId?: string
  revision?: number
  path?: string
  /** 线上契约：archived 是独立布尔（status 保持 open/done 等生命周期值，
   *  不存在 status:'archived'）——归档过滤与呈现必须以本字段为准。 */
  archived?: boolean
}

export interface HomeInfo {
  homeId: string
  name?: string
  kind?: string
  path?: string
}

export interface SavedFilters {
  query?: string
  statuses?: string[]
  types?: string[]
  priorities?: number[]
  showArchived?: boolean
  showId?: boolean
  sortOrder?: string
}

export interface RunSummary {
  id: string
  title?: string
  status: string
  itemsTotal?: number
  itemsDone?: number
  [k: string]: unknown
}

export interface RunItemView {
  nodeId: string
  status: string
  executor?: string
  leaseExpiresAt?: string
  [k: string]: unknown
}

export interface RunDetail {
  run: RunSummary
  items: RunItemView[]
}

export const STATUS_COLORS: Record<string, string> = {
  open: '#8b949e',
  in_progress: '#58a6ff',
  done: '#3fb950',
  blocked: '#f85149',
  skipped: '#d29922',
  archived: '#6e7681',
}

export const NEXT_TYPES: Record<NodeType, NodeType | null> = {
  epic: 'story',
  story: 'ticket',
  substory: 'ticket',
  ticket: 'subticket',
  subticket: null,
}
