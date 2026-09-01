//! Ledgered schema migration v3→v4 (R8) behind a read-only future-schema
//! preflight.
//!
//! - Version detection: `PRAGMA user_version` first; legacy databases carry
//!   the TS marker `meta.schema_version` ('1'/'2'/'3'); a markerless database
//!   with legacy tables is v1-shaped. Anything ABOVE [`KNOWN_SCHEMA_VERSION`]
//!   is TOO NEW: the preflight opens strictly READ-ONLY (no write-capable
//!   pragma, no WAL checkpoint, no DDL), reports diagnostics, and fails with
//!   SCHEMA_TOO_NEW leaving db/-wal/-shm byte-identical.
//! - Migration runs under EXCLUSIVE ownership (BEGIN IMMEDIATE per step),
//!   writes one ledger row per step, and bumps user_version at each step.
//! - Fixture builders reconstruct the exact v1/v2/v3 DDL history of
//!   `src/host/store.ts` for lossless-migration tests.

use crate::clock::MillisClock;
use crate::files::sha256_hex;
use crate::store::{self, KNOWN_SCHEMA_VERSION, SCHEMA_V4_SQL};
use crate::{Problem, Result};
use omt_domain::error;
use rusqlite::{params, Connection, OpenFlags};

// ── detection ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedVersion {
    /// No file / empty database: create fresh at v4.
    Fresh,
    /// Legacy marker or reconstructed legacy shape: migrate n → 4.
    Legacy(i64),
    /// Already current.
    Current,
    /// Newer than this binary knows — fail closed.
    TooNew(i64),
}

/// Read-only version detection over an ALREADY-OPEN connection.
pub fn detect_version(conn: &Connection) -> Result<DetectedVersion> {
    let user_version = store::get_user_version(conn)?;
    if user_version > 0 {
        return Ok(classify(user_version));
    }
    // Legacy marker path (TS core never sets user_version).
    let marker = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    if let Some(text) = marker {
        return text.parse::<i64>().map_or_else(
            |_| {
                Err(Problem::with_details(
                    error::IO,
                    "unreadable schema_version marker",
                    |d| {
                        d.insert("schemaVersion".into(), text.clone().into());
                    },
                ))
            },
            |version| Ok(classify(version)),
        );
    }
    // Markerless: does any legacy table exist? (TS v1 homes may lack the
    // marker entirely until their first reindex.)
    let has_tables: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('nodes','edges','meta','nodes_search')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .unwrap_or(false);
    if has_tables {
        Ok(DetectedVersion::Legacy(1))
    } else {
        Ok(DetectedVersion::Fresh)
    }
}

fn classify(version: i64) -> DetectedVersion {
    match version.cmp(&KNOWN_SCHEMA_VERSION) {
        std::cmp::Ordering::Less => DetectedVersion::Legacy(version),
        std::cmp::Ordering::Equal => DetectedVersion::Current,
        std::cmp::Ordering::Greater => DetectedVersion::TooNew(version),
    }
}

/// STRICTLY read-only future-schema preflight (R8/AE7): open with
/// SQLITE_OPEN_READ_ONLY, run PRAGMA queries only, and fail SCHEMA_TOO_NEW
/// when the on-disk schema is newer than this binary. No journal_mode
/// changes, no checkpointing, no DDL — the database, its WAL/SHM sidecars,
/// and every other file stay byte-identical.
pub fn preflight_read_only(db_path: &std::path::Path) -> Result<DetectedVersion> {
    let conn =
        Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|err| {
            Problem::with_details(error::IO, format!("preflight open failed: {err}"), |d| {
                d.insert("db".into(), db_path.display().to_string().into());
            })
        })?;
    // Diagnostic-only queries (read-only connection enforces this):
    let detected = detect_version(&conn)?;
    drop(conn); // close WITHOUT any checkpoint (read-only handles cannot anyway)
    match detected {
        DetectedVersion::TooNew(found) => Err(Problem::with_details(
            error::SCHEMA_TOO_NEW,
            format!(
                "home schema v{found} is newer than this binary understands (v{KNOWN_SCHEMA_VERSION}); upgrade required"
            ),
            |d| {
                d.insert("foundSchemaVersion".into(), found.into());
                d.insert("knownSchemaVersion".into(), KNOWN_SCHEMA_VERSION.into());
            },
        )),
        other => Ok(other),
    }
}

