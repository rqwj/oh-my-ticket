//! Journal recovery scenarios beyond the kill grid: manual drift during a
//! pending plan fails closed with byte-identical files; the recovery-copy
//! restore path resurrects a lost target; command-id reuse with a different
//! input is rejected; a migrated legacy home opens end-to-end through
//! `Storage::open`.

#[path = "common/mod.rs"]
mod common;

use common::*;
use omt_domain::store::FileStore as _;
use omt_storage::files::DiskFiles;
use omt_storage::journal::FileOp;
use omt_storage::migrate::fixtures;
use omt_storage::{FaultSchedule, FixedClock, RecoveryReport, Storage};

struct Seed {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
    clock: std::sync::Arc<FixedClock>,
    ticket: omt_domain::types::NodeRow,
    /// Exact on-disk bytes after seeding — the revert witness.
    seeded_bytes: String,
}

fn seed_two_ticket_home() -> Seed {
    let (dir, home) = temp_home();
    let clock = fixed_clock();
    let mut storage = open_storage(&home, &clock);
    let ticket = node_row(
        "TICKET-0001",
        "ticket",
        "drift target",
        "open",
        "tickets/TICKET-0001-drift-target/TICKET.md",
    );
    execute_simple(&mut storage, "seed", &ticket, None, "original body");
    let seeded_bytes = storage.files().read(&ticket.path).expect("seeded file");
    storage.release_lock().ok();
    Seed {
        _dir: dir,
        home,
        clock,
        ticket,
        seeded_bytes,
    }
}

