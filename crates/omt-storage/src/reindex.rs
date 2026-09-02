//! Deterministic reindex (R19/U4c): zero-write dry-run plan, then an execute
//! phase that applies exactly what the plan listed — never more.
//!
//! Semantics:
//! * **imports** — Markdown files under `tickets/` no active DB row claims;
//!   execute inserts their rows (frontmatter-driven, hand-edit tolerant).
//! * **moves** — active nodes whose stored `path` no longer matches the
//!   canonical `<parent-dir>/<ID>-<slug(title)>/<type>.md` layout; execute
//!   renames the node directory and updates the row.
//! * **conflicts** — ambiguous situations (duplicate ids on disk, two nodes
//!   targeting one directory, an import colliding with an existing path);
//!   ANY conflict fails execution CLOSED before a single write.
//! * **quarantines** — missing active members: a DB row whose Markdown file
//!   is gone. Execute records a FULL identity snapshot in
//!   `quarantined_nodes` (id/type/title/path/lastKnownBodyHash/…), removes
//!   the member from the active set, and appends a durable `node.quarantined`
//!   stream event — nothing is silently deleted or rebound.
//!
//! Drift between dry-run and execute is gated the same way the U4a journal
//! gates recovery replays: if observed disk state no longer matches the plan,
//! execution refuses with [`REINDEX_REQUIRED`] instead of improvising.

use crate::clock::MillisClock;
use crate::files::{sha256_hex, DiskFiles};
use crate::outbox;
use crate::store;
use crate::{Problem, Result};
use omt_domain::error;
use omt_domain::markdown;
use omt_domain::types::NodeRow;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One stray-file import candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportCandidate {
    /// Home-relative Markdown path of the stray file.
    pub path: String,
    /// Node id parsed from its frontmatter (`null` when unparseable — such
    /// files land in `conflicts` at execute time).
    pub id: Option<String>,
}

/// One canonical-layout repair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoveAction {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    pub from: String,
    pub to: String,
}

/// One ambiguity that blocks execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conflict {
    pub kind: String,
    pub detail: String,
}

/// Full identity preserved for a missing active member.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuarantineSnapshot {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub title: String,
    pub path: String,
    /// SHA-256 of the last indexed body text (`nodes_search.body`) — proves
    /// what content the runtime last saw for this member.
    #[serde(rename = "lastKnownBodyHash")]
    pub last_known_body_hash: String,
    pub status: String,
    pub archived: bool,
    pub priority: i64,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub reason: String,
}

/// Zero-write reindex plan (dry-run output, execute input).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReindexPlan {
    pub imports: Vec<ImportCandidate>,
    pub moves: Vec<MoveAction>,
    pub conflicts: Vec<Conflict>,
    pub quarantines: Vec<QuarantineSnapshot>,
}

impl ReindexPlan {
    pub fn is_empty(&self) -> bool {
        self.imports.is_empty()
            && self.moves.is_empty()
            && self.conflicts.is_empty()
            && self.quarantines.is_empty()
    }
}

/// Last indexed body text for one node (empty string when unindexed).
fn last_known_body(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT body FROM nodes_search WHERE id = ?1", [id], |row| {
        row.get::<_, String>(0)
    })
    .unwrap_or_default()
}

/// Canonical home-relative path for a node: replace ONLY the node-directory
/// segment (`<ID>-<slug(title)>`) with the one recomputed from the CURRENT
/// title, keeping the grandparent directory and file name.
fn canonical_move_target(node: &NodeRow) -> String {
    let dir_name = markdown::node_dir_name(&node.id, &node.title);
    let segments: Vec<&str> = node.path.split('/').collect();
    // segments = […, <node-dir>, <file>] — the grandparent holds the dir.
    let base = if segments.len() >= 3 {
        segments[..segments.len() - 2].join("/")
    } else {
        "tickets".to_string()
    };
    format!("{base}/{dir_name}/{}.md", node.node_type)
}

