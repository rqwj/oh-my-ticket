//! Domain row model mirroring `src/host/types.ts` + the SQLite row shapes of
//! `src/host/store.ts` (the behavioral specification ported by U3).
//!
//! Enum vocabulary (`NodeType`, `NodeStatus`, `RunStatus`, `RunItemState`,
//! `RunReportOutcome`, `RunControlAction`) is REUSED from `omt-contracts`
//! (generated from `schema/common.schema.json`) instead of redefined — the
//! schema documents are the single source of truth (KTD2/R3). The row
//! structs stay local: the corpus compares snake_case DB-row projections
//! (`node_id`, `created_at`, …), which are the store's shape, not the wire
//! DTOs' camelCase shape. Mapping rows → protocol Views is a later unit's
//! concern.

// Re-export the schema-generated vocabulary so every domain module (and
// downstream consumers) shares one set of enum definitions.
pub use omt_contracts::{
    NodeStatus, NodeType, RunControlAction, RunItemState, RunReportOutcome, RunStatus,
};
use serde::Serialize;

// ── hierarchy ───────────────────────────────────────────────────────────

/// Legal child types per parent type (`HIERARCHY` in types.ts). Root
/// creation is allowed for epic only.
pub fn hierarchy_children(parent: NodeType) -> &'static [NodeType] {
    match parent {
        NodeType::Epic => &[NodeType::Story],
        NodeType::Story => &[NodeType::Substory, NodeType::Ticket],
        NodeType::Substory => &[NodeType::Ticket],
        NodeType::Ticket => &[NodeType::Subticket],
        NodeType::Subticket => &[],
    }
}

pub fn hierarchy_allows(parent: NodeType, child: NodeType) -> bool {
    hierarchy_children(parent).contains(&child)
}

/// ID prefix per type (`TYPE_PREFIX`); counters are independent per type.
pub fn type_prefix(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::Epic => "EPIC",
        NodeType::Story => "STORY",
        NodeType::Substory => "SUBSTORY",
        NodeType::Ticket => "TICKET",
        NodeType::Subticket => "SUBTICKET",
    }
}

pub fn prefix_type(prefix: &str) -> Option<NodeType> {
    match prefix {
        "EPIC" => Some(NodeType::Epic),
        "STORY" => Some(NodeType::Story),
        "SUBSTORY" => Some(NodeType::Substory),
        "TICKET" => Some(NodeType::Ticket),
        "SUBTICKET" => Some(NodeType::Subticket),
        _ => None,
    }
}

/// `^(EPIC|STORY|SUBSTORY|TICKET|SUBTICKET)-(\d{4,})$`
pub fn id_matches_pattern(id: &str) -> bool {
    let Some((prefix, digits)) = id.split_once('-') else {
        return false;
    };
    prefix_type(prefix).is_some()
        && !digits.is_empty()
        && digits.len() >= 4
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Only task-bearing nodes execute inside a run (`RUN_MEMBER_NODE_TYPES`);
/// hierarchy containers are context only.
pub fn is_run_member_node_type(t: NodeType) -> bool {
    matches!(t, NodeType::Ticket | NodeType::Subticket)
}

// ── nodes ───────────────────────────────────────────────────────────────

/// One node row as stored in SQLite `nodes` (metadata authority).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeRow {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    pub status: NodeStatus,
    /// Archive is a separate dimension: the lifecycle status is preserved.
    pub archived: bool,
    pub priority: i64,
    /// Markdown path relative to the OMT home.
    pub path: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One parent→child relation row as stored in SQLite `edges`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeRow {
    pub parent_id: String,
    pub child_id: String,
    pub ord: i64,
}

/// Tree projection assembled from nodes + edges.
#[derive(Debug, Clone, Serialize)]
pub struct TreeNodeRow {
    #[serde(flatten)]
    pub node: NodeRow,
    pub children: Vec<TreeNodeRow>,
}

// ── runs (EPIC-0003) ────────────────────────────────────────────────────

/// Continuation-nudge budget (TICKET-0062): a pending item at/above this
/// many nudges reads as stalled until a human retries it.
pub const NUDGE_BUDGET: i64 = 3;

/// Execution configuration of one run (`DEFAULT_RUN_CONFIG`: stopOnFailure
/// false, autoContinue true, autoVerify false, concurrency 1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunConfigValue {
    #[serde(rename = "stopOnFailure")]
    pub stop_on_failure: bool,
    #[serde(rename = "autoContinue")]
    pub auto_continue: bool,
    #[serde(rename = "autoVerify")]
    pub auto_verify: bool,
    #[serde(rename = "concurrency")]
    pub concurrency: i64,
}

