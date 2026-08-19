/**
 * OMT domain model: node types, hierarchy rules, statuses, and shared shapes.
 * Hierarchy: epic → story → [substory] → ticket → [subticket]; the bracketed
 * levels are optional, each at most one level deep (enforced by HIERARCHY).
 */

export const NODE_TYPES = ['epic', 'story', 'substory', 'ticket', 'subticket'] as const
export type NodeType = (typeof NODE_TYPES)[number]

export const STATUSES = ['open', 'in_progress', 'done'] as const
export type Status = (typeof STATUSES)[number]

/** Legal child types per parent type. Root creation is allowed for epic only. */
export const HIERARCHY: Readonly<Record<NodeType, readonly NodeType[]>> = {
  epic: ['story'],
  story: ['substory', 'ticket'],
  substory: ['ticket'],
  ticket: ['subticket'],
  subticket: [],
}

/** ID prefix per type; ids look like `EPIC-0001`, counters independent per type. */
export const TYPE_PREFIX: Readonly<Record<NodeType, string>> = {
  epic: 'EPIC',
  story: 'STORY',
  substory: 'SUBSTORY',
  ticket: 'TICKET',
  subticket: 'SUBTICKET',
}

export const ID_PATTERN = /^(EPIC|STORY|SUBSTORY|TICKET|SUBTICKET)-(\d{4,})$/

/** One node row as stored in SQLite `nodes` (metadata authority). */
export interface OmtNode {
  readonly id: string
  readonly type: NodeType
  readonly title: string
  readonly status: Status
  /** Archive is a separate dimension: the lifecycle status is preserved. */
  readonly archived: boolean
  readonly priority: number
  /** Markdown file path relative to OMT home, e.g. `tickets/EPIC-0001-x/epic.md`. */
  readonly path: string
  readonly created_at: string
  readonly updated_at: string
}

/** One parent→child relation row as stored in SQLite `edges`. */
export interface OmtEdge {
  readonly parent_id: string
  readonly child_id: string
  readonly ord: number
}

/** Tree projection assembled from nodes + edges. */
export interface OmtTreeNode extends OmtNode {
  readonly children: OmtTreeNode[]
}

/** Frontmatter attributes persisted inside the node Markdown file. */
export interface NodeFrontmatter {
  id: string
  type: NodeType
  title: string
  status: Status
  archived?: boolean
  priority: number
  parent?: string
  created_at: string
  updated_at: string
}

/** Error with a stable machine-readable code for tool/API surfaces. */
export class OmtError extends Error {
  constructor(
    readonly code: 'NOT_FOUND' | 'INVALID_HIERARCHY' | 'INVALID_INPUT' | 'CONFLICT' | 'IO',
    message: string,
  ) {
    super(message)
    this.name = 'OmtError'
  }
}

export function isNodeType(value: unknown): value is NodeType {
  return typeof value === 'string' && (NODE_TYPES as readonly string[]).includes(value)
}

export function isStatus(value: unknown): value is Status {
  return typeof value === 'string' && (STATUSES as readonly string[]).includes(value)
}
