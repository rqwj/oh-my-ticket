//! Property tests over the pure decision functions (plan U3): exhaustive
//! transition-matrix legality, concurrency validation, trust-gate narrowness,
//! lease-fenced report authorization, and an LCG-randomized sweep-equivalence
//! property against a session-membership oracle (ratified U3 decision 1).
//!
//! These pin the DECISION layer independently of the corpus; the corpus pins
//! the SEQUENCING. Together they make divergence from the TS leg detectable.

use std::collections::BTreeSet;

use omt_contracts::{RunItemState, RunStatus};
use omt_domain::janitor::{plan_sweep, SweepRun};
use omt_domain::ports::{LeaseGrant, LeaseTable, MemoryLeases};
use omt_domain::runs::ReportVerdict;
use omt_domain::runs::{
    authorize_report, derive_terminal, trust_gate_gates, validate_concurrency, ReportAuthority,
};
use omt_domain::types::*;

/// Deterministic LCG so failures reproduce exactly.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Lcg {
        Lcg(seed.wrapping_mul(2685821657736338717).wrapping_add(1))
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

const ALL_ITEM_STATES: [RunItemState; 8] = [
    RunItemState::Pending,
    RunItemState::Running,
    RunItemState::AwaitingConfirmation,
    RunItemState::Done,
    RunItemState::Failed,
    RunItemState::Blocked,
    RunItemState::Skipped,
    RunItemState::Interrupted,
];

const ALL_RUN_STATUSES: [RunStatus; 7] = [
    RunStatus::Pending,
    RunStatus::Running,
    RunStatus::Paused,
    RunStatus::Interrupted,
    RunStatus::Completed,
    RunStatus::CompletedWithFailures,
    RunStatus::Canceled,
];

// ── 1. exhaustive transition matrices (verbatim vs core.ts 147–176) ─────

#[test]
fn item_transition_matrix_matches_frozen_table() {
    // (from → allowed targets), copied from core.ts ITEM_TRANSITIONS.
    let expected: Vec<(RunItemState, Vec<RunItemState>)> = vec![
        (
            RunItemState::Pending,
            vec![
                RunItemState::Running,
                RunItemState::Done,
                RunItemState::Blocked,
                RunItemState::Skipped,
            ],
        ),
        (
            RunItemState::Running,
            vec![
                RunItemState::Done,
                RunItemState::Failed,
                RunItemState::Blocked,
                RunItemState::Skipped,
                RunItemState::AwaitingConfirmation,
                RunItemState::Interrupted,
            ],
        ),
        (
            RunItemState::AwaitingConfirmation,
            vec![
                RunItemState::Done,
                RunItemState::Failed,
                RunItemState::Blocked,
                RunItemState::Skipped,
                RunItemState::Running,
                RunItemState::Interrupted,
            ],
        ),
        (RunItemState::Done, vec![]),
        (RunItemState::Failed, vec![]),
        (RunItemState::Blocked, vec![]),
        (RunItemState::Skipped, vec![]),
        (RunItemState::Interrupted, vec![]),
    ];
    for (from, targets) in &expected {
        for to in ALL_ITEM_STATES {
            let allowed = item_transition_allowed(*from, to);
            assert_eq!(
                allowed,
                targets.contains(&to),
                "item {from:?} → {to:?}: got {allowed}, want {}",
                targets.contains(&to)
            );
        }
    }
}

#[test]
fn run_transition_matrix_matches_frozen_table() {
    let expected: Vec<(RunStatus, Vec<RunStatus>)> = vec![
        (
            RunStatus::Pending,
            vec![RunStatus::Running, RunStatus::Canceled],
        ),
        (
            RunStatus::Running,
            vec![
                RunStatus::Paused,
                RunStatus::Canceled,
                RunStatus::Completed,
                RunStatus::CompletedWithFailures,
                RunStatus::Interrupted,
            ],
        ),
        (
            RunStatus::Paused,
            vec![
                RunStatus::Running,
                RunStatus::Canceled,
                RunStatus::Completed,
                RunStatus::CompletedWithFailures,
                RunStatus::Interrupted,
            ],
        ),
        (
            RunStatus::Interrupted,
            vec![RunStatus::Running, RunStatus::Canceled],
        ),
        (RunStatus::Completed, vec![]),
        (RunStatus::CompletedWithFailures, vec![RunStatus::Running]),
        (RunStatus::Canceled, vec![]),
    ];
    for (from, targets) in &expected {
        for to in ALL_RUN_STATUSES {
            let allowed = run_transition_allowed(*from, to);
            assert_eq!(
                allowed,
                targets.contains(&to),
                "run {from:?} → {to:?}: got {allowed}, want {}",
                targets.contains(&to)
            );
        }
    }
}

