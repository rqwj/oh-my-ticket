//! THE kill-point grid (plan U4 / AE2): every injectable step × every
//! canonical operation {create, update, move(2 files), archive, report}.
//! For each cell: seed a baseline home, build the op's plan, fire the fault
//! at step k, DROP the storage handle (crash simulation), reopen with
//! recovery, and assert FULL convergence:
//!   - every planned file's final bytes are on disk (or moved),
//!   - DB rows match the expected final state,
//!   - the operations row exists once, journal phase = acknowledged,
//!   - outbox events were appended exactly once,
//!   - the recovery directory was pruned,
//!   - re-executing the same command returns the stored result (idempotency).

#[path = "common/mod.rs"]
mod common;

use common::*;
use omt_domain::store::FileStore;
use omt_domain::types::NodeRow;
use omt_storage::journal::{report_item_patch, DbChange, FileOp};
use omt_storage::store;
use omt_storage::{FaultSchedule, FixedClock, ItemPatch, PreparedMutation, Problem, Storage};

// ── grid harness ────────────────────────────────────────────────────────

/// One seeded baseline home: EPIC-0001 root + STORY-0001 child +
/// TICKET-0001/0002 under the story + a second epic EPIC-0002 +
/// RUN-0001 holding both tickets.
struct Baseline {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
    clock: std::sync::Arc<FixedClock>,
    epic1: NodeRow,
    epic2: NodeRow,
    story: NodeRow,
    ticket1: NodeRow,
    ticket2: NodeRow,
}

fn seed_baseline() -> Baseline {
    let (dir, home) = temp_home();
    let clock = fixed_clock();
    let mut storage = open_storage(&home, &clock);

    let mk = |id: &str, ty: &str, title: &str, path: String| node_row(id, ty, title, "open", &path);
    let epic1 = mk(
        "EPIC-0001",
        "epic",
        "root epic",
        format!("tickets/EPIC-0001-{}/EPIC.md", slug("root epic")),
    );
    let epic2 = mk(
        "EPIC-0002",
        "epic",
        "other epic",
        format!("tickets/EPIC-0002-{}/EPIC.md", slug("other epic")),
    );
    execute_simple(&mut storage, "seed-epic1", &epic1, None, "epic one body");
    execute_simple(&mut storage, "seed-epic2", &epic2, None, "epic two body");

    let story_dir = format!(
        "EPIC-0001-{}/STORY-0001-{}",
        slug("root epic"),
        slug("the story")
    );
    let story = mk(
        "STORY-0001",
        "story",
        "the story",
        format!("tickets/{story_dir}/STORY.md"),
    );
    execute_simple(
        &mut storage,
        "seed-story",
        &story,
        Some(&epic1),
        "story body",
    );

    let t1_dir = format!("{story_dir}/TICKET-0001-{}", slug("first ticket"));
    let ticket1 = {
        let mut n = mk(
            "TICKET-0001",
            "ticket",
            "first ticket",
            format!("tickets/{t1_dir}/TICKET.md"),
        );
        n.status = "open".parse().unwrap();
        n
    };
    execute_simple(
        &mut storage,
        "seed-t1",
        &ticket1,
        Some(&story),
        "ticket one body",
    );

    let t2_dir = format!("{story_dir}/TICKET-0002-{}", slug("second ticket"));
    let mut ticket2 = mk(
        "TICKET-0002",
        "ticket",
        "second ticket",
        format!("tickets/{t2_dir}/TICKET.md"),
    );
    ticket2.status = "in_progress".parse().unwrap();
    execute_simple(
        &mut storage,
        "seed-t2",
        &ticket2,
        Some(&story),
        "ticket two body",
    );

    // Seed counters past every allocated id (pool-sync semantics).
    store::set_meta(storage.conn(), "counter_EPIC", "2").expect("counter epic");
    store::set_meta(storage.conn(), "counter_STORY", "1").expect("counter story");
    store::set_meta(storage.conn(), "counter_TICKET", "2").expect("counter ticket");
    store::set_meta(storage.conn(), "counter_RUN", "1").expect("counter run");

    store::insert_run(storage.conn(), &run_row("RUN-0001", "running")).expect("insert run");
    store::insert_run_item(storage.conn(), &item_row("RUN-0001", "TICKET-0001", 0))
        .expect("item 1");
    let mut running_item = item_row("RUN-0001", "TICKET-0002", 1);
    running_item.state = "running".parse().unwrap();
    running_item.executor_session_id = Some("sess-a".to_string());
    running_item.attempts = 1;
    running_item.started_at = Some(iso_at(T0_MS));
    store::insert_run_item(storage.conn(), &running_item).expect("item 2");

    Baseline {
        _dir: dir,
        home,
        clock,
        epic1,
        epic2,
        story,
        ticket1,
        ticket2,
    }
}

