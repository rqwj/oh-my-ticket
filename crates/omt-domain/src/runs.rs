//! Run/item decision functions — the pure core of the EPIC-0003 run state
//! machine (`src/host/core.ts` + `store.ts` claim logic).
//!
//! Everything here is a total function over row snapshots: no clocks, no
//! files, no leases. The core orchestrator feeds snapshots in and applies
//! returned plans, which keeps transition legality unit-testable as
//! properties (see `tests/properties.rs`).

use super::error::{Problem, Result};
use super::types::*;
use omt_contracts::{RunItemState, RunStatus};

/// Result of one atomic claim: the claimed item plus unexecutable skips
/// drained in the same transaction.
#[derive(Debug, Clone, Default)]
pub struct ClaimNextResult {
    pub claimed: Option<RunItemRow>,
    /// Pending archived or legacy container members marked skipped while
    /// draining the queue.
    pub skipped: Vec<RunItemRow>,
}

/// Terminal derivation (EPIC-0003 decision 7): once every item is final,
/// all done/skipped → completed; any failed/interrupted/blocked →
/// completed_with_failures. Runs have no failed. Returns `None` while the
/// run cannot derive (non running/paused status or non-final items remain).
pub fn derive_terminal(status: RunStatus, items: &[RunItemRow]) -> Option<RunStatus> {
    if !matches!(status, RunStatus::Running | RunStatus::Paused) {
        return None;
    }
    if items.iter().any(|item| !is_run_item_final(item.state)) {
        return None;
    }
    let failed = items.iter().any(|item| is_run_item_failure(item.state));
    Some(if failed { RunStatus::CompletedWithFailures } else { RunStatus::Completed })
}

/// Trust gate (TICKET-0064) — ratified U3 semantics decision 5: the gate is
/// NARROW. Only the executing principal's OWN bare `done` on its own RUNNING
/// item gates a waiting item:
/// - reports (`reported = true`) are always trusted;
/// - other sessions' and agent-less updates land done directly;
/// - autoVerify runs skip the gate entirely;
/// - repeated bare dones never bypass: once an item sits in
///   awaiting_confirmation, only an explicit report may finish it.
pub fn trust_gate_gates(
    item_state: RunItemState,
    reported: bool,
    observer_session: Option<&str>,
    item_executor_session: Option<&str>,
    auto_verify: bool,
) -> bool {
    if reported {
        return false;
    }
    // Repeated bare done over an already-gated item never bypasses.
    if item_state == RunItemState::AwaitingConfirmation {
        return false;
    }
    item_state == RunItemState::Running
        && observer_session.is_some()
        && observer_session == item_executor_session.as_deref()
        && !auto_verify
}

/// Concurrency validation (`INVALID_CONCURRENCY`, U2 subdivision): must be a
/// positive integer. Accepts JSON numbers only — `"3"` strings and booleans
/// fail like `Number.isInteger` would.
pub fn validate_concurrency(value: &serde_json::Value) -> Result<i64> {
    let raw = value.clone();
    let number = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()));
    match number {
        Some(concurrency) if concurrency >= 1 => Ok(concurrency),
        _ => Err(Problem::with_details(
            super::error::INVALID_CONCURRENCY,
            format!(
                "concurrency must be a positive integer, got {}",
                match &raw {
                    serde_json::Value::String(text) => text.clone(),
                    serde_json::Value::Null => "null".to_string(),
                    other => other.to_string(),
                }
            ),
            |d| {
                d.insert("value".into(), raw);
            },
        )),
    }
}

// ── lease-fenced report authorization (R10 decision layer) ──────────────

use super::ports::LeaseGrant;

/// Who is trying to land a report on an in-flight item.
#[derive(Debug, Clone)]
pub enum ReportAuthority {
    /// The executor presenting its claim lease (token + actor namespace).
    ExecutorLease { token: String, actor: String },
    /// Human administration (audited, non-delegable): takes a DISTINCT
    /// authorization path that never consults lease state.
    Administrator { reason: String },
}

/// Authorization verdict for one report attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportVerdict {
    /// Authorized through the executor's own live lease.
    ExecutorAuthorized,
    /// Authorized through audited administration (distinct path).
    AdminAuthorized,
}

/// Lease-fenced report authorization for one in-flight item.
///
/// Denial rules (each a structured CONFLICT with a `rule` detail so the
/// registry stays untouched):
/// - not in flight → `report-state-gate`;
/// - administrator override → allowed regardless of lease state, returning
///   [`ReportVerdict::AdminAuthorized`] so callers can audit it distinctly;
/// - missing/expired grant → `lease-stale`;
/// - wrong attempt (stale attempt-one token against attempt-two work) →
///   `lease-attempt`;
/// - wrong token → `lease-token`;
/// - foreign actor namespace → `actor-mismatch`.
pub fn authorize_report(
    item_state: RunItemState,
    item_attempts: i64,
    grant: Option<&LeaseGrant>,
    authority: &ReportAuthority,
    now_ms: i64,
    expected_token: Option<&str>,
) -> Result<ReportVerdict> {
    if !is_run_item_in_flight(item_state) {
        return Err(Problem::with_details(
            super::error::CONFLICT,
            format!("only in-flight items can report ({item_state})"),
            |d| {
                d.insert("rule".into(), "report-state-gate".into());
                d.insert("itemState".into(), item_state.to_string().into());
                d.insert(
                    "required".into(),
                    serde_json::json!(["running", "awaiting_confirmation"]),
                );
            },
        ));
    }
    match authority {
        ReportAuthority::Administrator { .. } => Ok(ReportVerdict::AdminAuthorized),
        ReportAuthority::ExecutorLease { token, actor } => {
            let Some(grant) = grant else {
                return Err(conflict_rule("lease-stale", "no live lease holds this item"));
            };
            if now_ms >= grant.expires_at {
                return Err(conflict_rule("lease-stale", "the claiming lease has expired"));
            }
            if grant.attempt != item_attempts {
                return Err(conflict_rule(
                    "lease-attempt",
                    format!("lease attempt {} is stale; item is at attempt {item_attempts}", grant.attempt),
                ));
            }
            if let Some(expected_token) = expected_token {
                if expected_token != token {
                    return Err(conflict_rule("lease-token", "presented token does not fence this item"));
                }
            }
            if grant.principal != *actor {
                return Err(conflict_rule(
                    "actor-mismatch",
                    format!("actor {actor} does not belong to lease principal {}", grant.principal),
                ));
            }
            Ok(ReportVerdict::ExecutorAuthorized)
        }
    }
}

fn conflict_rule(rule: &str, message: impl Into<String>) -> Problem {
    Problem::with_details(super::error::CONFLICT, message, |d| {
        d.insert("rule".into(), rule.into());
    })
}
