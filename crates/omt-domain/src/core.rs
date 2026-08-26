//! OmtCore — Rust port of the TypeScript orchestration layer
//! (`src/host/core.ts`): dual-write mutations across the metadata store and
//! the markdown file tree, run lifecycle, passive observation, trust gate,
//! claims, reports, and the startup janitor.
//!
//! Everything decision-shaped lives in sibling modules (`hierarchy`,
//! `runs`, `janitor`) as pure functions; this module sequences their
//! application with the same side-effect ORDER as core.ts, because the
//! frozen corpus pins observable orderings: event emission after mutation,
//! write-before-observe failure propagation (`report-write-order.json`),
//! derivation after final transitions, claim events before dispatch.
//!
//! The database side ([`Store`]) lives behind a shared handle so a
//! close/reopen cycle keeps its contents — exactly what SQLite-on-disk does
//! for the TypeScript leg. Only `reopen {freshDb:true}` wipes it.

use crate::error::{self, not_found_run, not_found_run_item, Problem, Result};
use crate::hierarchy;
use crate::janitor::{plan_sweep, SweepPlan, SweepRun};
use crate::markdown::{
    render_children_entries, replace_children_block, serialize_node_file, strip_children_block,
    ChildEntry,
};
use crate::ports::{Clock, LeaseGrant, LeaseTable};
use crate::runs::{derive_terminal, trust_gate_gates, validate_concurrency, ClaimNextResult};
use crate::store::{frontmatter_lines, read_node_file, FileStore, Store};
use crate::types::*;
use omt_contracts::{NodeStatus, NodeType, RunItemState, RunStatus};
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;

// ── events (TICKET-0065) ────────────────────────────────────────────────

/// Run/item transition broadcast emitted AFTER the mutation lands, carrying
/// post-change snapshots. Item events fire for transitionItem/retryItem/
/// replayItem and both halves of a claim; run events fire for every status
/// change (setRunStatus — including stop-on-failure pauses and terminal
/// derivation). Janitor ITEM demotions use the store directly and NEVER emit
/// item events; listeners attach post-open so startup demotions stay
/// invisible (the runner re-attaches collectors after reopen to mirror this).
#[derive(Debug, Clone)]
pub struct RunEvent {
    pub kind_is_item: bool,
    pub run: RunRow,
    pub item: Option<RunItemRow>,
    pub from_item_state: Option<RunItemState>,
    pub from_run_status: Option<RunStatus>,
}