impl Default for RunConfigValue {
    fn default() -> Self {
        RunConfigValue {
            stop_on_failure: false,
            auto_continue: true,
            auto_verify: false,
            concurrency: 1,
        }
    }
}

/// One row of `runs` (DB-only; no markdown representation).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunRow {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: RunStatus,
    pub config: RunConfigValue,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// One row of `run_items`; `(run_id, node_id)` is the primary key.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunItemRow {
    pub run_id: String,
    pub node_id: String,
    pub position: i64,
    pub state: RunItemState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor_session_id: Option<String>,
    pub attempts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudged_at: Option<String>,
    pub nudge_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

impl RunItemRow {
    pub fn new(run_id: &str, node_id: &str, position: i64, state: RunItemState) -> Self {
        RunItemRow {
            run_id: run_id.to_string(),
            node_id: node_id.to_string(),
            position,
            state,
            executor_session_id: None,
            attempts: 0,
            last_error: None,
            nudged_at: None,
            nudge_count: 0,
            started_at: None,
            finished_at: None,
        }
    }
}

// ── transition tables (verbatim from core.ts) ───────────────────────────

/// Legal run transitions. `interrupted` is not absolute (resume reopens it);
/// `completed_with_failures` reopens only via row-level retry; `canceled`
/// and `completed` are absolute terminals.
pub fn run_transition_allowed(from: RunStatus, to: RunStatus) -> bool {
    use RunStatus::*;
    match from {
        Pending => matches!(to, Running | Canceled),
        Running => matches!(
            to,
            Paused | Canceled | Completed | CompletedWithFailures | Interrupted
        ),
        Paused => matches!(
            to,
            Running | Canceled | Completed | CompletedWithFailures | Interrupted
        ),
        Interrupted => matches!(to, Running | Canceled),
        Completed => false,
        CompletedWithFailures => matches!(to, Running),
        Canceled => false,
    }
}

/// Legal direct item transitions. Terminal-ish states have no direct exits —
/// they move only through dedicated retry/replay paths. Pending allows
/// done/blocked/skipped so passive observation can map direct ticket sets
/// onto not-yet-dispatched items without wedging the run.
pub fn item_transition_allowed(from: RunItemState, to: RunItemState) -> bool {
    use RunItemState::*;
    match from {
        Pending => matches!(to, Running | Done | Blocked | Skipped),
        Running => matches!(
            to,
            Done | Failed | Blocked | Skipped | AwaitingConfirmation | Interrupted
        ),
        // awaiting_confirmation → interrupted is the 打回 rejection path.
        AwaitingConfirmation => matches!(
            to,
            Done | Failed | Blocked | Skipped | Running | Interrupted
        ),
        Done | Failed | Blocked | Skipped | Interrupted => false,
    }
}

/// Item states that count as "finished" for terminal derivation.
pub fn is_run_item_final(state: RunItemState) -> bool {
    use RunItemState::*;
    matches!(state, Done | Failed | Blocked | Skipped | Interrupted)
}

/// Item states marking the run outcome as not fully successful.
pub fn is_run_item_failure(state: RunItemState) -> bool {
    use RunItemState::*;
    matches!(state, Failed | Blocked | Interrupted)
}

/// In-flight states: actively executed or awaiting human confirmation.
/// Paused runs let only in-flight items advance; in-flight items cannot be
/// removed and accept reports.
pub fn is_run_item_in_flight(state: RunItemState) -> bool {
    matches!(
        state,
        RunItemState::Running | RunItemState::AwaitingConfirmation
    )
}

/// Stalled convention (TICKET-0062): pending + exhausted nudge budget IS the
/// stalled marker; there is no dedicated state.
pub fn is_run_item_stalled(state: RunItemState, nudge_count: i64) -> bool {
    state == RunItemState::Pending && nudge_count >= NUDGE_BUDGET
}

/// Active runs accept members (`RUN_ACTIVE_STATUSES`): pending/running/paused.
pub fn is_run_active(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Pending | RunStatus::Running | RunStatus::Paused
    )
}

/// History runs fold into the collapsed 历史 group.
pub fn is_run_history(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed | RunStatus::CompletedWithFailures | RunStatus::Canceled
    )
}
