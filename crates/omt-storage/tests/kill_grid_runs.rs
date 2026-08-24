//! THE run-plane kill-point grid (U5b): the journal vocabulary gained
//! RunInsert / RunPatch / ItemInsert, so run create/control/report now flow
//! through the SAME phased journal as node ops. For each canonical
//! run-plane op × every injectable ordinal: fire the fault, DROP the
//! storage handle (crash simulation), reopen with recovery, and assert FULL
//! convergence — run/item rows exactly once, journal acknowledged, events
//! appended once, recovery pruned, idempotent re-execution returning the
//! stored result. Idempotency semantics are IDENTICAL to the node grid by
//! construction (same operations table + input fingerprint path).

#[path = "common/mod.rs"]
mod common;

use common::*;
use omt_domain::store::FileStore;
use omt_domain::types::RunItemRow;
use omt_storage::journal::{report_item_patch, DbChange, FileOp, RunPatch};
use omt_storage::{store, FaultSchedule, FixedClock, PreparedMutation, Storage};

// ── shared harness ──────────────────────────────────────────────────────

/// Baseline identical to kill_grid.rs: two epics, one story, two tickets,
/// RUN-0001 holding both tickets (TICKET-0001 pending, TICKET-0002 running
/// under lease "sess-a").
struct Baseline {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
    clock: std::sync::Arc<FixedClock>,
}

fn seed_baseline() -> Baseline {
    let (dir, home) = temp_home();
    let clock = fixed_clock();
    let mut storage = open_storage(&home, &clock);

    let mk = |id: &str, ty: &str, title: &str, status: &str, path: String| {
        let mut n = node_row(id, ty, title, status, &path);
        n.created_at = iso_at(T0_MS);
        n.updated_at = iso_at(T0_MS);
        n
    };
    let epic = mk(
        "EPIC-0001",
        "epic",
        "root epic",
        "open",
        format!("tickets/EPIC-0001-{}/EPIC.md", slug("root epic")),
    );
    execute_simple(&mut storage, "seed-epic", &epic, None, "epic body");

    let story_dir = format!(
        "EPIC-0001-{}/STORY-0001-{}",
        slug("root epic"),
        slug("the story")
    );
    let story = mk(
        "STORY-0001",
        "story",
        "the story",
        "open",
        format!("tickets/{story_dir}/STORY.md"),
    );
    execute_simple(
        &mut storage,
        "seed-story",
        &story,
        Some(&epic),
        "story body",
    );

    for (id, title, status) in [
        ("TICKET-0001", "first ticket", "open"),
        ("TICKET-0002", "second ticket", "in_progress"),
    ] {
        let tdir = format!("{story_dir}/{}-{}/TICKET.md", id, slug(title));
        let ticket = mk(id, "ticket", title, status, format!("tickets/{tdir}"));
        execute_simple(
            &mut storage,
            &format!("seed-{id}"),
            &ticket,
            Some(&story),
            "body",
        );
    }

    store::set_meta(storage.conn(), "counter_EPIC", "1").expect("counter epic");
    store::set_meta(storage.conn(), "counter_STORY", "1").expect("counter story");
    store::set_meta(storage.conn(), "counter_TICKET", "2").expect("counter ticket");
    store::set_meta(storage.conn(), "counter_RUN", "0").expect("counter run");

    store::insert_run(storage.conn(), &run_row("RUN-0001", "running")).expect("insert run");
    store::insert_run_item(storage.conn(), &item_row("RUN-0001", "TICKET-0001", 0))
        .expect("item 1");
    let mut running = item_row("RUN-0001", "TICKET-0002", 1);
    running.state = "running".parse().unwrap();
    running.executor_session_id = Some("sess-a".to_string());
    running.attempts = 1;
    running.started_at = Some(iso_at(T0_MS));
    store::insert_run_item(storage.conn(), &running).expect("item 2");

    Baseline {
        _dir: dir,
        home,
        clock,
    }
}

fn run_changed_event(run_id: &str) -> (&'static str, serde_json::Value) {
    (
        "run.changed",
        serde_json::json!({ "kind": "run.changed", "ref": { "homeId": "grid-home", "runId": run_id } }),
    )
}