// ── inputs ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct CreateInput {
    /// Pre-allocated id (pool-wide uniqueness); omit to use the counter.
    pub id: Option<String>,
    /// Raw type string; validated against the contracts enum.
    pub node_type: String,
    pub title: String,
    pub parent_id: Option<String>,
    /// Replace the default role template body.
    pub body: Option<String>,
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateInput {
    pub id: String,
    pub title: Option<String>,
    pub status: Option<String>,
    /// Archive (true) / restore (false) — the only change an archived node accepts.
    pub archived: Option<bool>,
    pub priority: Option<i64>,
    /// Replace the whole user body.
    pub body: Option<String>,
    /// Append a progress note (ignored when body is set).
    pub append: Option<String>,
    /// Session behind this change (observation attribution / trust gate).
    pub executor_session_id: Option<String>,
    /// Explicit-report signal (TICKET-0064): rides an omt_run_report
    /// double-write and must bypass the awaiting_confirmation gate.
    pub reported: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TransitionOptions {
    pub executor_session_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AddRunMemberInput {
    pub node_id: String,
    /// `'running'` is the explicit in_progress join (direct insert, no transition).
    pub state: Option<String>,
    pub executor_session_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateRunInput {
    pub title: Option<String>,
    /// Raw JSON config overrides; missing keys take DEFAULT_RUN_CONFIG.
    pub config: Option<Value>,
    pub node_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ReindexResult {
    pub nodes: usize,
    pub edges: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub struct JanitorResult {
    pub interrupted_runs: Vec<String>,
    pub interrupted_items: Vec<RunItemRow>,
}

#[derive(Debug, Clone)]
pub struct ReportResult {
    pub item: RunItemRow,
    pub node: NodeRow,
}

#[derive(Debug, Clone)]
pub struct AddRunMembersResult {
    pub added: Vec<RunItemRow>,
    pub duplicates: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ShowResult {
    pub node: NodeRow,
    /// Raw file body INCLUDING the managed children block (`show.body` in TS).
    pub body: String,
    pub parent: Option<NodeRow>,
    pub children: Vec<NodeRow>,
}

/// Shared ports bundle handed to [`OmtCore::open`]. The store handle is
/// shared across open/close cycles (the database outlives one core object).
pub struct CoreDeps {
    pub clock: Rc<RefCell<dyn Clock>>,
    pub files: Rc<RefCell<dyn FileStore>>,
    pub leases: Rc<RefCell<dyn LeaseTable>>,
    pub store: Rc<RefCell<Store>>,
}

/// One change listener registered on a core (clippy::type_complexity).
type Listener = Rc<dyn Fn(&RunEvent)>;

/// The dual-write core bound to one home.
pub struct OmtCore {
    home: String,
    deps: CoreDeps,
    listeners: Vec<Listener>,
}

impl OmtCore {
    // ── open / close ────────────────────────────────────────────────────

    /// Open the home: create directories, open the database, reindex when
    /// the database is fresh but markdown files exist, then run the startup
    /// janitor with the given live-session binding (ratified decision 1:
    /// the list becomes an exclusive far-future lease set at the port).
    pub fn open(home: &str, active_session_ids: &[String], deps: CoreDeps) -> Result<OmtCore> {
        deps.files.borrow_mut().mkdir_all("tickets")?;
        let core = OmtCore {
            home: home.to_string(),
            deps,
            listeners: Vec::new(),
        };
        let mut core = core;
        if core.db().schema_version().is_none() {
            let existing = core.deps.files.borrow().list_markdown_under_tickets();
            if !existing.is_empty() {
                core.reindex()?;
            } else {
                core.db_mut().mark_schema_version();
            }
        }
        core.deps
            .leases
            .borrow_mut()
            .mark_exclusive(active_session_ids);
        core.janitor_sweep()?;
        Ok(core)
    }

    /// Drop every database table's content (crash boundary:
    /// `reopen {freshDb:true}` deletes the SQLite files before reopening).
    pub fn wipe_database(&self) {
        *self.deps.store.borrow_mut() = Store::default();
    }

    pub fn home(&self) -> &str {
        &self.home
    }

    fn db(&self) -> std::cell::Ref<'_, Store> {
        self.deps.store.borrow()
    }

    fn db_mut(&self) -> std::cell::RefMut<'_, Store> {
        self.deps.store.borrow_mut()
    }

    fn now(&self) -> String {
        self.deps.clock.borrow().now_iso()
    }

    // ── events ──────────────────────────────────────────────────────────

    /// Subscribe to run/item transitions; returns the detach index.
    pub fn on_run_event(&mut self, listener: Rc<dyn Fn(&RunEvent)>) -> usize {
        self.listeners.push(listener);
        self.listeners.len() - 1
    }

    pub fn detach_run_event(&mut self, index: usize) {
        if index < self.listeners.len() {
            self.listeners.remove(index);
        }
    }

    pub fn clear_run_events(&mut self) {
        self.listeners.clear();
    }

    fn emit(&self, event: RunEvent) {
        for listener in &self.listeners {
            listener(&event);
        }
    }

    fn emit_item_event(&self, run: RunRow, item: &RunItemRow, from: RunItemState) {
        self.emit(RunEvent {
            kind_is_item: true,
            run,
            item: Some(item.clone()),
            from_item_state: Some(from),
            from_run_status: None,
        });
    }

    // ── create ──────────────────────────────────────────────────────────

    pub fn create(&mut self, input: CreateInput) -> Result<NodeRow> {
        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(Problem::with_details(
                error::INVALID_INPUT,
                "title must not be empty",
                |d| {
                    d.insert("field".into(), "title".into());
                },
            ));
        }
        let node_type = parse_node_type(&input.node_type).ok_or_else(|| {
            Problem::with_details(
                error::INVALID_INPUT,
                format!("unknown node type: {}", input.node_type),
                |d| {
                    d.insert("field".into(), "type".into());
                    d.insert("value".into(), input.node_type.clone().into());
                },
            )
        })?;

        let parent = match &input.parent_id {
            None => {
                hierarchy::check_root_allowed(node_type)?;
                None
            }
            Some(parent_id) => {
                let parent = hierarchy::require_node(&self.db(), parent_id)?.clone();
                hierarchy::check_child_type(&parent, node_type)?;
                Some(parent)
            }
        };

        let id = match &input.id {
            Some(explicit) => explicit.clone(),
            None => self.db_mut().next_id(node_type),
        };
        if self.db().get_node(&id).is_some() {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!("duplicate node id: {id}"),
                |d| {
                    d.insert("rule".into(), "duplicate-node-id".into());
                    d.insert("nodeId".into(), id.clone().into());
                },
            ));
        }

        let now = self.now();
        let path = markdown_path_for(
            &node_type.to_string(),
            &id,
            &title,
            parent.as_ref().map(|p| p.path.as_str()),
        );
        let node = NodeRow {
            id: id.clone(),
            node_type,
            title,
            status: NodeStatus::Open,
            archived: false,
            priority: input.priority.unwrap_or(0),
            path: path.clone(),
            created_at: now.clone(),
            updated_at: now,
        };

        let body = input
            .body
            .clone()
            .unwrap_or_else(|| default_body(&node_type));
        let empty_children = render_children_entries(&[]);
        let full_body = replace_children_block(&body, &empty_children);
        let content = serialize_node_file(
            frontmatter_lines(&node, parent.as_ref().map(|p| p.id.as_str())),
            &full_body,
        );
        self.deps.files.borrow_mut().write_file(&path, &content)?;

        self.db_mut().insert_node(node.clone());
        if let Some(parent) = &parent {
            let ord = self.db().children_of(&parent.id).len() as i64;
            self.db_mut().insert_edge(&parent.id, &id, ord);
        }
        self.db_mut()
            .index_node(&id, &node.title, &strip_children_block(&full_body));
        if let Some(parent) = &parent {
            self.refresh_children_block(&parent.id)?;
        }
        Ok(node)
    }

    // ── update ──────────────────────────────────────────────────────────

    pub fn update(&mut self, input: UpdateInput) -> Result<NodeRow> {
        let node = hierarchy::require_node(&self.db(), &input.id)?.clone();

        // Archived nodes are sealed: the only accepted change is restoring.
        if node.archived && input.archived != Some(false) {
            let touches_content = input.title.is_some()
                || input.status.is_some()
                || input.priority.is_some()
                || input.body.is_some()
                || input.append.is_some();
            if touches_content || input.archived == Some(true) {
                return Err(Problem::with_details(
                    error::ARCHIVED_READONLY,
                    format!("{} 已归档，请先恢复（archived: false）再做修改", input.id),
                    |d| {
                        d.insert("nodeId".into(), input.id.clone().into());
                        d.insert("operation".into(), "update".into());
                    },
                ));
            }
        }

        let now = self.now();
        let file = read_node_file(&mut *self.deps.files.borrow_mut(), &node.path)?;
        let parent = self.db().parent_of(&node.id);

        let mut patch_title: Option<String> = None;
        let mut patch_status: Option<NodeStatus> = None;
        if let Some(raw_title) = &input.title {
            let trimmed = raw_title.trim().to_string();
            if trimmed.is_empty() {
                return Err(Problem::with_details(
                    error::INVALID_INPUT,
                    "title must not be empty",
                    |d| {
                        d.insert("field".into(), "title".into());
                        d.insert("nodeId".into(), input.id.clone().into());
                    },
                ));
            }
            patch_title = Some(trimmed);
        }
        if let Some(raw_status) = &input.status {
            let status = parse_node_status(raw_status).ok_or_else(|| {
                Problem::with_details(
                    error::INVALID_INPUT,
                    format!("unknown status: {raw_status}"),
                    |d| {
                        d.insert("field".into(), "status".into());
                        d.insert("value".into(), raw_status.clone().into());
                        d.insert("nodeId".into(), input.id.clone().into());
                    },
                )
            })?;
            patch_status = Some(status);
        }

        // Body replacement / append (append collapses trailing whitespace).
        let mut body = file.body;
        if let Some(new_body) = &input.body {
            body = new_body.clone();
        } else if let Some(appendage) = &input.append {
            body = format!("{}\n\n{}\n", body.trim_end(), appendage);
        }

        self.db_mut().update_node(
            &node.id,
            patch_title.clone(),
            patch_status,
            input.archived,
            input.priority,
            None,
            Some(now.clone()),
        );
        let updated = NodeRow {
            title: patch_title.clone().unwrap_or_else(|| node.title.clone()),
            status: patch_status.unwrap_or(node.status),
            archived: input.archived.unwrap_or(node.archived),
            priority: input.priority.unwrap_or(node.priority),
            updated_at: now,
            ..node.clone()
        };
        let content = serialize_node_file(
            frontmatter_lines(&updated, parent.as_ref().map(|p| p.id.as_str())),
            &body,
        );
        self.deps
            .files
            .borrow_mut()
            .write_file(&node.path, &content)?;
        self.db_mut()
            .index_node(&node.id, &updated.title, &strip_children_block(&body));

        // A title change shows up in the parent's managed children list.
        if patch_title.is_some() {
            if let Some(parent) = &parent {
                self.refresh_children_block(&parent.id)?;
            }
        }

        // Passive observation (TICKET-0061) funnels through here: every
        // ordinary update path broadcasts the change to active runs exactly
        // once — callers must not re-observe.
        let observed_status = input.status.as_deref().and_then(parse_node_status_opt);
        self.observe_node_status(
            &input.id,
            observed_status,
            input.archived,
            input.executor_session_id.as_deref(),
            input.reported,
        )?;

        // A status change shows up in the parent's managed children list too.
        if patch_title.is_some() || patch_status.is_some() {
            if let Some(parent) = parent.as_ref() {
                let parent_id = parent.id.clone();
                self.refresh_children_block(&parent_id)?;
            }
        }

        // Ancestor activation (STORY-0022): work starting anywhere lights up
        // the chain above it; recursion through update() is idempotent.
        if input.status.as_deref() == Some("in_progress") {
            self.activate_ancestors(&input.id)?;
        }
        Ok(updated)
    }

    /// Upgrade every OPEN ancestor of `childId` to in_progress (STORY-0022).
    /// done/blocked/skipped ancestors are never reopened and archived ones
    /// are skipped silently: activation must never fail or mutate human
    /// decisions.
    fn activate_ancestors(&mut self, child_id: &str) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(child_id.to_string());
        let mut current = self.db().parent_of(child_id);
        while let Some(parent) = current {
            if seen.contains(&parent.id) {
                break;
            }
            seen.insert(parent.id.clone());
            if !parent.archived && parent.status == NodeStatus::Open {
                self.update(UpdateInput {
                    id: parent.id.clone(),
                    status: Some("in_progress".to_string()),
                    ..UpdateInput::default()
                })?;
            }
            current = self.db().parent_of(&parent.id);
        }
        Ok(())
    }

    // ── move ────────────────────────────────────────────────────────────

    pub fn move_node(&mut self, id: &str, new_parent_id: &str) -> Result<NodeRow> {
        let node = hierarchy::require_node(&self.db(), id)?.clone();
        let new_parent = hierarchy::require_node(&self.db(), new_parent_id)?.clone();
        hierarchy::check_self_parent(id, new_parent_id)?;
        hierarchy::check_child_type(&new_parent, node.node_type)?;
        hierarchy::check_descendant_cycle(&self.db(), id, new_parent_id)?;

        let old_parent = self.db().parent_of(id);
        let old_path = node.path.clone();
        let new_path = markdown_path_for(
            &node.node_type.to_string(),
            id,
            &node.title,
            Some(&new_parent.path),
        );
        if old_path == new_path {
            return Err(Problem::with_details(
                error::CONFLICT,
                "node is already at the target location",
                |d| {
                    d.insert("rule".into(), "already-at-target".into());
                    d.insert("nodeId".into(), id.into());
                },
            ));
        }

        let now = self.now();
        let old_dir = crate::markdown::dirname(&old_path);
        let new_dir = crate::markdown::dirname(&new_path);
        self.deps.files.borrow_mut().move_dir(&old_dir, &new_dir)?;

        // The move relocates the whole subtree: rewrite stored path prefixes.
        let subtree_ids: Vec<String> = std::iter::once(id.to_string())
            .chain(
                hierarchy::descendants_of(&self.db(), id)
                    .into_iter()
                    .map(|n| n.id),
            )
            .collect();
        for member_id in &subtree_ids {
            let member = self.db().get_node(member_id).cloned();
            let Some(member) = member else { continue };
            let relocated = member.path.replacen(&old_dir, &new_dir, 1);
            self.db_mut().update_node(
                member_id,
                None,
                None,
                None,
                None,
                Some(relocated),
                (member_id == id).then(|| now.clone()),
            );
        }

        if let Some(old_parent) = &old_parent {
            self.db_mut().delete_edge(&old_parent.id, id);
        }
        let ord = self.db().children_of(new_parent_id).len() as i64;
        self.db_mut().insert_edge(new_parent_id, id, ord);

        let moved = hierarchy::require_node(&self.db(), id)?.clone();
        let file = read_node_file(&mut *self.deps.files.borrow_mut(), &moved.path)?;
        let content =
            serialize_node_file(frontmatter_lines(&moved, Some(new_parent_id)), &file.body);
        self.deps
            .files
            .borrow_mut()
            .write_file(&moved.path, &content)?;

        if let Some(old_parent) = &old_parent {
            self.refresh_children_block(&old_parent.id)?;
        }
        self.refresh_children_block(new_parent_id)?;
        Ok(moved)
    }

    // ── queries ─────────────────────────────────────────────────────────

    pub fn get_node(&self, id: &str) -> Option<NodeRow> {
        self.db().get_node(id).cloned()
    }

    pub fn show(&self, id: &str) -> Result<ShowResult> {
        let node = hierarchy::require_node(&self.db(), id)?.clone();
        let file = read_node_file(&mut *self.deps.files.borrow_mut(), &node.path)?;
        Ok(ShowResult {
            body: file.body,
            node,
            parent: self.db().parent_of(id),
            children: self.db().children_of(id),
        })
    }

    /// List with search shortcut: a non-empty query routes through the
    /// content mirror instead of the filters.
    pub fn list(
        &self,
        node_type: Option<NodeType>,
        status: Option<NodeStatus>,
        query: Option<&str>,
    ) -> Vec<NodeRow> {
        if let Some(query) = query {
            if !query.trim().is_empty() {
                return self
                    .db()
                    .search(query, 20)
                    .into_iter()
                    .filter_map(|id| self.db().get_node(&id).cloned())
                    .collect();
            }
        }
        self.db().list_nodes(node_type, status)
    }

    /// Assemble the forest (epics as roots), children ordered by edge ord.
    /// Built from a parent→children index in normative (parent_id, ord)
    /// order, then assembled RECURSIVELY so nesting depth never depends on
    /// edge iteration order.
    pub fn tree(&self, root_id: Option<&str>) -> Result<Vec<TreeNodeRow>> {
        let db = self.db();
        let ordered_edges = db.all_edges();
        let mut children_of: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for edge in &ordered_edges {
            children_of
                .entry(edge.parent_id.clone())
                .or_default()
                .push(edge.child_id.clone());
        }
        fn build(
            id: &str,
            db: &Store,
            children_of: &std::collections::BTreeMap<String, Vec<String>>,
        ) -> TreeNodeRow {
            TreeNodeRow {
                node: db
                    .get_node(id)
                    .expect("edge targets an indexed node")
                    .clone(),
                children: children_of
                    .get(id)
                    .map(|kids| kids.iter().map(|kid| build(kid, db, children_of)).collect())
                    .unwrap_or_default(),
            }
        }
        match root_id {
            None => {
                let roots: Vec<TreeNodeRow> = db
                    .list_nodes(None, None)
                    .iter()
                    .filter(|node| db.parent_of(&node.id).is_none())
                    .map(|node| build(&node.id, &db, &children_of))
                    .collect();
                Ok(roots)
            }
            Some(root_id) => {
                hierarchy::require_node(&db, root_id)?;
                Ok(vec![build(root_id, &db, &children_of)])
            }
        }
    }

    // ── reindex ─────────────────────────────────────────────────────────

    /// Rebuild the index from the markdown files (the content authority).
    /// Frontmatter `parent` fields define edges; only resolvable,
    /// hierarchy-legal links survive; managed blocks regenerate afterwards.
    pub fn reindex(&mut self) -> Result<ReindexResult> {
        let now = self.now();
        let paths = self.deps.files.borrow().list_markdown_under_tickets();

        let mut nodes: Vec<NodeRow> = Vec::new();
        let mut bodies: std::collections::BTreeMap<String, (String, String)> = Default::default();
        let mut pending_parent: Vec<(String, String)> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut skipped = 0usize;

        for rel_path in &paths {
            let parsed = self
                .deps
                .files
                .borrow_mut()
                .read_file(rel_path)
                .and_then(|content| crate::markdown::parse_node_file(&content));
            let Ok(file) = parsed else {
                skipped += 1;
                continue;
            };
            let attrs = file.attrs;
            let id_valid = attrs.id.as_deref().map(id_matches_pattern).unwrap_or(false);
            let type_valid = attrs
                .node_type
                .as_deref()
                .and_then(parse_node_type_opt)
                .is_some();
            let id = match attrs.id.clone() {
                Some(id) if id_valid && type_valid && !seen.contains(&id) => id,
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            seen.insert(id.clone());

            let title = attrs
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());
            let status = attrs
                .status
                .as_deref()
                .and_then(parse_node_status_opt)
                .unwrap_or(NodeStatus::Open);
            let priority = attrs.priority.map(|p| p as i64).unwrap_or(0);
            let node = NodeRow {
                id: id.clone(),
                node_type: parse_node_type_opt(attrs.node_type.as_deref().unwrap_or_default())
                    .unwrap(),
                title,
                status,
                archived: attrs.archived == Some(true),
                priority,
                path: rel_path.clone(),
                created_at: attrs.created_at.clone().unwrap_or_else(|| now.clone()),
                updated_at: attrs.updated_at.clone().unwrap_or_else(|| now.clone()),
            };
            pending_parent.extend(
                attrs
                    .parent
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|p| (id.clone(), p.to_string())),
            );
            bodies.insert(
                id.clone(),
                (node.title.clone(), strip_children_block(&file.body)),
            );
            nodes.push(node);
        }

        // Edges second pass: keep only resolvable, hierarchy-legal links.
        let mut edges: Vec<EdgeRow> = Vec::new();
        let mut ord_counter: std::collections::BTreeMap<String, i64> = Default::default();
        for (child_id, parent_id) in &pending_parent {
            let child = nodes.iter().find(|n| &n.id == child_id);
            let parent = nodes.iter().find(|n| &n.id == parent_id);
            match (child, parent) {
                (Some(child), Some(parent))
                    if hierarchy_allows(parent.node_type, child.node_type) =>
                {
                    let ord = ord_counter.entry(parent_id.clone()).or_insert(0);
                    edges.push(EdgeRow {
                        parent_id: parent_id.clone(),
                        child_id: child_id.clone(),
                        ord: *ord,
                    });
                    *ord += 1;
                }
                _ => {
                    skipped += 1;
                }
            }
        }

        self.db_mut().rebuild(nodes.clone(), edges.clone(), bodies);

        // Normalize the managed children blocks on disk from rebuilt edges.
        for node in &nodes {
            let entries: Vec<ChildEntry> = self
                .db()
                .children_of(&node.id)
                .iter()
                .map(|child| ChildEntry {
                    id: child.id.clone(),
                    title: child.title.clone(),
                    dir_name: crate::markdown::node_dir_name(&child.id, &child.title),
                    node_type: child.node_type.to_string(),
                    status: child.status.to_string(),
                })
                .collect();
            let block = render_children_entries(&entries);
            let file = read_node_file(&mut *self.deps.files.borrow_mut(), &node.path)?;
            let body = replace_children_block(&file.body, &block);
            let parent_id = edges
                .iter()
                .find(|edge| edge.child_id == node.id)
                .map(|edge| edge.parent_id.clone());
            let content = serialize_node_file(frontmatter_lines(node, parent_id.as_deref()), &body);
            self.deps
                .files
                .borrow_mut()
                .write_file(&node.path, &content)?;
        }

        Ok(ReindexResult {
            nodes: nodes.len(),
            edges: edges.len(),
            skipped,
        })
    }

    // ── runs: creation & queries (EPIC-0003) ────────────────────────────

    pub fn create_run(&mut self, input: CreateRunInput) -> Result<RunRow> {
        let mut seen = std::collections::BTreeSet::new();
        for node_id in &input.node_ids {
            if !seen.insert(node_id.clone()) {
                return Err(Problem::with_details(
                    error::DUPLICATE_MEMBER,
                    format!("duplicate run member: {node_id}"),
                    |d| {
                        d.insert("nodeId".into(), node_id.clone().into());
                    },
                ));
            }
            self.require_run_member_node(node_id)?;
        }

        let config_json = merge_run_config(input.config.as_ref());
        let concurrency =
            validate_concurrency(config_json.get("concurrency").unwrap_or(&Value::Null))?;
        let config = RunConfigValue {
            stop_on_failure: config_json["stopOnFailure"].as_bool().unwrap_or(false),
            auto_continue: config_json["autoContinue"].as_bool().unwrap_or(true),
            auto_verify: config_json["autoVerify"].as_bool().unwrap_or(false),
            concurrency,
        };

        let id = self.db_mut().next_run_id();
        let title = input
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        let run = RunRow {
            id: id.clone(),
            title,
            status: RunStatus::Pending,
            config,
            created_at: self.now(),
            finished_at: None,
        };
        self.db_mut().insert_run(run.clone());
        for (position, node_id) in input.node_ids.iter().enumerate() {
            self.db_mut().insert_run_item(RunItemRow::new(
                &id,
                node_id,
                position as i64,
                RunItemState::Pending,
            ));
        }
        Ok(run)
    }

    pub fn get_run(&self, id: &str) -> Option<RunRow> {
        self.db().get_run(id).cloned()
    }

    pub fn require_run(&self, id: &str) -> Result<RunRow> {
        self.store_get_run(id).ok_or_else(|| not_found_run(id))
    }

    fn store_get_run(&self, id: &str) -> Option<RunRow> {
        self.db().get_run(id).cloned()
    }

    pub fn list_runs(&self, status: Option<RunStatus>) -> Vec<RunRow> {
        self.db().list_runs(status)
    }

    pub fn get_run_item(&self, run_id: &str, node_id: &str) -> Option<RunItemRow> {
        self.db().get_run_item(run_id, node_id).cloned()
    }

    pub fn run_items(&self, run_id: &str) -> Result<Vec<RunItemRow>> {
        self.require_run(run_id)?;
        Ok(self.db().list_run_items(run_id))
    }

    pub fn run_item_state_counts(&self, run_id: &str) -> Vec<(RunItemState, i64)> {
        self.db().run_item_state_counts(run_id)
    }

    /// Append members to an existing run (TICKET-0067). Only ACTIVE runs
    /// accept members; duplicates skip-and-report; membership validates
    /// against this home's nodes; `state:'running'` joins insert directly.
    pub fn add_run_members(
        &mut self,
        run_id: &str,
        members: &[AddRunMemberInput],
    ) -> Result<AddRunMembersResult> {
        let run = self.require_run(run_id)?;
        if !is_run_active(run.status) {
            let message = if run.status == RunStatus::Interrupted {
                format!("run {run_id} 处于 interrupted（需人工核对）；请先 resume 再加入成员")
            } else {
                format!(
                    "run {run_id} 已终态（{}），不可加入成员；请另建 run",
                    run.status
                )
            };
            return Err(Problem::with_details(error::CONFLICT, message, |d| {
                d.insert("rule".into(), "run-not-active".into());
                d.insert("runId".into(), run_id.into());
                d.insert("runStatus".into(), run.status.to_string().into());
            }));
        }

        let position = self
            .db()
            .list_run_items(run_id)
            .iter()
            .map(|item| item.position)
            .max()
            .unwrap_or(-1)
            + 1;
        self.add_run_members_inner(run_id, members, position)
    }

    fn add_run_members_inner(
        &mut self,
        run_id: &str,
        members: &[AddRunMemberInput],
        mut position: i64,
    ) -> Result<AddRunMembersResult> {
        let mut added = Vec::new();
        let mut duplicates = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for member in members {
            if seen.contains(&member.node_id)
                || self.db().get_run_item(run_id, &member.node_id).is_some()
            {
                duplicates.push(member.node_id.clone());
                continue;
            }
            seen.insert(member.node_id.clone());
            self.require_run_member_node(&member.node_id)?;
            let state_text = member
                .state
                .clone()
                .unwrap_or_else(|| "pending".to_string());
            let state = parse_item_state(&state_text).ok_or_else(|| {
                Problem::with_details(
                    error::INVALID_INPUT,
                    format!("unknown run item state: {state_text}"),
                    |d| {
                        d.insert("field".into(), "state".into());
                        d.insert("value".into(), state_text.clone().into());
                    },
                )
            })?;
            let mut item = RunItemRow::new(run_id, &member.node_id, position, state);
            if state == RunItemState::Running {
                item.started_at = Some(self.now());
                item.executor_session_id = member.executor_session_id.clone();
            }
            self.db_mut().insert_run_item(item.clone());
            added.push(item);
            position += 1;
        }
        Ok(AddRunMembersResult { added, duplicates })
    }

    /// Non-terminal runs holding one node with the node's item in each.
    /// Unknown nodes yield [] — callers surface NOT_FOUND themselves.
    pub fn runs_of_node(&self, node_id: &str) -> Vec<(RunRow, RunItemRow)> {
        self.db()
            .run_items_for_node(
                node_id,
                &[
                    RunStatus::Pending,
                    RunStatus::Running,
                    RunStatus::Paused,
                    RunStatus::Interrupted,
                ],
            )
            .into_iter()
            .filter_map(|(item, _)| {
                let run = self.store_get_run(&item.run_id)?;
                Some((run, item))
            })
            .collect()
    }

    // ── run lifecycle controls ──────────────────────────────────────────

    /// start: pending → running; an empty run derives completed immediately.
    pub fn start_run(&mut self, id: &str) -> Result<RunRow> {
        let run = self.require_run(id)?;
        if run.status != RunStatus::Pending {
            return Err(run_status_gate(id, run.status, &[RunStatus::Pending]));
        }
        self.set_run_status(id, RunStatus::Running)?;
        self.derive_run_terminal(id);
        self.require_run(id)
    }

    /// pause: running → paused; only dispatch stops; running items keep
    /// being observed.
    pub fn pause_run(&mut self, id: &str) -> Result<RunRow> {
        let run = self.require_run(id)?;
        if run.status != RunStatus::Running {
            return Err(run_status_gate(id, run.status, &[RunStatus::Running]));
        }
        self.set_run_status(id, RunStatus::Paused)?;
        self.require_run(id)
    }

    /// resume: paused|interrupted → running; interrupted items are NOT
    /// auto-reset (row-level retry only).
    pub fn resume_run(&mut self, id: &str) -> Result<RunRow> {
        let run = self.require_run(id)?;
        if run.status != RunStatus::Paused && run.status != RunStatus::Interrupted {
            return Err(run_status_gate(
                id,
                run.status,
                &[RunStatus::Paused, RunStatus::Interrupted],
            ));
        }
        self.set_run_status(id, RunStatus::Running)?;
        self.require_run(id)
    }

    /// cancel: pending/running/paused/interrupted → canceled; items freeze
    /// in place, tickets untouched.
    pub fn cancel_run(&mut self, id: &str) -> Result<RunRow> {
        self.set_run_status(id, RunStatus::Canceled)?;
        self.require_run(id)
    }

    /// Direct item transition with run-level gating: a running run allows
    /// all legal moves; a paused run lets only in-flight items advance.
    pub fn transition_item(
        &mut self,
        run_id: &str,
        node_id: &str,
        to: &str,
        options: TransitionOptions,
    ) -> Result<RunItemRow> {
        let to_state = parse_item_state(to).ok_or_else(|| {
            Problem::with_details(
                error::INVALID_INPUT,
                format!("unknown run item state: {to}"),
                |d| {
                    d.insert("field".into(), "to".into());
                    d.insert("value".into(), to.into());
                },
            )
        })?;
        let run = self.require_run(run_id)?;
        let item = self
            .get_run_item(run_id, node_id)
            .ok_or_else(|| not_found_run_item(run_id, node_id))?;

        if run.status == RunStatus::Paused {
            if !is_run_item_in_flight(item.state) {
                return Err(Problem::with_details(
                    error::CONFLICT,
                    format!(
                        "run {run_id} is paused; dispatch is stopped (item {node_id} is {})",
                        item.state
                    ),
                    |d| {
                        d.insert("rule".into(), "dispatch-paused".into());
                        d.insert("runId".into(), run_id.into());
                        d.insert("nodeId".into(), node_id.into());
                        d.insert("runStatus".into(), run.status.to_string().into());
                        d.insert("itemState".into(), item.state.to_string().into());
                    },
                ));
            }
        } else if run.status != RunStatus::Running {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!("run {run_id} is {}; items are frozen", run.status),
                |d| {
                    d.insert("rule".into(), "items-frozen".into());
                    d.insert("runId".into(), run_id.into());
                    d.insert("runStatus".into(), run.status.to_string().into());
                    d.insert("nodeId".into(), node_id.into());
                    d.insert("itemState".into(), item.state.to_string().into());
                },
            ));
        }
        if !item_transition_allowed(item.state, to_state) {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!(
                    "illegal item transition for {node_id}: {} → {}",
                    item.state, to_state
                ),
                |d| {
                    d.insert("rule".into(), "item-transition".into());
                    d.insert("runId".into(), run_id.into());
                    d.insert("nodeId".into(), node_id.into());
                    d.insert("from".into(), item.state.to_string().into());
                    d.insert("to".into(), to_state.to_string().into());
                },
            ));
        }

        let now = self.now();
        let entering_running = to_state == RunItemState::Running;
        let final_state = is_run_item_final(to_state);
        let last_error = if to_state == RunItemState::Failed {
            options.error.clone()
        } else {
            None
        };
        self.db_mut().update_run_item(
            run_id,
            node_id,
            Some(to_state),
            None,
            options.executor_session_id.clone().map(Some),
            None,
            last_error.map(Some),
            None,
            None,
            entering_running.then(|| Some(item.started_at.clone().unwrap_or_else(|| now.clone()))),
            final_state.then(|| Some(now.clone())),
        );

        // stop-on-failure: only `failed` triggers (blocked/skipped never do).
        if to_state == RunItemState::Failed
            && run.config.stop_on_failure
            && run.status == RunStatus::Running
        {
            self.set_run_status(run_id, RunStatus::Paused)?;
        }
        if final_state {
            self.derive_run_terminal(run_id);
        }
        let updated = self.get_run_item(run_id, node_id).unwrap();
        self.emit_item_event(self.require_run(run_id)?, &updated, item.state);
        Ok(updated)
    }

    /// Retry (decision 10): reset failed/interrupted/pending back to pending
    /// in place — attempts+1, last_error KEPT, nudge budget cleared,
    /// executor/timestamps cleared. Retrying inside completed_with_failures
    /// reopens it to running; canceled/completed accept no retry.
    pub fn retry_item(&mut self, run_id: &str, node_id: &str) -> Result<RunItemRow> {
        let run = self.require_run(run_id)?;
        if run.status == RunStatus::Canceled || run.status == RunStatus::Completed {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!("run {run_id} is {}; retry is unavailable", run.status),
                |d| {
                    d.insert("rule".into(), "retry-run-gate".into());
                    d.insert("runId".into(), run_id.into());
                    d.insert("runStatus".into(), run.status.to_string().into());
                },
            ));
        }
        let item = self
            .get_run_item(run_id, node_id)
            .ok_or_else(|| not_found_run_item(run_id, node_id))?;
        if !matches!(
            item.state,
            RunItemState::Failed | RunItemState::Interrupted | RunItemState::Pending
        ) {
            return Err(retry_state_gate(run_id, node_id, item.state));
        }
        self.db_mut().update_run_item(
            run_id,
            node_id,
            Some(RunItemState::Pending),
            None,
            Some(None),
            Some(item.attempts + 1),
            None, // last_error kept across retries
            Some(None),
            Some(0),
            Some(None),
            Some(None),
        );
        // The run has dispatchable work again: leave the terminal state.
        if run.status == RunStatus::CompletedWithFailures {
            self.set_run_status(run_id, RunStatus::Running)?;
        }
        let retried = self.get_run_item(run_id, node_id).unwrap();
        self.emit_item_event(self.require_run(run_id)?, &retried, item.state);
        Ok(retried)
    }

    /// Replay (decision 11): a member ticket went done/blocked/skipped →
    /// open while the run was in progress; falls back to pending keeping
    /// position and attempt history; the nudge budget resets so a replayed
    /// item must not read as stalled.
    pub fn replay_item(&mut self, run_id: &str, node_id: &str) -> Result<RunItemRow> {
        let run = self.require_run(run_id)?;
        if !matches!(
            run.status,
            RunStatus::Pending | RunStatus::Running | RunStatus::Paused | RunStatus::Interrupted
        ) {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!(
                    "run {run_id} is {}; replay requires an in-progress run",
                    run.status
                ),
                |d| {
                    d.insert("rule".into(), "replay-run-gate".into());
                    d.insert("runId".into(), run_id.into());
                    d.insert("runStatus".into(), run.status.to_string().into());
                },
            ));
        }
        let item = self
            .get_run_item(run_id, node_id)
            .ok_or_else(|| not_found_run_item(run_id, node_id))?;
        if !matches!(
            item.state,
            RunItemState::Done | RunItemState::Blocked | RunItemState::Skipped
        ) {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!(
                    "only done/blocked/skipped items can replay ({node_id} is {})",
                    item.state
                ),
                |d| {
                    d.insert("rule".into(), "replay-state-gate".into());
                    d.insert("runId".into(), run_id.into());
                    d.insert("nodeId".into(), node_id.into());
                    d.insert("itemState".into(), item.state.to_string().into());
                    d.insert(
                        "required".into(),
                        serde_json::json!(["done", "blocked", "skipped"]),
                    );
                },
            ));
        }
        self.db_mut().update_run_item(
            run_id,
            node_id,
            Some(RunItemState::Pending),
            None,
            Some(None),
            None, // attempts kept (history)
            None,
            Some(None),
            Some(0),
            Some(None),
            Some(None),
        );
        let replayed = self.get_run_item(run_id, node_id).unwrap();
        self.emit_item_event(self.require_run(run_id)?, &replayed, item.state);
        Ok(replayed)
    }

    /// Claim (TICKET-0058): atomically take the next pending item of a
    /// running run, bind the executor, and ISSUE ITS FENCED LEASE (R10).
    /// Emits drained-member [pending→skipped] events BEFORE the claimed
    /// [pending→running] event (ratified decision 3). Returns None when the
    /// queue is empty (explicit signal, not an error); claimed ownership
    /// wins over passive observation (decision 14).
    pub fn claim_run_item(
        &mut self,
        run_id: &str,
        executor_session_id: &str,
    ) -> Result<Option<RunItemRow>> {
        let run = self.require_run(run_id)?;
        if executor_session_id.trim().is_empty() {
            return Err(Problem::with_details(
                error::INVALID_INPUT,
                "claim requires an executor session id",
                |d| {
                    d.insert("field".into(), "executorSessionId".into());
                },
            ));
        }
        if run.status == RunStatus::Pending {
            return Err(run_status_gate(run_id, run.status, &[RunStatus::Running]));
        }
        if run.status != RunStatus::Running {
            return Err(run_status_gate(run_id, run.status, &[RunStatus::Running]));
        }

        let now = self.now();
        let ClaimNextResult { claimed, skipped } =
            self.db_mut()
                .claim_next_run_item(run_id, executor_session_id, &now);
        for item in &skipped {
            self.emit_item_event(self.require_run(run_id)?, item, RunItemState::Pending);
        }
        if let Some(claimed) = &claimed {
            self.emit_item_event(self.require_run(run_id)?, claimed, RunItemState::Pending);
            // Bind the attempt-fenced lease for the new owner (decision 1):
            // far-future expiry until the port layer expires/replaces it.
            let grant = LeaseGrant {
                token: format!("lease-{executor_session_id}-{}", claimed.attempts),
                attempt: claimed.attempts,
                principal: executor_session_id.to_string(),
                expires_at: crate::ports::FAR_FUTURE_MS,
            };
            self.deps
                .leases
                .borrow_mut()
                .issue(executor_session_id, grant);
            // Ancestor activation is best-effort on the claim path: a failed
            // cosmetic status write must never fail or undo the claim.
            let activation = self.activate_ancestors(&claimed.node_id);
            let _ = activation;
        }
        // When draining emptied the queue the run may be derivable now.
        if claimed.is_none() {
            self.derive_run_terminal(run_id);
        }
        Ok(claimed)
    }

    /// Every item owned by one executor session across active runs — the
    /// disposed hook's involvement probe (TICKET-0063). Ownership survives
    /// completion; callers filter by item state.
    pub fn executor_items(&self, session_id: &str) -> Vec<(RunRow, RunItemRow)> {
        let mut owned = Vec::new();
        for run in self
            .db()
            .list_runs_by_status(&[RunStatus::Running, RunStatus::Paused])
        {
            for item in self.db().list_run_items(&run.id) {
                if item.executor_session_id.as_deref() == Some(session_id) {
                    owned.push((run.clone(), item));
                }
            }
        }
        owned
    }

    /// Continuation candidates (TICKET-0062): RUNNING + autoContinue runs
    /// where the session owns ≥1 item (any state) → next pending item in
    /// position order. Paused runs gate out dispatch AND nudges (decision 9).
    pub fn continuation_candidates(&self, session_id: &str) -> Vec<(RunRow, RunItemRow)> {
        let mut candidates = Vec::new();
        for run in self.db().list_runs_by_status(&[RunStatus::Running]) {
            if !run.config.auto_continue {
                continue;
            }
            let items = self.db().list_run_items(&run.id);
            if !items
                .iter()
                .any(|item| item.executor_session_id.as_deref() == Some(session_id))
            {
                continue;
            }
            if let Some(next) = items
                .iter()
                .find(|item| item.state == RunItemState::Pending)
            {
                candidates.push((run.clone(), next.clone()));
            }
        }
        candidates
    }

    /// Record one continuation nudge (pure bookkeeping; budget/backoff
    /// policy lives in the hook; retryItem clears the fields).
    pub fn record_item_nudge(
        &mut self,
        run_id: &str,
        node_id: &str,
        at: &str,
    ) -> Result<RunItemRow> {
        self.require_run(run_id)?;
        let item = self
            .get_run_item(run_id, node_id)
            .ok_or_else(|| not_found_run_item(run_id, node_id))?;
        self.db_mut().update_run_item(
            run_id,
            node_id,
            None,
            None,
            None,
            None,
            None,
            Some(Some(at.to_string())),
            Some(item.nudge_count + 1),
            None,
            None,
        );
        Ok(self.get_run_item(run_id, node_id).unwrap())
    }

    /// Item-level removal (omt_run_control remove): drops the membership row
    /// only — the ticket node is never touched. In-flight items cannot be
    /// removed EXCEPT the wedge-recovery case (archived/missing node can
    /// never report). Afterwards the run may derive its terminal state.
    pub fn remove_run_item(&mut self, run_id: &str, node_id: &str) -> Result<()> {
        self.require_run(run_id)?;
        let item = self
            .get_run_item(run_id, node_id)
            .ok_or_else(|| not_found_run_item(run_id, node_id))?;
        if is_run_item_in_flight(item.state) {
            let force_allowed = match self.get_node(node_id) {
                Some(node) => node.archived,
                None => true, // dangling row: nothing left to protect
            };
            if !force_allowed {
                return Err(Problem::with_details(
                    error::CONFLICT,
                    format!(
                        "item {node_id} is {} (in-flight); it cannot be removed",
                        item.state
                    ),
                    |d| {
                        d.insert("rule".into(), "remove-in-flight".into());
                        d.insert("runId".into(), run_id.into());
                        d.insert("nodeId".into(), node_id.into());
                        d.insert("itemState".into(), item.state.to_string().into());
                    },
                ));
            }
        }
        self.db_mut().delete_run_item(run_id, node_id);
        self.derive_run_terminal(run_id);
        Ok(())
    }

    /// Explicit report (TICKET-0059). done/blocked/skipped double-write the
    /// ticket status and the item (reported channel bypasses the trust
    /// gate); failed touches ONLY the item and keeps the note in last_error.
    /// The transition lands FIRST: a failing note-append must not strand an
    /// untransitioned item with an orphaned note.
    pub fn report_run_item(
        &mut self,
        run_id: &str,
        node_id: &str,
        outcome: &str,
        note: Option<&str>,
    ) -> Result<ReportResult> {
        const OUTCOMES: [&str; 4] = ["done", "failed", "blocked", "skipped"];
        if !OUTCOMES.contains(&outcome) {
            return Err(Problem::with_details(
                error::INVALID_INPUT,
                format!("unknown report outcome: {outcome} (done/failed/blocked/skipped)"),
                |d| {
                    d.insert("field".into(), "outcome".into());
                    d.insert("value".into(), outcome.into());
                },
            ));
        }
        self.require_run(run_id)?;
        let node = hierarchy::require_node(&self.db(), node_id)?.clone();
        let executable = is_run_member_node_type(node.node_type);
        // Preserve the archived-ticket rejection while allowing legacy
        // archived containers to reach the quarantine path below.
        if node.archived && executable {
            return Err(Problem::with_details(
                error::ARCHIVED_READONLY,
                format!("{node_id} 已归档，无法接受 report"),
                |d| {
                    d.insert("nodeId".into(), node_id.into());
                    d.insert("operation".into(), "report".into());
                },
            ));
        }
        let item = self
            .get_run_item(run_id, node_id)
            .ok_or_else(|| not_found_run_item(run_id, node_id))?;
        if !is_run_item_in_flight(item.state) {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!(
                    "only in-flight items can report ({node_id} is {})",
                    item.state
                ),
                |d| {
                    d.insert("rule".into(), "report-state-gate".into());
                    d.insert("runId".into(), run_id.into());
                    d.insert("nodeId".into(), node_id.into());
                    d.insert("itemState".into(), item.state.to_string().into());
                    d.insert(
                        "required".into(),
                        serde_json::json!(["running", "awaiting_confirmation"]),
                    );
                },
            ));
        }

        // Upgrade safety: pre-filter runs may still contain an in-flight
        // hierarchy container. Quarantine without writing its status/body.
        if !executable {
            let quarantined =
                self.transition_item(run_id, node_id, "skipped", TransitionOptions::default())?;
            return Ok(ReportResult {
                item: quarantined,
                node,
            });
        }

        let note_text = note.map(str::trim).filter(|n| !n.is_empty());
        if outcome == "failed" {
            let transitioned = self.transition_item(
                run_id,
                node_id,
                "failed",
                TransitionOptions {
                    error: note_text.map(str::to_string),
                    ..TransitionOptions::default()
                },
            )?;
            if let Some(note_text) = note_text {
                self.update(UpdateInput {
                    id: node_id.to_string(),
                    append: Some(note_text.to_string()),
                    ..UpdateInput::default()
                })?;
            }
            let node_after = hierarchy::require_node(&self.db(), node_id)?.clone();
            return Ok(ReportResult {
                item: transitioned,
                node: node_after,
            });
        }

        let transitioned =
            self.transition_item(run_id, node_id, outcome, TransitionOptions::default())?;
        let updated = self.update(UpdateInput {
            id: node_id.to_string(),
            append: note_text.map(str::to_string),
            status: Some(outcome.to_string()),
            reported: true,
            ..UpdateInput::default()
        })?;
        Ok(ReportResult {
            item: transitioned,
            node: updated,
        })
    }

    // ── passive observation (TICKET-0061) ───────────────────────────────

    /// A node status/archive change advances matching items of every ACTIVE
    /// run (running|paused) — the cross-run broadcast of decision 1.
    /// Mapping: in_progress → pending dispatches to running (running runs
    /// only); done → done EXCEPT the trust gate; blocked/skipped → same
    /// state (pending included); archived → skipped; open over
    /// done/blocked/skipped → replay; open over awaiting_confirmation → 打回
    /// (interrupted). Claim priority: observation never rebinds ownership.
    /// Ratified decision 4: fully-terminal runs are ignored entirely — they
    /// derive + seal before late reports can arrive.
    fn observe_node_status(
        &mut self,
        node_id: &str,
        status_change: Option<NodeStatus>,
        archived_change: Option<bool>,
        observer_session: Option<&str>,
        reported: bool,
    ) -> Result<Vec<RunItemRow>> {
        if archived_change != Some(true) && status_change.is_none() {
            return Ok(vec![]);
        }
        let mut advanced = Vec::new();
        // Snapshot BEFORE mutating (the SQL scan materializes rows first).
        let snapshot = self
            .db()
            .run_items_for_node(node_id, &[RunStatus::Running, RunStatus::Paused]);
        for (item, run_status) in snapshot {
            let in_flight = is_run_item_in_flight(item.state);
            // In-flight always advances; pending only while dispatching.
            let advance = in_flight
                || (item.state == RunItemState::Pending && run_status == RunStatus::Running);

            if archived_change == Some(true) {
                if advance {
                    advanced.push(self.transition_item(
                        &item.run_id,
                        node_id,
                        "skipped",
                        TransitionOptions::default(),
                    )?);
                }
                continue;
            }
            let Some(change) = status_change else {
                continue;
            };
            match change {
                NodeStatus::InProgress => {
                    // Dispatch only — a claimed (already running) item is
                    // left alone so its executor attribution survives.
                    if item.state == RunItemState::Pending && run_status == RunStatus::Running {
                        advanced.push(self.transition_item(
                            &item.run_id,
                            node_id,
                            "running",
                            TransitionOptions {
                                executor_session_id: observer_session.map(str::to_string),
                                ..TransitionOptions::default()
                            },
                        )?);
                    }
                }
                NodeStatus::Done => {
                    if !advance {
                        continue;
                    }
                    let auto_verify = self
                        .store_get_run(&item.run_id)
                        .map(|r| r.config.auto_verify)
                        .unwrap_or(false);
                    let gated = trust_gate_gates(
                        item.state,
                        reported,
                        observer_session,
                        item.executor_session_id.as_deref(),
                        auto_verify,
                    );
                    let target = if gated {
                        "awaiting_confirmation"
                    } else {
                        "done"
                    };
                    advanced.push(self.transition_item(
                        &item.run_id,
                        node_id,
                        target,
                        TransitionOptions::default(),
                    )?);
                }
                NodeStatus::Blocked | NodeStatus::Skipped => {
                    if advance {
                        let target: &'static str = match change {
                            NodeStatus::Blocked => "blocked",
                            _ => "skipped",
                        };
                        advanced.push(self.transition_item(
                            &item.run_id,
                            node_id,
                            target,
                            TransitionOptions::default(),
                        )?);
                    }
                }
                NodeStatus::Open => {
                    if matches!(
                        item.state,
                        RunItemState::Done | RunItemState::Blocked | RunItemState::Skipped
                    ) {
                        advanced.push(self.replay_item(&item.run_id, node_id)?);
                    } else if item.state == RunItemState::AwaitingConfirmation {
                        // 打回 (TICKET-0064): reopening rejects the
                        // unconfirmed completion — interrupted awaits retry.
                        advanced.push(self.transition_item(
                            &item.run_id,
                            node_id,
                            "interrupted",
                            TransitionOptions::default(),
                        )?);
                    }
                }
            }
        }
        Ok(advanced)
    }

    // ── janitor (startup sweep) ─────────────────────────────────────────

    /// Two-pass sweep (see `janitor.rs` for the ratified semantics): demote
    /// orphaned running items across running|paused runs, then derive
    /// terminal states BEFORE falling back to interrupted. Paused runs keep
    /// their status unless derivation applies. Item demotions write the
    /// store directly and emit NO item events.
    pub fn janitor_sweep(&mut self) -> Result<JanitorResult> {
        let now = self.now();
        let candidates = self
            .db()
            .list_runs_by_status(&[RunStatus::Running, RunStatus::Paused]);
        let snapshots: Vec<SweepRun> = candidates
            .iter()
            .map(|run| SweepRun {
                run: run.clone(),
                items: self.db().list_run_items(&run.id),
            })
            .collect();
        let leases = self.deps.leases.clone();
        let plan: SweepPlan = plan_sweep(
            &snapshots,
            |session, attempt| leases.borrow().lease_alive(session, attempt),
            &now,
        )?;

        for (run_id, node_id) in &plan.demotions {
            self.db_mut().update_run_item(
                run_id,
                node_id,
                Some(RunItemState::Interrupted),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(Some(now.clone())),
            );
        }
        for (run_id, terminal) in &plan.derived {
            self.set_run_status(run_id, *terminal)?;
        }
        for run_id in &plan.interrupted_runs {
            self.set_run_status(run_id, RunStatus::Interrupted)?;
        }
        Ok(JanitorResult {
            interrupted_runs: plan.interrupted_runs,
            interrupted_items: plan.interrupted_items,
        })
    }

    // ── internals ───────────────────────────────────────────────────────

    /// Validated run status change; finished_at tracks absolute terminals
    /// (cleared when re-entering running, preserved otherwise).
    fn set_run_status(&mut self, id: &str, to: RunStatus) -> Result<()> {
        let run = self.require_run(id)?;
        if !run_transition_allowed(run.status, to) {
            return Err(Problem::with_details(
                error::CONFLICT,
                format!("illegal run transition for {id}: {} → {}", run.status, to),
                |d| {
                    d.insert("rule".into(), "run-transition".into());
                    d.insert("runId".into(), id.into());
                    d.insert("from".into(), run.status.to_string().into());
                    d.insert("to".into(), to.to_string().into());
                },
            ));
        }
        let now = self.now();
        let finished_at = match to {
            RunStatus::Running => None,
            RunStatus::Completed
            | RunStatus::CompletedWithFailures
            | RunStatus::Canceled
            | RunStatus::Interrupted => Some(now),
            RunStatus::Pending | RunStatus::Paused => run.finished_at.clone(),
        };
        self.db_mut().update_run_status(id, to, finished_at);
        let refreshed = self.require_run(id)?;
        self.emit(RunEvent {
            kind_is_item: false,
            run: refreshed,
            item: None,
            from_item_state: None,
            from_run_status: Some(run.status),
        });
        Ok(())
    }

    /// Terminal derivation wrapper returning whether a terminal was derived.
    fn derive_run_terminal(&mut self, id: &str) -> bool {
        let Some(run) = self.store_get_run(id) else {
            return false;
        };
        let items = self.db().list_run_items(id);
        let Some(terminal) = derive_terminal(run.status, &items) else {
            return false;
        };
        self.set_run_status(id, terminal).is_ok()
    }

    fn require_run_member_node(&self, id: &str) -> Result<NodeRow> {
        let node = hierarchy::require_node(&self.db(), id)?.clone();
        if !is_run_member_node_type(node.node_type) {
            return Err(Problem::with_details(
                error::INVALID_INPUT,
                format!(
                    "run member {id} must be an executable ticket/subticket ({} is context only)",
                    node.node_type
                ),
                |d| {
                    d.insert("rule".into(), "member-type".into());
                    d.insert("nodeId".into(), id.into());
                    d.insert("nodeType".into(), node.node_type.to_string().into());
                },
            ));
        }
        if node.archived {
            return Err(Problem::with_details(
                error::ARCHIVED_READONLY,
                format!("run member {id} is archived (已归档成员不能加入 run；请先恢复)"),
                |d| {
                    d.insert("nodeId".into(), id.into());
                    d.insert("operation".into(), "run-membership".into());
                },
            ));
        }
        Ok(node)
    }

    /// Re-render one node's managed children block from current edges.
    fn refresh_children_block(&mut self, id: &str) -> Result<()> {
        let node = hierarchy::require_node(&self.db(), id)?.clone();
        let entries: Vec<ChildEntry> = self
            .db()
            .children_of(id)
            .iter()
            .map(|child| ChildEntry {
                id: child.id.clone(),
                title: child.title.clone(),
                dir_name: crate::markdown::node_dir_name(&child.id, &child.title),
                node_type: child.node_type.to_string(),
                status: child.status.to_string(),
            })
            .collect();
        let block = render_children_entries(&entries);
        let file = read_node_file(&mut *self.deps.files.borrow_mut(), &node.path)?;
        let body = replace_children_block(&file.body, &block);
        let parent_id = self.db().parent_of(id).map(|p| p.id);
        let content = serialize_node_file(frontmatter_lines(&node, parent_id.as_deref()), &body);
        self.deps
            .files
            .borrow_mut()
            .write_file(&node.path, &content)?;
        Ok(())
    }
}

