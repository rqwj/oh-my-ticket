//! Post-migration invariant checks (R8/AE6): a full pre/post snapshot of
//! everything the migration must preserve — node ids/types/titles/statuses/
//! archived/priorities/paths/timestamps, edges with ords, counters, runs with
//! configs, run items with attempts/nudges, search mirror. `revision` (added
//! by v4) is deliberately excluded: it is NEW state, not preserved state.

use crate::{Problem, Result};
use omt_domain::error;
use rusqlite::Connection;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct HomeSnapshot {
    pub nodes: Vec<Value>,
    pub edges: Vec<Value>,
    pub meta: Vec<(String, String)>,
    pub runs: Vec<Value>,
    pub items: Vec<Value>,
    pub search_rows: i64,
}

/// Capture everything migration must preserve. Works on v1/v2/v3/v4 shapes.
pub fn snapshot(conn: &Connection) -> Result<HomeSnapshot> {
    let mut nodes = vec![];
    {
        // archived column exists only from v2 on; read it when present.
        let has_archived = table_columns(conn, "nodes")?
            .iter()
            .any(|c| c == "archived");
        let sql = if has_archived {
            "SELECT json_array(id, type, title, status, archived, priority, path, created_at, updated_at) FROM nodes ORDER BY id"
        } else {
            // v1: no archived column — normalize to 0 for comparison.
            "SELECT json_array(id, type, title, status, 0, priority, path, created_at, updated_at) FROM nodes ORDER BY id"
        };
        let mut stmt = conn.prepare(sql).map_err(err("nodes snapshot"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(err("nodes query"))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(err("nodes read"))?;
        for text in rows {
            nodes.push(serde_json::from_str(&text).unwrap_or(Value::Null));
        }
    }
    let mut edges = vec![];
    {
        let mut stmt = conn
            .prepare("SELECT json_array(parent_id, child_id, ord) FROM edges ORDER BY parent_id, child_id")
            .map_err(err("edges snapshot"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(err("edges query"))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(err("edges read"))?;
        for text in rows {
            edges.push(serde_json::from_str(&text).unwrap_or(Value::Null));
        }
    }
    let mut meta = vec![];
    {
        let mut stmt = conn
            .prepare("SELECT key, value FROM meta WHERE key != 'schema_version' ORDER BY key")
            .map_err(err("meta snapshot"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(err("meta query"))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(err("meta read"))?;
        meta.extend(rows);
    }
    let mut runs = vec![];
    {
        let has_runs = table_exists(conn, "runs")?;
        if has_runs {
            let mut stmt = conn
                .prepare("SELECT json_array(id, title, status, config, created_at, finished_at) FROM runs ORDER BY id")
                .map_err(err("runs snapshot"))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(err("runs query"))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(err("runs read"))?;
            for text in rows {
                runs.push(serde_json::from_str(&text).unwrap_or(Value::Null));
            }
        }
    }
    let mut items = vec![];
    {
        let has_items = table_exists(conn, "run_items")?;
        if has_items {
            let mut stmt = conn
                .prepare(
                    "SELECT json_array(run_id, node_id, position, state, executor_session_id, attempts, last_error, nudged_at, nudge_count, started_at, finished_at)
                     FROM run_items ORDER BY run_id, position, node_id",
                )
                .map_err(err("items snapshot"))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(err("items query"))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(err("items read"))?;
            for text in rows {
                items.push(serde_json::from_str(&text).unwrap_or(Value::Null));
            }
        }
    }
    let search_rows = if table_exists(conn, "nodes_search")? {
        conn.query_row("SELECT COUNT(*) FROM nodes_search", [], |row| row.get(0))
            .map_err(err("search count"))?
    } else {
        0
    };
    Ok(HomeSnapshot {
        nodes,
        edges,
        meta,
        runs,
        items,
        search_rows,
    })
}

fn err(context: &'static str) -> impl Fn(rusqlite::Error) -> Problem {
    move |e| {
        Problem::with_details(error::IO, format!("snapshot {context}: {e}"), |d| {
            d.insert("context".into(), context.into());
        })
    }
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get(0),
        )
        .map_err(err("table exists"))?;
    Ok(count > 0)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(err("table info"))?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(err("table info query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(err("table info read"))?;
    Ok(cols)
}

/// Assert `after` preserves everything in `before` (migration direction:
/// legacy → current). Any divergence is a CONFLICT problem listing the first
/// mismatched section and value pair.
pub fn assert_preserved(before: &HomeSnapshot, after: &HomeSnapshot) -> Result<()> {
    fn same<T: PartialEq + std::fmt::Debug>(
        section: &str,
        before: &[T],
        after: &[T],
    ) -> Result<()> {
        if before.len() != after.len() || !before.iter().zip(after.iter()).all(|(b, a)| b == a) {
            return Err(mismatch(
                section,
                &format!("{:?}", before.first()),
                &format!("{:?}", after.first()),
                before.len(),
                after.len(),
            ));
        }
        Ok(())
    }
    same("nodes", &before.nodes, &after.nodes)?;
    same("edges", &before.edges, &after.edges)?;
    same("runs", &before.runs, &after.runs)?;
    same("run_items", &before.items, &after.items)?;
    same("meta(counters)", &before.meta, &after.meta)?;
    if before.search_rows != after.search_rows {
        return Err(mismatch(
            "search mirror",
            &before.search_rows.to_string(),
            &after.search_rows.to_string(),
            0,
            0,
        ));
    }
    Ok(())
}

fn mismatch(section: &str, before: &str, after: &str, blen: usize, alen: usize) -> Problem {
    Problem::with_details(
        error::CONFLICT,
        format!("post-migration invariant violated in {section}"),
        |d| {
            d.insert("section".into(), section.into());
            d.insert("beforeSample".into(), before.into());
            d.insert("afterSample".into(), after.into());
            d.insert("beforeCount".into(), blen.into());
            d.insert("afterCount".into(), alen.into());
            d.insert("invariant".into(), "lossless-migration".into());
        },
    )
}
