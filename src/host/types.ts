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

// ── runs (EPIC-0003) ───────────────────────────────────────────────────
// Runs are DB-only (no markdown files): run_items is the sole membership
// authority (snapshot semantics; no live link to tree nodes).

export const RUN_STATUSES = ['pending', 'running', 'paused', 'completed', 'completed_with_failures', 'canceled', 'interrupted'] as const
export type RunStatus = (typeof RUN_STATUSES)[number]

export const RUN_ITEM_STATES = ['pending', 'running', 'done', 'failed', 'blocked', 'skipped', 'interrupted', 'awaiting_confirmation'] as const
export type RunItemState = (typeof RUN_ITEM_STATES)[number]

/** Item states that count as "finished" for run terminal derivation. */
export const RUN_ITEM_FINAL_STATES: readonly RunItemState[] = ['done', 'failed', 'blocked', 'skipped', 'interrupted']

/** Item states that mark the run outcome as not fully successful. */
export const RUN_ITEM_FAILURE_STATES: readonly RunItemState[] = ['failed', 'blocked', 'interrupted']

export interface RunConfig {
  /** Item failed → run pauses for a human decision (blocked/skipped do not trigger). */
  stopOnFailure: boolean
  /** Idle hook may nudge the executor to continue the next pending item. */
  autoContinue: boolean
  /** Trust policy: hook-observed completions land directly in done. */
  autoVerify: boolean
  /** Reserved for P3 concurrent execution. */
  concurrency: number
}

export const DEFAULT_RUN_CONFIG: RunConfig = {
  stopOnFailure: false,
  autoContinue: true,
  autoVerify: false,
  concurrency: 1,
}

/** One row of `runs` (DB-only; home is implied by the database file). */
export interface OmtRun {
  readonly id: string
  /** Optional human label for multi-run pickers; falls back to id. */
  readonly title?: string
  readonly status: RunStatus
  readonly config: RunConfig
  readonly created_at: string
  readonly finished_at?: string
}

/** One row of `run_items`; (run_id, node_id) is the primary key. */
export interface OmtRunItem {
  readonly run_id: string
  readonly node_id: string
  readonly position: number
  readonly state: RunItemState
  readonly executor_session_id?: string
  readonly attempts: number
  /** Kept across retries (no attempt history table). */
  readonly last_error?: string
  /** Idle-hook nudge budget bookkeeping (TICKET-0062). */
  readonly nudged_at?: string
  readonly nudge_count: number
  readonly started_at?: string
  readonly finished_at?: string
}

export function isRunStatus(value: unknown): value is RunStatus {
  return typeof value === 'string' && (RUN_STATUSES as readonly string[]).includes(value)
}

export function isRunItemState(value: unknown): value is RunItemState {
  return typeof value === 'string' && (RUN_ITEM_STATES as readonly string[]).includes(value)
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