// ── 2. terminal derivation property ─────────────────────────────────────

fn item(state: RunItemState) -> RunItemRow {
    let mut row = RunItemRow::new("RUN-0001", "TICKET-0001", 0, state);
    row.finished_at = is_run_item_final(state).then(|| "2026-08-19T00:00:00.000Z".to_string());
    row
}

/// For running|paused runs with every item final: completed iff no failure
/// state present, else completed_with_failures. Any non-final item ⇒ None.
/// Other run statuses never derive here.
#[test]
fn terminal_derivation_is_exact_over_all_state_combinations() {
    for status in [RunStatus::Running, RunStatus::Paused] {
        for first in ALL_ITEM_STATES {
            for second in ALL_ITEM_STATES {
                let items = vec![item(first), item(second)];
                let derived = derive_terminal(status, &items);
                let all_final = is_run_item_final(first) && is_run_item_final(second);
                if !all_final {
                    assert_eq!(derived, None, "{first:?}+{second:?} must not derive");
                    continue;
                }
                let any_failure = is_run_item_failure(first) || is_run_item_failure(second);
                let want = Some(if any_failure {
                    RunStatus::CompletedWithFailures
                } else {
                    RunStatus::Completed
                });
                assert_eq!(derived, want, "{first:?}+{second:?}");
            }
        }
    }
    // Non-active statuses never participate in derivation.
    for status in [
        RunStatus::Pending,
        RunStatus::Interrupted,
        RunStatus::Completed,
        RunStatus::CompletedWithFailures,
        RunStatus::Canceled,
    ] {
        let all_done = vec![item(RunItemState::Done)];
        assert_eq!(derive_terminal(status, &all_done), None);
    }
    // Empty runs derive only when started (running/paused + zero items).
    assert_eq!(
        derive_terminal(RunStatus::Running, &[]),
        Some(RunStatus::Completed)
    );
    assert_eq!(
        derive_terminal(RunStatus::Paused, &[]),
        Some(RunStatus::Completed)
    );
    assert_eq!(derive_terminal(RunStatus::Pending, &[]), None);
}

// ── 3. concurrency validation ───────────────────────────────────────────

use serde_json::json;

/// Exactly positive integers pass — floats, negatives, zero, numeric
/// strings, nulls and booleans are INVALID_CONCURRENCY (TICKET-0055 R9).
#[test]
fn concurrency_validation_accepts_exactly_positive_integers() {
    for value in [json!(1), json!(2), json!(64), json!(9007199254740992i64)] {
        validate_concurrency(&value)
            .unwrap_or_else(|error| panic!("{value} must be accepted: {error}"));
    }
    for value in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!(2.5),
        json!("3"),
        json!(""),
        serde_json::Value::Null,
        json!(true),
        json!(false),
        json!([]),
        json!({}),
    ] {
        let problem = validate_concurrency(&value).expect_err(&format!("{value} must be rejected"));
        assert_eq!(problem.code, "INVALID_CONCURRENCY", "{value}");
        let details = problem.details.expect("details carry the raw value");
        assert_eq!(
            details.get("value"),
            Some(&value),
            "details echo the raw value"
        );
    }
}

// ── 4. trust gate narrowness (ratified decision 5) ──────────────────────

const AUTO_VERIFY: [bool; 2] = [false, true];
const OBSERVERS: [Option<&str>; 3] = [Some("session-a"), Some("session-b"), None];
const EXECUTORS: [Option<&str>; 3] = [Some("session-a"), Some("session-b"), None];

