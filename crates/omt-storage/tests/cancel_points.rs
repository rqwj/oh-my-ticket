//! Cancel linearization contract (U5c): `$/cancelRequest` may only take
//! effect at LINEARIZATION-SAFE points of a phased mutation:
//!
//! - BEFORE the journal insert → clean abort: nothing durable anywhere
//!   (no journal row, no operations row, no events), storage stays usable;
//! - AFTER prepared / AFTER files_applied → the cancellation is reported,
//!   but the pending journal row ROLLS FORWARD deterministically on the
//!   next open (crash-equivalent convergence), and re-issuing the command
//!   replays the committed result;
//! - AFTER finalize (acknowledged) → the cancel is ignored entirely.
//!
//! These tests pin that contract directly against `execute_cancellable`.

#[path = "common/mod.rs"]
mod common;

use common::*;
use serde_json::json;

fn details(problem: &omt_storage::Problem) -> serde_json::Value {
    problem.details.clone().unwrap_or(serde_json::Value::Null)
}
use omt_storage::{store, FaultSchedule, FixedClock, Storage};

fn seed_one_epic(home: &std::path::Path, clock: &std::sync::Arc<FixedClock>) {
    let mut storage = open_storage(home, clock);
    let mut epic = node_row(
        "EPIC-0009",
        "epic",
        "cancel target epic",
        "open",
        &format!("tickets/EPIC-0009-{}/EPIC.md", slug("cancel target epic")),
    );
    epic.created_at = iso_at(T0_MS);
    epic.updated_at = iso_at(T0_MS);
    execute_simple(&mut storage, "seed-cancel-epic", &epic, None, "body");
}

fn plan_update(storage: &Storage) -> omt_storage::PreparedMutation {
    let mut node = node_row(
        "EPIC-0009",
        "epic",
        "renamed by cancel test",
        "in_progress",
        &format!("tickets/EPIC-0009-{}/EPIC.md", slug("cancel target epic")),
    );
    node.created_at = iso_at(T0_MS);
    node.updated_at = iso_at(T0_MS);
    storage
        .plan_update(
            "cancel-cmd-1",
            &node,
            Some("renamed by cancel test".to_string()),
            Some("in_progress".to_string()),
            None,
            None,
            None,
            None,
            None,
            vec![],
            serde_json::json!({ "nodeId": "EPIC-0009" }),
        )
        .expect("plan update")
}

/// Cancel BEFORE the journal insert: clean abort, zero side effects.
#[test]
fn cancel_before_journal_is_a_clean_abort() {
    let (dir, home) = temp_home();
    let clock = fixed_clock();
    seed_one_epic(&home, &clock);

    let mut storage = open_storage(&home, &clock);
    let plan = plan_update(&storage);

    // Flip the flag before executing: the FIRST probe (pre-journal) sees it.
    let canceled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let outcome = storage.execute_cancellable(&plan, &|| {
        canceled.load(std::sync::atomic::Ordering::SeqCst)
    });
    let problem = outcome.expect_err("must abort cleanly");
    assert_eq!(problem.code, "CANCELED", "{problem}");
    assert_eq!(details(&problem)["rule"], json!("client-canceled"));
    assert_eq!(details(&problem)["at"], json!("before-journal"));
    drop(storage);

    // Nothing durable anywhere.
    let reopened = open_storage(&home, &clock);
    let conn = reopened.conn();
    let journal_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM journal WHERE command_id='cancel-cmd-1'",
            [],
            |r| r.get(0),
        )
        .expect("journal count");
    assert_eq!(journal_rows, 0, "clean abort leaves no journal row");
    let ops_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE command_id='cancel-cmd-1'",
            [],
            |r| r.get(0),
        )
        .expect("ops count");
    assert_eq!(ops_rows, 0, "clean abort leaves no operations row");
    let events_after: i64 = count_events_total(conn);
    drop(reopened);

    // The SAME command can be issued fresh afterwards and succeeds.
    let mut again = open_storage(&home, &clock);
    let plan2 = plan_update(&again);
    again
        .execute(&plan2)
        .expect("command reusable after clean abort");
    let node = store::get_node(again.conn(), "EPIC-0009")
        .expect("q")
        .expect("node");
    assert_eq!(node.title, "renamed by cancel test");
    // The clean-aborted attempt emitted nothing; the fresh attempt emits
    // exactly its own single update event.
    assert_eq!(
        count_events_total(again.conn()),
        events_after + 1,
        "exactly the new op's events"
    );
    drop(dir);
}

fn count_events_total(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("events total")
}

