//! Whole-home backup bundle + restore drill (R8/U4c):
//!   * the SQLite online backup API captures committed-but-uncheckpointed
//!     WAL content (the drill destroys the original home entirely and
//!     verifies every row comes back);
//!   * a crash mid-backup leaves the destination unusable while the source
//!     stays untouched;
//!   * restore verifies hashes BEFORE writing, rebuilds the tree, installs
//!     the DB, and passes integrity_check.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::*;
use omt_storage::backup::{
    backup_home, list_home_markdown, restore_home, verify_bundle, BackupManifest, DB_BUNDLE_NAME,
    FILES_DIR_NAME, MANIFEST_NAME,
};
use omt_storage::clock::MillisClock;
use omt_storage::files::sha256_hex_bytes;
use omt_storage::{FixedClock, Storage};

fn hash_tree(home: &std::path::Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for rel in list_home_markdown(home) {
        let bytes = std::fs::read(home.join(&rel)).expect("read tree file");
        out.push((rel, sha256_hex_bytes(&bytes)));
    }
    let db = std::fs::read(home.join("omt.db")).expect("read db");
    out.push(("omt.db".to_string(), sha256_hex_bytes(&db)));
    let wal = std::fs::read(home.join("omt.db-wal")).unwrap_or_default();
    out.push(("omt.db-wal".to_string(), sha256_hex_bytes(&wal)));
    out.sort();
    out
}

/// THE drill: rows written but never checkpointed survive a total loss of
/// the original home because the backup API reads through the WAL.
#[test]
fn backup_captures_uncheckpointed_wal_and_restore_rebuilds_the_home() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home) = temp_home();
    let mut storage = open_storage(&home, &clock);

    // Seed: epic root + one ticket under it.
    let epic = node_row(
        "EPIC-0001",
        "epic",
        "备份演练 Epic",
        "open",
        "tickets/EPIC-0001-backup-drill/epic.md",
    );
    execute_simple(&mut storage, "cmd-seed-1", &epic, None, "# 根节点");
    let ticket = node_row(
        "TICKET-0001",
        "ticket",
        "未 checkpoint 的行",
        "in_progress",
        "tickets/EPIC-0001-backup-drill/TICKET-0001-uncheckpointed/ticket.md",
    );
    execute_simple(&mut storage, "cmd-seed-2", &ticket, Some(&epic), "正文内容");

    // The WAL carries uncheckpointed frames at this point (small writes stay
    // below SQLite's auto-checkpoint threshold; no explicit checkpoint runs).
    assert!(home.join("omt.db-wal").exists(), "wal file present");

    let dest_guard = tempfile::tempdir().expect("dest tempdir");
    let dest = dest_guard.path().join("bundle");

    // Keep `storage` (a live writer connection) OPEN during the backup —
    // this is what keeps the WAL uncheckpointed. Then destroy the original
    // home directory INCLUDING the live wal/shm files.
    let any_clock: Arc<dyn MillisClock> = clock.clone();
    let manifest = {
        let m = backup_home(&home, &dest, any_clock.as_ref()).expect("backup");
        drop(std::fs::remove_dir_all(&home));
        m
    };
    drop(storage); // writer handle now points at a deleted tree

    // The bundle recorded both Markdown files and a hashed DB.
    assert_eq!(
        manifest.files.len(),
        2,
        "manifest covers every markdown file"
    );
    assert_eq!(manifest.db_sha256.len(), 64);
    assert!(manifest.home_id.is_some());

    // Restore into a fresh location; verify + integrity_check run inside.
    let target_guard = tempfile::tempdir().expect("target tempdir");
    let target = target_guard.path().join("restored-home");
    let restored_manifest: BackupManifest = restore_home(&dest, &target).expect("restore");

    // Manifest identity survives round-trip.
    assert_eq!(restored_manifest, manifest);

    // Every row is back — including the ones that lived only in the WAL.
    let reopened = open_storage(&target, &clock);
    let mut ids = store_list_ids(reopened.conn());
    ids.sort();
    assert_eq!(
        ids,
        vec!["EPIC-0001".to_string(), "TICKET-0001".to_string()],
        "uncheckpointed rows survived the drill"
    );
    let restored_body = std::fs::read_to_string(target.join(&ticket.path)).expect("restored md");
    assert!(restored_body.contains("正文内容"));
    let integrity: String = reopened
        .conn()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity_check");
    assert_eq!(integrity, "ok");
}

fn store_list_ids(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn.prepare("SELECT id FROM nodes").expect("stmt");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("map");
    rows.filter_map(Result::ok).collect()
}