/// Gates iff: autoVerify=false AND reported=false AND observer==item executor
/// AND item state==running AND that executor exists. awaiting_confirmation
/// NEVER gates again (repeated bare dones cannot bypass confirmation).
#[test]
fn trust_gate_gates_exactly_the_narrow_predicate() {
    for state in ALL_ITEM_STATES {
        for reported in [true, false] {
            for auto_verify in AUTO_VERIFY {
                for observer in OBSERVERS {
                    for executor in EXECUTORS {
                        let gated =
                            trust_gate_gates(state, reported, observer, executor, auto_verify);
                        let want = !reported
                            && !auto_verify
                            && state == RunItemState::Running
                            && observer.is_some()
                            && observer == executor;
                        assert_eq!(
                            gated, want,
                            "state={state:?} reported={reported} auto={auto_verify} observer={observer:?} executor={executor:?}"
                        );
                    }
                }
            }
        }
    }
}

// ── 5. lease-fenced report authorization (R10) ──────────────────────────

fn grant(attempt: i64, expires_at: i64) -> LeaseGrant {
    LeaseGrant {
        token: format!("lease-session-a-{attempt}"),
        attempt,
        principal: "session-a".into(),
        expires_at,
    }
}

#[test]
fn stale_or_mismatched_leases_are_denied_with_distinct_rules() {
    let now = 10_000;
    let cases: Vec<(&str, Option<LeaseGrant>, i64)> = vec![
        ("lease-stale", Some(grant(1, 9_000)), 1), // expired at now
        ("lease-attempt", Some(grant(1, 20_000)), 2), // attempt mismatch
        ("lease-stale", None, 1),                  // nothing issued
    ];
    for (rule, lease_entry, attempt) in cases {
        let presented = lease_entry.clone();
        let verdict = authorize_report(
            RunItemState::Running,
            attempt,
            presented.as_ref(),
            &ReportAuthority::ExecutorLease {
                token: "lease-session-a-1".into(),
                actor: "session-a".into(),
            },
            now,
            Some("lease-session-a-1"),
        )
        .expect_err(&format!("{rule} must deny"));
        assert_eq!(verdict.code, "CONFLICT");
        assert_eq!(
            verdict
                .details
                .as_ref()
                .and_then(|d| d.get("rule"))
                .and_then(|r| r.as_str()),
            Some(rule),
        );
    }

    // Token fencing: presenting a different token than the stored grant's
    // denies via lease-token even though the grant itself is alive.
    let stored = Some(grant(1, 20_000));
    let problem = authorize_report(
        RunItemState::Running,
        1,
        stored.as_ref(),
        &ReportAuthority::ExecutorLease {
            token: "forged-token".into(),
            actor: "session-a".into(),
        },
        now,
        Some("lease-session-a-1"),
    )
    .expect_err("forged token must be denied");
    assert_eq!(
        problem
            .details
            .as_ref()
            .and_then(|d| d.get("rule"))
            .and_then(|r| r.as_str()),
        Some("lease-token")
    );

    // Valid grant authorizes the executor path…
    let valid = Some(grant(1, 20_000));
    match authorize_report(
        RunItemState::Running,
        1,
        valid.as_ref(),
        &ReportAuthority::ExecutorLease {
            token: "lease-session-a-1".into(),
            actor: "session-a".into(),
        },
        now,
        Some("lease-session-a-1"),
    ) {
        Ok(authorized) => assert!(matches!(authorized, ReportVerdict::ExecutorAuthorized)),
        Err(problem) => panic!("valid lease denied: {problem}"),
    }

    // …and the administrator path bypasses EVERYTHING (distinct route).
    for lease_entry in [None, Some(grant(7, 1))] {
        match authorize_report(
            RunItemState::Running,
            99,
            lease_entry.as_ref(),
            &ReportAuthority::Administrator {
                reason: "human decision".into(),
            },
            now,
            None,
        ) {
            Ok(authorized) => {
                assert!(matches!(authorized, ReportVerdict::AdminAuthorized))
            }
            Err(problem) => panic!("admin override denied: {problem}"),
        }
    }

    // A foreign principal behind the same lease shape is actor-mismatch.
    let held = Some(grant(1, 20_000));
    let problem = authorize_report(
        RunItemState::Running,
        1,
        held.as_ref(),
        &ReportAuthority::ExecutorLease {
            token: "lease-session-a-1".into(),
            actor: "session-z".into(),
        },
        now,
        Some("lease-session-a-1"),
    )
    .expect_err("foreign actor must be denied");
    assert_eq!(
        problem
            .details
            .as_ref()
            .and_then(|d| d.get("rule"))
            .and_then(|r| r.as_str()),
        Some("actor-mismatch")
    );

    // Non-running item state denies via the report-state gate FIRST.
    let held = Some(grant(1, 20_000));
    let problem = authorize_report(
        RunItemState::Pending,
        1,
        held.as_ref(),
        &ReportAuthority::ExecutorLease {
            token: "lease-session-a-1".into(),
            actor: "session-a".into(),
        },
        now,
        Some("lease-session-a-1"),
    )
    .expect_err("pending item must not accept reports");
    assert_eq!(
        problem
            .details
            .as_ref()
            .and_then(|d| d.get("rule"))
            .and_then(|r| r.as_str()),
        Some("report-state-gate")
    );
}

