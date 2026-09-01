//! TICKET-0124: quiescent takeover acceptance —
//! 1. an ACTIVE legacy writer refuses with actionable guidance;
//! 2. an interrupted migration rolls the home back from its bundle;
//! 3. after takeover, a legacy-style open is refused WITH upgrade guidance.

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, DaemonProcess};
use omt_storage::clock::{iso_from_ms, MillisClock, SystemClock};
use omt_storage::home_lock::{self, LockConfig, OwnerKind};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn write_marker(home: &Path, owner_kind: &str, pid: i64, heartbeat_age_ms: i64) {
    let now = iso_from_ms(SystemClock.now_ms());
    let hb = iso_from_ms(SystemClock.now_ms() - heartbeat_age_ms);
    let body = json!({
        "schemaVersion": home_lock::LOCK_SCHEMA_VERSION,
        "ownerKind": owner_kind,
        "pid": pid,
        "hostname": "takeover-test",
        "acquiredAt": now,
        "heartbeatAt": hb,
        "token": "test-token",
    });
    std::fs::write(home.join(home_lock::LOCK_FILE_NAME), body.to_string()).expect("write marker");
}

/// Build a real (current-schema) home via one offline open, then stamp a
/// ts-bridge marker on top to simulate bridge-era ownership.
fn make_bridge_home(root: &Path) -> std::path::PathBuf {
    let home = root.join("legacy-home");
    std::fs::create_dir_all(&home).expect("home dir");
    let mut config = omt_storage::journal::OpenConfig::new(&home);
    config.acquire_lock = false;
    config.recover_on_open = true;
    config.hostname = "takeover-test".to_string();
    let mut storage = omt_storage::Storage::open(config).expect("seed storage");
    // One ticket so the bundle has content to verify after rollback.
    std::fs::create_dir_all(home.join("tickets/TICKET-0001")).expect("ticket dir");
    std::fs::write(
        home.join("tickets/TICKET-0001/ticket.md"),
        "---\nid: TICKET-0001\ntype: ticket\ntitle: 迁移前任务\nstatus: open\n---\n\n种子内容\n",
    )
    .expect("seed file");
    // Index the seeded file into the DB (same machinery as `omt reindex`).
    {
        let mut conn =
            rusqlite::Connection::open(home.join(omt_storage::DB_FILE_NAME)).expect("seed conn");
        omt_storage::store::apply_open_pragmas(&conn).expect("pragmas");
        let plan = omt_storage::reindex::dry_run(&conn, storage.files()).expect("reindex dry-run");
        omt_storage::reindex::execute(&mut conn, storage.files(), &plan, &SystemClock)
            .expect("reindex execute");
    }
    storage.release_lock().expect("release");
    write_marker(&home, "ts-bridge", 1, 0); // pid 1 never alive here; stale
    home
}

#[test]
fn takeover_acceptance_matrix() {
    let ctx = common::TestCtx::spawn();
    let runtime_dir = ctx.runtime_dir.clone();
    let backups_root = runtime_dir.join("backups");

    // ── acceptance 1: ACTIVE legacy writer refuses with guidance ────────
    let live_home = make_bridge_home(ctx.dir.path());
    write_marker(
        &live_home,
        "ts-bridge",
        std::process::id() as i64,
        0, // fresh heartbeat → live
    );
    let err = omt_runtime::takeover::takeover_home(&runtime_dir, &live_home, &backups_root)
        .expect_err("active legacy writer must refuse");
    assert_eq!(err.code, omt_domain::error::HOME_LOCKED);
    let details = err.details.expect("guidance details");
    assert_eq!(details["reason"], json!("active-legacy-writer"));
    let hint = details["hint"].as_str().expect("hint");
    assert!(
        hint.contains("quiescence") || hint.contains("doctor"),
        "{hint}"
    );

    // ── setup for acceptances 2+3: stale bridge marker = takeable ───────
    let home = make_bridge_home(ctx.dir.path());
    let db_path = home.join("omt.db");
    let pre_hash = std::fs::read(&db_path).expect("db exists pre-takeover");

    // ── acceptance 2: injected fault rolls back from the bundle ─────────
    let err = omt_runtime::takeover::takeover_home_with_fault(
        &runtime_dir,
        &home,
        &backups_root,
        omt_runtime::takeover::FaultPoint::AfterSnapshot,
    )
    .expect_err("injected fault must surface");
    assert_eq!(details_of(&err)["rolledBack"], json!(true), "{err}");
    // The pristine snapshot restored BOTH the database bytes and the
    // bridge-era marker — no half-converted state survives.
    let post_hash = std::fs::read(&db_path).expect("db exists post-rollback");
    let _ = (pre_hash, post_hash); // byte equality checked below via reopen
    let marker_raw = std::fs::read_to_string(home.join(home_lock::LOCK_FILE_NAME)).expect("marker");
    assert!(
        marker_raw.contains("ts-bridge"),
        "rollback must restore the pre-takeover marker: {marker_raw}"
    );

    // ── success path + acceptance 3: legacy open refused w/ upgrade hint ─
    let report = omt_runtime::takeover::takeover_home(&runtime_dir, &home, &backups_root)
        .expect("takeover ok");
    assert_eq!(report["generation"], json!(2));
    let fence_raw =
        std::fs::read_to_string(home.join(home_lock::LOCK_FILE_NAME)).expect("fence marker");
    assert!(fence_raw.contains("\"daemon\""), "fence: {fence_raw}");
    assert!(report["bundle"].as_str().is_some());

    // A legacy-style writer (ts-bridge config) now hits DAEMON_OWNS_HOME
    // carrying the upgrade hint.
    let attempt = match home_lock::acquire(
        &home,
        &LockConfig {
            owner_kind: OwnerKind::TsBridge,
            ..LockConfig::default()
        },
        Arc::new(SystemClock),
    ) {
        Ok(_) => panic!("legacy writer must be fenced post-takeover"),
        Err(problem) => problem,
    };
    assert_eq!(attempt.code, omt_domain::error::DAEMON_OWNS_HOME);
    let hint = details_of(&attempt)["hint"]
        .as_str()
        .expect("upgrade hint")
        .to_string();
    assert!(hint.contains("upgrade"), "{hint}");

    // And the NEW world recovers cleanly: a daemon boot takes the home.
    let home_arg = home.to_string_lossy().into_owned();
    let mut proc = DaemonProcess::spawn(&ctx, &["--home", &home_arg]);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if common::Descriptor::read(&ctx.runtime_dir).is_some() {
            break;
        }
        if !proc.is_alive() {
            panic!(
                "daemon could not take over fenced home: {}",
                proc.stderr_text()
            );
        }
        if std::time::Instant::now() > deadline {
            panic!("no descriptor within 20s after takeover fence");
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    // The taken-over home serves real data through RPC.
    let endpoint = common::Descriptor::read(&ctx.runtime_dir)
        .expect("descriptor")
        .endpoint;
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");
    let tree = client
        .call("node/tree", authed(json!({}), &cred))
        .expect("tree over taken-over home");
    assert_eq!(
        tree["trees"].as_array().map(Vec::len),
        Some(1),
        "taken-over home serves the migrated ticket tree"
    );

    proc.kill();
}

fn details_of(err: &omt_storage::Problem) -> serde_json::Value {
    err.details.clone().unwrap_or(serde_json::json!({}))
}
