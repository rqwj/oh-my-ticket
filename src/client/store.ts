/**
 * Client state types shared by the controller and components. All state lives
 * in the apply closure's snapshot stores (hooks compartment route — the
 * registrant-private reactive fact channel); no module-level handles.
 */

/** One tree node as delivered by the /omt tree endpoint. */
export interface OmtTreeNode {
  readonly id: string
  readonly type: 'epic' | 'story' | 'substory' | 'ticket' | 'subticket'
  readonly title: string
  readonly status: 'open' | 'in_progress' | 'done' | 'blocked' | 'skipped'
  /** Archive is a separate dimension from the lifecycle status. */
  readonly archived: boolean
  readonly priority: number
  readonly path: string
  readonly created_at: string
  readonly updated_at: string
  readonly children: readonly OmtTreeNode[]
}

export interface NodeSummary {
  readonly id: string
  readonly type: OmtTreeNode['type']
  readonly title: string
  readonly status: OmtTreeNode['status']
  readonly archived: boolean
  readonly priority: number
}

export interface DocData {
  /** OMT home of the owning core (absolute path copy). */
  readonly home?: string
  /** Present while a session is executing this ticket. */
  readonly running?: { readonly sessionId: string; readonly sessionLabel: string; readonly since: string }
  readonly node: OmtTreeNode
  readonly parent?: NodeSummary
  readonly ancestors?: readonly NodeSummary[]
  readonly children: readonly NodeSummary[]
  readonly body: string
  /** Every non-terminal run holding this ticket (run links, TICKET-0068). */
  readonly runs?: readonly DocRunLink[]
}

export type TreeState =
  | { readonly status: 'idle' }
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly forest: readonly OmtTreeNode[] }
  | { readonly status: 'error'; readonly message: string }

export type DocState =
  | { readonly status: 'idle' }
  | { readonly status: 'loading'; readonly id: string }
  | { readonly status: 'ready'; readonly data: DocData }
  | { readonly status: 'error'; readonly id: string; readonly message: string }

/** The active ticket fact shown in the composer dock strip. */
export interface ActiveInfo {
  readonly id: string
  readonly title: string
  readonly status: OmtTreeNode['status']
  readonly priority: number
}

// ── run view values (STORY-0013) ─────────────────────────────────────────
// Structural copies of the /omt run endpoint payloads (src/host/rpc.ts);
// the client bundle must not import host modules, so shapes are re-declared.

export type RunStatus = 'pending' | 'running' | 'paused' | 'completed' | 'completed_with_failures' | 'canceled' | 'interrupted'
export type RunItemState = 'pending' | 'running' | 'done' | 'failed' | 'blocked' | 'skipped' | 'interrupted' | 'awaiting_confirmation'

export interface RunProgress {
  readonly total: number
  readonly pending: number
  readonly running: number
  readonly done: number
  readonly failed: number
  readonly blocked: number
  readonly skipped: number
  readonly interrupted: number
  readonly awaiting_confirmation: number
}

/** One run row as delivered by the /omt run-list endpoint. */
export interface RunSummary {
  readonly id: string
  readonly title?: string
  readonly status: RunStatus
  /** 加入-run picker eligibility (TICKET-0067): pending/running/paused. */
  readonly active: boolean
  /** Folds into the 历史 group (TICKET-0068); interrupted stays in the main list. */
  readonly history: boolean
  readonly created_at: string
  readonly finished_at?: string
  readonly progress: RunProgress
  /** Pending items whose 续跑 nudge budget is exhausted (TICKET-0062). */
  readonly stalled: number
}

/** Executor lineage snapshot on a run item (父会话 ↳ subagent, TICKET-0066). */
export interface RunExecutor {
  readonly sessionId: string
  readonly label: string
  readonly parentSessionId?: string
  readonly isSubagent?: boolean
}

/** One run-detail item row as delivered by the /omt run-show endpoint. */
export interface RunItemView {
  readonly node_id: string
  readonly position: number
  readonly state: RunItemState
  readonly attempts: number
  readonly last_error?: string
  /** Derived host-side: pending with the nudge budget exhausted. */
  readonly stalled?: boolean
  readonly started_at?: string
  readonly finished_at?: string
  readonly executor?: RunExecutor
  readonly node?: { readonly id: string; readonly title: string; readonly status: OmtTreeNode['status']; readonly archived: boolean }
}

/** Run config, read-only in the UI (advanced config stays model-side). */
export interface RunConfigView {
  readonly stopOnFailure: boolean
  readonly autoContinue: boolean
  readonly autoVerify: boolean
  readonly concurrency: number
}

export interface RunDetailData {
  readonly run: RunSummary & { readonly config: RunConfigView }
  readonly items: readonly RunItemView[]
}

export type RunListState =
  | { readonly status: 'idle' }
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly runs: readonly RunSummary[] }
  | { readonly status: 'error'; readonly message: string }

export type RunDetailState =
  | { readonly status: 'idle' }
  | { readonly status: 'loading'; readonly id: string }
  | { readonly status: 'ready'; readonly id: string; readonly data: RunDetailData }
  | { readonly status: 'error'; readonly id: string; readonly message: string }

/** Panel section: the ticket tree vs the peer Runs 区块 (TICKET-0068). */
export type PanelSection = 'tickets' | 'runs'

/** Join-run picker state (TICKET-0067): several active runs → user picks. */
export interface RunPickerState {
  readonly nodeId: string
  /** Non-terminal runs only (interrupted excluded — resume first). */
  readonly options: readonly RunSummary[]
}

/**
 * Transient result notice (join counts / host errors). `key`+`params` render
 * through the locale; `text` carries raw host messages (already localized).
 */
export interface Notice {
  readonly kind: 'ok' | 'error'
  readonly key?: 'run.noticeAdded' | 'run.noticeCreated'
  readonly params?: Record<string, unknown>
  readonly text?: string
}

/** One non-terminal run holding a ticket (get.runs member, TICKET-0068/0070). */
export interface DocRunLink {
  readonly id: string
  readonly title?: string
  readonly status: RunStatus
  /** The ticket's item state inside this run (awaiting_confirmation 标识). */
  readonly itemState: RunItemState
  readonly progress: RunProgress
}

/** Run-dimension hint on an SSE change event (additive, TICKET-0071). */
export interface OmtRunChangeHint {
  readonly id: string
  readonly kind: 'run' | 'item'
  readonly nodeId?: string
}