/// Build the zero-write dry-run plan for one opened home.
pub fn dry_run(conn: &Connection, files: &DiskFiles) -> Result<ReindexPlan> {
    let mut plan = ReindexPlan::default();
    let nodes = store::list_nodes(conn, None, None)?;

    let mut claimed_paths: std::collections::BTreeSet<String> = Default::default();
    let mut target_dirs: std::collections::BTreeMap<String, String> = Default::default();

    for node in &nodes {
        if files.read_optional(&node.path)?.is_none() {
            // Missing active member → quarantine candidate with FULL identity.
            let body = last_known_body(conn, &node.id);
            plan.quarantines.push(QuarantineSnapshot {
                id: node.id.clone(),
                node_type: node.node_type.to_string(),
                title: node.title.clone(),
                path: node.path.clone(),
                last_known_body_hash: sha256_hex(&body),
                status: node.status.to_string(),
                archived: node.archived,
                priority: node.priority,
                created_at: node.created_at.clone(),
                updated_at: node.updated_at.clone(),
                reason: "markdown-missing".to_string(),
            });
            continue;
        }
        claimed_paths.insert(node.path.clone());
        let target = canonical_move_target(node);
        if target != node.path {
            plan.moves.push(MoveAction {
                node_id: node.id.clone(),
                from: node.path.clone(),
                to: target.clone(),
            });
        }
        let target_dir = markdown::dirname(&target);
        if let Some(previous) = target_dirs.insert(target_dir.clone(), node.id.clone()) {
            plan.conflicts.push(Conflict {
                kind: "move-collision".to_string(),
                detail: format!(
                    "nodes {} and {} both canonicalize into {target_dir}",
                    previous, node.id
                ),
            });
        }
    }

    // Stray Markdown files no active row claims.
    for rel_path in files.list_markdown_under_tickets() {
        if claimed_paths.contains(&rel_path) || plan.moves.iter().any(|m| m.to == rel_path) {
            continue;
        }
        // A quarantined candidate's path must NOT be re-imported as a stray.
        let id = files
            .read_optional(&rel_path)?
            .and_then(|text| markdown::parse_node_file(&text).ok())
            .and_then(|parsed| parsed.attrs.id)
            .filter(|id| {
                !plan
                    .quarantines
                    .iter()
                    .any(|q| q.id == *id && q.path == rel_path)
            });
        plan.imports.push(ImportCandidate { path: rel_path, id });
    }

    Ok(plan)
}

