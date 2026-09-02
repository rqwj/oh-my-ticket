//! Deterministic reindex + quarantine (R19/U4c):
//!   * dry-run is zero-write and lists imports/moves/conflicts/quarantines;
//!   * missing active members quarantine with an EXACT identity snapshot
//!     (id/type/title/path/lastKnownBodyHash) plus a durable
//!     `node.quarantined` stream event — never a silent delete/rebind;
//!   * execute applies exactly the dry-run mapping; rerunning is stable
//!     (no double-quarantine);
//!   * drift between dry-run and execute fails closed (REINDEX_REQUIRED).

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::*;
use omt_storage::clock::MillisClock;
use omt_storage::files::DiskFiles;
use omt_storage::reindex::{dry_run, execute, ReindexPlan};
use omt_storage::{FixedClock, Storage};

fn disk_files(home: &std::path::Path) -> DiskFiles {
    DiskFiles::new(home)
}

/// An owned mutable connection for `execute` (it opens transactions), kept
/// separate from the observing `Storage` handle.
fn exec_conn(home: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(home.join("omt.db")).expect("open exec connection");
    omt_storage::store::apply_open_pragmas(&conn).expect("pragmas");
    conn
}

/// Canonical layout paths (computed through the REAL slug rules) so a fresh
/// home starts with an empty reindex plan.
pub fn epic_path() -> String {
    format!(
        "tickets/{}/epic.md",
        omt_domain::markdown::node_dir_name("EPIC-0001", "重索引演练")
    )
}

pub fn ticket_path() -> String {
    format!(
        "tickets/{}/{}/ticket.md",
        omt_domain::markdown::node_dir_name("EPIC-0001", "重索引演练"),
        omt_domain::markdown::node_dir_name("TICKET-0001", "存活成员"),
    )
}

fn seed_home(clock: &Arc<FixedClock>) -> (tempfile::TempDir, std::path::PathBuf, Storage) {
    let (dir, home) = temp_home();
    let mut storage = open_storage(&home, clock);
    let epic = node_row("EPIC-0001", "epic", "重索引演练", "open", &epic_path());
    execute_simple(
        &mut storage,
        "cmd-rx-1",
        &epic,
        None,
        "根节点正文，用于身份快照校验",
    );
    let ticket = node_row("TICKET-0001", "ticket", "存活成员", "done", &ticket_path());
    execute_simple(&mut storage, "cmd-rx-2", &ticket, Some(&epic), "工单正文");
    // Sanity: a fresh canonical home yields an EMPTY plan.
    let empty_plan = dry_run(storage.conn(), &disk_files(&home)).expect("baseline plan");
    assert!(
        empty_plan.is_empty(),
        "fresh home must plan nothing: {empty_plan:?}"
    );
    (dir, home, storage)
}

/// The identity snapshot records EXACTLY what the packet demands:
/// id / type / title / path / lastKnownBodyHash — the hash being SHA-256 of
/// the last indexed body text.
#[test]
fn quarantine_snapshot_fields_are_exact() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home, storage) = seed_home(&clock);

    // Simulate the missing-member condition: DB row stays, Markdown vanishes.
    let missing_path = ticket_path();
    std::fs::remove_file(home.join(&missing_path)).expect("delete member markdown");

    let files = disk_files(&home);
    let plan = dry_run(storage.conn(), &files).expect("dry run");
    assert_eq!(plan.quarantines.len(), 1, "exactly one member missing");
    let snapshot = &plan.quarantines[0];
    assert_eq!(snapshot.id, "TICKET-0001");
    assert_eq!(snapshot.node_type, "ticket");
    assert_eq!(snapshot.title, "存活成员");
    assert_eq!(snapshot.path, missing_path);
    // lastKnownBodyHash = sha256("工单正文") — the indexed body mirror.
    let expected_hash = omt_storage::files::sha256_hex("工单正文");
    assert_eq!(snapshot.last_known_body_hash, expected_hash);
    assert_eq!(snapshot.reason, "markdown-missing");
}