// ── migration steps ─────────────────────────────────────────────────────

/// One ledgered migration step.
#[derive(Debug, Clone)]
pub struct MigrationStep {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// The v4 steps applied ON TOP of any v3 database. Each runs in its own
/// exclusive transaction with a ledger row + user_version bump.
pub fn migration_steps() -> &'static [MigrationStep] {
    MIGRATION_STEPS
}

static MIGRATION_STEPS: &[MigrationStep] = &[
    MigrationStep {
        version: 41,
        name: "0004-homes",
        sql: "CREATE TABLE homes (\n  home_id    TEXT PRIMARY KEY,\n  path       TEXT NOT NULL,\n  created_at TEXT NOT NULL\n);\nINSERT INTO homes (home_id, path, created_at)\n  VALUES (NULL, '', ''); -- placeholder replaced by the runner\n",
    },
    MigrationStep {
        version: 42,
        name: "0005-node-revisions",
        sql: "ALTER TABLE nodes ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;\nUPDATE nodes SET revision = 1;\n",
    },
    MigrationStep {
        version: 43,
        name: "0006-events-outbox",
        sql: "CREATE TABLE events (\n  seq INTEGER PRIMARY KEY AUTOINCREMENT,\n  home_id TEXT NOT NULL,\n  event_type TEXT NOT NULL,\n  payload TEXT NOT NULL,\n  created_at TEXT NOT NULL\n);\nCREATE INDEX idx_events_home ON events(home_id, seq);\n",
    },
    MigrationStep {
        version: 44,
        name: "0007-leases",
        sql: "CREATE TABLE leases (\n  token TEXT PRIMARY KEY,\n  session_id TEXT NOT NULL,\n  principal TEXT NOT NULL,\n  run_id TEXT,\n  node_id TEXT,\n  attempt INTEGER NOT NULL DEFAULT 0,\n  issued_at TEXT NOT NULL,\n  expires_ms INTEGER NOT NULL\n);\nCREATE INDEX idx_leases_session ON leases(session_id);\n",
    },
    MigrationStep {
        version: 45,
        name: "0008-operations-journal",
        sql: "CREATE TABLE operations (\n  command_id TEXT PRIMARY KEY,\n  op_kind TEXT NOT NULL,\n  input_hash TEXT NOT NULL,\n  result_json TEXT NOT NULL,\n  committed_at TEXT NOT NULL\n);\nCREATE TABLE journal (\n  op_id INTEGER PRIMARY KEY AUTOINCREMENT,\n  command_id TEXT NOT NULL UNIQUE,\n  op_kind TEXT NOT NULL,\n  phase TEXT NOT NULL,\n  input_hash TEXT NOT NULL,\n  plan_json TEXT NOT NULL,\n  created_at TEXT NOT NULL,\n  updated_at TEXT NOT NULL\n);\n",
    },
    MigrationStep {
        version: 46,
        name: "0009-quarantine-snapshots",
        sql: "CREATE TABLE quarantined_nodes (\n  node_id TEXT PRIMARY KEY,\n  snapshot_json TEXT NOT NULL,\n  reason TEXT NOT NULL,\n  quarantined_at TEXT NOT NULL\n);\n",
    },
];

/// Legacy-shape compatibility steps (the reconstructed v1→v2→v3 history of
/// src/host/store.ts). Raw v1/v2 databases reach v4 THROUGH them so every
/// intermediate table/column exists exactly once.
pub const V2_COMPAT_STEP: MigrationStep = MigrationStep {
    version: 2,
    name: "0002-archive-column",
    sql: "ALTER TABLE nodes ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;\nUPDATE nodes SET archived = 1, status = 'open' WHERE status = 'archived';\n",
};

pub const V3_COMPAT_STEP: MigrationStep = MigrationStep {
    version: 3,
    name: "0003-runs-tables",
    sql: "CREATE TABLE IF NOT EXISTS runs (\n  id          TEXT PRIMARY KEY,\n  title       TEXT,\n  status      TEXT NOT NULL DEFAULT 'pending',\n  config      TEXT NOT NULL,\n  created_at  TEXT NOT NULL,\n  finished_at TEXT\n);\nCREATE TABLE IF NOT EXISTS run_items (\n  run_id              TEXT NOT NULL REFERENCES runs(id),\n  node_id             TEXT NOT NULL,\n  position            INTEGER NOT NULL,\n  state               TEXT NOT NULL DEFAULT 'pending',\n  executor_session_id TEXT,\n  attempts            INTEGER NOT NULL DEFAULT 0,\n  last_error          TEXT,\n  nudged_at           TEXT,\n  nudge_count         INTEGER NOT NULL DEFAULT 0,\n  started_at          TEXT,\n  finished_at         TEXT,\n  PRIMARY KEY (run_id, node_id)\n);\nCREATE INDEX IF NOT EXISTS idx_run_items_node ON run_items(node_id);\n",
};