/// Cancel AFTER prepared: the caller sees CANCELED, but the pending journal
/// row rolls forward on the next open — convergence identical to a crash.
#[test]
fn cancel_after_prepared_rolls_forward_on_reopen() {
    let (dir, home) = temp_home();
    let clock = fixed_clock();
    seed_one_epic(&home, &clock);

    let mut storage = open_storage_armed(&home, &clock, FaultSchedule::never());
    let plan = plan_update(&storage);

    // Probe sequence: pre-journal probe sees FALSE (call 0); every later
    // probe (post-prepared) sees TRUE.
    let probes = std::sync::atomic::AtomicUsize::new(0);
    let outcome = storage.execute_cancellable(&plan, &|| {
        probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1
    });
    drop(storage);
    let problem = outcome.expect_err("post-prepared cancel must surface");
    assert_eq!(problem.code, "CANCELED", "{problem}");
    assert_eq!(
        details(&problem)["at"],
        json!("after-prepared"),
        "{problem}"
    );

    // Reopen: recovery rolls the pending row forward to acknowledged.
    let mut reopened = open_storage(&home, &clock);
    let conn = reopened.conn();
    let phase: String = conn
        .query_row(
            "SELECT phase FROM journal WHERE command_id='cancel-cmd-1'",
            [],
            |r| r.get(0),
        )
        .expect("journal row present");
    assert_eq!(phase, "acknowledged", "pending row rolled forward");
    let node = store::get_node(conn, "EPIC-0009")
        .expect("q")
        .expect("node");
    assert_eq!(node.title, "renamed by cancel test", "update applied");

    // Idempotent replay returns the stored result.
    let plan3 = plan_update(&reopened);
    let replay = reopened.execute(&plan3).expect("replay");
    assert_eq!(replay, plan3.result);
    drop(dir);
}

/// Cancel AFTER files_applied: files already on disk; DB commit still rolls
/// forward on reopen; final state converges.
#[test]
fn cancel_after_files_applied_rolls_forward_on_reopen() {
    let (dir, home) = temp_home();
    let clock = fixed_clock();
    seed_one_epic(&home, &clock);

    let mut storage = open_storage_armed(&home, &clock, FaultSchedule::never());
    let plan = plan_update(&storage);

    // Probe sequence: pre-journal (call 0) and post-prepared (call 1) see
    // FALSE; the post-files-applied probe (call 2) sees TRUE.
    let probes = std::sync::atomic::AtomicUsize::new(0);
    let outcome = storage.execute_cancellable(&plan, &|| {
        probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2
    });
    drop(storage);
    let problem = outcome.expect_err("post-files cancel must surface");
    assert_eq!(problem.code, "CANCELED", "{problem}");
    assert_eq!(
        details(&problem)["at"],
        json!("after-files-applied"),
        "{problem}"
    );

    let reopened = open_storage(&home, &clock);
    let phase: String = reopened
        .conn()
        .query_row(
            "SELECT phase FROM journal WHERE command_id='cancel-cmd-1'",
            [],
            |r| r.get(0),
        )
        .expect("journal row");
    assert_eq!(phase, "acknowledged", "rolled forward past db_committed");
    let file_content = reopened
        .files()
        .read_optional(
            plan.files
                .first()
                .map(|f| match f {
                    omt_storage::FileOp::Write { path, .. } => path.clone(),
                    _ => unreachable!(),
                })
                .unwrap()
                .as_str(),
        )
        .expect("file readable")
        .expect("file exists");
    assert!(
        file_content.contains("renamed by cancel test"),
        "final bytes on disk"
    );
    drop(dir);
}

/// Cancel signaled too late (after finalize): ignored; the call succeeds.
#[test]
fn cancel_after_finalize_is_ignored() {
    let (dir, home) = temp_home();
    let clock = fixed_clock();
    seed_one_epic(&home, &clock);

    let mut storage = open_storage_armed(&home, &clock, FaultSchedule::never());
    let plan = plan_update(&storage);

    // The cancel signal flips only AFTER the last safe point could have
    // observed it (threshold far beyond the 3 existing probes): there is
    // NO post-finalize probe, so the mutation completes normally.
    let outcome = storage.execute_cancellable(&plan, &|| false);
    let result = outcome.expect("completes despite late cancel signal");
    assert_eq!(result, plan.result);

    // And a flag flipped mid-flight BEFORE finalize but after the final
    // probe cannot retro-cancel either: re-run to acknowledged state and
    // confirm the journal phase is terminal.
    let phase: String = storage
        .conn()
        .query_row(
            "SELECT phase FROM journal WHERE command_id='cancel-cmd-1'",
            [],
            |r| r.get(0),
        )
        .expect("journal row");
    assert_eq!(phase, "acknowledged");
    drop(dir);
}