/// Dry-run == execute mapping; the quarantine row carries the SAME snapshot
/// JSON; the stream event lands exactly once; a rerun is stable.
#[test]
fn execute_matches_dry_run_and_reruns_are_stable() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home, storage) = seed_home(&clock);
    let any_clock: Arc<dyn MillisClock> = clock.clone();

    let missing_path = ticket_path();
    std::fs::remove_file(home.join(&missing_path)).expect("delete member markdown");

    let files = disk_files(&home);

    // ── first pass ──
    let plan: ReindexPlan = dry_run(storage.conn(), &files).expect("dry run 1");
    assert_eq!(plan.quarantines.len(), 1);
    assert!(plan.moves.is_empty());
    assert!(plan.imports.is_empty());
    assert!(plan.conflicts.is_empty());

    let mut exec = exec_conn(&home);
    let executed = execute(&mut exec, &files, &plan, any_clock.as_ref()).expect("execute 1");
    assert_eq!(executed.quarantined, vec!["TICKET-0001".to_string()]);
    drop(executed);
    drop(exec);
    drop(storage); // release the writer handle before reopening
    let storage = open_storage(&home, &clock); // observe committed state

    // Row removed from the active set…
    let active_count: i64 = storage
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE id = 'TICKET-0001'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(active_count, 0, "member left the active set");

    // …but its FULL identity lives in quarantined_nodes with the same JSON.
    let (snapshot_json, reason): (String, String) = storage
        .conn()
        .query_row(
            "SELECT snapshot_json, reason FROM quarantined_nodes WHERE node_id = 'TICKET-0001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("quarantine row");
    assert_eq!(reason, "markdown-missing");
    let stored: serde_json::Value = serde_json::from_str(&snapshot_json).expect("parse snapshot");
    assert_eq!(stored["id"], "TICKET-0001");
    assert_eq!(stored["type"], "ticket");
    assert_eq!(stored["title"], "存活成员");
    assert_eq!(stored["path"], missing_path);
    assert_eq!(
        stored["lastKnownBodyHash"],
        omt_storage::files::sha256_hex("工单正文")
    );

    // Durable event appended exactly once.
    let events = omt_storage::outbox::resume_since(storage.conn(), 0, 100).expect("events");
    let quarantine_events: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == "node.quarantined")
        .collect();
    assert_eq!(quarantine_events.len(), 1, "one durable quarantine event");
    assert_eq!(quarantine_events[0].payload["nodeId"], "TICKET-0001");
    assert_eq!(quarantine_events[0].payload["path"], missing_path);

    // ── rerun: stable — nothing left to quarantine, no double-quarantine ──
    let plan2 = dry_run(storage.conn(), &files).expect("dry run 2");
    assert!(plan2.is_empty(), "rerun finds nothing to do: {plan2:?}");
    let mut exec2 = exec_conn(&home);
    let executed2 = execute(&mut exec2, &files, &plan2, any_clock.as_ref()).expect("execute 2");
    assert!(executed2.quarantined.is_empty());
    let quarantine_rows: i64 = storage
        .conn()
        .query_row("SELECT COUNT(*) FROM quarantined_nodes", [], |r| r.get(0))
        .expect("count rows");
    assert_eq!(quarantine_rows, 1, "still exactly one quarantine record");
    let events2 = omt_storage::outbox::resume_since(storage.conn(), 0, 100).expect("events 2");
    assert_eq!(
        events2
            .iter()
            .filter(|e| e.event_type == "node.quarantined")
            .count(),
        1,
        "no duplicate event on rerun"
    );
}

/// A stale directory layout (DB title updated while the folder kept the old
/// slug) becomes a move; execute renames the directory and updates the row.
#[test]
fn move_mapping_renames_directory_and_updates_row() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home, mut storage) = seed_home(&clock);
    let any_clock: Arc<dyn MillisClock> = clock.clone();

    // Drift: title changed in the DB only; the folder still uses the old slug.
    storage
        .conn()
        .execute(
            "UPDATE nodes SET title = '改名后的成员' WHERE id = 'TICKET-0001'",
            [],
        )
        .expect("drift title");

    let files = disk_files(&home);
    let plan = dry_run(storage.conn(), &files).expect("dry run");
    assert_eq!(plan.moves.len(), 1, "one canonical-layout repair planned");
    assert_eq!(plan.moves[0].node_id, "TICKET-0001");
    assert_eq!(
        plan.moves[0].to,
        format!(
            "tickets/{}/{}/ticket.md",
            omt_domain::markdown::node_dir_name("EPIC-0001", "重索引演练"),
            omt_domain::markdown::node_dir_name("TICKET-0001", "改名后的成员"),
        )
    );
    let old_rel = ticket_path();

    let mut exec = exec_conn(&home);
    let executed = execute(&mut exec, &files, &plan, any_clock.as_ref()).expect("execute");
    assert_eq!(executed.moved, 1);
    drop(exec);
    storage = open_storage(&home, &clock);

    // File physically relocated; row points at the new path.
    let new_rel = plan.moves[0].to.clone();
    assert!(home.join(&new_rel).exists(), "directory renamed");
    assert!(!home.join(&old_rel).exists(), "old path gone");
    let stored_path: String = storage
        .conn()
        .query_row("SELECT path FROM nodes WHERE id = 'TICKET-0001'", [], |r| {
            r.get(0)
        })
        .expect("row path");
    assert_eq!(stored_path, new_rel);

    // Rerun: canonical already — stable.
    let plan2 = dry_run(storage.conn(), &files).expect("dry run 2");
    assert!(plan2.is_empty(), "rerun stable: {plan2:?}");
}