/// The full ordered step list a database starting at `from` must walk.
pub fn steps_for_legacy(from: i64) -> Vec<&'static MigrationStep> {
    let mut steps: Vec<&'static MigrationStep> = Vec::new();
    if from <= 1 {
        steps.push(&V2_COMPAT_STEP);
        steps.push(&V3_COMPAT_STEP);
    } else if from == 2 {
        steps.push(&V3_COMPAT_STEP);
    }
    steps.extend(migration_steps().iter());
    steps
}

/// Migrate a legacy database to v4. Caller must hold EXCLUSIVE ownership
/// (kernel home lock). Every step: BEGIN IMMEDIATE → DDL/backfill → ledger
/// row → user_version bump → COMMIT; then post-migrate invariant checks are
/// the caller's job ([`crate::invariants`] against a before-snapshot).
pub fn migrate_to_current(
    conn: &mut Connection,
    home_path: &str,
    clock: &dyn MillisClock,
    home_id: Option<&str>,
) -> Result<MigrationReport> {
    let detected = detect_version(conn)?;
    match detected {
        DetectedVersion::Current => Ok(MigrationReport {
            from: KNOWN_SCHEMA_VERSION,
            to: KNOWN_SCHEMA_VERSION,
            steps: vec![],
        }),
        DetectedVersion::TooNew(found) => Err(Problem::with_details(
            error::SCHEMA_TOO_NEW,
            format!("refusing to touch schema v{found} newer than known v{KNOWN_SCHEMA_VERSION}"),
            |d| {
                d.insert("foundSchemaVersion".into(), found.into());
                d.insert("knownSchemaVersion".into(), KNOWN_SCHEMA_VERSION.into());
            },
        )),
        DetectedVersion::Fresh => {
            conn.execute_batch(SCHEMA_V4_SQL)
                .map_err(store::sql_err("fresh v4 ddl"))?;
            let now_iso = crate::clock::iso_from_ms(clock.now_ms());
            let home_id_owned = store::ensure_home_row(conn, home_path, &now_iso, home_id)?;
            seed_ledger(conn, &now_iso)?;
            store::set_user_version(conn, KNOWN_SCHEMA_VERSION)?;
            Ok(MigrationReport {
                from: 0,
                to: KNOWN_SCHEMA_VERSION,
                steps: vec![format!("fresh-v4 ({home_id_owned})")],
            })
        }
        DetectedVersion::Legacy(from) => {
            let mut report = MigrationReport {
                from,
                to: KNOWN_SCHEMA_VERSION,
                steps: Vec::new(),
            };
            let now_iso = crate::clock::iso_from_ms(clock.now_ms());
            // Bootstrap: the LEDGER itself must exist before the first
            // ledgered step records it.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS migrations_ledger (
                   version    INTEGER PRIMARY KEY,
                   name       TEXT NOT NULL,
                   checksum   TEXT NOT NULL,
                   applied_at TEXT NOT NULL
                 );",
            )
            .map_err(store::sql_err("ledger bootstrap"))?;
            for step in steps_for_legacy(from) {
                apply_step(conn, step, &now_iso)?;
                report.steps.push(step.name.to_string());
            }
            // Home identity assignment happens once, inside the last txn;
            // then the schema version marker lands at CURRENT exactly once.
            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(store::sql_err("homes begin"))?;
            let outcome = (|| -> Result<()> {
                store::ensure_home_row(conn, home_path, &now_iso, home_id)?;
                store::set_user_version(conn, KNOWN_SCHEMA_VERSION)?;
                Ok(())
            })();
            store::finish_txn(conn, outcome)?;
            report.to = KNOWN_SCHEMA_VERSION;
            Ok(report)
        }
    }
}