/// Build each canonical op's plan against a fresh baseline.
fn build_plan(baseline: &Baseline, storage: &Storage, op: &str) -> PreparedMutation {
    match op {
        // Auto-allocated id create (counter bump inside finalize).
        "create" => {
            let mut node = node_row(
                "TICKET-0003",
                "ticket",
                "third ticket",
                "open",
                "tickets/TICKET-0003-third-ticket/TICKET.md",
            );
            node.created_at = iso_at(T0_MS);
            node.updated_at = iso_at(T0_MS);
            storage
                .plan_create(
                    "grid-create",
                    &node,
                    Some(&baseline.story),
                    "fresh body",
                    true,
                )
                .expect("plan create")
        }
        // Plain update: title + status change.
        "update" => storage
            .plan_update(
                "grid-update",
                &baseline.ticket1,
                Some("renamed first".to_string()),
                Some("in_progress".to_string()),
                None,
                None,
                None,
                None,
                Some(&baseline.story),
                vec![],
                serde_json::json!({ "nodeId": "TICKET-0001" }),
            )
            .expect("plan update"),
        // Multi-file move: STORY-0001 (+ both ticket files) relocates from
        // EPIC-0001 to EPIC-0002 → 1 MoveDir + 4 Writes.
        "move" => {
            let new_story_dir = format!(
                "EPIC-0002-{}/STORY-0001-{}",
                slug("other epic"),
                slug("the story")
            );
            let path_map = move_path_map(&new_story_dir);
            let subtree = vec![
                baseline.story.clone(),
                baseline.ticket1.clone(),
                baseline.ticket2.clone(),
            ];
            storage
                .plan_move(
                    "grid-move",
                    &baseline.story,
                    &baseline.epic2,
                    Some(&baseline.epic1),
                    &subtree,
                    &path_map,
                    vec![],
                )
                .expect("plan move")
        }
        // Archive TICKET-0002 (update carrying the archived flag + the
        // passive-observation skip transition of its active-run item).
        "archive" => storage
            .plan_update(
                "grid-archive",
                &baseline.ticket2,
                None,
                None,
                Some(true),
                None,
                None,
                None,
                Some(&baseline.story),
                vec![DbChange::ItemPatch {
                    patch: ItemPatch {
                        run_id: "RUN-0001".into(),
                        node_id: "TICKET-0002".into(),
                        set_state: Some("skipped".into()),
                        set_finished_at: Some(iso_at(T0_MS)),
                        ..ItemPatch::default()
                    },
                }],
                serde_json::json!({ "nodeId": "TICKET-0002", "archived": true }),
            )
            .expect("plan archive"),
        // Report TICKET-0002 done with a note.
        "report" => {
            let item = store::get_run_item(storage.conn(), "RUN-0001", "TICKET-0002")
                .expect("item lookup")
                .expect("item exists");
            let patch = report_item_patch(&item, "done", iso_at(T0_MS), None);
            storage
                .plan_report(
                    "grid-report",
                    &baseline.ticket2,
                    Some(&baseline.story),
                    Some("done".to_string()),
                    Some("completed by grid".to_string()),
                    patch,
                    vec![],
                )
                .expect("plan report")
        }
        other => panic!("unknown grid op {other}"),
    }
}

fn move_path_map(new_story_dir: &str) -> impl Fn(&str) -> Option<String> + '_ {
    move |id: &str| match id {
        "STORY-0001" => Some(format!("tickets/{new_story_dir}/STORY.md")),
        "TICKET-0001" => Some(format!(
            "tickets/{new_story_dir}/TICKET-0001-{}/TICKET.md",
            slug("first ticket")
        )),
        "TICKET-0002" => Some(format!(
            "tickets/{new_story_dir}/TICKET-0002-{}/TICKET.md",
            slug("second ticket")
        )),
        _ => None,
    }
}

fn count_events(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("event count")
}

