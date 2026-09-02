//! SQLite metadata index (rusqlite, bundled SQLite): the v4 schema and the
//! row/query surface ported 1:1 from `src/host/store.ts` — every normative
//! ORDER BY clause is preserved (`nodes ORDER BY id`, children
//! `ORDER BY e.ord, e.child_id`, runs `ORDER BY id`, items
//! `ORDER BY position, node_id`, per-node items `ORDER BY run_id`).
//!
//! Open pragmas mirror the TS core plus durability for journal correctness:
//! `journal_mode=WAL`, `busy_timeout=5000`, `synchronous=FULL` (a finalize
//! transaction must be durable before its acknowledge), `foreign_keys=ON`.

use crate::files::{generate_home_id, sha256_hex};
use crate::{Problem, Result};
use omt_contracts::{NodeType, RunItemState, RunStatus};
use omt_domain::error;
use omt_domain::types::{
    is_run_member_node_type, type_prefix, EdgeRow, NodeRow, RunConfigValue, RunItemRow, RunRow,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

/// Latest known schema version of this binary (R8 preflight reference).
pub const KNOWN_SCHEMA_VERSION: i64 = 4;

/// v4 base DDL — everything a freshly created home needs. The legacy v3
/// tables keep their exact TS column shapes; new tables are additive.
pub const SCHEMA_V4_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS nodes (
  id         TEXT PRIMARY KEY,
  type       TEXT NOT NULL,
  title      TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'open',
  archived   INTEGER NOT NULL DEFAULT 0,
  priority   INTEGER NOT NULL DEFAULT 0,
  path       TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  revision   INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS edges (
  parent_id  TEXT NOT NULL REFERENCES nodes(id),
  child_id   TEXT NOT NULL REFERENCES nodes(id),
  ord        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (parent_id, child_id)
);
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS nodes_search (
  id    TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  body  TEXT NOT NULL
);
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
CREATE TABLE IF NOT EXISTS homes (
  home_id    TEXT PRIMARY KEY,
  path       TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  home_id    TEXT NOT NULL,
  event_type TEXT NOT NULL,
  payload    TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_home ON events(home_id, seq);
CREATE TABLE IF NOT EXISTS leases (
  token       TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL,
  principal   TEXT NOT NULL,
  run_id      TEXT,
  node_id     TEXT,
  attempt     INTEGER NOT NULL DEFAULT 0,
  issued_at   TEXT NOT NULL,
  expires_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_leases_session ON leases(session_id);
CREATE TABLE IF NOT EXISTS operations (
  command_id   TEXT PRIMARY KEY,
  op_kind      TEXT NOT NULL,
  input_hash   TEXT NOT NULL,
  result_json  TEXT NOT NULL,
  committed_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS journal (
  op_id       INTEGER PRIMARY KEY AUTOINCREMENT,
  command_id  TEXT NOT NULL UNIQUE,
  op_kind     TEXT NOT NULL,
  phase       TEXT NOT NULL,
  input_hash  TEXT NOT NULL,
  plan_json   TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS migrations_ledger (
  version    INTEGER PRIMARY KEY,
  name       TEXT NOT NULL,
  checksum   TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS quarantined_nodes (
  node_id        TEXT PRIMARY KEY,
  snapshot_json  TEXT NOT NULL,
  reason         TEXT NOT NULL,
  quarantined_at TEXT NOT NULL
);
"#;

/// Open pragmas: WAL + busy timeout (plan mandate) + FULL sync + FKs.
pub fn apply_open_pragmas(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(sql_err("journal_mode"))?;
    conn.busy_timeout(std::time::Duration::from_millis(5_000))
        .map_err(sql_err("busy_timeout"))?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(sql_err("synchronous"))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(sql_err("foreign_keys"))?;
    Ok(())
}

/// Create the whole v4 schema in one transaction and record the ledger rows
/// marking a database born at v4.
pub fn create_fresh_v4(
    conn: &Connection,
    now_iso: &str,
    home_path: &str,
    home_id: Option<&str>,
) -> Result<String> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(sql_err("begin fresh v4"))?;
    let outcome = (|| -> Result<String> {
        conn.execute_batch(SCHEMA_V4_SQL)
            .map_err(sql_err("v4 ddl"))?;
        let id = ensure_home_row(conn, home_path, now_iso, home_id)?;
        // Ledger rows prove which steps produced this database.
        for step in crate::migrate::steps_for_legacy(1) {
            conn.execute(
                "INSERT OR IGNORE INTO migrations_ledger (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
                params![step.version, step.name, sha256_hex(step.sql), now_iso],
            )
            .map_err(sql_err("ledger seed"))?;
        }
        set_user_version(conn, KNOWN_SCHEMA_VERSION)?;
        Ok(id)
    })();
    finish_txn(conn, outcome)
}

/// Insert the single homes row when absent; returns the home id.
pub fn ensure_home_row(
    conn: &Connection,
    home_path: &str,
    now_iso: &str,
    home_id: Option<&str>,
) -> Result<String> {
    let existing: Option<String> = conn
        .query_row("SELECT home_id FROM homes LIMIT 1", [], |row| row.get(0))
        .optional()
        .map_err(sql_err("homes lookup"))?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = home_id.map(str::to_string).unwrap_or_else(generate_home_id);
    conn.execute(
        "INSERT INTO homes (home_id, path, created_at) VALUES (?1, ?2, ?3)",
        params![id, home_path, now_iso],
    )
    .map_err(sql_err("homes insert"))?;
    Ok(id)
}

pub fn get_home_id(conn: &Connection) -> Result<Option<String>> {
    conn.query_row("SELECT home_id FROM homes LIMIT 1", [], |row| row.get(0))
        .optional()
        .map_err(sql_err("homes lookup"))
}

// ── user_version / meta ─────────────────────────────────────────────────

pub fn get_user_version(conn: &Connection) -> Result<i64> {
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(sql_err("user_version"))
}

pub fn set_user_version(conn: &Connection, version: i64) -> Result<()> {
    conn.pragma_update(None, "user_version", version)
        .map_err(sql_err("user_version write"))?;
    // Legacy readers (TypeScript bridge) key off meta.schema_version too.
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version.to_string()],
    )
    .map_err(sql_err("schema_version meta"))?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(sql_err("meta get"))
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(sql_err("meta set"))?;
    Ok(())
}

// ── counters ────────────────────────────────────────────────────────────

pub fn counter_value_of(conn: &Connection, prefix: &str) -> Result<i64> {
    Ok(get_meta(conn, &format!("counter_{prefix}"))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

pub fn bump_counter(conn: &Connection, prefix: &str) -> Result<i64> {
    let next = counter_value_of(conn, prefix)? + 1;
    set_meta(conn, &format!("counter_{prefix}"), &next.to_string())?;
    Ok(next)
}

pub fn reset_counters(conn: &Connection, ids: &[String]) -> Result<()> {
    for id in ids {
        let Some((prefix, digits)) = id.split_once('-') else {
            continue;
        };
        let Ok(seen) = digits.parse::<i64>() else {
            continue;
        };
        if seen > counter_value_of(conn, prefix)? {
            set_meta(conn, &format!("counter_{prefix}"), &seen.to_string())?;
        }
    }
    Ok(())
}

// ── node rows ───────────────────────────────────────────────────────────

const NODE_COLS: &str = "id, type, title, status, archived, priority, path, created_at, updated_at";

fn read_node(row: &Row<'_>) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        id: row.get(0)?,
        node_type: parse_enum(row.get::<_, String>(1)?, "node type"),
        title: row.get(2)?,
        status: parse_enum(row.get::<_, String>(3)?, "node status"),
        archived: row.get::<_, i64>(4)? == 1,
        priority: row.get(5)?,
        path: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

pub fn insert_node(conn: &Connection, node: &NodeRow, revision: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO nodes (id, type, title, status, archived, priority, path, created_at, updated_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            node.id,
            node.node_type.to_string(),
            node.title,
            node.status.to_string(),
            node.archived as i64,
            node.priority,
            node.path,
            node.created_at,
            node.updated_at,
            revision,
        ],
    )
    .map_err(sql_err("insert node"))?;
    Ok(())
}

/// Dynamic patch mirroring `OmtStore.updateNode`: `None` fields are skipped;
/// every applied patch bumps the optimistic-concurrency revision (R9).
#[derive(Debug, Clone, Default)]
pub struct NodePatchValues {
    pub title: Option<String>,
    pub status: Option<String>,
    pub archived: Option<bool>,
    pub priority: Option<i64>,
    pub path: Option<String>,
    pub updated_at: Option<String>,
}

pub fn update_node(conn: &Connection, id: &str, patch: &NodePatchValues) -> Result<i64> {
    let mut assignments: Vec<(&str, Box<dyn rusqlite::types::ToSql>)> = Vec::new();
    if let Some(v) = &patch.title {
        assignments.push(("title", Box::new(v.clone())));
    }
    if let Some(v) = &patch.status {
        assignments.push(("status", Box::new(v.clone())));
    }
    if let Some(v) = patch.archived {
        assignments.push(("archived", Box::new(v as i64)));
    }
    if let Some(v) = patch.priority {
        assignments.push(("priority", Box::new(v)));
    }
    if let Some(v) = &patch.path {
        assignments.push(("path", Box::new(v.clone())));
    }
    if let Some(v) = &patch.updated_at {
        assignments.push(("updated_at", Box::new(v.clone())));
    }
    if assignments.is_empty() {
        return current_revision(conn, id);
    }
    let mut sql = String::from("UPDATE nodes SET ");
    for (index, (column, _)) in assignments.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("{column} = ?{}", index + 1));
    }
    sql.push_str(&format!(
        ", revision = revision + 1 WHERE id = ?{}",
        assignments.len() + 1
    ));
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
        assignments.into_iter().map(|(_, value)| value).collect();
    params_vec.push(Box::new(id.to_string()));
    let refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, refs.as_slice())
        .map_err(sql_err("update node"))?;
    current_revision(conn, id)
}

pub fn current_revision(conn: &Connection, id: &str) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT revision FROM nodes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_err("revision lookup"))?
        .unwrap_or(0))
}

