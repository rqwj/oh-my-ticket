//! Known-homes catalog (runtime-level persistence): which directories on
//! this machine are omt-manageable, surviving daemon generations.
//!
//! Placement ruling: the catalog is DAEMON runtime metadata, so it lives
//! under the runtime dir (`<runtime>/known-homes.db`), NEVER inside a
//! home's own store (homes are per-project data; embedding the catalog
//! there would pollute backup/takeover semantics and create a
//! open-a-home-to-learn-about-homes cycle).
//!
//! Identity ruling: the persistent key is the CANONICAL PATH. A home's id
//! is stored INSIDE the home's own database, so reopening the same path
//! keeps the same id across daemon generations (ids only change when the
//! home store is wiped/recreated); `last_home_id` is informational —
//! nothing joins on it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use omt_storage::Problem;
use rusqlite::Connection;

const DB_FILE: &str = "known-homes.db";
const KNOWN_SCHEMA_VERSION: i64 = 1;

/// One catalog row.
#[derive(Debug, Clone)]
pub struct KnownHomeEntry {
    pub canonical_path: PathBuf,
    pub name: String,
    pub kind: String,
    /// Last homeId this path was opened under (informational — stable
    /// while the home's own store persists; changes only on wipe/recreate).
    pub last_home_id: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

#[derive(Clone)]
pub struct KnownHomes {
    conn: Arc<Mutex<Connection>>,
}

impl KnownHomes {
    /// Open (creating on first boot) the catalog under the runtime dir.
    pub fn open(runtime_dir: &Path) -> Result<KnownHomes, Problem> {
        let path = runtime_dir.join(DB_FILE);
        let conn = Connection::open(&path).map_err(|err| {
            Problem::new(
                "IO",
                format!("known-homes db open {}: {err}", path.display()),
            )
        })?;
        let user_version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|err| Problem::new("IO", format!("known-homes user_version: {err}")))?;
        if user_version > KNOWN_SCHEMA_VERSION {
            return Err(Problem::new(
                "SCHEMA_TOO_NEW",
                format!(
                    "known-homes.db schema v{user_version} is newer than this build's v{KNOWN_SCHEMA_VERSION}"
                ),
            ));
        }
        if user_version < KNOWN_SCHEMA_VERSION {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS known_homes (
                    canonical_path TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    last_home_id TEXT NOT NULL,
                    first_seen_at TEXT NOT NULL,
                    last_seen_at TEXT NOT NULL
                );
                PRAGMA user_version = 1;",
            )
            .map_err(|err| Problem::new("IO", format!("known-homes migrate: {err}")))?;
        }
        Ok(KnownHomes {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Upsert one entry on successful open/declare: refresh name/kind/
    /// last_home_id/last_seen_at, preserve first_seen_at.
    pub fn record(
        &self,
        canonical_path: &Path,
        name: &str,
        kind: &str,
        home_id: &str,
        now_iso: &str,
    ) -> Result<(), Problem> {
        let conn = self.conn.lock().expect("known-homes");
        conn.execute(
            "INSERT INTO known_homes
                (canonical_path, name, kind, last_home_id, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(canonical_path) DO UPDATE SET
                name = excluded.name,
                kind = excluded.kind,
                last_home_id = excluded.last_home_id,
                last_seen_at = excluded.last_seen_at",
            rusqlite::params![
                canonical_path.display().to_string(),
                name,
                kind,
                home_id,
                now_iso,
            ],
        )
        .map_err(|err| Problem::new("IO", format!("known-homes record: {err}")))?;
        Ok(())
    }

    /// All rows, oldest-first by first_seen (stable picker ordering).
    pub fn list(&self) -> Result<Vec<KnownHomeEntry>, Problem> {
        let conn = self.conn.lock().expect("known-homes");
        let mut stmt = conn
            .prepare(
                "SELECT canonical_path, name, kind, last_home_id, first_seen_at, last_seen_at
                 FROM known_homes ORDER BY first_seen_at ASC, canonical_path ASC",
            )
            .map_err(|err| Problem::new("IO", format!("known-homes list prepare: {err}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(KnownHomeEntry {
                    canonical_path: PathBuf::from(row.get::<_, String>(0)?),
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    last_home_id: row.get(3)?,
                    first_seen_at: row.get(4)?,
                    last_seen_at: row.get(5)?,
                })
            })
            .map_err(|err| Problem::new("IO", format!("known-homes list: {err}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| Problem::new("IO", format!("known-homes row: {err}")))?);
        }
        Ok(out)
    }
}