// ── 6. sweep equivalence against the membership oracle (decision 1/2) ───

const STATUSES: [RunStatus; 3] = [
    RunStatus::Running,
    RunStatus::Paused,
    RunStatus::Interrupted,
];
const STATES: [RunItemState; 8] = ALL_ITEM_STATES;

struct Fleet {
    runs: Vec<SweepRun>,
    /// Per item: Option(executor_session_id) — None = legacy undefined.
    executors: Vec<Vec<Option<String>>>,
    attempts: Vec<Vec<i64>>,
}

fn random_fleet(rng: &mut Lcg) -> Fleet {
    let run_total = rng.below(4) as usize;
    let mut fleet = Fleet {
        runs: Vec::new(),
        executors: Vec::new(),
        attempts: Vec::new(),
    };
    for run_index in 0..run_total {
        let status = STATUSES[rng.below(STATUSES.len() as u64) as usize];
        let item_count = rng.below(4) as usize;
        let mut rows = Vec::new();
        let mut executors = Vec::new();
        let mut attempts = Vec::new();
        for position in 0..item_count {
            let state = STATES[rng.below(STATES.len() as u64) as usize];
            let mut row = RunItemRow::new(
                &format!("RUN-{run_index:04}"),
                &format!("TICKET-{run_index:04}-{position:04}"),
                position as i64,
                state,
            );
            row.attempts = rng.below(3) as i64;
            attempts.push(row.attempts);
            let has_executor = rng.below(4) > 0;
            row.executor_session_id = has_executor.then(|| format!("session-{}", rng.below(3)));
            executors.push(row.executor_session_id.clone());
            rows.push(row);
        }
        fleet.runs.push(SweepRun {
            run: RunRow {
                id: format!("RUN-{run_index:04}"),
                title: None,
                status,
                config: RunConfigValue {
                    stop_on_failure: false,
                    auto_continue: true,
                    auto_verify: false,
                    concurrency: 1,
                },
                created_at: "2026-08-19T00:00:00.000Z".into(),
                finished_at: None,
            },
            items: rows,
        });
        fleet.executors.push(executors);
        fleet.attempts.push(attempts);
    }
    fleet
}

/// The TS-leg oracle: liveness == session-membership of RUNNING items'
/// executors (undefined executors are always orphaned).
fn oracle_live<'a>(
    fleet: &'a Fleet,
    live_sessions: &'a BTreeSet<String>,
) -> impl Fn(&str, i64) -> bool + 'a {
    move |session_id: &str, _attempt: i64| {
        let executing_now = fleet.runs.iter().any(|sweep_run| {
            sweep_run.run.status == RunStatus::Running
                && sweep_run.items.iter().any(|row| {
                    row.state == RunItemState::Running
                        && row.executor_session_id.as_deref() == Some(session_id)
                })
        });
        executing_now || live_sessions.contains(session_id)
    }
}

fn plan_signature(plan: &omt_domain::janitor::SweepPlan) -> String {
    format!(
        "demotions={:?} interrupted_items={:?} derived={:?} interrupted_runs={:?}",
        plan.demotions,
        plan.interrupted_items
            .iter()
            .map(|row| (row.run_id.clone(), row.node_id.clone()))
            .collect::<Vec<_>>(),
        plan.derived,
        plan.interrupted_runs,
    )
}