/// Post-recovery convergence assertions shared by every grid cell.
fn assert_converged(
    home: &std::path::Path,
    clock: &std::sync::Arc<FixedClock>,
    plan: &PreparedMutation,
    op: &str,
) {
    let mut reopened = open_storage(home, clock);

    // 1. Files: every Write target carries its planned final bytes.
    for file_op in &plan.files {
        match file_op {
            FileOp::Write { path, content, .. } => {
                let actual = reopened
                    .files()
                    .read_optional(path)
                    .expect("read converged file");
                assert_eq!(
                    actual.as_deref(),
                    Some(content.as_str()),
                    "file {path} did not converge ({op})"
                );
            }
            FileOp::MoveDir { from, to } => {
                assert!(
                    !reopened.files().is_dir(from),
                    "moved-from dir still present: {from}"
                );
                assert!(
                    reopened.files().is_dir(to),
                    "moved-to dir missing after recovery ({op}): {to}"
                );
            }
        }
    }

    // 2. Operations row exactly once; journal acknowledged.
    let committed: i64 = reopened
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE command_id = ?1",
            [&plan.command_id],
            |row| row.get(0),
        )
        .expect("operations count");
    assert_eq!(
        committed, 1,
        "operations rows for {} ({op})",
        plan.command_id
    );
    let phase: String = reopened
        .conn()
        .query_row(
            "SELECT phase FROM journal WHERE command_id = ?1",
            [&plan.command_id],
            |row| row.get(0),
        )
        .expect("journal phase");
    assert_eq!(
        phase, "acknowledged",
        "journal not acknowledged after recovery ({op})"
    );

    // 3. Recovery directory pruned.
    assert!(
        !reopened
            .files()
            .exists(&omt_storage::files::DiskFiles::recovery_dir_rel(
                &plan.command_id
            )),
        "recovery dir not pruned for {op}"
    );

    // 4. Op-specific durable-state expectations.
    match op {
        "create" => {
            let node = store::get_node(reopened.conn(), "TICKET-0003")
                .expect("query")
                .expect("created node");
            assert_eq!(node.title, "third ticket");
            assert_eq!(
                store::counter_value_of(reopened.conn(), "TICKET").unwrap(),
                3,
                "counter bumped exactly once"
            );
            let edge = store::children_of_count(reopened.conn(), "STORY-0001");
            assert_eq!(edge, 3, "story gained the third child exactly once");
        }
        "update" => {
            let node = store::get_node(reopened.conn(), "TICKET-0001")
                .expect("query")
                .expect("node");
            assert_eq!(node.title, "renamed first");
            assert_eq!(node.status.to_string(), "in_progress");
        }
        "move" => {
            let node = store::get_node(reopened.conn(), "STORY-0001")
                .expect("query")
                .expect("moved node");
            assert!(
                node.path.contains("EPIC-0002"),
                "path not relocated: {}",
                node.path
            );
            for member in ["TICKET-0001", "TICKET-0002"] {
                let m = store::get_node(reopened.conn(), member)
                    .expect("query")
                    .expect("member");
                assert!(
                    m.path.contains("EPIC-0002"),
                    "{member} path not relocated: {}",
                    m.path
                );
            }
        }
        "archive" => {
            let node = store::get_node(reopened.conn(), "TICKET-0002")
                .expect("query")
                .expect("node");
            assert!(node.archived, "not archived");
            let item = store::get_run_item(reopened.conn(), "RUN-0001", "TICKET-0002")
                .expect("query")
                .expect("item");
            assert_eq!(
                item.state.to_string(),
                "skipped",
                "active-run item not skipped"
            );
        }
        "report" => {
            let item = store::get_run_item(reopened.conn(), "RUN-0001", "TICKET-0002")
                .expect("query")
                .expect("item");
            assert_eq!(item.state.to_string(), "done");
            let node = store::get_node(reopened.conn(), "TICKET-0002")
                .expect("query")
                .expect("node");
            assert_eq!(
                node.status.to_string(),
                "done",
                "ticket status not double-written"
            );
            let file = reopened.files().read(&node.path).expect("reported file");
            assert!(file.contains("completed by grid"), "note append missing");
        }
        _ => unreachable!(),
    }

    // 5. Idempotent re-execution returns the stored result without side effects.
    let events_before = count_events(reopened.conn());
    let replayed = reopened.execute(plan).expect("idempotent re-execute");
    assert_eq!(
        replayed, plan.result,
        "stored result mismatch on re-execute ({op})"
    );
    assert_eq!(
        count_events(reopened.conn()),
        events_before,
        "duplicate events on re-execute ({op})"
    );
}

/// THE GRID: every op × every kill ordinal converges.
#[test]
fn kill_point_grid_converges_from_every_step() {
    let ops = ["create", "update", "move", "archive", "report"];
    let mut total_cells = 0usize;

    for op in ops {
        // Discover the op's step count from a clean dry run.
        let baseline = seed_baseline();
        let steps = {
            let storage = open_storage(&baseline.home, &baseline.clock);
            build_plan(&baseline, &storage, op).step_count()
        };

        for kill_ordinal in 0..steps {
            let baseline = seed_baseline();
            let plan = {
                let mut storage = open_storage_armed(
                    &baseline.home,
                    &baseline.clock,
                    FaultSchedule::at(kill_ordinal),
                );
                let plan = build_plan(&baseline, &storage, op);

                let outcome: Result<serde_json::Value, Problem> = storage.execute(&plan);
                // Every ordinal fires BEFORE its step completes, so the live
                // call always surfaces the injected problem.
                let problem = expect_problem(outcome, "IO");
                assert_eq!(
                    problem
                        .details
                        .as_ref()
                        .and_then(|d| d.get("injected"))
                        .and_then(|v| v.as_str()),
                    Some("kill-point"),
                    "fault at ordinal {kill_ordinal}/{steps} did not fire for {op}"
                );
                drop(storage); // crash simulation: no acknowledge, no recovery
                plan
            };

            // Restart over the same home: recovery must converge everything.
            assert_converged(&baseline.home, &baseline.clock, &plan, op);
            total_cells += 1;
        }
    }
    eprintln!("GRID SIZE: {total_cells} cells (5 ops × every injectable step)");
    assert!(total_cells >= 30, "grid unexpectedly small: {total_cells}");
}