// ── free helpers ────────────────────────────────────────────────────────

fn run_status_gate(run_id: &str, current: RunStatus, required: &[RunStatus]) -> Problem {
    Problem::with_details(
        error::CONFLICT,
        format!("run-status-gate violated for {run_id}"),
        |d| {
            d.insert("rule".into(), "run-status-gate".into());
            d.insert("runId".into(), run_id.into());
            d.insert("current".into(), current.to_string().into());
            d.insert(
                "required".into(),
                Value::Array(
                    required
                        .iter()
                        .map(|r| Value::String(r.to_string()))
                        .collect(),
                ),
            );
        },
    )
}

fn retry_state_gate(run_id: &str, node_id: &str, item_state: RunItemState) -> Problem {
    Problem::with_details(
        error::CONFLICT,
        format!("only failed/interrupted/pending items can retry ({node_id} is {item_state})"),
        |d| {
            d.insert("rule".into(), "retry-state-gate".into());
            d.insert("runId".into(), run_id.into());
            d.insert("nodeId".into(), node_id.into());
            d.insert("itemState".into(), item_state.to_string().into());
            d.insert(
                "required".into(),
                serde_json::json!(["failed", "interrupted", "pending"]),
            );
        },
    )
}

/// Home-relative POSIX path for a node file (`files.pathFor`).
fn markdown_path_for(node_type: &str, id: &str, title: &str, parent_path: Option<&str>) -> String {
    crate::markdown::path_for(node_type, id, title, parent_path)
}