/// Build each run-plane op's plan against a fresh baseline.
fn build_plan(baseline: &Baseline, storage: &Storage, op: &str) -> PreparedMutation {
    let conn = storage.conn();
    match op {
        // RUN CREATE via the new vocabulary: counter bump + RunInsert +
        // two pending ItemInserts (FK-safe pass ordering) + stream event.
        "run_create" => {
            let mut run = run_row("RUN-0002", "pending");
            run.created_at = iso_at(T0_MS);
            let items = vec![
                item_row("RUN-0002", "TICKET-0001", 0),
                item_row("RUN-0002", "TICKET-0002", 1),
            ];
            storage
                .plan_run_create("grid-run-create", &run, &items, "grid-home")
                .expect("plan run create")
        }
        // RUN CONTROL start: RunPatch pending→running + event.
        "run_control_start" => {
            let patch = RunPatch {
                run_id: "RUN-0001".into(),
                set_status: Some("running".into()),
                set_finished_at: None,
                clear_finished_at: false,
            };
            storage.plan_run_mutation(
                "grid-control-start",
                "run_control",
                &["RUN-0001", "start"],
                vec![DbChange::RunPatch { patch }],
                vec![run_changed_event("RUN-0001")],
                serde_json::json!({ "runId": "RUN-0001", "status": "running" }),
            )
        }
        // REPORT failed WITH stop-on-failure: ItemPatch(failed) inside the
        // plan + RunPatch(paused) as an extra change — ONE mutation.
        "report_stop_on_failure" => {
            let item = store::get_run_item(conn, "RUN-0001", "TICKET-0002")
                .expect("item lookup")
                .expect("item exists");
            let patch = report_item_patch(&item, "failed", iso_at(T0_MS), Some("boom".into()));
            let stop_pause = RunPatch {
                run_id: "RUN-0001".into(),
                set_status: Some("paused".into()),
                set_finished_at: None,
                clear_finished_at: false,
            };
            storage
                .plan_report(
                    "grid-report-stop",
                    &baseline_ticket(baseline),
                    None,
                    None,
                    None,
                    patch,
                    vec![DbChange::RunPatch { patch: stop_pause }],
                )
                .expect("plan report stop")
        }
        // REPORT done deriving terminal completion: ItemPatch(done) +
        // RunPatch(completed, finished_at stamped) in the same mutation.
        "report_terminal_complete" => {
            // Park TICKET-0001 done first (outside the mutation) so the
            // reported one is the LAST non-final item.
            let mut parking =
                open_storage_armed(&baseline.home, &baseline.clock, FaultSchedule::never());
            let other = store::get_run_item(parking.conn(), "RUN-0001", "TICKET-0001")
                .expect("lookup")
                .expect("exists");
            let park_patch = report_item_patch(&other, "done", iso_at(T0_MS), None);
            let park_plan = parking
                .plan_report(
                    "grid-park-first",
                    &parking_ticket(baseline),
                    None,
                    Some("done".into()),
                    None,
                    park_patch,
                    vec![],
                )
                .expect("park plan");
            parking.execute(&park_plan).expect("park");
            drop(parking);

            let item = store::get_run_item(conn, "RUN-0001", "TICKET-0002")
                .expect("item lookup")
                .expect("item exists");
            let patch = report_item_patch(&item, "done", iso_at(T0_MS), None);
            let complete = RunPatch {
                run_id: "RUN-0001".into(),
                set_status: Some("completed_with_failures".into()),
                set_finished_at: Some(iso_at(T0_MS)),
                clear_finished_at: false,
            };
            storage
                .plan_report(
                    "grid-report-terminal",
                    &baseline_ticket(baseline),
                    None,
                    Some("done".into()),
                    None,
                    patch,
                    vec![DbChange::RunPatch { patch: complete }],
                )
                .expect("plan report terminal")
        }
        // REMOVE member: ItemDelete + terminal derivation RunPatch.
        "remove_item_terminal" => {
            let items_after: Vec<RunItemRow> = store::list_run_items(conn, "RUN-0001")
                .expect("items")
                .into_iter()
                .filter(|it| it.node_id != "TICKET-0002")
                .collect();
            assert_eq!(items_after.len(), 1, "simulation precondition");
            // With only the pending ticket left the run is NOT terminal;
            // make the simulation terminal by parking the survivor done.
            let mut changes = vec![DbChange::ItemDelete {
                run_id: "RUN-0001".into(),
                node_id: "TICKET-0002".into(),
            }];
            if !items_after.is_empty() {
                changes.push(DbChange::ItemPatch {
                    patch: omt_storage::journal::ItemPatch {
                        run_id: "RUN-0001".into(),
                        node_id: "TICKET-0001".into(),
                        set_state: Some("skipped".into()),
                        set_finished_at: Some(iso_at(T0_MS)),
                        ..omt_storage::ItemPatch::default()
                    },
                });
            }
            changes.push(DbChange::RunPatch {
                patch: RunPatch {
                    run_id: "RUN-0001".into(),
                    set_status: Some("completed".into()),
                    set_finished_at: Some(iso_at(T0_MS)),
                    clear_finished_at: false,
                },
            });
            storage.plan_run_mutation(
                "grid-remove-terminal",
                "run_control",
                &["RUN-0001", "remove", "TICKET-0002"],
                changes,
                vec![run_changed_event("RUN-0001")],
                serde_json::json!({ "removed": true }),
            )
        }
        other => panic!("unknown run-grid op {other}"),
    }
}