/// For randomized fleets: lease-based liveness (MemoryLeases seeded so each
/// RUNNING item's executor holds an unexpired attempt-fenced grant iff the
/// oracle says so) produces EXACTLY the SweepPlan the membership oracle does;
/// undefined-executor items always demote; paused runs keep status unless
/// derivation applies (two-pass ordering).
#[test]
fn sweep_plan_is_equivalent_under_lease_and_membership_liveness() {
    let now_iso = "2026-08-19T00:01:30.000Z";
    let now_ms = 90_000i64;
    for seed in 0..200u64 {
        let mut rng = Lcg::new(seed);
        let fleet = random_fleet(&mut rng);

        // Random live-session set for the oracle.
        let mut live_sessions = BTreeSet::new();
        for index in 0..3 {
            if rng.below(2) == 0 {
                live_sessions.insert(format!("session-{index}"));
            }
        }

        // Seed MemoryLeases to MATCH the oracle exactly: a session holds an
        // unexpired far-future grant iff the oracle considers it live.
        let mut leases = MemoryLeases::new();
        leases.set_now_ms(now_ms);
        let oracle = oracle_live(&fleet, &live_sessions);
        for sweep_run in &fleet.runs {
            for row in &sweep_run.items {
                if let Some(session_id) = &row.executor_session_id {
                    if oracle(session_id, row.attempts) {
                        leases.issue(
                            session_id,
                            LeaseGrant {
                                token: format!("lease-{session_id}-{}", row.attempts),
                                attempt: row.attempts,
                                principal: session_id.clone(),
                                expires_at: i64::MAX / 4,
                            },
                        );
                    } else {
                        leases.expire(session_id);
                    }
                }
            }
        }

        let by_leases = plan_sweep(
            &fleet.runs,
            |session, attempt| leases.lease_alive(session, attempt),
            now_iso,
        )
        .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        let by_oracle = plan_sweep(&fleet.runs, |session, _| oracle(session, 0), now_iso)
            .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        assert_eq!(
            plan_signature(&by_leases),
            plan_signature(&by_oracle),
            "seed {seed}: lease liveness diverges from membership oracle"
        );

        // Decision 1 clause: undefined-executor RUNNING items ALWAYS demote.
        for sweep_run in &fleet.runs {
            if sweep_run.run.status != RunStatus::Running {
                continue;
            }
            for row in &sweep_run.items {
                if row.state == RunItemState::Running && row.executor_session_id.is_none() {
                    assert!(
                        by_leases
                            .demotions
                            .contains(&(sweep_run.run.id.clone(), row.node_id.clone())),
                        "seed {seed}: undefined-executor running item {}/{} must demote",
                        sweep_run.run.id,
                        row.node_id
                    );
                }
            }
        }

        // Structural invariants of the two passes:
        // (a) interrupted_items contains ONLY this sweep's demotions;
        for (_, node_id) in &by_leases.demotions {
            assert!(
                by_leases
                    .interrupted_items
                    .iter()
                    .any(|row| &row.node_id == node_id),
                "demoted item {node_id} missing from interrupted_items"
            );
        }
        // (b) derived runs are exactly the candidates that ended fully-final;
        for (run_id, terminal) in &by_leases.derived {
            let sweep_run = fleet.runs.iter().find(|sr| &sr.run.id == run_id).unwrap();
            assert!(
                terminal == &RunStatus::Completed || terminal == &RunStatus::CompletedWithFailures
            );
            assert!(
                sweep_run.items.iter().all(|row| {
                    is_run_item_final(row.state)
                        || by_leases
                            .demotions
                            .contains(&(sweep_run.run.id.clone(), row.node_id.clone()))
                }),
                "derived run {run_id} had unfinished items"
            );
            // A derived run is never also interrupted (pass-2 ordering).
            assert!(
                !by_leases.interrupted_runs.contains(run_id),
                "run {run_id} both derived and interrupted"
            );
        }
        // (c) a candidate never appears BOTH derived and interrupted.
        for run_id in &by_leases.interrupted_runs {
            assert!(
                !by_leases
                    .derived
                    .iter()
                    .any(|(derived_id, _)| derived_id == run_id),
                "run {run_id} both derived and interrupted (ordering violated)"
            );
        }
    }
}