pub fn parse_node_type(raw: &str) -> Option<NodeType> {
    raw.parse().ok()
}

pub fn parse_node_type_opt(raw: &str) -> Option<NodeType> {
    parse_node_type(raw)
}

pub fn parse_node_status(raw: &str) -> Option<NodeStatus> {
    raw.parse().ok()
}

pub fn parse_node_status_opt(raw: &str) -> Option<NodeStatus> {
    parse_node_status(raw)
}

pub fn parse_item_state(raw: &str) -> Option<RunItemState> {
    raw.parse().ok()
}

/// Merge raw config overrides onto DEFAULT_RUN_CONFIG (JSON object keys:
/// stopOnFailure / autoContinue / autoVerify / concurrency).
pub fn merge_run_config(overrides: Option<&Value>) -> serde_json::Map<String, Value> {
    let mut merged = serde_json::Map::new();
    merged.insert("stopOnFailure".into(), Value::Bool(false));
    merged.insert("autoContinue".into(), Value::Bool(true));
    merged.insert("autoVerify".into(), Value::Bool(false));
    merged.insert("concurrency".into(), serde_json::json!(1));
    if let Some(Value::Object(map)) = overrides {
        for (key, value) in map {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

/// Default body template per node type (`defaultBody` in markdown.ts):
/// sections separated by two blank lines, single trailing newline.
pub fn default_body(node_type: &NodeType) -> String {
    let sections: Vec<&str> = match node_type {
        NodeType::Epic => {
            vec![
                "## 总体目标",
                "## 范围",
                "## 非范围",
                "## 全局约束",
                "## 成功标准",
            ]
        }
        NodeType::Story | NodeType::Substory => vec![
            "## 能力结果",
            "## 使用者或调用方",
            "## 范围",
            "## 非范围",
            "## 共享规则与约束",
            "## 验收标准",
        ],
        NodeType::Ticket | NodeType::Subticket => vec![
            "## 交付结果",
            "## 工作范围",
            "## 依赖",
            "## 验收标准",
            "## 进度记录",
        ],
    };
    sections
        .iter()
        .map(|section| format!("{section}\n\n\n"))
        .collect::<String>()
        .trim_end_matches('\n')
        .to_string()
        + "\n"
}