/// Crash mid-backup: dest has partial content but NO manifest → restore
/// refuses; the source home is byte-for-byte untouched by any backup step.
#[test]
fn crash_mid_backup_leaves_dest_unusable_and_source_untouched() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home) = temp_home();
    let mut storage = open_storage(&home, &clock);
    let epic = node_row(
        "EPIC-0001",
        "epic",
        "crash 演练",
        "open",
        "tickets/EPIC-0001-crash-drill/epic.md",
    );
    execute_simple(&mut storage, "cmd-crash-1", &epic, None, "根正文");

    let before = hash_tree(&home);

    // Simulate a crash after step 1 (db copied) but before the manifest:
    // build a partial bundle by hand — same layout backup_home produces
    // before its final atomic manifest promotion.
    let dest_guard = tempfile::tempdir().expect("dest");
    let dest = dest_guard.path().join("bundle");
    std::fs::create_dir_all(dest.join(FILES_DIR_NAME)).expect("bundle dirs");
    std::fs::copy(home.join("omt.db"), dest.join(DB_BUNDLE_NAME)).expect("partial db copy");

    // Restore refuses: no readable manifest.
    let target_guard = tempfile::tempdir().expect("target");
    let err = expect_problem(
        restore_home(&dest, target_guard.path().join("new-home").as_path()),
        "IO",
    );
    assert!(
        err.message.contains("manifest"),
        "refusal names the missing manifest: {err}"
    );

    // Source untouched: identical hashes for every tree file + db + wal.
    let after = hash_tree(&home);
    assert_eq!(before, after, "backup steps never mutate the source home");
}

/// Tampering with ANY bundled artifact is caught by hash verification.
#[test]
fn bundle_verification_detects_tampering() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home) = temp_home();
    let mut storage = open_storage(&home, &clock);
    let epic = node_row(
        "EPIC-0001",
        "epic",
        "tamper 演练",
        "open",
        "tickets/EPIC-0001-tamper/epic.md",
    );
    execute_simple(&mut storage, "cmd-tamper-1", &epic, None, "原始正文");

    let dest_guard = tempfile::tempdir().expect("dest");
    let dest = dest_guard.path().join("bundle");
    let any_clock: Arc<dyn MillisClock> = clock.clone();
    backup_home(&home, &dest, any_clock.as_ref()).expect("backup");

    // 1. File tampering.
    let bundled_md = dest.join(FILES_DIR_NAME).join(&epic.path);
    std::fs::write(&bundled_md, "# 篡改后的内容\n").expect("tamper md");
    expect_problem(verify_bundle(&dest), "IO");
    let original = std::fs::read(home.join(&epic.path)).expect("orig bytes");
    std::fs::write(&bundled_md, original).expect("restore md");

    // 2. DB tampering.
    let bundled_db = dest.join(DB_BUNDLE_NAME);
    let mut bytes = std::fs::read(&bundled_db).expect("db bytes");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&bundled_db, &bytes).expect("tamper db");
    expect_problem(verify_bundle(&dest), "IO");

    // 3. Missing manifest at all.
    std::fs::remove_file(dest.join(MANIFEST_NAME)).expect("drop manifest");
    expect_problem(verify_bundle(&dest), "IO");
}

/// A valid bundle restores into a fresh home whose Storage reopens cleanly
/// with the same home id (identity travels in the homes table).
#[test]
fn restored_home_keeps_its_identity_and_reopens_as_storage() {
    let clock: Arc<FixedClock> = fixed_clock();
    let (_dir, home) = temp_home();
    let mut storage = open_storage(&home, &clock);
    let epic = node_row(
        "EPIC-0001",
        "epic",
        "identity 演练",
        "open",
        "tickets/EPIC-0001-identity/epic.md",
    );
    execute_simple(&mut storage, "cmd-id-1", &epic, None, "身份正文");
    let original_home_id: String = storage
        .conn()
        .query_row("SELECT home_id FROM homes LIMIT 1", [], |row| row.get(0))
        .expect("home id");
    drop(storage);

    let dest_guard = tempfile::tempdir().expect("dest");
    let dest = dest_guard.path().join("bundle");
    let any_clock: Arc<dyn MillisClock> = clock.clone();
    backup_home(&home, &dest, any_clock.as_ref()).expect("backup");

    let target_guard = tempfile::tempdir().expect("target");
    let target = target_guard.path().join("home");
    restore_home(&dest, &target).expect("restore");

    let any_clock: Arc<dyn MillisClock> = clock.clone();
    let reopened = Storage::open(open_config(&target, &any_clock)).expect("reopen restored home");
    let restored_home_id: String = reopened
        .conn()
        .query_row("SELECT home_id FROM homes LIMIT 1", [], |row| row.get(0))
        .expect("restored home id");
    assert_eq!(
        restored_home_id, original_home_id,
        "identity travels with the bundle"
    );
}
