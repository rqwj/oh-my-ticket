/**
 * OMT domain model: node types, hierarchy rules, statuses, and shared shapes.
 * Hierarchy: epic → story → [substory] → ticket → [subticket]; the bracketed
 * levels are optional, each at most one level deep (enforced by HIERARCHY).
 */

export const NODE_TYPES = ['epic', 'story', 'substory', 'ticket', 'subticket'] as const
export type NodeType = (typeof NODE_TYPES)[number]

/** Only task-bearing nodes execute inside a run; hierarchy containers are context. */
export const RUN_MEMBER_NODE_TYPES = ['ticket', 'subticket'] as const
export type RunMemberNodeType = (typeof RUN_MEMBER_NODE_TYPES)[number]

export function isRunMemberNodeType(type: NodeType): type is RunMemberNodeType {
  return (RUN_MEMBER_NODE_TYPES as readonly NodeType[]).includes(type)
}

/**
 * Node lifecycle statuses. `blocked`/`skipped` (EPIC-0003 decision 4):
 * blocked = cannot continue due to an external condition (resumable once it
 * clears); skipped = deliberately or necessarily skipped. `stopped` was
 * ruled out in round-2 review (no production path, no item mapping).
 */
export const STATUSES = ['open', 'in_progress', 'done', 'blocked', 'skipped'] as const
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

/**
 * Run grouping for UI surfaces (TICKET-0067/0068): history runs fold into
 * the collapsed 历史 group; active runs are the 加入-run picker targets.
 * `interrupted` is neither — resumable but needing human review, it stays
 * ungrouped in the main list and accepts no new members.
 */
export const RUN_HISTORY_STATUSES: readonly RunStatus[] = ['completed', 'completed_with_failures', 'canceled']
export const RUN_ACTIVE_STATUSES: readonly RunStatus[] = ['pending', 'running', 'paused']

export function isRunHistory(status: RunStatus): boolean {
  return (RUN_HISTORY_STATUSES as readonly string[]).includes(status)
}

export function isRunActive(status: RunStatus): boolean {
  return (RUN_ACTIVE_STATUSES as readonly string[]).includes(status)
}

/** Item states that count as "finished" for run terminal derivation. */
export const RUN_ITEM_FINAL_STATES: readonly RunItemState[] = ['done', 'failed', 'blocked', 'skipped', 'interrupted']

/** Item states that mark the run outcome as not fully successful. */
export const RUN_ITEM_FAILURE_STATES: readonly RunItemState[] = ['failed', 'blocked', 'interrupted']

/**
 * Continuation-nudge budget (TICKET-0062 / EPIC-0003 decision 5): the idle
 * hook nudges one pending item at most this many times (exponential
 * backoff); afterwards the item reads as stalled until a human retries it.
 */
export const NUDGE_BUDGET = 3

export interface RunConfig {
  /** Item failed → run pauses for a human decision (blocked/skipped do not trigger). */
  stopOnFailure: boolean
  /** Idle hook may nudge the executor to continue the next pending item. */
  autoContinue: boolean
  /**
   * Trust policy (TICKET-0064): when false, a RUNNING item completed by its
   * own executor session through a bare omt_update (no omt_run_report)
   * lands in awaiting_confirmation for a human to confirm/reject; when
   * true the completion lands done directly. Explicit reports are always
   * trusted, regardless of this flag.
   */
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

export function isRunItemState(value: unknown): value is RunItemState {
  return typeof value === 'string' && (RUN_ITEM_STATES as readonly string[]).includes(value)
}

/**
 * In-flight item states: actively executed (running) or awaiting human
 * confirmation (awaiting_confirmation). Paused runs let only in-flight
 * items advance; in-flight items cannot be removed and accept reports.
 */
export function isRunItemInFlight(state: RunItemState): boolean {
  return state === 'running' || state === 'awaiting_confirmation'
}

/**
 * Stalled convention (TICKET-0062): no dedicated item state — a pending item
 * whose nudge budget is exhausted IS the stalled marker. UI/query surfaces
 * derive it here; `retryItem` (budget reset) is the way back.
 */
export function isRunItemStalled(item: Pick<OmtRunItem, 'state' | 'nudge_count'>): boolean {
  return item.state === 'pending' && item.nudge_count >= NUDGE_BUDGET
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