pub fn get_node(conn: &Connection, id: &str) -> Result<Option<NodeRow>> {
    conn.query_row(
        &format!("SELECT {NODE_COLS} FROM nodes WHERE id = ?1"),
        params![id],
        read_node,
    )
    .optional()
    .map_err(sql_err("get node"))
}

pub fn list_nodes(
    conn: &Connection,
    node_type: Option<NodeType>,
    status: Option<omt_contracts::NodeStatus>,
) -> Result<Vec<NodeRow>> {
    let mut sql = format!("SELECT {NODE_COLS} FROM nodes");
    let mut conditions: Vec<String> = Vec::new();
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(t) = node_type {
        params_vec.push(Box::new(t.to_string()));
        conditions.push(format!("type = ?{}", params_vec.len()));
    }
    if let Some(s) = status {
        params_vec.push(Box::new(s.to_string()));
        conditions.push(format!("status = ?{}", params_vec.len()));
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY id");
    let refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(sql_err("list nodes"))?;
    let rows = stmt
        .query_map(refs.as_slice(), read_node)
        .map_err(sql_err("list nodes query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_err("list nodes read"))?;
    Ok(rows)
}

// ── edges ───────────────────────────────────────────────────────────────

pub fn insert_edge(conn: &Connection, parent_id: &str, child_id: &str, ord: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO edges (parent_id, child_id, ord) VALUES (?1, ?2, ?3)
         ON CONFLICT(parent_id, child_id) DO UPDATE SET ord = excluded.ord",
        params![parent_id, child_id, ord],
    )
    .map_err(sql_err("insert edge"))?;
    Ok(())
}

pub fn delete_edge(conn: &Connection, parent_id: &str, child_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM edges WHERE parent_id = ?1 AND child_id = ?2",
        params![parent_id, child_id],
    )
    .map_err(sql_err("delete edge"))?;
    Ok(())
}

pub fn children_of(conn: &Connection, parent_id: &str) -> Result<Vec<NodeRow>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {NODE_COLS} FROM edges e JOIN nodes n ON n.id = e.child_id
             WHERE e.parent_id = ?1 ORDER BY e.ord, e.child_id"
        ))
        .map_err(sql_err("children"))?;
    let rows = stmt
        .query_map(params![parent_id], read_node)
        .map_err(sql_err("children query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_err("children read"))?;
    Ok(rows)
}

/// Edge-child count for one parent (test/convergence convenience).
pub fn children_of_count(conn: &Connection, parent_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM edges WHERE parent_id = ?1",
        params![parent_id],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

pub fn parent_of(conn: &Connection, child_id: &str) -> Result<Option<NodeRow>> {
    conn
        .query_row(
            &format!(
                "SELECT {NODE_COLS} FROM edges e JOIN nodes n ON n.id = e.parent_id WHERE e.child_id = ?1"
            ),
            params![child_id],
            read_node,
        )
        .optional()
        .map_err(sql_err("parent"))
}

pub fn all_edges(conn: &Connection) -> Result<Vec<EdgeRow>> {
    let mut stmt = conn
        .prepare("SELECT parent_id, child_id, ord FROM edges ORDER BY parent_id, ord")
        .map_err(sql_err("all edges"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(EdgeRow {
                parent_id: row.get(0)?,
                child_id: row.get(1)?,
                ord: row.get(2)?,
            })
        })
        .map_err(sql_err("all edges query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_err("all edges read"))?;
    Ok(rows)
}

// ── search mirror ───────────────────────────────────────────────────────

pub fn index_node(conn: &Connection, id: &str, title: &str, body: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO nodes_search (id, title, body) VALUES (?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, body = excluded.body",
        params![id, title, body],
    )
    .map_err(sql_err("index node"))?;
    Ok(())
}

/// LIKE search over the content mirror: every whitespace token must appear
/// (AND); title hits rank first; ties break by id (TS semantics).
pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<String>> {
    let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return Ok(vec![]);
    }
    let escape = |token: &str| {
        format!(
            "%{}%",
            token
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    };
    let conditions = tokens
        .iter()
        .map(|_| "(title LIKE ? ESCAPE '\\' OR body LIKE ? ESCAPE '\\')".to_string())
        .collect::<Vec<_>>()
        .join(" AND ");
    let mut bind: Vec<String> = tokens.iter().flat_map(|t| [escape(t), escape(t)]).collect();
    let sql = format!("SELECT id, title FROM nodes_search WHERE {conditions}");
    let mut stmt = conn.prepare(&sql).map_err(sql_err("search prepare"))?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = bind
        .iter()
        .map(|p| p as &dyn rusqlite::types::ToSql)
        .collect();
    let raw: Vec<(String, String)> = stmt
        .query_map(refs.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sql_err("search query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_err("search read"))?;
    bind.clear();
    let contains_all = |haystack: &str| {
        tokens.iter().all(|t| {
            haystack
                .to_ascii_lowercase()
                .contains(&t.to_ascii_lowercase())
        })
    };
    let mut scored: Vec<(bool, String)> = raw
        .into_iter()
        .map(|(id, title)| (contains_all(&title), id))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(scored.into_iter().take(limit).map(|(_, id)| id).collect())
}

// ── runs ────────────────────────────────────────────────────────────────

fn read_run(row: &Row<'_>) -> rusqlite::Result<RunRow> {
    let config_text: String = row.get(3)?;
    let config_value: Value = serde_json::from_str(&config_text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let fallback = RunConfigValue::default();
    let get_bool = |key: &str, default: bool| {
        config_value
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    };
    Ok(RunRow {
        id: row.get(0)?,
        title: row.get(1)?,
        status: parse_enum(row.get::<_, String>(2)?, "run status"),
        config: RunConfigValue {
            stop_on_failure: get_bool("stopOnFailure", fallback.stop_on_failure),
            auto_continue: get_bool("autoContinue", fallback.auto_continue),
            auto_verify: get_bool("autoVerify", fallback.auto_verify),
            concurrency: config_value
                .get("concurrency")
                .and_then(Value::as_i64)
                .unwrap_or(fallback.concurrency),
        },
        created_at: row.get(4)?,
        finished_at: row.get(5)?,
    })
}

pub fn insert_run(conn: &Connection, run: &RunRow) -> Result<()> {
    let config = serde_json::json!({
        "stopOnFailure": run.config.stop_on_failure,
        "autoContinue": run.config.auto_continue,
        "autoVerify": run.config.auto_verify,
        "concurrency": run.config.concurrency,
    });
    conn.execute(
        "INSERT INTO runs (id, title, status, config, created_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![run.id, run.title, run.status.to_string(), config.to_string(), run.created_at, run.finished_at],
    )
    .map_err(sql_err("insert run"))?;
    Ok(())
}

pub fn update_run(
    conn: &Connection,
    id: &str,
    status: Option<RunStatus>,
    finished_at: Option<Option<String>>,
    title: Option<Option<String>>,
) -> Result<()> {
    let mut sets: Vec<String> = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    macro_rules! push {
        ($column:expr, $value:expr) => {{
            bind.push(Box::new($value));
            sets.push(format!("{} = ?{}", $column, bind.len()));
        }};
    }
    if let Some(v) = status {
        push!("status", v.to_string());
    }
    if let Some(v) = finished_at {
        push!("finished_at", v);
    }
    if let Some(v) = title {
        push!("title", v);
    }
    if sets.is_empty() {
        return Ok(());
    }
    bind.push(Box::new(id.to_string()));
    let sql = format!(
        "UPDATE runs SET {} WHERE id = ?{}",
        sets.join(", "),
        bind.len()
    );
    let refs: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, refs.as_slice())
        .map_err(sql_err("update run"))?;
    Ok(())
}

pub fn get_run(conn: &Connection, id: &str) -> Result<Option<RunRow>> {
    conn.query_row(
        "SELECT id, title, status, config, created_at, finished_at FROM runs WHERE id = ?1",
        params![id],
        read_run,
    )
    .optional()
    .map_err(sql_err("get run"))
}

pub fn list_runs(conn: &Connection, statuses: &[RunStatus]) -> Result<Vec<RunRow>> {
    let mut sql =
        String::from("SELECT id, title, status, config, created_at, finished_at FROM runs");
    let mut bind: Vec<String> = Vec::new();
    if !statuses.is_empty() {
        let placeholders = statuses
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" WHERE status IN ({placeholders})"));
        bind.extend(statuses.iter().map(|s| s.to_string()));
    }
    sql.push_str(" ORDER BY id");
    let mut stmt = conn.prepare(&sql).map_err(sql_err("list runs"))?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = bind
        .iter()
        .map(|p| p as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt
        .query_map(refs.as_slice(), read_run)
        .map_err(sql_err("list runs query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_err("list runs read"))?;
    Ok(rows)
}

// ── run items ───────────────────────────────────────────────────────────

const ITEM_COLS: &str = "run_id, node_id, position, state, executor_session_id, attempts, last_error, nudged_at, nudge_count, started_at, finished_at";

fn read_item(row: &Row<'_>) -> rusqlite::Result<RunItemRow> {
    Ok(RunItemRow {
        run_id: row.get(0)?,
        node_id: row.get(1)?,
        position: row.get(2)?,
        state: parse_enum(row.get::<_, String>(3)?, "item state"),
        executor_session_id: row.get(4)?,
        attempts: row.get(5)?,
        last_error: row.get(6)?,
        nudged_at: row.get(7)?,
        nudge_count: row.get(8)?,
        started_at: row.get(9)?,
        finished_at: row.get(10)?,
    })
}

pub fn insert_run_item(conn: &Connection, item: &RunItemRow) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO run_items ({ITEM_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
        ),
        params![
            item.run_id,
            item.node_id,
            item.position,
            item.state.to_string(),
            item.executor_session_id,
            item.attempts,
            item.last_error,
            item.nudged_at,
            item.nudge_count,
            item.started_at,
            item.finished_at,
        ],
    )
    .map_err(sql_err("insert run item"))?;
    Ok(())
}

/// Explicit-set item patch (`None` = leave unchanged; explicit clears use the
/// dedicated clear flags so SQL NULL writes stay distinguishable).
#[derive(Debug, Clone, Default)]
pub struct ItemPatchValues {
    pub state: Option<RunItemState>,
    pub position: Option<i64>,
    pub executor_session_id: Option<String>,
    pub clear_executor: bool,
    pub attempts: Option<i64>,
    pub last_error: Option<String>,
    pub clear_last_error: bool,
    pub nudged_at: Option<String>,
    pub nudge_count: Option<i64>,
    pub started_at: Option<String>,
    /// COALESCE semantics: keep an existing started_at, set only when absent.
    pub preserve_started_at: bool,
    pub finished_at: Option<String>,
    pub clear_finished_at: bool,
}

pub fn update_run_item(
    conn: &Connection,
    run_id: &str,
    node_id: &str,
    patch: &ItemPatchValues,
) -> Result<()> {
    let mut sets: Vec<String> = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    macro_rules! push {
        ($column:expr, $value:expr) => {{
            bind.push(Box::new($value));
            sets.push(format!("{} = ?{}", $column, bind.len()));
        }};
    }
    if let Some(v) = patch.state {
        push!("state", v.to_string());
    }
    if let Some(v) = patch.position {
        push!("position", v);
    }
    if let Some(v) = &patch.executor_session_id {
        push!("executor_session_id", v.clone());
    }
    if patch.clear_executor {
        push!("executor_session_id", Option::<String>::None);
    }
    if let Some(v) = patch.attempts {
        push!("attempts", v);
    }
    if let Some(v) = &patch.last_error {
        push!("last_error", v.clone());
    }
    if patch.clear_last_error {
        push!("last_error", Option::<String>::None);
    }
    if let Some(v) = &patch.nudged_at {
        push!("nudged_at", v.clone());
    }
    if let Some(v) = patch.nudge_count {
        push!("nudge_count", v);
    }
    if patch.preserve_started_at {
        if let Some(v) = &patch.started_at {
            bind.push(Box::new(v.clone()));
            sets.push(format!(
                "started_at = COALESCE(started_at, ?{})",
                bind.len()
            ));
        }
    } else if let Some(v) = &patch.started_at {
        push!("started_at", v.clone());
    }
    if let Some(v) = &patch.finished_at {
        push!("finished_at", v.clone());
    }
    if patch.clear_finished_at {
        push!("finished_at", Option::<String>::None);
    }
    if sets.is_empty() {
        return Ok(());
    }
    bind.push(Box::new(run_id.to_string()));
    bind.push(Box::new(node_id.to_string()));
    let sql = format!(
        "UPDATE run_items SET {} WHERE run_id = ?{} AND node_id = ?{}",
        sets.join(", "),
        bind.len() - 1,
        bind.len()
    );
    let refs: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|p| p.as_ref()).collect();
    let changed = conn
        .execute(&sql, refs.as_slice())
        .map_err(sql_err("update run item"))?;
    if changed == 0 {
        return Err(Problem::with_details(
            error::NOT_FOUND,
            format!("no run item {run_id}/{node_id} to patch"),
            |d| {
                d.insert("kind".into(), "run-item".into());
                d.insert("runId".into(), run_id.into());
                d.insert("nodeId".into(), node_id.into());
            },
        ));
    }
    Ok(())
}

pub fn get_run_item(conn: &Connection, run_id: &str, node_id: &str) -> Result<Option<RunItemRow>> {
    conn.query_row(
        &format!("SELECT {ITEM_COLS} FROM run_items WHERE run_id = ?1 AND node_id = ?2"),
        params![run_id, node_id],
        read_item,
    )
    .optional()
    .map_err(sql_err("get run item"))
}

pub fn list_run_items(conn: &Connection, run_id: &str) -> Result<Vec<RunItemRow>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {ITEM_COLS} FROM run_items WHERE run_id = ?1 ORDER BY position, node_id"
        ))
        .map_err(sql_err("list items"))?;
    let rows = stmt
        .query_map(params![run_id], read_item)
        .map_err(sql_err("list items query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_err("list items read"))?;
    Ok(rows)
}

pub fn delete_run_item(conn: &Connection, run_id: &str, node_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM run_items WHERE run_id = ?1 AND node_id = ?2",
        params![run_id, node_id],
    )
    .map_err(sql_err("delete run item"))?;
    Ok(())
}

/// Atomic claim (TICKET-0058): drain unexecutable pending members to skipped,
/// then claim the first executable pending item — ONE immediate transaction
/// so two claimers can never receive the same item. Mirrors the TS SQL.
pub fn claim_next_run_item(
    conn: &Connection,
    run_id: &str,
    executor_session_id: &str,
    now: &str,
) -> Result<(Option<RunItemRow>, Vec<RunItemRow>)> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(sql_err("claim begin"))?;
    let outcome = (|| -> Result<(Option<RunItemRow>, Vec<RunItemRow>)> {
        conn.execute(
            "UPDATE run_items SET state = 'skipped', finished_at = ?1
             WHERE run_id = ?2 AND state = 'pending'
               AND node_id IN (SELECT id FROM nodes WHERE archived = 1 OR type NOT IN ('ticket','subticket'))",
            params![now, run_id],
        )
        .map_err(sql_err("claim drain"))?;
        let drained: Vec<RunItemRow> = {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {ITEM_COLS} FROM run_items WHERE run_id = ?1 AND state = 'skipped' AND finished_at = ?2 ORDER BY position, node_id"
                ))
                .map_err(sql_err("claim drained select"))?;
            let rows: Vec<RunItemRow> = stmt
                .query_map(params![run_id, now], read_item)
                .map_err(sql_err("claim drained read"))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_err("claim drained collect"))?;
            rows
        };
        let next: Option<String> = conn
            .query_row(
                "SELECT i.node_id FROM run_items i JOIN nodes n ON n.id = i.node_id
                 WHERE i.run_id = ?1 AND i.state = 'pending' AND n.archived = 0 AND n.type IN ('ticket','subticket')
                 ORDER BY i.position, i.node_id LIMIT 1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err("claim select"))?;
        let claimed = match next {
            None => None,
            Some(node_id) => {
                conn.execute(
                    "UPDATE run_items SET state = 'running', executor_session_id = ?1, started_at = COALESCE(started_at, ?2)
                     WHERE run_id = ?3 AND node_id = ?4 AND state = 'pending'",
                    params![executor_session_id, now, run_id, node_id],
                )
                .map_err(sql_err("claim update"))?;
                get_run_item(conn, run_id, &node_id)?
            }
        };
        Ok((claimed, drained))
    })();
    finish_txn(conn, outcome)
}

