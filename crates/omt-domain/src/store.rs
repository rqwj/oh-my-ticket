//! In-memory store mirroring `src/host/store.ts` query semantics — the
//! ordering clauses are normative (corpus assertions rely on them):
//! nodes `ORDER BY id`, children `ORDER BY ord, child_id`, edges
//! `ORDER BY parent_id, ord`, runs `ORDER BY id`, items
//! `ORDER BY position, node_id`, per-node items `ORDER BY run_id`.
//! U4 replaces this with SQLite behind the same call surface.

use super::error::{self, Problem, Result};
use super::markdown::{self, FrontmatterAttrs, ParsedNodeFile};
use super::runs::ClaimNextResult;
use super::types::*;
use omt_contracts::{NodeStatus, NodeType, RunItemState, RunStatus};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// One searchable content mirror row (`nodes_search`).
#[derive(Debug, Clone)]
pub struct SearchEntry {
    pub title: String,
    pub body: String,
}

/// The whole database side of one home: metadata index + relations + search
/// mirror + counters + runs. Files remain the content authority; this map is
/// rebuilt wholesale by [`Store::rebuild`].
#[derive(Debug, Clone, Default)]
pub struct Store {
    pub nodes: BTreeMap<String, NodeRow>,
    pub edges: Vec<EdgeRow>,
    pub search: BTreeMap<String, SearchEntry>,
    pub meta: BTreeMap<String, String>,
    /// (run_id, node_id) keyed items; iteration order comes from explicit sorts.
    pub run_items: BTreeMap<(String, String), RunItemRow>,
    pub runs: BTreeMap<String, RunRow>,
}

impl Store {
    // ── meta / counters ──────────────────────────────────────────────

    pub fn get_meta(&self, key: &str) -> Option<&String> {
        self.meta.get(key)
    }

    pub fn set_meta(&mut self, key: &str, value: &str) {
        self.meta.insert(key.to_string(), value.to_string());
    }

    pub fn schema_version(&self) -> Option<&String> {
        self.get_meta("schema_version")
    }

    pub fn mark_schema_version(&mut self) {
        self.set_meta("schema_version", "3");
    }

    fn counter_key(counter_prefix: &str) -> String {
        format!("counter_{counter_prefix}")
    }