#[test]
fn manual_drift_during_pending_plan_fails_closed_without_overwrite() {
    let Seed {
        _dir,
        home,
        clock,
        ticket,
        seeded_bytes,
    } = seed_two_ticket_home();

    // Crash mid-update: kill at the RenameOver phase of the file op
    // (ordinal 3 = RecoveryCopy(0) WriteTemp(1) FsyncTemp(2) RenameOver(3)).
    let _plan = {
        let mut storage = open_storage_armed(&home, &clock, FaultSchedule::at(3));
        let plan = storage
            .plan_update(
                "drift-cmd",
                &ticket,
                None,
                Some("done".into()),
                None,
                None,
                None,
                Some("updated body".to_string()),
                None,
                vec![],
                serde_json::json!({}),
            )
            .expect("plan");
        expect_problem(storage.execute(&plan), "IO");
        drop(storage);
        plan
    };

    // The user hand-edits the target while the writer was down.
    let drifted = "# ---\nid: TICKET-0001\n---\n\nuser rewrote this by hand\n";
    DiskFiles::new(&home)
        .atomic_write(&ticket.path, drifted, "user")
        .unwrap();

    // Reopen must REFUSE closed: REINDEX_REQUIRED, and the user's bytes are
    // exactly what remains on disk (no overwrite).
    let problem = expect_problem(
        {
            let mut cfg = open_config(
                &home,
                &(clock.clone() as std::sync::Arc<dyn omt_storage::MillisClock>),
            );
            cfg.recover_on_open = true;
            Storage::open(cfg)
        },
        "REINDEX_REQUIRED",
    );
    assert_eq!(
        problem.details.as_ref().unwrap()["driftedPath"],
        ticket.path.clone()
    );
    assert_eq!(problem.details.as_ref().unwrap()["requiresReindex"], true);
    assert_eq!(
        DiskFiles::new(&home).read(&ticket.path).unwrap(),
        drifted,
        "recovery overwrote manual edits"
    );

    // The pending journal row survives for diagnosis; nothing was finalized.
    let reopened_once = open_storage_no_recover(&home, &clock);
    let phase: String = reopened_once
        .conn()
        .query_row(
            "SELECT phase FROM journal WHERE command_id='drift-cmd'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(phase, "prepared");
    let committed: i64 = reopened_once
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE command_id='drift-cmd'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(committed, 0);
    drop(reopened_once);

    // After the user reverts their edit byte-for-byte to the before-state,
    // recovery replays cleanly to convergence.
    DiskFiles::new(&home)
        .atomic_write(&ticket.path, &seeded_bytes, "revert")
        .unwrap();
    let mut recovered = open_storage(&home, &clock);
    recovered
        .recover_pending()
        .expect("clean replay after revert");
}

fn open_storage_no_recover(home: &std::path::Path, clock: &std::sync::Arc<FixedClock>) -> Storage {
    let any_clock: std::sync::Arc<dyn omt_storage::MillisClock> = clock.clone();
    let mut config = open_config(home, &any_clock);
    config.recover_on_open = false;
    Storage::open(config).expect("storage open (no recover)")
}

use omt_storage::OpenConfig;

#[test]
fn missing_target_is_restored_from_recovery_copy_then_replayed() {
    let Seed {
        _dir,
        home,
        clock,
        ticket,
        seeded_bytes: _,
    } = seed_two_ticket_home();

    // Kill AFTER the recovery copy exists but BEFORE the rename lands:
    // ordinals 0..=2 leave copy+temp staged; pick ordinal 2 (FsyncTemp).
    let plan = {
        let mut storage = open_storage_armed(&home, &clock, FaultSchedule::at(2));
        let plan = storage
            .plan_update(
                "restore-cmd",
                &ticket,
                None,
                Some("blocked".into()),
                None,
                None,
                None,
                Some("restored body".to_string()),
                None,
                vec![],
                serde_json::json!({}),
            )
            .expect("plan");
        expect_problem(storage.execute(&plan), "IO");
        drop(storage);
        plan
    };

    // Catastrophic loss of the target between crash and restart.
    std::fs::remove_file(home.join(&ticket.path)).unwrap();
    assert!(!DiskFiles::new(&home).exists(&ticket.path));

    // Reopen WITHOUT auto-recovery, then run the pass explicitly so the
    // report is observable.
    let mut reopened = open_storage_no_recover(&home, &clock);
    let report: RecoveryReport = reopened.recover_pending().expect("recovery succeeds");
    assert_eq!(
        report.restored,
        vec![ticket.path.clone()],
        "restore path not exercised"
    );
    let final_bytes = reopened.files().read(&ticket.path).unwrap();
    match &plan.files[0] {
        FileOp::Write { content, .. } => {
            assert_eq!(final_bytes, *content, "restored+replayed bytes")
        }
        other => panic!("unexpected first op {other:?}"),
    }
    let status: String = reopened
        .conn()
        .query_row("SELECT status FROM nodes WHERE id='TICKET-0001'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "blocked");
}

#[test]
fn roll_forward_never_reverts_committed_state_and_reports() {
    let Seed {
        _dir,
        home,
        clock,
        ticket,
        seeded_bytes: _,
    } = seed_two_ticket_home();

    // Kill at Acknowledge: commit landed, ack did not.
    let plan = {
        let steps = 7usize; // single Write → 5 phases + TxnCommit + Acknowledge
        let mut storage = open_storage_armed(&home, &clock, FaultSchedule::at(steps - 1));
        let plan = storage
            .plan_update(
                "rollfwd-cmd",
                &ticket,
                Some("rolled".into()),
                None,
                None,
                None,
                None,
                None,
                None,
                vec![],
                serde_json::json!({}),
            )
            .expect("plan");
        expect_problem(storage.execute(&plan), "IO");
        drop(storage);
        plan
    };

    // Even if the file drifted AFTER commit, recovery rolls FORWARD only.
    let mut reopened = open_storage_no_recover(&home, &clock);
    let report = reopened.recover_pending().expect("roll forward");
    assert_eq!(report.rolled_forward, vec!["rollfwd-cmd".to_string()]);
    assert!(report.replayed.is_empty());
    let title: String = reopened
        .conn()
        .query_row("SELECT title FROM nodes WHERE id='TICKET-0001'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(title, "rolled", "committed state must survive");
    let _ = plan;
}

#[test]
fn command_id_reuse_with_different_input_fails_closed() {
    let Seed {
        _dir,
        home,
        clock,
        ticket,
        seeded_bytes: _,
    } = seed_two_ticket_home();
    let mut storage = open_storage(&home, &clock);

    let original = storage
        .plan_update(
            "reuse-cmd",
            &ticket,
            Some("first".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            vec![],
            serde_json::json!({}),
        )
        .expect("plan");
    storage.execute(&original).expect("execute");

    // Same id, different fingerprint → refuse.
    let forged = storage
        .plan_update(
            "reuse-cmd",
            &ticket,
            Some("second".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            vec![],
            serde_json::json!({}),
        )
        .expect("plan");
    expect_problem(storage.execute(&forged), "CONFLICT");

    // Same id + same input → stored result returned without side effects.
    let events_before: i64 = storage
        .conn()
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    let replayed = storage.execute(&original).expect("idempotent replay");
    assert_eq!(replayed, original.result);
    let events_after: i64 = storage
        .conn()
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events_before, events_after);
}

#[test]
fn migrated_v3_home_opens_end_to_end_through_storage_open() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = home.join("omt.db");
    {
        let conn = fixtures::build_v3(&db);
        fixtures::seed_realistic_home(&conn);
    }

    let any_clock: std::sync::Arc<dyn omt_storage::MillisClock> = fixed_clock();
    let mut config = OpenConfig::new(&home);
    config.clock = any_clock;
    config.home_id = Some("h_migrated01".to_string());
    let storage = Storage::open(config).expect("legacy home opens");

    assert_eq!(storage.home_id(), Some("h_migrated01"));
    let node = omt_storage::store::get_node(storage.conn(), "EPIC-0001")
        .expect("query")
        .expect("node preserved through migration + open");
    assert_eq!(node.title, "旧项目 重构");
    let attempts: i64 = storage
        .conn()
        .query_row(
            "SELECT attempts FROM run_items WHERE run_id='RUN-0001' AND node_id='TICKET-0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 2);
}
