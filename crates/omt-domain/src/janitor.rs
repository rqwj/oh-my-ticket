//! Startup janitor — two-pass sweep over orphaned in-flight work, ported
//! from `OmtCore::janitorSweep` (`src/host/core.ts`).
//!
//! ## Ratified U3 semantics decisions (authoritative for this module)
//!
//! 1. **Liveness is lease-based.** The TypeScript core decides liveness by
//!    `executor_session_id ∈ activeSessions` snapshots. Rust replaces this
//!    with real leases: an item is live iff it holds an unexpired lease
//!    bound to its attempt ([`super::ports::LeaseTable::lease_alive`]).
//!    Corpus ops keep the same scenario JSON — `openSession`/`closeSession`
//!    map to issueLease(expiry=long)/expireLease at the port layer
//!    ([`super::ports::MemoryLeases::mark_exclusive`]). Items whose executor
//!    is undefined are ALWAYS demoted (unchanged from TS).
//! 2. **Sweep is two-pass.** Pass one demotes items across ALL running|paused
//!    runs first; pass two derives terminal state (all-final → derived)
//!    BEFORE falling back to interrupted. Paused runs keep their status
//!    unless derivation applies. The order is normative: deriving before
//!    interrupting is what lets a demotion that finishes the last item seal
//!    a run as completed(_with_failures) instead of wedging it interrupted.
//! 3. **Claim event order** (implemented in `core.rs`, recorded here because
//!    it is part of the ratified set): claim emits [pending→skipped] events
//!    for drained members BEFORE the claimed item's [pending→running].
//! 4. **Observation ignores fully-terminal runs** (also implemented in
//!    `core.rs`): they derive + seal before late reports can arrive.
//! 5. **Trust gate is narrow** (see [`super::runs::trust_gate_gates`] docs):
//!    only the executing principal's own bare `done` on its own RUNNING item
//!    gates; repeated bare dones never bypass.
//! 6. **Seeding without an explicit id consumes the RUN counter**
//!    (corpus `seedRun` parity): implemented by the store's `next_run_id`
//!    allocation inside the seed path.

use super::error::Result;
use super::runs::derive_terminal;
use super::types::*;
use omt_contracts::{RunItemState, RunStatus};

/// One candidate run snapshot with its items at sweep time.
#[derive(Debug, Clone)]
pub struct SweepRun {
    pub run: RunRow,
    pub items: Vec<RunItemRow>,
}

/// The mutation plan produced by pass one + pass two. Applying it is the
/// orchestrator's job (demotions write rows directly and emit NO item
/// events; derived/interrupted statuses go through validated status writes).
#[derive(Debug, Clone, Default)]
pub struct SweepPlan {
    /// Pass one: `(run_id, node_id)` pairs demoted running → interrupted.
    pub demotions: Vec<(String, String)>,
    /// Post-demotion item snapshots (state=interrupted, finished_at set),
    /// reported as `interruptedItems`.
    pub interrupted_items: Vec<RunItemRow>,
    /// Pass two: runs whose terminal state was DERIVED (status change).
    pub derived: Vec<(String, RunStatus)>,
    /// Pass two: running runs left without live work → interrupted.
    pub interrupted_runs: Vec<String>,
}

/// Plan the two-pass sweep over `candidates` (running | paused runs only,
/// ordered by id — the caller passes them pre-ordered from the store).
///
/// * `live`* — the lease predicate: `(executor_session_id, item_attempt) ->
///   bool`. Items with no executor never count as live.
///
/// Purity notes:
/// - Pass one reads each item ONCE (the TS loop iterates the snapshot list
///   per run and re-reads rows only for reporting), so folding demotions
///   into local copies reproduces the store state exactly.
/// - Pass two re-derives from post-demotion state: any remaining running
///   item has a live executor and blocks both derivation and interruption;
///   otherwise derivation wins over interruption (decision 2); only then
///   does a still-`running` snapshot fall to interrupted. Paused runs stay
///   paused unless derived (human-controlled already).
pub fn plan_sweep(
    candidates: &[SweepRun],
    live: impl Fn(&str, i64) -> bool,
    now: &str,
) -> Result<SweepPlan> {
    let mut plan = SweepPlan::default();

    // ── pass one: demote orphaned running items across every candidate ──
    let mut folded: Vec<SweepRun> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let mut run_snapshot = candidate.clone();
        for item in run_snapshot.items.iter_mut() {
            if item.state != RunItemState::Running {
                continue;
            }
            let executor_live = match &item.executor_session_id {
                Some(session_id) => live(session_id, item.attempts),
                // Undefined executor → always demoted (decision 1).
                None => false,
            };
            if executor_live {
                continue;
            }
            item.state = RunItemState::Interrupted;
            item.finished_at = Some(now.to_string());
            plan.demotions.push((candidate.run.id.clone(), item.node_id.clone()));
            plan.interrupted_items.push(item.clone());
        }
        folded.push(run_snapshot);
    }

    // ── pass two: derive terminal first, then interrupt running runs ──
    for snapshot in &folded {
        if snapshot.items.iter().any(|item| item.state == RunItemState::Running) {
            continue; // still actively executed work
        }
        // The demotion may have finished the last in-flight item: derive the
        // terminal state instead of interrupting. Applies to paused runs too
        // — skipping them would silently stall a paused run whose last
        // running item was just demoted.
        if let Some(terminal) = derive_terminal(snapshot.run.status, &snapshot.items) {
            plan.derived.push((snapshot.run.id.clone(), terminal));
            continue;
        }
        if snapshot.run.status != RunStatus::Running {
            continue; // paused runs keep their status unless derived
        }
        plan.interrupted_runs.push(snapshot.run.id.clone());
    }

    // `interruptedItems` mirrors the TS result: exactly the items demoted by
    // THIS sweep's pass one, in (run id, position, node) order.
    Ok(plan)
}