// ── rebuild (reindex) ───────────────────────────────────────────────────

/// Replace index content (nodes/edges/search), preserving runs/run_items and
/// moving counters past every observed id — TS `rebuild` semantics verbatim.
pub fn rebuild(
    conn: &Connection,
    nodes: &[NodeRow],
    edges: &[EdgeRow],
    bodies: &[(String, String, String)], // (id, title, body)
    now_iso: &str,
) -> Result<()> {
    conn.execute_batch("BEGIN")
        .map_err(sql_err("rebuild begin"))?;
    let outcome = (|| -> Result<()> {
        conn.execute("DELETE FROM edges", [])
            .map_err(sql_err("rebuild edges"))?;
        conn.execute("DELETE FROM nodes", [])
            .map_err(sql_err("rebuild nodes"))?;
        conn.execute("DELETE FROM nodes_search", [])
            .map_err(sql_err("rebuild search"))?;
        for node in nodes {
            insert_node(conn, node, 1)?;
        }
        for edge in edges {
            insert_edge(conn, &edge.parent_id, &edge.child_id, edge.ord)?;
        }
        for (id, title, body) in bodies {
            index_node(conn, id, title, body)?;
        }
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        reset_counters(conn, &ids)?;
        set_meta(conn, "schema_version", &KNOWN_SCHEMA_VERSION.to_string())?;
        let _ = now_iso;
        Ok(())
    })();
    finish_txn(conn, outcome)
}