/// Stray Markdown files import; conflicts fail CLOSED without writing.
#[test]
fn imports_apply_and_conflicts_fail_closed() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home, mut storage) = seed_home(&clock);
    let any_clock: Arc<dyn MillisClock> = clock.clone();
    let files = disk_files(&home);

    // Hand-drop a stray ticket with valid frontmatter.
    let stray_dir = home.join("tickets/TICKET-0099-stray-import");
    std::fs::create_dir_all(&stray_dir).expect("stray dir");
    std::fs::write(
        stray_dir.join("ticket.md"),
        "---\nid: TICKET-0099\ntype: ticket\ntitle: 手放文件\nstatus: open\npriority: 0\ncreated_at: '2026-08-24T05:00:00.000Z'\nupdated_at: '2026-08-24T05:00:00.000Z'\n---\n\n导入的正文\n",
    )
    .expect("stray file");

    let plan = dry_run(storage.conn(), &files).expect("dry run");
    assert_eq!(plan.imports.len(), 1);
    assert_eq!(plan.imports[0].id.as_deref(), Some("TICKET-0099"));

    let mut exec = exec_conn(&home);
    let executed = execute(&mut exec, &files, &plan, any_clock.as_ref()).expect("execute");
    assert_eq!(executed.imported, 1);
    drop(exec);
    storage = open_storage(&home, &clock);
    let imported_title: String = storage
        .conn()
        .query_row(
            "SELECT title FROM nodes WHERE id = 'TICKET-0099'",
            [],
            |r| r.get(0),
        )
        .expect("imported row");
    assert_eq!(imported_title, "手放文件");

    // CONFLICT path: a stray file duplicating an ACTIVE id fails closed and
    // leaves every row untouched.
    let dupe_dir = home.join("tickets/duplicate-id-stray");
    std::fs::create_dir_all(&dupe_dir).expect("dupe dir");
    std::fs::write(
        dupe_dir.join("ticket.md"),
        "---\nid: EPIC-0001\ntype: epic\ntitle: 重复 ID\nstatus: open\npriority: 0\n---\n\n冲突正文\n",
    )
    .expect("dupe file");

    let plan2 = dry_run(storage.conn(), &files).expect("dry run 2");
    assert_eq!(plan2.imports.len(), 1);
    let before_count: i64 = storage
        .conn()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .expect("count before");
    let mut exec3 = exec_conn(&home);
    expect_problem(
        execute(&mut exec3, &files, &plan2, any_clock.as_ref()),
        "CONFLICT",
    );
    let after_count: i64 = storage
        .conn()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .expect("count after");
    assert_eq!(before_count, after_count, "failed execute wrote NOTHING");
}

/// State changed between dry-run and execute → REINDEX_REQUIRED, no writes.
#[test]
fn drift_between_dry_run_and_execute_fails_closed() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home, storage) = seed_home(&clock);
    let any_clock: Arc<dyn MillisClock> = clock.clone();
    let files = disk_files(&home);

    // Plan a quarantine…
    let missing_path = ticket_path();
    std::fs::remove_file(home.join(&missing_path)).expect("delete member markdown");
    let plan = dry_run(storage.conn(), &files).expect("dry run");
    assert_eq!(plan.quarantines.len(), 1);

    // …then reality changes BEFORE execute: the file comes back.
    std::fs::create_dir_all(home.join(&missing_path).parent().unwrap()).expect("mkdir");
    std::fs::write(
        home.join(&missing_path),
        "---\nid: TICKET-0001\n---\n\n复活\n",
    )
    .expect("revive");

    let mut exec = exec_conn(&home);
    let problem = expect_problem(
        execute(&mut exec, &files, &plan, any_clock.as_ref()),
        "REINDEX_REQUIRED",
    );
    assert!(problem.details.is_some(), "carries requiresReindex details");
    // Nothing was written: the member is STILL active.
    let still_active: i64 = storage
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE id = 'TICKET-0001'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(still_active, 1, "fail-closed left the active set untouched");
}
