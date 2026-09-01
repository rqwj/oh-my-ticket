//! Migration suite (plan U4 / AE4-AE6): v1/v2/v3 fixtures migrate losslessly
//! with ledger rows; a TOO-NEW schema fails closed via the strictly read-only
//! preflight leaving db/-wal/-shm byte-identical; post-migrate invariants
//! hold against full before/after snapshots.

#[path = "common/mod.rs"]
mod common;

use common::*;
use omt_storage::clock::iso_from_ms;
use omt_storage::invariants::{assert_preserved, snapshot};
use omt_storage::migrate::{self, fixtures, DetectedVersion};
use omt_storage::store;
use std::path::Path;

fn open_conn(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).expect("open fixture conn")
}

/// Byte-hash of db + sidecars — the too-new byte-stability witness.
fn file_hashes(db_path: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    for suffix in ["", "-wal", "-shm"] {
        let text = format!("{}{suffix}", db_path.display());
        let path = Path::new(&text);
        match std::fs::read(path) {
            Ok(bytes) => out.push((
                suffix.to_string(),
                omt_storage::files::sha256_hex_bytes(&bytes),
            )),
            Err(_) => out.push((suffix.to_string(), "<absent>".to_string())),
        }
    }
    out.sort();
    out
}

#[test]
fn v1_fixture_migrates_losslessly() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("omt.db");
    {
        let conn = fixtures::build_v1(&db, true);
        // v1 shape: status carried 'archived' (no column yet).
        conn.execute_batch(
            r#"
INSERT INTO nodes VALUES ('EPIC-0001', 'epic', '旧项目', 'archived', 0, 'tickets/x/EPIC.md', 't0', 't0');
INSERT INTO nodes VALUES ('TICKET-0001', 'ticket', 'plain', 'open', 3, 'tickets/y/TICKET.md', 't0', 't1');
INSERT INTO edges VALUES ('EPIC-0001', 'TICKET-0001', 0);
INSERT INTO meta VALUES ('counter_EPIC', '1');
INSERT INTO nodes_search VALUES ('TICKET-0001', 'plain', 'body text');
"#,
        )
        .expect("v1 seed");
    }
    let mut conn = open_conn(&db);
    assert_eq!(
        migrate::detect_version(&conn).unwrap(),
        DetectedVersion::Legacy(1)
    );
    let mut before = snapshot(&conn).expect("before snapshot");
    // The ONE documented v1→v2 semantic rewrite (status='archived' becomes
    // archived=1 + status='open'): normalize the BEFORE snapshot so the
    // lossless comparison asserts everything ELSE is untouched.
    for node in &mut before.nodes {
        if node[3] == serde_json::Value::String("archived".into()) {
            node[3] = serde_json::Value::String("open".into());
            node[4] = serde_json::Value::Number(1.into());
        }
    }

    let report = migrate::migrate_to_current(
        &mut conn,
        dir.path().to_str().unwrap(),
        &*fixed_clock(),
        None,
    )
    .expect("migration succeeds");
    assert_eq!(report.from, 1);
    assert_eq!(report.to, 4);
    assert_eq!(report.steps.len(), migrate::steps_for_legacy(1).len());

    let after = snapshot(&conn).expect("after snapshot");
    assert_preserved(&before, &after).expect("lossless");

    // The archived-status row became archived=1 + status='open' (the ONE
    // documented v2 semantic rewrite).
    let archived: i64 = conn
        .query_row("SELECT archived FROM nodes WHERE id='EPIC-0001'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let status: String = conn
        .query_row("SELECT status FROM nodes WHERE id='EPIC-0001'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!((archived, status.as_str()), (1, "open"));
    // Revisions backfilled.
    let revision: i64 = conn
        .query_row(
            "SELECT revision FROM nodes WHERE id='TICKET-0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revision, 1);
    // Home identity assigned once, pattern-valid.
    let home_id: String = conn
        .query_row("SELECT home_id FROM homes", [], |r| r.get(0))
        .unwrap();
    assert!(
        home_id.starts_with("h_") && home_id.len() >= 8,
        "home id {home_id}"
    );
    // Ledger rows per step + user_version current.
    let ledger: i64 = conn
        .query_row("SELECT COUNT(*) FROM migrations_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ledger as usize, migrate::steps_for_legacy(1).len());
    assert_eq!(store::get_user_version(&conn).unwrap(), 4);
    // New tables exist and are empty.
    for table in [
        "events",
        "leases",
        "operations",
        "journal",
        "quarantined_nodes",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or(-1);
        assert_eq!(count, 0, "table {table} not empty after migration");
    }
}

#[test]
fn v2_fixture_migrates_losslessly() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("omt.db");
    {
        let conn = fixtures::build_v2(&db);
        conn.execute_batch(
            r#"
INSERT INTO nodes VALUES ('STORY-0001', 'story', '迁移故事', 'done', 1, 'tickets/s/STORY.md', 'a', 'b', 1);
INSERT INTO meta VALUES ('counter_STORY', '1');
"#,
        )
        .expect("v2 seed");
    }
    let mut conn = open_conn(&db);
    assert_eq!(
        migrate::detect_version(&conn).unwrap(),
        DetectedVersion::Legacy(2)
    );
    let before = snapshot(&conn).unwrap();
    migrate::migrate_to_current(
        &mut conn,
        dir.path().to_str().unwrap(),
        &*fixed_clock(),
        None,
    )
    .expect("migrate v2→4");
    assert_preserved(&before, &snapshot(&conn).unwrap()).expect("v2 lossless");
    let archived: i64 = conn
        .query_row(
            "SELECT archived FROM nodes WHERE id='STORY-0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived, 1);
}

#[test]
fn v3_fixture_migrates_runs_and_attempts_losslessly() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("omt.db");
    {
        let conn = fixtures::build_v3(&db);
        fixtures::seed_realistic_home(&conn);
    }
    let mut conn = open_conn(&db);
    assert_eq!(
        migrate::detect_version(&conn).unwrap(),
        DetectedVersion::Legacy(3)
    );
    let before = snapshot(&conn).unwrap();

    migrate::migrate_to_current(
        &mut conn,
        dir.path().to_str().unwrap(),
        &*fixed_clock(),
        Some("h_fixed123"),
    )
    .expect("migrate v3→4");

    let after = snapshot(&conn).unwrap();
    assert_preserved(&before, &after).expect("v3 lossless incl. runs/attempts/counters");

    // Explicit spot checks beyond the structural snapshot.
    let attempts: i64 = conn
        .query_row(
            "SELECT attempts FROM run_items WHERE run_id='RUN-0001' AND node_id='TICKET-0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 2, "attempts preserved");
    let nudges: i64 = conn
        .query_row(
            "SELECT nudge_count FROM run_items WHERE run_id='RUN-0001' AND node_id='TICKET-0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nudges, 1);
    let home_id: String = conn
        .query_row("SELECT home_id FROM homes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(home_id, "h_fixed123", "explicit id honored");
    let counter_run: Option<String> = store::get_meta(&conn, "counter_RUN").unwrap();
    assert_eq!(counter_run.as_deref(), Some("1"), "counters preserved");
}

#[test]
fn fresh_home_is_born_at_v4_with_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("omt.db");
    {
        let conn = open_conn(&db);
        store::apply_open_pragmas(&conn).unwrap();
        store::create_fresh_v4(
            &conn,
            &iso_from_ms(T0_MS),
            dir.path().to_str().unwrap(),
            None,
        )
        .expect("fresh v4");
    }
    let conn = open_conn(&db);
    assert_eq!(
        migrate::detect_version(&conn).unwrap(),
        DetectedVersion::Current
    );
    let ledger: i64 = conn
        .query_row("SELECT COUNT(*) FROM migrations_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ledger as usize, migrate::steps_for_legacy(1).len());
}

#[test]
fn too_new_schema_fails_closed_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("omt.db");
    {
        let _conn = fixtures::build_too_new(&db);
    }
    let before = file_hashes(&db);

    let problem = migrate::preflight_read_only(&db).expect_err("must refuse");
    assert_eq!(problem.code, "SCHEMA_TOO_NEW");
    let details = problem.details.expect("details");
    assert_eq!(details["foundSchemaVersion"], 99);

    // Byte-identical: database AND sidecars untouched by the refusal.
    assert_eq!(file_hashes(&db), before, "preflight mutated bytes");

    // A write-capable open also refuses BEFORE any DDL runs.
    let mut conn = open_conn(&db);
    let problem = migrate::migrate_to_current(&mut conn, "", &*fixed_clock(), None)
        .expect_err("migration refuses");
    assert_eq!(problem.code, "SCHEMA_TOO_NEW");
    assert_eq!(file_hashes(&db), before);
}

#[test]
fn too_new_preflight_leaves_wal_sidecar_untouched_while_hot() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("omt.db");
    {
        let _too_new = fixtures::build_too_new(&db);
    }
    // Keep a live writer so -wal/-shm exist during the preflight.
    let hot = open_conn(&db);
    hot.execute_batch("CREATE TABLE marker (x TEXT); INSERT INTO marker VALUES ('hot');")
        .unwrap();
    drop(hot); // clean close removes wal/shm; reopen + keep open instead:
    let hot = open_conn(&db);
    hot.pragma_update(None, "journal_mode", "WAL").unwrap();
    hot.execute_batch("CREATE TABLE marker2 (x TEXT);").unwrap();

    let before = file_hashes(&db);
    let problem = migrate::preflight_read_only(&db).expect_err("refuses while hot");
    assert_eq!(problem.code, "SCHEMA_TOO_NEW");
    let after = file_hashes(&db);
    // A strictly read-only connection cannot write frames, checkpoint, or
    // run DDL: database and WAL stay byte-identical. Documented caveat: the
    // -shm INDEX carries SQLite-internal reader slots that even read-only
    // attachers mark presence in — data bytes are untouched (see report).
    let pick = |hashes: &[(String, String)], suffix: &str| {
        hashes
            .iter()
            .find(|(s, _)| s == suffix)
            .map(|(_, h)| h.clone())
            .unwrap()
    };
    assert_eq!(
        pick(&after, ""),
        pick(&before, ""),
        "database bytes mutated"
    );
    assert_eq!(
        pick(&after, "-wal"),
        pick(&before, "-wal"),
        "WAL bytes mutated"
    );
    assert_eq!(problem.details.unwrap()["foundSchemaVersion"], 99);
    drop(hot);
}