// ── txn helpers / misc ──────────────────────────────────────────────────

/// Commit on `Ok`, roll back on `Err`, then forward the inner result.
pub fn finish_txn<T>(conn: &Connection, outcome: Result<T>) -> Result<T> {
    match &outcome {
        Ok(_) => conn.execute_batch("COMMIT").map_err(sql_err("commit"))?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    outcome
}

/// Run `f` inside one immediate transaction (finalize shape).
pub fn in_transaction<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(sql_err("begin"))?;
    finish_txn(conn, f(conn))
}

pub fn sql_err(context: &'static str) -> impl Fn(rusqlite::Error) -> Problem {
    move |err| {
        Problem::with_details(error::IO, format!("sqlite {context}: {err}"), |d| {
            d.insert("sqliteContext".into(), context.into());
            d.insert("sqliteError".into(), err.to_string().into());
        })
    }
}

fn parse_enum<E>(raw: String, what: &str) -> E
where
    E: std::str::FromStr,
    <E as std::str::FromStr>::Err: std::fmt::Display,
{
    raw.parse()
        .unwrap_or_else(|err| panic!("stored {what} is invalid ({err}): {raw}"))
}

/// Counter prefix for a node type (storage-local convenience wrapper).
pub fn counter_prefix(node_type: NodeType) -> &'static str {
    type_prefix(node_type)
}

/// True when this node's type may execute inside a run.
pub fn is_executable_member(node: &NodeRow) -> bool {
    is_run_member_node_type(node.node_type)
}