    /// Current counter value for a prefix (last allocated number; 0 = none).
    pub fn counter_value_of(&self, counter_prefix: &str) -> i64 {
        self.get_meta(&Self::counter_key(counter_prefix))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    pub fn counter_value(&self, node_type: NodeType) -> i64 {
        self.counter_value_of(type_prefix(node_type))
    }

    pub fn set_counter_of(&mut self, counter_prefix: &str, value: i64) {
        self.set_meta(&Self::counter_key(counter_prefix), &value.to_string());
    }

    pub fn set_counter(&mut self, node_type: NodeType, value: i64) {
        self.set_counter_of(type_prefix(node_type), value);
    }

    /// Allocate the next `PREFIX-NNNN` id from the shared meta counter.
    pub fn allocate_counter_id(&mut self, counter_prefix: &str) -> String {
        let next = self.counter_value_of(counter_prefix) + 1;
        self.set_counter_of(counter_prefix, next);
        format!("{counter_prefix}-{next:04}")
    }

    pub fn next_id(&mut self, node_type: NodeType) -> String {
        self.allocate_counter_id(type_prefix(node_type))
    }

    /// Allocate the next run id (`RUN-0001`; shared RUN counter).
    pub fn next_run_id(&mut self) -> String {
        self.allocate_counter_id("RUN")
    }

    /// After a reindex, move counters past every id seen on disk.
    pub fn reset_counters(&mut self, ids: &[String]) {
        for id in ids {
            let Some((prefix, digits)) = id.split_once('-') else {
                continue;
            };
            let Ok(seen) = digits.parse::<i64>() else {
                continue;
            };
            if prefix_type(prefix).is_none() {
                continue;
            }
            let current = self.counter_value_of(prefix);
            if seen > current {
                self.set_counter_of(prefix, seen);
            }
        }
    }

    // ── nodes ────────────────────────────────────────────────────────

    pub fn insert_node(&mut self, node: NodeRow) {
        self.nodes.insert(node.id.clone(), node);
    }

    // Mirrors the patch shape of the update RPC one-to-one; callers pass
    // positional Options, so a params struct would only relocate the count
    // (clippy::too_many_arguments accepted deliberately).
    #[allow(clippy::too_many_arguments)]
    pub fn update_node(
        &mut self,
        id: &str,
        patch_title: Option<String>,
        patch_status: Option<NodeStatus>,
        patch_archived: Option<bool>,
        patch_priority: Option<i64>,
        patch_path: Option<String>,
        patch_updated_at: Option<String>,
    ) {
        if let Some(node) = self.nodes.get_mut(id) {
            if let Some(title) = patch_title {
                node.title = title;
            }
            if let Some(status) = patch_status {
                node.status = status;
            }
            if let Some(archived) = patch_archived {
                node.archived = archived;
            }
            if let Some(priority) = patch_priority {
                node.priority = priority;
            }
            if let Some(path) = patch_path {
                node.path = path;
            }
            if let Some(updated_at) = patch_updated_at {
                node.updated_at = updated_at;
            }
        }
    }

    pub fn get_node(&self, id: &str) -> Option<&NodeRow> {
        self.nodes.get(id)
    }

    /// `SELECT … ORDER BY id` with optional type/status equality filters.
    pub fn list_nodes(
        &self,
        node_type: Option<NodeType>,
        status: Option<NodeStatus>,
    ) -> Vec<NodeRow> {
        self.nodes
            .values()
            .filter(|node| node_type.is_none_or(|t| node.node_type == t))
            .filter(|node| status.is_none_or(|s| node.status == s))
            .cloned()
            .collect()
    }

    // ── edges ────────────────────────────────────────────────────────

    pub fn insert_edge(&mut self, parent_id: &str, child_id: &str, ord: i64) {
        self.edges
            .retain(|edge| !(edge.parent_id == parent_id && edge.child_id == child_id));
        self.edges.push(EdgeRow {
            parent_id: parent_id.to_string(),
            child_id: child_id.to_string(),
            ord,
        });
    }

    pub fn delete_edge(&mut self, parent_id: &str, child_id: &str) {
        self.edges
            .retain(|edge| !(edge.parent_id == parent_id && edge.child_id == child_id));
    }

    /// Children of one parent, `ORDER BY e.ord, e.child_id`.
    pub fn children_of(&self, parent_id: &str) -> Vec<NodeRow> {
        let mut child_ids: Vec<(&i64, &String)> = self
            .edges
            .iter()
            .filter(|edge| edge.parent_id == parent_id)
            .map(|edge| (&edge.ord, &edge.child_id))
            .collect();
        child_ids.sort();
        child_ids
            .into_iter()
            .filter_map(|(_, child_id)| self.nodes.get(child_id).cloned())
            .collect()
    }

    pub fn parent_of(&self, child_id: &str) -> Option<NodeRow> {
        self.edges
            .iter()
            .find(|edge| edge.child_id == child_id)
            .and_then(|edge| self.nodes.get(&edge.parent_id))
            .cloned()
    }

    /// All edges `ORDER BY parent_id, ord`.
    pub fn all_edges(&self) -> Vec<EdgeRow> {
        let mut edges = self.edges.clone();
        edges.sort_by(|a, b| (&a.parent_id, &a.ord).cmp(&(&b.parent_id, &b.ord)));
        edges
    }

    // ── content search ───────────────────────────────────────────────

    pub fn index_node(&mut self, id: &str, title: &str, body: &str) {
        self.search.insert(
            id.to_string(),
            SearchEntry {
                title: title.to_string(),
                body: body.to_string(),
            },
        );
    }

    /// Content search over titles and bodies: every whitespace-separated
    /// token must appear somewhere (AND); title hits rank before body-only
    /// hits; ties break by id. LIKE is case-insensitive over ASCII (SQLite
    /// default), which is all the corpus exercises.
    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
        if tokens.is_empty() {
            return vec![];
        }
        let contains_token = |haystack: &str, token: &str| -> bool {
            let needle_lower = token.to_ascii_lowercase();
            haystack.to_ascii_lowercase().contains(&needle_lower)
        };
        let mut scored: Vec<(bool, String)> = self
            .search
            .iter()
            .filter(|(_, entry)| {
                tokens.iter().all(|token| {
                    contains_token(&entry.title, token) || contains_token(&entry.body, token)
                })
            })
            .map(|(id, entry)| {
                let title_hit = tokens
                    .iter()
                    .all(|token| contains_token(&entry.title, token));
                (title_hit, id.clone())
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().take(limit).map(|(_, id)| id).collect()
    }

    // ── runs ─────────────────────────────────────────────────────────

    pub fn insert_run(&mut self, run: RunRow) {
        self.runs.insert(run.id.clone(), run);
    }

    pub fn update_run_status(&mut self, id: &str, status: RunStatus, finished_at: Option<String>) {
        if let Some(run) = self.runs.get_mut(id) {
            run.status = status;
            run.finished_at = finished_at;
        }
    }

    pub fn update_run_finished_at_null(&mut self, id: &str) {
        if let Some(run) = self.runs.get_mut(id) {
            run.finished_at = None;
        }
    }

    pub fn get_run(&self, id: &str) -> Option<&RunRow> {
        self.runs.get(id)
    }

    /// Runs ordered by id, optionally filtered to one status.
    pub fn list_runs(&self, status: Option<RunStatus>) -> Vec<RunRow> {
        self.runs
            .values()
            .filter(|run| status.is_none_or(|s| run.status == s))
            .cloned()
            .collect()
    }

    /// All runs whose status is one of `statuses` (janitor sweep input),
    /// `ORDER BY id`.
    pub fn list_runs_by_status(&self, statuses: &[RunStatus]) -> Vec<RunRow> {
        self.runs
            .values()
            .filter(|run| statuses.contains(&run.status))
            .cloned()
            .collect()
    }

    // ── run items ────────────────────────────────────────────────────

    pub fn insert_run_item(&mut self, item: RunItemRow) {
        self.run_items
            .insert((item.run_id.clone(), item.node_id.clone()), item);
    }

    /// Patch selected fields of one item row (`applyPatch` equivalent:
    /// only supplied fields move).
    #[allow(clippy::too_many_arguments)]
    pub fn update_run_item(
        &mut self,
        run_id: &str,
        node_id: &str,
        state: Option<RunItemState>,
        position: Option<i64>,
        executor_session_id: Option<Option<String>>,
        attempts: Option<i64>,
        last_error: Option<Option<String>>,
        nudged_at: Option<Option<String>>,
        nudge_count: Option<i64>,
        started_at: Option<Option<String>>,
        finished_at: Option<Option<String>>,
    ) {
        if let Some(item) = self
            .run_items
            .get_mut(&(run_id.to_string(), node_id.to_string()))
        {
            if let Some(state) = state {
                item.state = state;
            }
            if let Some(position) = position {
                item.position = position;
            }
            if let Some(executor) = executor_session_id {
                item.executor_session_id = executor;
            }
            if let Some(attempts) = attempts {
                item.attempts = attempts;
            }
            if let Some(last_error) = last_error {
                item.last_error = last_error;
            }
            if let Some(nudged_at) = nudged_at {
                item.nudged_at = nudged_at;
            }
            if let Some(nudge_count) = nudge_count {
                item.nudge_count = nudge_count;
            }
            if let Some(started_at) = started_at {
                item.started_at = started_at;
            }
            if let Some(finished_at) = finished_at {
                item.finished_at = finished_at;
            }
        }
    }

    pub fn get_run_item(&self, run_id: &str, node_id: &str) -> Option<&RunItemRow> {
        self.run_items
            .get(&(run_id.to_string(), node_id.to_string()))
    }

    pub fn delete_run_item(&mut self, run_id: &str, node_id: &str) {
        self.run_items
            .remove(&(run_id.to_string(), node_id.to_string()));
    }

    /// Items of one run `ORDER BY position, node_id`.
    pub fn list_run_items(&self, run_id: &str) -> Vec<RunItemRow> {
        let mut items: Vec<&RunItemRow> = self
            .run_items
            .iter()
            .filter(|((run, _), _)| run == run_id)
            .map(|(_, item)| item)
            .collect();
        items.sort_by(|a, b| (a.position, &a.node_id).cmp(&(b.position, &b.node_id)));
        items.into_iter().cloned().collect()
    }

    /// Per-state member counts for one run, sorted by state name for a
    /// deterministic projection (SQL GROUP BY order is unspecified and never
    /// asserted by the corpus).
    pub fn run_item_state_counts(&self, run_id: &str) -> Vec<(RunItemState, i64)> {
        let mut counts: BTreeMap<String, (RunItemState, i64)> = BTreeMap::new();
        for item in self.list_run_items(run_id) {
            counts
                .entry(item.state.to_string())
                .or_insert_with(|| (item.state, 0))
                .1 += 1;
        }
        counts.into_values().collect()
    }

    /// Every item for one node across runs in the given statuses,
    /// `ORDER BY i.run_id`, carrying each run's status.
    pub fn run_items_for_node(
        &self,
        node_id: &str,
        run_statuses: &[RunStatus],
    ) -> Vec<(RunItemRow, RunStatus)> {
        let mut rows: Vec<(&String, RunItemRow, RunStatus)> = self
            .run_items
            .iter()
            .filter_map(|((_, node), item)| {
                if node != node_id {
                    return None;
                }
                let run = self.runs.get(&item.run_id)?;
                if run_statuses.contains(&run.status) {
                    Some((&item.run_id, item.clone(), run.status))
                } else {
                    None
                }
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        rows.into_iter()
            .map(|(_, item, status)| (item, status))
            .collect()
    }

    /// Atomic claim (TICKET-0058): drain unexecutable pending members to
    /// skipped, then claim the first executable pending item — both sides
    /// returned so the core can broadcast transitions in order.
    /// Selection order: `position, node_id`.
    pub fn claim_next_run_item(
        &mut self,
        run_id: &str,
        executor_session_id: &str,
        now: &str,
    ) -> super::runs::ClaimNextResult {
        // Pass 1: pending members whose node is archived OR not executable.
        let mut skipped_ids: Vec<String> = self
            .list_run_items(run_id)
            .into_iter()
            .filter(|item| item.state == RunItemState::Pending)
            .filter(|item| match self.get_node(&item.node_id) {
                Some(node) => node.archived || !is_run_member_node_type(node.node_type),
                None => true, // dangling membership rows can never execute
            })
            .map(|item| item.node_id)
            .collect();
        skipped_ids.sort(); // list_run_items already orders position,node_id

        for node_id in &skipped_ids {
            self.update_run_item(
                run_id,
                node_id,
                Some(RunItemState::Skipped),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(Some(now.to_string())),
            );
        }

        // Pass 2: first remaining executable non-archived pending item.
        let claimed_id = self
            .list_run_items(run_id)
            .into_iter()
            .find(|item| {
                item.state == RunItemState::Pending
                    && matches!(self.get_node(&item.node_id), Some(node) if !node.archived && is_run_member_node_type(node.node_type))
            })
            .map(|item| item.node_id);

        if let Some(node_id) = &claimed_id {
            self.update_run_item(
                run_id,
                node_id,
                Some(RunItemState::Running),
                None,
                Some(Some(executor_session_id.to_string())),
                None,
                None,
                None,
                None,
                Some(started_at_preserve(
                    self.get_run_item(run_id, node_id)
                        .and_then(|item| item.started_at.clone()),
                    now,
                )),
                None,
            );
        }

        ClaimNextResult {
            claimed: claimed_id
                .map(|node_id| self.get_run_item(run_id, &node_id).cloned().unwrap()),
            skipped: skipped_ids
                .into_iter()
                .map(|node_id| self.get_run_item(run_id, &node_id).cloned().unwrap())
                .collect(),
        }
    }

    // ── rebuild (reindex) ────────────────────────────────────────────

    /// Replace the whole index content. Protection (EPIC-0003 decision 13):
    /// `runs`/`run_items` are deliberately NOT touched — a rebuild must
    /// never drop run history.
    pub fn rebuild(
        &mut self,
        nodes: Vec<NodeRow>,
        edges: Vec<EdgeRow>,
        bodies: BTreeMap<String, (String, String)>,
    ) {
        self.edges.clear();
        self.nodes.clear();
        self.search.clear();
        for edge in edges {
            self.edges.push(edge);
        }
        for node in nodes {
            self.nodes.insert(node.id.clone(), node);
        }
        for (id, (title, body)) in bodies {
            self.index_node(&id, &title, &body);
        }
        let ids: Vec<String> = self.nodes.keys().cloned().collect();
        self.reset_counters(&ids);
        self.mark_schema_version();
    }
}

fn started_at_preserve(existing: Option<String>, now: &str) -> Option<String> {
    // COALESCE(started_at, ?): keep the original dispatch stamp.
    existing.or_else(|| Some(now.to_string()))
}

// ── file-system port ────────────────────────────────────────────────────

/// Node-file storage port: the domain reads/writes whole files through this
/// interface. The corpus leg backs it with an in-memory virtual FS; U4's
/// storage crate swaps in real disk IO with atomic writes.
pub trait FileStore {
    /// Read one home-relative file as UTF-8 text.
    fn read_file(&mut self, rel_path: &str) -> Result<String>;
    /// Write/overwrite one home-relative file (creating directories).
    fn write_file(&mut self, rel_path: &str, content: &str) -> Result<()>;
    /// Rename a directory (subtree included) — used by move.
    fn move_dir(&mut self, old_rel_dir: &str, new_rel_dir: &str) -> Result<()>;
    /// Delete one home-relative file.
    fn delete_file(&mut self, rel_path: &str) -> Result<()>;
    /// True when the path exists as a file or directory.
    fn exists(&self, rel_path: &str) -> bool;
    /// True when the path exists as a directory.
    fn is_dir(&self, rel_path: &str) -> bool;
    /// Create a directory (recursively).
    fn mkdir_all(&mut self, rel_path: &str) -> Result<()>;
    /// List every `.md` file under `tickets/`, sorted by relative path.
    fn list_markdown_under_tickets(&self) -> Vec<String>;
}

/// Parse a stored node file via the codec (IO error mapped like
/// `OmtFiles.readNode`: missing file → IO problem).
pub fn read_node_file(files: &mut dyn FileStore, rel_path: &str) -> Result<ParsedNodeFile> {
    let content = files.read_file(rel_path).map_err(|problem| {
        if problem.code == error::IO {
            Problem::new(error::IO, format!("node file missing: {rel_path}"))
        } else {
            problem
        }
    })?;
    markdown::parse_node_file(&content)
}

/// Build frontmatter attribute lines in `frontmatterOf` insertion order.
pub fn frontmatter_lines(node: &NodeRow, parent_id: Option<&str>) -> Vec<(&'static str, Value)> {
    let mut lines: Vec<(&'static str, Value)> = vec![
        ("id", Value::String(node.id.clone())),
        ("type", Value::String(node.node_type.to_string())),
        ("title", Value::String(node.title.clone())),
        ("status", Value::String(node.status.to_string())),
    ];
    if node.archived {
        lines.push(("archived", Value::Bool(true)));
    }
    lines.push(("priority", Value::Number(node.priority.into())));
    if let Some(parent_id) = parent_id {
        lines.push(("parent", Value::String(parent_id.to_string())));
    }
    lines.push(("created_at", Value::String(node.created_at.clone())));
    lines.push(("updated_at", Value::String(node.updated_at.clone())));
    lines
}

/// Convenience: serialize from a node row + body.
pub fn serialize_from_row(node: &NodeRow, parent_id: Option<&str>, body: &str) -> String {
    markdown::serialize_node_file(frontmatter_lines(node, parent_id), body)
}

/// Extract attrs into an owned map (used by tests).
pub fn attrs_to_value(attrs: &FrontmatterAttrs) -> Value {
    let mut map = Map::new();
    let put = |map: &mut Map<String, Value>, key: &str, value: &Option<String>| {
        if let Some(value) = value {
            map.insert(key.to_string(), Value::String(value.clone()));
        }
    };
    put(&mut map, "id", &attrs.id);
    put(&mut map, "type", &attrs.node_type);
    put(&mut map, "title", &attrs.title);
    put(&mut map, "status", &attrs.status);
    if let Some(archived) = attrs.archived {
        map.insert("archived".into(), Value::Bool(archived));
    }
    if let Some(priority) = attrs.priority {
        if let Some(number) = serde_json::Number::from_f64(priority) {
            map.insert("priority".into(), Value::Number(number));
        }
    }
    put(&mut map, "parent", &attrs.parent);
    put(&mut map, "created_at", &attrs.created_at);
    put(&mut map, "updated_at", &attrs.updated_at);
    Value::Object(map)
}