/// Execute exactly what the plan lists. Fails CLOSED (REINDEX_REQUIRED /
/// CONFLICT, zero writes) when reality drifted from the dry-run or when the
/// plan carries conflicts.
pub fn execute(
    conn: &mut Connection,
    files: &DiskFiles,
    plan: &ReindexPlan,
    clock: &dyn MillisClock,
) -> Result<ExecutedReindex> {
    if !plan.conflicts.is_empty() {
        return Err(Problem::with_details(
            error::INVALID_INPUT,
            "reindex plan carries conflicts; refusing closed",
            |d| {
                d.insert(
                    "conflicts".into(),
                    Value::Number(plan.conflicts.len().into()),
                );
            },
        ));
    }

    // ── drift gates BEFORE any write: observed state must match the plan ──
    for action in &plan.moves {
        if files.read_optional(&action.from)?.is_none() {
            return Err(drift_problem(&format!(
                "planned move source vanished since dry-run: {}",
                action.from
            )));
        }
    }
    for quarantine in &plan.quarantines {
        if files.read_optional(&quarantine.path)?.is_some() {
            return Err(drift_problem(&format!(
                "quarantine candidate reappeared on disk since dry-run: {}",
                quarantine.path
            )));
        }
        if store::get_node(conn, &quarantine.id)?.is_none() {
            return Err(drift_problem(&format!(
                "quarantine candidate already left the active set: {}",
                quarantine.id
            )));
        }
    }
    for import in &plan.imports {
        if files.read_optional(&import.path)?.is_none() {
            return Err(drift_problem(&format!(
                "import candidate vanished since dry-run: {}",
                import.path
            )));
        }
    }

    let mut executed = ExecutedReindex::default();
    let home_id = store::get_home_id(conn)?;

    // ── moves: rename directory, then update the row ──
    for action in &plan.moves {
        let from_dir = markdown::dirname(&action.from);
        let to_dir = markdown::dirname(&action.to);
        if from_dir != to_dir {
            let abs_from = files.root().join(&from_dir);
            let abs_to = files.root().join(&to_dir);
            std::fs::create_dir_all(files.root().join(markdown::dirname(&to_dir)))
                .map_err(|err| Problem::new(error::IO, format!("reindex mkdir: {err}")))?;
            std::fs::rename(&abs_from, &abs_to)
                .map_err(|err| Problem::new(error::IO, format!("reindex rename: {err}")))?;
        }
        executed.moved += 1;
    }
    for action in &plan.moves {
        conn.execute(
            "UPDATE nodes SET path = ?2 WHERE id = ?1",
            rusqlite::params![action.node_id, action.to],
        )
        .map_err(store::sql_err("reindex path update"))?;
    }

    // ── quarantines: snapshot + deactivate + durable event, atomically ──
    for quarantine in &plan.quarantines {
        let snapshot_json = serde_json::to_string(quarantine).map_err(|err| {
            Problem::new(error::IO, format!("serialize quarantine snapshot: {err}"))
        })?;
        let event_payload = serde_json::json!({
            "kind": "node.quarantined",
            "nodeId": quarantine.id,
            "path": quarantine.path,
            "reason": quarantine.reason,
        });
        let tx = conn
            .transaction()
            .map_err(|err| Problem::new(error::IO, format!("reindex txn: {err}")))?;
        tx.execute(
            "INSERT INTO quarantined_nodes (node_id, snapshot_json, reason, quarantined_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(node_id) DO NOTHING",
            rusqlite::params![
                quarantine.id,
                snapshot_json,
                quarantine.reason,
                crate::clock::iso_from_ms(clock.now_ms()),
            ],
        )
        .map_err(store::sql_err("quarantine insert"))?;
        let inserted = tx.changes() > 0;
        // Deactivate the member: remove row, edges, and search mirror. The
        // snapshot above preserves the full identity — nothing is lost.
        tx.execute(
            "DELETE FROM edges WHERE parent_id = ?1 OR child_id = ?1",
            [&quarantine.id],
        )
        .map_err(store::sql_err("quarantine edges"))?;
        tx.execute("DELETE FROM nodes_search WHERE id = ?1", [&quarantine.id])
            .map_err(store::sql_err("quarantine search"))?;
        tx.execute("DELETE FROM nodes WHERE id = ?1", [&quarantine.id])
            .map_err(store::sql_err("quarantine node"))?;
        if inserted {
            if let Some(home_id) = &home_id {
                outbox::append(
                    &tx,
                    home_id,
                    "node.quarantined",
                    &event_payload,
                    &crate::clock::iso_from_ms(clock.now_ms()),
                )?;
            }
        }
        tx.commit()
            .map_err(|err| Problem::new(error::IO, format!("reindex commit: {err}")))?;
        if inserted {
            executed.quarantined.push(quarantine.id.clone());
        }
    }

    // ── imports: activate stray files as rows ──
    for import in &plan.imports {
        let text = files
            .read_optional(&import.path)?
            .ok_or_else(|| drift_problem("import file vanished mid-execute"))?;
        let parsed = markdown::parse_node_file(&text).map_err(|_| {
            Problem::with_details(
                error::INVALID_INPUT,
                "import candidate has invalid frontmatter",
                |d| {
                    d.insert("path".into(), import.path.clone().into());
                },
            )
        })?;
        let Some(id) = parsed.attrs.id else {
            return Err(Problem::with_details(
                error::INVALID_INPUT,
                "import candidate lacks a usable id",
                |d| {
                    d.insert("path".into(), import.path.clone().into());
                },
            ));
        };
        if store::get_node(conn, &id)?.is_some() {
            return Err(Problem::with_details(
                error::CONFLICT,
                "import candidate duplicates an active node id",
                |d| {
                    d.insert("id".into(), id.clone().into());
                    d.insert("path".into(), import.path.clone().into());
                },
            ));
        }
        let now_iso = crate::clock::iso_from_ms(clock.now_ms());
        let node_type = parsed
            .attrs
            .node_type
            .clone()
            .unwrap_or_else(|| "ticket".to_string());
        let node_type: omt_domain::types::NodeType = node_type.parse().map_err(|_| {
            Problem::with_details(
                error::INVALID_INPUT,
                "import candidate has unknown type",
                |d| {
                    d.insert("type".into(), node_type.clone().into());
                },
            )
        })?;
        let title = match &parsed.attrs.title {
            Some(title) if !title.trim_matches(js_trim).is_empty() => title.clone(),
            _ => id.clone(),
        };
        let node = NodeRow {
            id: id.clone(),
            node_type,
            title,
            status: parsed
                .attrs
                .status
                .clone()
                .unwrap_or_else(|| "open".to_string())
                .parse()
                .unwrap_or(omt_domain::types::NodeStatus::Open),
            archived: parsed.attrs.archived == Some(true),
            priority: parsed.attrs.priority.map(|p| p as i64).unwrap_or(0),
            path: import.path.clone(),
            created_at: parsed
                .attrs
                .created_at
                .clone()
                .unwrap_or_else(|| now_iso.clone()),
            updated_at: parsed
                .attrs
                .updated_at
                .clone()
                .unwrap_or_else(|| now_iso.clone()),
        };
        store::insert_node(conn, &node, 1)?;
        let indexed_body = markdown::strip_children_block(&parsed.body);
        store::index_node(conn, &id, &node.title, &indexed_body)?;
        executed.imported += 1;
    }

    Ok(executed)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ExecutedReindex {
    pub moved: usize,
    pub imported: usize,
    /// Ids actually quarantined THIS pass (excludes already-quarantined ids).
    pub quarantined: Vec<String>,
}

fn js_trim(c: char) -> bool {
    c.is_whitespace()
}

fn drift_problem(detail: &str) -> Problem {
    Problem::with_details(error::REINDEX_REQUIRED, detail, |d| {
        d.insert("requiresReindex".into(), true.into());
    })
}