fn baseline_ticket(_b: &Baseline) -> omt_domain::types::NodeRow {
    node_row(
        "TICKET-0002",
        "ticket",
        "second ticket",
        "in_progress",
        "tickets/EPIC-0001-root-epic/STORY-0001-the-story/TICKET-0002-second-ticket/TICKET.md",
    )
}

fn parking_ticket(_b: &Baseline) -> omt_domain::types::NodeRow {
    node_row(
        "TICKET-0001",
        "ticket",
        "first ticket",
        "open",
        "tickets/EPIC-0001-root-epic/STORY-0001-the-story/TICKET-0001-first-ticket/TICKET.md",
    )
}

fn count_events(conn: &rusqlite::Connection, event_type: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE event_type = ?1",
        [event_type],
        |row| row.get(0),
    )
    .expect("event count")
}

/// One grid cell: fire the fault at EVERY ordinal of the plan, drop the
/// handle, reopen, and converge.
fn run_cell(op: &'static str) {
    // Determine the plan's ordinal span once on a clean copy.
    let probe = seed_baseline();
    let probe_storage = open_storage(&probe.home, &probe.clock);
    let probe_plan = build_plan(&probe, &probe_storage, op);
    let total_steps = probe_plan
        .files
        .iter()
        .map(FileOp::step_count)
        .sum::<usize>()
        + 2;
    drop(probe_storage);

    for ordinal in 0..total_steps {
        let baseline = seed_baseline();
        let mut storage =
            open_storage_armed(&baseline.home, &baseline.clock, FaultSchedule::at(ordinal));
        let plan = build_plan(&baseline, &storage, op);

        // Fire the fault at this ordinal; the injected IO problem is
        // EXPECTED — anything else fails the cell.
        let outcome = storage.execute_cancellable(&plan, &|| false);
        match &outcome {
            Err(problem) => {
                assert!(
                    problem.message.contains("injected kill-point"),
                    "[{op}@{ordinal}] unexpected problem: {problem}"
                );
            }
            Ok(_) => panic!("[{op}@{ordinal}] fault did not fire"),
        }
        // CRASH SIMULATION: drop every handle including the lock fd.
        drop(storage);

        // Recovery rolls the pending row forward deterministically.
        assert_converged_runs(&baseline.home, &baseline.clock, &plan, op, ordinal);
    }
}