fn apply_step(conn: &Connection, step: &MigrationStep, now_iso: &str) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(store::sql_err("step begin"))?;
    let outcome = (|| -> Result<()> {
        let expanded = expand_step(step);
        conn.execute_batch(&expanded)
            .map_err(store::sql_err(step.name))?;
        conn.execute(
            "INSERT INTO migrations_ledger (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
            params![step.version, step.name, sha256_hex(step.sql), now_iso],
        )
        .map_err(store::sql_err("ledger insert"))?;
        Ok(())
    })();
    store::finish_txn(conn, outcome)
}

/// The homes placeholder row needs the real identity at insert time; every
/// other step's SQL applies verbatim.
fn expand_step(step: &MigrationStep) -> String {
    if step.name == "0004-homes" {
        // The placeholder INSERT is stripped; ensure_home_row assigns the id
        // later inside the migration's final transaction.
        return step
            .sql
            .lines()
            .filter(|line| !line.trim_start().starts_with("INSERT INTO homes"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    step.sql.to_string()
}

fn seed_ledger(conn: &Connection, now_iso: &str) -> Result<()> {
    for step in migration_steps() {
        conn.execute(
            "INSERT OR IGNORE INTO migrations_ledger (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
            params![step.version, step.name, sha256_hex(step.sql), now_iso],
        )
        .map_err(store::sql_err("ledger seed"))?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct MigrationReport {
    pub from: i64,
    pub to: i64,
    pub steps: Vec<String>,
}

// ── fixtures (checked-in SQL builders) ──────────────────────────────────

/// Fixture builders reproducing the EXACT DDL history of src/host/store.ts:
///
/// - v1 (`8de29e9`): nodes WITHOUT `archived` (status carried 'archived'),
///   edges/meta/nodes_search; no schema_version marker unless written.
/// - v2 (`77c1e47` era): + `archived` column, marker 'schema_version'='2'.
/// - v3 (`55f5046`): + runs/run_items tables, marker '3'.
pub mod fixtures {
    use rusqlite::{params, Connection};

    pub const V1_DDL: &str = r#"
CREATE TABLE nodes (
  id         TEXT PRIMARY KEY,
  type       TEXT NOT NULL,
  title      TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'open',
  priority   INTEGER NOT NULL DEFAULT 0,
  path       TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE edges (
  parent_id  TEXT NOT NULL REFERENCES nodes(id),
  child_id   TEXT NOT NULL REFERENCES nodes(id),
  ord        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (parent_id, child_id)
);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE nodes_search (id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL);
"#;

    pub const V2_MIGRATION_SQL: &str = r#"
ALTER TABLE nodes ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
UPDATE nodes SET archived = 1, status = 'open' WHERE status = 'archived';
"#;

    pub const V3_ADDITION_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS runs (
  id          TEXT PRIMARY KEY,
  title       TEXT,
  status      TEXT NOT NULL DEFAULT 'pending',
  config      TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  finished_at TEXT
);
CREATE TABLE IF NOT EXISTS run_items (
  run_id              TEXT NOT NULL REFERENCES runs(id),
  node_id             TEXT NOT NULL,
  position            INTEGER NOT NULL,
  state               TEXT NOT NULL DEFAULT 'pending',
  executor_session_id TEXT,
  attempts            INTEGER NOT NULL DEFAULT 0,
  last_error          TEXT,
  nudged_at           TEXT,
  nudge_count         INTEGER NOT NULL DEFAULT 0,
  started_at          TEXT,
  finished_at         TEXT,
  PRIMARY KEY (run_id, node_id)
);
CREATE INDEX IF NOT EXISTS idx_run_items_node ON run_items(node_id);
"#;

    /// Build a v1-shape database (marker optional — true v1 homes had none).
    pub fn build_v1(db_path: &std::path::Path, with_marker: bool) -> Connection {
        let conn = Connection::open(db_path).expect("fixture v1 open");
        conn.pragma_update(None, "journal_mode", "DELETE")
            .expect("journal mode");
        conn.execute_batch(V1_DDL).expect("v1 ddl");
        if with_marker {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', '1')",
                [],
            )
            .expect("marker");
        }
        conn
    }

    /// Build a v2-shape database (archived column + marker '2').
    pub fn build_v2(db_path: &std::path::Path) -> Connection {
        let conn = build_v1(db_path, false);
        conn.execute_batch(V2_MIGRATION_SQL).expect("v2 migration");
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', '2')",
            [],
        )
        .expect("marker");
        conn
    }

    /// Build a v3-shape database (runs tables + marker '3').
    pub fn build_v3(db_path: &std::path::Path) -> Connection {
        let conn = build_v2(db_path);
        conn.execute_batch(V3_ADDITION_SQL).expect("v3 addition");
        conn.execute(
            "UPDATE meta SET value = '3' WHERE key = 'schema_version'",
            [],
        )
        .expect("marker");
        conn
    }

    /// Seed one realistic v3 home: CJK titles, hierarchy, counters, search
    /// mirror, one run with mixed item states/attempts/nudges.
    pub fn seed_realistic_home(conn: &Connection) {
        // Column order matches the historical DDL: …, created_at, updated_at,
        // archived (appended LAST by the v2 ALTER).
        conn.execute_batch(
            r#"
INSERT INTO nodes VALUES ('EPIC-0001', 'epic', '旧项目 重构', 'in_progress', 2, 'tickets/EPIC-0001-dai-zhong-gou/EPIC.md', '2026-07-01T00:00:00.000Z', '2026-08-01T00:00:00.000Z', 0);
INSERT INTO nodes VALUES ('STORY-0001', 'story', '存储层迁移', 'done', 1, 'tickets/EPIC-0001-dai-zhong-gou/STORY-0001-cun-chu-ceng-qian-yi/STORY.md', '2026-07-02T00:00:00.000Z', '2026-08-02T00:00:00.000Z', 0);
INSERT INTO nodes VALUES ('TICKET-0001', 'ticket', 'Write schema 🎌', 'open', 0, 'tickets/EPIC-0001-dai-zhong-gou/STORY-0001-cun-chu-ceng-qian-yi/TICKET-0001-write-schema/TICKET.md', '2026-07-03T00:00:00.000Z', '2026-08-03T00:00:00.000Z', 0);
INSERT INTO nodes VALUES ('TICKET-0002', 'ticket', 'Backfill revisions', 'blocked', 0, 'tickets/ticket-0002-backfill-revisions/TICKET.md', '2026-07-04T00:00:00.000Z', '2026-08-04T00:00:00.000Z', 1);
"#,
        )
        .expect("seed nodes");
        conn.execute_batch(
            r#"
INSERT INTO edges VALUES ('EPIC-0001', 'STORY-0001', 0);
INSERT INTO edges VALUES ('STORY-0001', 'TICKET-0001', 0);
INSERT INTO meta VALUES ('counter_EPIC', '1');
INSERT INTO meta VALUES ('counter_STORY', '1');
INSERT INTO meta VALUES ('counter_TICKET', '2');
INSERT INTO meta VALUES ('counter_RUN', '1');
INSERT INTO nodes_search VALUES ('EPIC-0001', '旧项目 重构', '重构目标');
INSERT INTO nodes_search VALUES ('TICKET-0001', 'Write schema 🎌', 'schema body');
"#,
        )
        .expect("seed edges/meta/search");
        conn.execute(
            "INSERT INTO runs (id, title, status, config, created_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "RUN-0001",
                "first run",
                "paused",
                r#"{"stopOnFailure":true,"autoContinue":false,"autoVerify":true,"concurrency":1}"#,
                "2026-08-05T00:00:00.000Z",
                Option::<String>::None,
            ],
        )
        .expect("seed run");
        conn.execute_batch(
            r#"
INSERT INTO run_items VALUES ('RUN-0001', 'TICKET-0001', 0, 'done', 'sess-a', 2, 'older failure note', '2026-08-06T00:00:00.000Z', 1, '2026-08-05T01:00:00.000Z', '2026-08-06T01:00:00.000Z');
INSERT INTO run_items VALUES ('RUN-0001', 'TICKET-0002', 1, 'running', 'sess-b', 1, NULL, NULL, 0, '2026-08-05T02:00:00.000Z', NULL);
"#,
        )
        .expect("seed items");
    }

    /// Build a TOO-NEW database (user_version far beyond known) with a table
    /// only that future would have.
    pub fn build_too_new(db_path: &std::path::Path) -> Connection {
        let conn = Connection::open(db_path).expect("fixture too-new open");
        conn.execute_batch("CREATE TABLE future_table (x TEXT);")
            .expect("future ddl");
        conn.pragma_update(None, "user_version", 99)
            .expect("user_version");
        conn
    }
}