/// Post-recovery convergence assertions for run-plane cells.
fn assert_converged_runs(
    home: &std::path::Path,
    clock: &std::sync::Arc<FixedClock>,
    plan: &PreparedMutation,
    op: &str,
    ordinal: usize,
) {
    let mut reopened = open_storage(home, clock);
    {
        let conn = reopened.conn();

        // Files: every Write target carries its planned final bytes (report
        // cells carry node-file writes).
        for file_op in &plan.files {
            if let FileOp::Write { path, content, .. } = file_op {
                let actual = reopened.files().read_optional(path).expect("read file");
                assert_eq!(
                    actual.as_deref(),
                    Some(content.as_str()),
                    "[{op}@{ordinal}] file {path} did not converge"
                );
            }
        }

        // Operations row exactly once; journal acknowledged.
        let committed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM operations WHERE command_id = ?1",
                [&plan.command_id],
                |row| row.get(0),
            )
            .expect("operations count");
        assert_eq!(
            committed, 1,
            "[{op}@{ordinal}] operations rows for {}",
            plan.command_id
        );
        let phase: String = conn
            .query_row(
                "SELECT phase FROM journal WHERE command_id = ?1",
                [&plan.command_id],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| "missing".to_string());
        assert_eq!(
            phase, "acknowledged",
            "[{op}@{ordinal}] journal not acknowledged after recovery"
        );

        // Recovery directory pruned.
        assert!(
            !reopened
                .files()
                .exists(&omt_storage::files::DiskFiles::recovery_dir_rel(
                    &plan.command_id
                )),
            "[{op}@{ordinal}] recovery dir not pruned"
        );

        // Op-specific durable-state expectations (exactly-once everywhere).
        match op {
            "run_create" => {
                let run = store::get_run(conn, "RUN-0002")
                    .expect("query")
                    .expect("run row exists once");
                assert_eq!(run.status.to_string(), "pending");
                let items = store::list_run_items(conn, "RUN-0002").expect("items");
                assert_eq!(items.len(), 2, "both members inserted exactly once");
                assert_eq!(
                    store::counter_value_of(conn, "RUN").unwrap(),
                    1,
                    "counter bumped exactly once"
                );
                assert_eq!(
                    count_events(conn, "run.changed"),
                    1,
                    "run.changed appended exactly once"
                );
            }
            "run_control_start" => {
                let run = store::get_run(conn, "RUN-0001")
                    .expect("query")
                    .expect("run");
                assert_eq!(run.status.to_string(), "running");
                assert_eq!(count_events(conn, "run.changed"), 1, "event exactly once");
            }
            "report_stop_on_failure" => {
                let item = store::get_run_item(conn, "RUN-0001", "TICKET-0002")
                    .expect("query")
                    .expect("item");
                assert_eq!(item.state.to_string(), "failed");
                assert_eq!(item.last_error.as_deref(), Some("boom"));
                let run = store::get_run(conn, "RUN-0001")
                    .expect("query")
                    .expect("run");
                assert_eq!(
                    run.status.to_string(),
                    "paused",
                    "stop-on-failure pause applied"
                );
                assert!(run.finished_at.is_none(), "pause leaves finished_at unset");
            }
            "report_terminal_complete" => {
                let item = store::get_run_item(conn, "RUN-0001", "TICKET-0002")
                    .expect("query")
                    .expect("item");
                assert_eq!(item.state.to_string(), "done");
                let run = store::get_run(conn, "RUN-0001")
                    .expect("query")
                    .expect("run");
                assert_eq!(
                    run.status.to_string(),
                    "completed_with_failures",
                    "terminal derivation applied"
                );
                assert_eq!(
                    run.finished_at.as_deref(),
                    Some(iso_at(T0_MS).as_str()),
                    "finished_at stamped exactly once"
                );
            }
            "remove_item_terminal" => {
                assert!(
                    store::get_run_item(conn, "RUN-0001", "TICKET-0002")
                        .expect("query")
                        .is_none(),
                    "member deleted exactly once"
                );
                let survivor = store::get_run_item(conn, "RUN-0001", "TICKET-0001")
                    .expect("query")
                    .expect("survivor");
                assert_eq!(survivor.state.to_string(), "skipped");
                let run = store::get_run(conn, "RUN-0001")
                    .expect("query")
                    .expect("run");
                assert_eq!(run.status.to_string(), "completed");
            }
            other => panic!("unknown convergence check {other}"),
        }
    } // end immutable-borrow scope

    // Idempotency: re-executing the SAME command returns the stored result
    // without applying anything again (identical semantics to node grid).
    let replay = reopened.execute(plan).expect("replay after recovery");
    assert_eq!(
        replay, plan.result,
        "[{op}@{ordinal}] stored result returned on replay"
    );
    // And state is unchanged by the replay (still converged).
    let conn = reopened.conn();
    match op {
        "run_create" => assert_eq!(
            store::list_run_items(conn, "RUN-0002")
                .expect("items")
                .len(),
            2,
            "replay duplicated nothing"
        ),
        "report_terminal_complete" => assert_eq!(
            store::get_run(conn, "RUN-0001")
                .expect("q")
                .expect("run")
                .status
                .to_string(),
            "completed_with_failures"
        ),
        _ => {}
    }
}

#[test]
fn grid_run_create_converges_from_every_ordinal() {
    run_cell("run_create");
}

#[test]
fn grid_run_control_start_converges_from_every_ordinal() {
    run_cell("run_control_start");
}

#[test]
fn grid_report_stop_on_failure_converges_from_every_ordinal() {
    run_cell("report_stop_on_failure");
}

#[test]
fn grid_report_terminal_complete_converges_from_every_ordinal() {
    run_cell("report_terminal_complete");
}

#[test]
fn grid_remove_item_terminal_converges_from_every_ordinal() {
    run_cell("remove_item_terminal");
}
