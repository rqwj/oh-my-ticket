//! Shared infrastructure for the Rust behavioral-corpus leg (U3): a virtual
//! filesystem, in-memory ports, the closed operation vocabulary, and the
//! comparison engine — all mirroring `corpus/runner/ts/harness.ts` line for
//! line. The scenario JSON documents are NEVER edited; this module is the
//! per-leg binding layer (ratified U3 decision 1: session-set liveness on
//! the TS leg becomes exclusive far-future lease sets here).

use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use omt_domain::core::{
    merge_run_config, AddRunMemberInput, AddRunMembersResult, CoreDeps, CreateInput,
    CreateRunInput, OmtCore, ReindexResult, ReportResult, ShowResult, TransitionOptions,
    UpdateInput,
};
use omt_domain::core::{parse_item_state, parse_node_status, parse_node_type};
use omt_domain::error;
use omt_domain::ports::{FixedClock, MemoryLeases};

/// Test-fixture handle triple returned by [`FixtureHome::ensure_handle`]
/// (clippy::type_complexity).
type HomeHandles = (
    Rc<RefCell<MemoryFiles>>,
    Rc<RefCell<Store>>,
    Rc<RefCell<MemoryLeases>>,
);
use omt_domain::store::{FileStore, Store};
use omt_domain::types::*;
use omt_domain::Problem;

/// Placeholder written over volatile fields before comparison.
pub const MASK_PLACEHOLDER: &str = "__MASKED__";

/// Volatile wall-clock stamps masked by default (envelope may override).
pub const DEFAULT_MASK_KEYS: [&str; 5] = [
    "created_at",
    "updated_at",
    "nudged_at",
    "started_at",
    "finished_at",
];

/// Fixed nudge stamp used by the `nudge` op (masked anyway).
pub const FIXED_NUDGE_AT: &str = "2026-08-19T00:00:00.000Z";

// ── virtual filesystem ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Entry {
    File(String),
    Dir,
}

pub type Vfs = Rc<RefCell<BTreeMap<String, Entry>>>;

pub fn new_vfs() -> Vfs {
    Rc::new(RefCell::new(BTreeMap::new()))
}

pub fn join(home: &str, rel: &str) -> String {
    format!("{}/{}", home.trim_end_matches('/'), rel)
}

fn parent_dir(path: &str) -> Option<String> {
    path.rfind('/').map(|index| path[..index].to_string())
}

/// In-memory [`FileStore`] rooted at one home inside a shared VFS.
pub struct MemoryFiles {
    pub home: String,
    pub vfs: Vfs,
}

impl MemoryFiles {
    pub fn mount(home: &str, vfs: &Vfs) -> Rc<RefCell<MemoryFiles>> {
        Rc::new(RefCell::new(MemoryFiles {
            home: home.to_string(),
            vfs: Rc::clone(vfs),
        }))
    }

    fn key(&self, rel_path: &str) -> String {
        join(&self.home, rel_path)
    }
}

impl FileStore for MemoryFiles {
    fn read_file(&mut self, rel_path: &str) -> omt_domain::Result<String> {
        let key = self.key(rel_path);
        match self.vfs.borrow().get(&key) {
            Some(Entry::File(content)) => Ok(content.clone()),
            _ => Err(error::Problem::new(
                error::IO,
                format!("ENOENT: no such file or directory, open '{key}'"),
            )),
        }
    }

    fn write_file(&mut self, rel_path: &str, content: &str) -> omt_domain::Result<()> {
        let key = self.key(rel_path);
        let mut vfs = self.vfs.borrow_mut();
        let mut dir = parent_dir(&key);
        while let Some(d) = dir {
            if vfs.contains_key(&d) {
                break;
            }
            vfs.insert(d.clone(), Entry::Dir);
            dir = parent_dir(&d);
        }
        vfs.insert(key, Entry::File(content.to_string()));
        Ok(())
    }

    fn move_dir(&mut self, old_rel_dir: &str, new_rel_dir: &str) -> omt_domain::Result<()> {
        let old_key = self.key(old_rel_dir);
        let new_key = self.key(new_rel_dir);
        let prefix = format!("{old_key}/");
        let moved: Vec<(String, Entry)> = {
            let vfs = self.vfs.borrow();
            let mut collected = Vec::new();
            for (key, entry) in vfs.iter() {
                if key == &old_key || key.starts_with(&prefix) {
                    collected.push((key.clone(), entry.clone()));
                }
            }
            collected
        };
        if moved.is_empty() {
            return Err(error::Problem::new(
                error::IO,
                format!("ENONENT: no such file or directory, rename '{old_key}'"),
            ));
        }
        let mut vfs = self.vfs.borrow_mut();
        for (key, entry) in moved {
            let relocated = if key == old_key {
                new_key.clone()
            } else {
                format!("{new_key}/{}", &key[prefix.len()..])
            };
            // Ensure destination directories exist.
            let mut dir = parent_dir(&relocated);
            while let Some(d) = dir {
                if vfs.contains_key(&d) {
                    break;
                }
                vfs.insert(d.clone(), Entry::Dir);
                dir = parent_dir(&d);
            }
            vfs.remove(&key);
            vfs.insert(relocated, entry);
        }
        Ok(())
    }

    fn delete_file(&mut self, rel_path: &str) -> omt_domain::Result<()> {
        let key = self.key(rel_path);
        match self.vfs.borrow_mut().remove(&key) {
            Some(_) => Ok(()),
            None => Err(error::Problem::new(
                error::IO,
                format!("ENOENT: no such file or directory, unlink '{key}'"),
            )),
        }
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.vfs.borrow().contains_key(&self.key(rel_path))
    }

    fn is_dir(&self, rel_path: &str) -> bool {
        matches!(self.vfs.borrow().get(&self.key(rel_path)), Some(Entry::Dir))
    }

    fn mkdir_all(&mut self, rel_path: &str) -> omt_domain::Result<()> {
        let key = self.key(rel_path);
        let mut vfs = self.vfs.borrow_mut();
        let mut dir = Some(key);
        while let Some(d) = dir {
            if vfs.contains_key(&d) {
                break;
            }
            vfs.insert(d.clone(), Entry::Dir);
            dir = parent_dir(&d);
        }
        Ok(())
    }

    fn list_markdown_under_tickets(&self) -> Vec<String> {
        let tickets_root = self.key("tickets");
        let prefix = format!("{tickets_root}/");
        let vfs = self.vfs.borrow();
        let mut found: Vec<String> = vfs
            .iter()
            .filter(|(key, entry)| {
                matches!(entry, Entry::File(_)) && key.starts_with(&prefix) && key.ends_with(".md")
            })
            .map(|(key, _)| key[self.home.len() + 1..].to_string())
            .collect();
        found.sort();
        found
    }
}

// ── per-home runtime ────────────────────────────────────────────────────

pub struct HomeHandle {
    pub files: Rc<RefCell<MemoryFiles>>,
    pub store: Rc<RefCell<Store>>,
    pub leases: Rc<RefCell<MemoryLeases>>,
    pub core: Option<OmtCore>,
    pub listener: Option<usize>,
}

/// One `(node_id, from_state, to_state)` captured item event.
pub type ItemEvent = (String, String, String);

pub struct PoolFixture {
    pub global_home: String,
}

/// Scenario execution context (mirrors `RunContext` + the pool fixture).
pub struct Executor {
    pub vfs: Vfs,
    #[allow(dead_code)] // kept for parity with the TS RunContext shape
    pub root: String,
    pub fixture: Option<PoolFixture>,
    pub vars: BTreeMap<String, String>,
    pub homes: BTreeMap<String, HomeHandle>,
    pub active_home: String,
    pub results: Vec<Value>,
    pub aliases: BTreeMap<String, Value>,
    pub events: Rc<RefCell<Vec<ItemEvent>>>,
    pub recording: bool,
    #[allow(dead_code)] // surfaced through ScenarioSummary instead
    pub failures: Vec<String>,
    #[allow(dead_code)]
    pub checks: usize,
}

impl Executor {
    pub fn new(root: &str) -> Executor {
        Executor {
            vfs: new_vfs(),
            root: root.to_string(),
            fixture: None,
            vars: BTreeMap::new(),
            homes: BTreeMap::new(),
            active_home: String::new(),
            results: Vec::new(),
            aliases: BTreeMap::new(),
            events: Rc::new(RefCell::new(Vec::new())),
            recording: false,
            failures: Vec::new(),
            checks: 0,
        }
    }

    fn ensure_handle(&mut self, home: &str) -> HomeHandles {
        let handle = self
            .homes
            .entry(home.to_string())
            .or_insert_with(|| HomeHandle {
                files: MemoryFiles::mount(home, &self.vfs),
                store: Rc::new(RefCell::new(Store::default())),
                leases: Rc::new(RefCell::new(MemoryLeases::new())),
                core: None,
                listener: None,
            });
        (
            Rc::clone(&handle.files),
            Rc::clone(&handle.store),
            Rc::clone(&handle.leases),
        )
    }

    /// Full open flow: directories, database, reindex-if-fresh, exclusive
    /// lease binding, startup janitor. Used for initial opens AND reopen.
    pub fn open_core(&mut self, home: &str, sessions: &[String]) -> Result<(), Problem> {
        let (files, store, leases) = self.ensure_handle(home);
        let deps = CoreDeps {
            clock: Rc::new(RefCell::new(FixedClock)),
            files,
            leases,
            store,
        };
        let core = OmtCore::open(home, sessions, deps)?;
        self.install_core(home, core);
        Ok(())
    }

    /// Swap the active core (closing any previous one) and reset event
    /// collection — listeners attach AFTER open, so startup-janitor
    /// demotions stay invisible (mirrors `collectItemEvents`).
    fn install_core(&mut self, home: &str, core: OmtCore) {
        let handle = self.homes.get_mut(home).expect("home registered");
        if let Some(mut previous) = handle.core.take() {
            previous.clear_run_events();
        }
        handle.core = Some(core);
        self.active_home = home.to_string();
        if self.recording {
            self.collect_item_events();
        }
    }

    pub fn collect_item_events(&mut self) {
        self.events.borrow_mut().clear();
        let sink = Rc::clone(&self.events);
        let handle = self.homes.get_mut(&self.active_home).expect("active home");
        if let Some(core) = handle.core.as_mut() {
            let listener = Rc::new(move |event: &omt_domain::core::RunEvent| {
                if event.kind_is_item {
                    if let Some(item) = &event.item {
                        sink.borrow_mut().push((
                            item.node_id.clone(),
                            event
                                .from_item_state
                                .map(|s| s.to_string())
                                .unwrap_or_default(),
                            item.state.to_string(),
                        ));
                    }
                }
            });
            let index = core.on_run_event(listener);
            handle.listener = Some(index);
        }
    }

    pub fn core(&mut self) -> &mut OmtCore {
        self.homes
            .get_mut(&self.active_home)
            .and_then(|handle| handle.core.as_mut())
            .expect("active home has an open core")
    }

    pub fn active_store(&mut self) -> Rc<RefCell<Store>> {
        Rc::clone(
            &self
                .homes
                .get(&self.active_home)
                .expect("active home")
                .store,
        )
    }

    pub fn active_leases(&mut self) -> Rc<RefCell<MemoryLeases>> {
        Rc::clone(
            &self
                .homes
                .get(&self.active_home)
                .expect("active home")
                .leases,
        )
    }

    /// Pool routing rule (option A): a workspace cwd carrying `.omt/` wins;
    /// everything else falls back to the global home.
    pub fn home_for(&self, cwd: Option<&str>) -> String {
        let global = self
            .fixture
            .as_ref()
            .expect("pool fixture")
            .global_home
            .clone();
        match cwd {
            Some(cwd) => {
                let local = join(cwd, ".omt");
                if self
                    .vfs
                    .borrow()
                    .get(&local)
                    .is_some_and(|e| matches!(e, Entry::Dir))
                {
                    local
                } else {
                    global
                }
            }
            None => global,
        }
    }

    /// Resolve `$var` references inside cwd params (unknown vars fall back
    /// to the raw string, mirroring the harness).
    pub fn resolve_cwd(&self, raw: Option<&Value>) -> Option<String> {
        let raw = raw?;
        let text = raw.as_str()?;
        if let Some(name) = text.strip_prefix('$') {
            return Some(
                self.vars
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| text.to_string()),
            );
        }
        Some(text.to_string())
    }

    pub fn read_virtual_file(&self, abs_path: &str) -> Result<String, String> {
        match self.vfs.borrow().get(abs_path) {
            Some(Entry::File(content)) => Ok(content.clone()),
            _ => Err(format!(
                "ENOENT: no such file or directory, open '{abs_path}'"
            )),
        }
    }
}

// ── row projections (snake_case DB-row shapes, optional fields omitted) ─

pub fn node_value(node: &NodeRow) -> Value {
    json!({
        "id": node.id,
        "type": node.node_type.to_string(),
        "title": node.title,
        "status": node.status.to_string(),
        "archived": node.archived,
        "priority": node.priority,
        "path": node.path,
        "created_at": node.created_at,
        "updated_at": node.updated_at,
    })
}

pub fn tree_node_value(node: &TreeNodeRow) -> Value {
    let mut value = node_value(&node.node);
    value["children"] = Value::Array(node.children.iter().map(tree_node_value).collect());
    value
}

pub fn run_value(run: &RunRow) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), json!(run.id));
    if let Some(title) = &run.title {
        map.insert("title".into(), json!(title));
    }
    map.insert("status".into(), json!(run.status.to_string()));
    map.insert(
        "config".into(),
        json!({
            "stopOnFailure": run.config.stop_on_failure,
            "autoContinue": run.config.auto_continue,
            "autoVerify": run.config.auto_verify,
            "concurrency": run.config.concurrency,
        }),
    );
    map.insert("created_at".into(), json!(run.created_at));
    if let Some(finished_at) = &run.finished_at {
        map.insert("finished_at".into(), json!(finished_at));
    }
    Value::Object(map)
}

pub fn item_value(item: &RunItemRow) -> Value {
    let mut map = Map::new();
    map.insert("run_id".into(), json!(item.run_id));
    map.insert("node_id".into(), json!(item.node_id));
    map.insert("position".into(), json!(item.position));
    map.insert("state".into(), json!(item.state.to_string()));
    if let Some(executor) = &item.executor_session_id {
        map.insert("executor_session_id".into(), json!(executor));
    }
    map.insert("attempts".into(), json!(item.attempts));
    if let Some(last_error) = &item.last_error {
        map.insert("last_error".into(), json!(last_error));
    }
    if let Some(nudged_at) = &item.nudged_at {
        map.insert("nudged_at".into(), json!(nudged_at));
    }
    map.insert("nudge_count".into(), json!(item.nudge_count));
    if let Some(started_at) = &item.started_at {
        map.insert("started_at".into(), json!(started_at));
    }
    if let Some(finished_at) = &item.finished_at {
        map.insert("finished_at".into(), json!(finished_at));
    }
    Value::Object(map)
}

pub fn show_value(result: &ShowResult) -> Value {
    let mut map = Map::new();
    map.insert("node".into(), node_value(&result.node));
    map.insert("body".into(), json!(result.body));
    if let Some(parent) = &result.parent {
        map.insert("parent".into(), node_value(parent));
    }
    map.insert(
        "children".into(),
        Value::Array(result.children.iter().map(node_value).collect()),
    );
    Value::Object(map)
}

pub fn report_value(result: &ReportResult) -> Value {
    json!({ "item": item_value(&result.item), "node": node_value(&result.node) })
}

pub fn add_members_value(result: &AddRunMembersResult) -> Value {
    json!({
        "added": result.added.iter().map(item_value).collect::<Vec<_>>(),
        "duplicates": result.duplicates,
    })
}

pub fn reindex_value(result: &ReindexResult) -> Value {
    json!({ "nodes": result.nodes, "edges": result.edges, "skipped": result.skipped })
}

pub fn problem_to_value(problem: &Problem) -> Value {
    let mut error = Map::new();
    error.insert("code".into(), json!(problem.code));
    error.insert("message".into(), json!(problem.message));
    if let Some(details) = &problem.details {
        error.insert("details".into(), details.clone());
    }
    json!({ "error": error })
}

/// Wrap any fallible core call into the stored-result shape.
pub fn caught(call: impl FnOnce() -> Result<Value, Problem>) -> Value {
    match call() {
        Ok(value) => value,
        Err(problem) => problem_to_value(&problem),
    }
}

// ── params extraction ───────────────────────────────────────────────────

fn p_str<'a>(params: &'a Map<String, Value>, key: &str) -> &'a str {
    params.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn p_opt<'a>(params: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    params.get(key).filter(|value| !value.is_null())
}

fn p_opt_str(params: &Map<String, Value>, key: &str) -> Option<String> {
    p_opt(params, key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn p_opt_i64(params: &Map<String, Value>, key: &str) -> Option<i64> {
    p_opt(params, key).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
}

// ── the closed operation vocabulary ─────────────────────────────────────

pub fn apply_op(ex: &mut Executor, op: &str, params: &Value) -> Value {
    let empty = Map::new();
    let p = params.as_object().unwrap_or(&empty);
    match op {
        // ── nodes ──
        "create" => caught(|| {
            let node = ex.core().create(CreateInput {
                id: p_opt_str(p, "id"),
                node_type: p_str(p, "type").to_string(),
                title: p_str(p, "title").to_string(),
                parent_id: p_opt_str(p, "parentId"),
                body: p_opt_str(p, "body"),
                priority: p_opt_i64(p, "priority"),
            })?;
            Ok(node_value(&node))
        }),
        "update" => caught(|| {
            let node = ex.core().update(UpdateInput {
                id: p_str(p, "id").to_string(),
                title: p_opt_str(p, "title"),
                status: p_opt_str(p, "status"),
                archived: p_opt(p, "archived").and_then(Value::as_bool),
                priority: p_opt_i64(p, "priority"),
                body: p_opt_str(p, "body"),
                append: p_opt_str(p, "append"),
                executor_session_id: p_opt_str(p, "executorSessionId"),
                reported: false,
            })?;
            Ok(node_value(&node))
        }),
        "move" => caught(|| {
            let node = ex
                .core()
                .move_node(p_str(p, "id"), p_str(p, "newParentId"))?;
            Ok(node_value(&node))
        }),
        "show" => caught(|| {
            let shown = ex.core().show(p_str(p, "id"))?;
            Ok(show_value(&shown))
        }),
        "list" => {
            let nodes = ex.core().list(
                p_opt_str(p, "type").as_deref().and_then(parse_node_type),
                p_opt_str(p, "status")
                    .as_deref()
                    .and_then(parse_node_status),
                p_opt_str(p, "query").as_deref(),
            );
            Value::Array(nodes.iter().map(node_value).collect())
        }
        "tree" => caught(|| {
            let forest = ex.core().tree(p_opt_str(p, "rootId").as_deref())?;
            Ok(Value::Array(forest.iter().map(tree_node_value).collect()))
        }),
        "getNode" => match ex.core().get_node(p_str(p, "id")) {
            Some(node) => node_value(&node),
            None => Value::Null,
        },
        "reindex" => caught(|| {
            let result = ex.core().reindex()?;
            Ok(reindex_value(&result))
        }),

        // ── files ──
        "readFile" => {
            let path = join(&ex.active_home, p_str(p, "path"));
            match ex.read_virtual_file(&path) {
                Ok(content) => Value::String(content),
                Err(message) => json!({ "error": { "code": "UNKNOWN", "message": message } }),
            }
        }
        "writeFile" => {
            let rel = p_str(p, "path");
            let text = p_str(p, "text");
            let files = Rc::clone(&ex.homes.get(&ex.active_home).expect("active home").files);
            let outcome = files.borrow_mut().write_file(rel, text);
            match outcome {
                Ok(()) => json!({ "written": rel }),
                Err(problem) => problem_to_value(&problem),
            }
        }
        "deleteFile" => {
            let rel = p_str(p, "path");
            let files = Rc::clone(&ex.homes.get(&ex.active_home).expect("active home").files);
            let outcome = files.borrow_mut().delete_file(rel);
            match outcome {
                Ok(()) => json!({ "deleted": rel }),
                Err(problem) => problem_to_value(&problem),
            }
        }

        // ── runs ──
        "createRun" => caught(|| {
            let run = ex.core().create_run(CreateRunInput {
                title: p_opt_str(p, "title"),
                config: p_opt(p, "config").cloned(),
                node_ids: p_opt(p, "nodeIds")
                    .and_then(Value::as_array)
                    .map(|array| {
                        array
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            })?;
            Ok(run_value(&run))
        }),
        "getRun" => match ex.core().get_run(p_str(p, "id")) {
            Some(run) => run_value(&run),
            None => Value::Null,
        },
        "listRuns" => {
            let status_filter = p_opt_str(p, "status")
                .as_deref()
                .and_then(|raw: &str| raw.parse().ok());
            let runs = ex.core().list_runs(status_filter);
            Value::Array(runs.iter().map(run_value).collect())
        }
        "runItems" => caught(|| {
            let items = ex.core().run_items(p_str(p, "runId"))?;
            Ok(Value::Array(items.iter().map(item_value).collect()))
        }),
        "getRunItem" => match ex
            .core()
            .get_run_item(p_str(p, "runId"), p_str(p, "nodeId"))
        {
            Some(item) => item_value(&item),
            None => Value::Null,
        },
        "runItemStateCounts" => {
            let counts = ex.core().run_item_state_counts(p_str(p, "runId"));
            Value::Array(
                counts
                    .iter()
                    .map(|(state, count)| json!({ "state": state.to_string(), "count": count }))
                    .collect(),
            )
        }
        "addRunMembers" => caught(|| {
            let members: Vec<AddRunMemberInput> = p_opt(p, "members")
                .and_then(Value::as_array)
                .map(|array| {
                    array
                        .iter()
                        .map(|member| AddRunMemberInput {
                            node_id: member
                                .get("nodeId")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            state: member
                                .get("state")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            executor_session_id: member
                                .get("executorSessionId")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let result = ex.core().add_run_members(p_str(p, "runId"), &members)?;
            Ok(add_members_value(&result))
        }),
        "runsOfNode" => {
            let pairs = ex.core().runs_of_node(p_str(p, "nodeId"));
            Value::Array(
                pairs
                    .iter()
                    .map(|(run, item)| json!({ "run": run_value(run), "item": item_value(item) }))
                    .collect(),
            )
        }
        "startRun" => caught(|| Ok(run_value(&ex.core().start_run(p_str(p, "id"))?))),
        "pauseRun" => caught(|| Ok(run_value(&ex.core().pause_run(p_str(p, "id"))?))),
        "resumeRun" => caught(|| Ok(run_value(&ex.core().resume_run(p_str(p, "id"))?))),
        "cancelRun" => caught(|| Ok(run_value(&ex.core().cancel_run(p_str(p, "id"))?))),
        "transitionItem" => caught(|| {
            let item = ex.core().transition_item(
                p_str(p, "runId"),
                p_str(p, "nodeId"),
                p_str(p, "to"),
                TransitionOptions {
                    executor_session_id: p_opt_str(p, "executorSessionId"),
                    error: p_opt_str(p, "error"),
                },
            )?;
            Ok(item_value(&item))
        }),
        "retryItem" => caught(|| {
            let item = ex
                .core()
                .retry_item(p_str(p, "runId"), p_str(p, "nodeId"))?;
            Ok(item_value(&item))
        }),
        "replayItem" => caught(|| {
            let item = ex
                .core()
                .replay_item(p_str(p, "runId"), p_str(p, "nodeId"))?;
            Ok(item_value(&item))
        }),
        "claimRunItem" => caught(|| {
            match ex
                .core()
                .claim_run_item(p_str(p, "runId"), p_str(p, "executorSessionId"))?
            {
                Some(item) => Ok(item_value(&item)),
                None => Ok(Value::Null),
            }
        }),
        "reportRunItem" => caught(|| {
            let result = ex.core().report_run_item(
                p_str(p, "runId"),
                p_str(p, "nodeId"),
                p_str(p, "outcome"),
                p_opt_str(p, "note").as_deref(),
            )?;
            Ok(report_value(&result))
        }),
        "removeRunItem" => caught(|| {
            ex.core()
                .remove_run_item(p_str(p, "runId"), p_str(p, "nodeId"))?;
            Ok(json!({ "removed": p_str(p, "nodeId") }))
        }),

        // ── continuation ──
        "nudge" => {
            let count = p_opt(p, "count").and_then(Value::as_i64).unwrap_or(1);
            let mut last = Value::Null;
            for _ in 0..count.max(0) {
                match ex.core().record_item_nudge(
                    p_str(p, "runId"),
                    p_str(p, "nodeId"),
                    FIXED_NUDGE_AT,
                ) {
                    Ok(item) => last = item_value(&item),
                    Err(problem) => return problem_to_value(&problem),
                }
            }
            last
        }
        "stallCheck" => match ex
            .core()
            .get_run_item(p_str(p, "runId"), p_str(p, "nodeId"))
        {
            Some(item) => {
                json!({ "stalled": is_run_item_stalled(item.state, item.nudge_count) })
            }
            // The TS harness asserts non-null (`!`), so absence surfaces as
            // an UNKNOWN error result.
            None => {
                json!({ "error": { "code": "UNKNOWN", "message": "Cannot read properties of undefined" } })
            }
        },
        "continuationCandidates" => {
            let pairs = ex.core().continuation_candidates(p_str(p, "sessionId"));
            pairs_to_value(&pairs)
        }
        "executorItems" => {
            let pairs = ex.core().executor_items(p_str(p, "sessionId"));
            pairs_to_value(&pairs)
        }

        // ── lifecycle ──
        "sweep" => {
            let sessions: Vec<String> = p_opt(p, "activeSessions")
                .and_then(Value::as_array)
                .map(|array| {
                    array
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            ex.active_leases().borrow_mut().mark_exclusive(&sessions);
            caught(|| {
                let result = ex.core().janitor_sweep()?;
                Ok(json!({
                    "interruptedRuns": result.interrupted_runs,
                    "interruptedItems": result.interrupted_items.iter().map(item_value).collect::<Vec<_>>(),
                }))
            })
        }
        "reopen" => {
            let sessions: Vec<String> = p_opt(p, "activeSessionIds")
                .and_then(Value::as_array)
                .map(|array| {
                    array
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if p_opt(p, "freshDb").and_then(Value::as_bool) == Some(true) {
                let store = ex.active_store();
                *store.borrow_mut() = Store::default();
            }
            let home = ex.active_home.clone();
            match ex.open_core(&home, &sessions) {
                Ok(()) => json!({ "reopened": true }),
                Err(problem) => problem_to_value(&problem),
            }
        }
        "seedRun" => {
            let store = ex.active_store();
            let config_json = merge_run_config(p.get("config"));
            let concurrency = config_json["concurrency"].as_i64().unwrap_or(1);
            let mut store_ref = store.borrow_mut();
            let id = match p_opt_str(p, "id") {
                Some(id) => id,
                None => store_ref.next_run_id(),
            };
            let status = p_str(p, "status")
                .parse()
                .ok()
                .unwrap_or(RunStatus::Pending);
            store_ref.insert_run(RunRow {
                id: id.clone(),
                title: p_opt_str(p, "title"),
                status,
                config: RunConfigValue {
                    stop_on_failure: config_json["stopOnFailure"].as_bool().unwrap_or(false),
                    auto_continue: config_json["autoContinue"].as_bool().unwrap_or(true),
                    auto_verify: config_json["autoVerify"].as_bool().unwrap_or(false),
                    concurrency,
                },
                created_at: p_opt_str(p, "createdAt").unwrap_or_else(|| FIXED_NUDGE_AT.to_string()),
                finished_at: None,
            });
            if let Some(items) = p_opt(p, "items").and_then(Value::as_array) {
                for item in items {
                    let state = parse_item_state(
                        item.get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("pending"),
                    )
                    .unwrap_or(RunItemState::Pending);
                    let mut row = RunItemRow::new(
                        &id,
                        item.get("nodeId")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        item.get("position").and_then(Value::as_i64).unwrap_or(0),
                        state,
                    );
                    row.attempts = item.get("attempts").and_then(Value::as_i64).unwrap_or(0);
                    row.nudge_count = item.get("nudgeCount").and_then(Value::as_i64).unwrap_or(0);
                    row.executor_session_id = item
                        .get("executorSessionId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    row.last_error = item
                        .get("lastError")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    row.started_at = item
                        .get("startedAt")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    row.finished_at = item
                        .get("finishedAt")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    store_ref.insert_run_item(row);
                }
            }
            json!({ "id": id })
        }

        // ── pool workspace routing ──
        "resolveHome" => match &ex.fixture {
            None => {
                json!({ "error": { "code": "UNKNOWN", "message": "resolveHome requires setup.pool" } })
            }
            Some(_) => Value::String(ex.home_for(ex.resolve_cwd(p.get("cwd")).as_deref())),
        },
        "openRouted" => {
            if ex.fixture.is_none() {
                return json!({ "error": { "code": "UNKNOWN", "message": "openRouted requires setup.pool" } });
            }
            let cwd = ex.resolve_cwd(p.get("cwd"));
            let home = ex.home_for(cwd.as_deref());
            if ex.homes.contains_key(&home) {
                // Cached core instance (pool semantics): just route to it.
                ex.active_home = home.clone();
                if ex.recording {
                    ex.collect_item_events();
                }
                json!({ "home": home })
            } else {
                match ex.open_core(&home, &[]) {
                    Ok(()) => json!({ "home": home }),
                    Err(problem) => problem_to_value(&problem),
                }
            }
        }

        other => {
            let _ = other;
            json!({ "error": { "code": "UNKNOWN", "message": format!("unknown op \"{op}\"") } })
        }
    }
}

fn pairs_to_value(pairs: &[(RunRow, RunItemRow)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(run, item)| json!({ "run": run_value(run), "item": item_value(item) }))
            .collect(),
    )
}

// ── comparison engine (verbatim port of the harness helpers) ────────────

pub fn mask_value(value: &Value, keys: &HashSet<&str>) -> Value {
    match value {
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| mask_value(entry, keys))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, entry) in map {
                if keys.contains(key.as_str()) {
                    out.insert(key.clone(), json!(MASK_PLACEHOLDER));
                } else {
                    out.insert(key.clone(), mask_value(entry, keys));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Dotted path lookup; numeric segments index arrays. Missing → `None`.
pub fn get_path<'a>(root: Option<&'a Value>, path: Option<&str>) -> Option<&'a Value> {
    let Some(path) = path else { return root };
    if path.is_empty() {
        return root;
    }
    let mut current = root;
    for segment in path.split('.') {
        match current {
            Some(Value::Object(map)) => current = map.get(segment),
            Some(Value::Array(array)) => {
                current = segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| array.get(index))
            }
            _ => return None,
        }
    }
    current
}

fn numbers_eq(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// `deepEqual` with the undefined≡null equivalence folded in: `None` (an
/// absent key/row) compares equal to `Some(Value::Null)`.
pub fn deep_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (None, Some(Value::Null)) | (Some(Value::Null), None) => true,
        (Some(x), Some(y)) => value_eq(x, y),
        _ => false,
    }
}

fn value_eq(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => numbers_eq(x, y),
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys.iter()).all(|(x, y)| value_eq(x, y))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, value)| right.get(key).is_some_and(|other| value_eq(value, other)))
        }
        _ => false,
    }
}

pub fn subset_match(actual: Option<&Value>, expected: &Value) -> bool {
    match expected {
        Value::Array(expected_entries) => match actual {
            Some(Value::Array(actual_entries))
                if actual_entries.len() == expected_entries.len() =>
            {
                expected_entries
                    .iter()
                    .zip(actual_entries.iter())
                    .all(|(expected, actual)| subset_match(Some(actual), expected))
            }
            _ => false,
        },
        Value::Object(expected_map) => match actual {
            Some(Value::Object(actual_map)) => expected_map
                .iter()
                .all(|(key, value)| subset_match(actual_map.get(key), value)),
            _ => false,
        },
        scalar => deep_eq(actual, Some(scalar)),
    }
}

/// Substitute `$var` references inside expected strings (recursively).
pub fn resolve_expected(value: &Value, vars: &BTreeMap<String, String>) -> Value {
    match value {
        Value::String(text) => match text.strip_prefix('$') {
            Some(name) if !text.starts_with("$$") => vars
                .get(name)
                .cloned()
                .map(Value::String)
                .unwrap_or_else(|| value.clone()),
            _ => value.clone(),
        },
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| resolve_expected(entry, vars))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, entry) in map {
                out.insert(key.clone(), resolve_expected(entry, vars));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

pub fn stringify(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| format!("{value}"))
}

// ── scenario execution (runScenario port) ───────────────────────────────

#[derive(Debug, Clone)]
pub struct ScenarioSummary {
    pub ok: bool,
    pub name: String,
    pub checks: usize,
    pub failures: Vec<String>,
    pub debug_results: Option<Vec<Value>>,
}

fn fail(failures: &mut Vec<String>, name: &str, index: usize, invariant: &Value, message: String) {
    failures.push(format!(
        "[{name}#{index}] {}: {message}",
        invariant
            .get("expect")
            .and_then(Value::as_str)
            .unwrap_or("?"),
    ));
}

/// Execute one scenario document against a fresh in-memory home tree.
pub fn run_scenario(doc: &Value) -> ScenarioSummary {
    let name = doc
        .pointer("/meta/name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>")
        .to_string();
    let mut failures: Vec<String> = Vec::new();
    let mut checks = 0usize;
    let mask_keys: HashSet<&str> = match doc.pointer("/setup/mask").and_then(Value::as_array) {
        Some(array) => array.iter().filter_map(|v| v.as_str()).collect(),
        None => DEFAULT_MASK_KEYS.iter().copied().collect(),
    };

    let root = format!("/omt-corpus/{}", sanitize(&name));
    let mut ex = Executor::new(&root);
    ex.recording = doc.pointer("/meta/recordEvents").and_then(Value::as_bool) == Some(true);

    // ── setup ──
    'setup: {
        if let Some(pool) = doc.pointer("/setup/pool") {
            let global_dir_name = pool
                .get("globalDirName")
                .and_then(Value::as_str)
                .unwrap_or("global");
            let global_home = join(&root, global_dir_name);
            ex.vars.insert("global".into(), global_home.clone());
            let mut workspaces: Vec<(String, bool)> = Vec::new();
            if let Some(list) = pool.get("workspaces").and_then(Value::as_array) {
                for workspace in list {
                    let ws_name = workspace
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let omt = workspace.get("omt").and_then(Value::as_bool) == Some(true);
                    workspaces.push((ws_name.clone(), omt));
                    let dir = join(&root, &ws_name);
                    if omt {
                        let _ = MemoryFiles::mount(&global_home, &ex.vfs)
                            .borrow_mut()
                            .mkdir_all(&join(&dir, ".omt"));
                        // mkdir_all is rooted per-home; create via raw vfs.
                        let dot = join(&dir, ".omt");
                        ex.vfs.borrow_mut().insert(dot, Entry::Dir);
                    }
                    ex.vars.insert(ws_name, dir);
                }
            }
            let _ = workspaces;
            ex.fixture = Some(PoolFixture { global_home });
            // No core until openRouted resolves one.
            let first_routing = doc
                .get("operations")
                .and_then(Value::as_array)
                .and_then(|ops| {
                    ops.iter()
                        .find(|op| op.get("op").and_then(Value::as_str) == Some("openRouted"))
                });
            let Some(first_routing) = first_routing else {
                failures.push("pool scenario without an openRouted operation".into());
                break 'setup;
            };
            let cwd_raw = first_routing.pointer("/params/cwd");
            let cwd = ex.resolve_cwd(cwd_raw);
            let home = ex.home_for(cwd.as_deref());
            if let Err(problem) = ex.open_core(&home, &[]) {
                failures.push(format!("pool open failed: {problem}"));
            }
        } else {
            let home = join(&root, "home");
            let sessions: Vec<String> = doc
                .pointer("/setup/activeSessionIds")
                .and_then(Value::as_array)
                .map(|array| {
                    array
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if let Err(problem) = ex.open_core(&home, &sessions) {
                failures.push(format!("open failed: {problem}"));
            }
        }

        // setup.nodes: sequential core.create calls (pre-operation fixture).
        if let Some(nodes) = doc.pointer("/setup/nodes").and_then(Value::as_array) {
            for input in nodes {
                let empty = Map::new();
                let params = input.as_object().unwrap_or(&empty);
                let result = caught(|| {
                    let node = ex.core().create(CreateInput {
                        id: p_opt_str(params, "id"),
                        node_type: p_str(params, "type").to_string(),
                        title: p_str(params, "title").to_string(),
                        parent_id: p_opt_str(params, "parentId"),
                        body: None,
                        priority: None,
                    })?;
                    Ok(node_value(&node))
                });
                if result.get("error").is_some() {
                    failures.push(format!(
                        "setup.nodes {} failed: {}",
                        stringify(input),
                        stringify(&result)
                    ));
                }
            }
        }
    }

    // ── operations ──
    if let Some(operations) = doc.get("operations").and_then(Value::as_array) {
        for operation in operations {
            let op_name = operation.get("op").and_then(Value::as_str).unwrap_or("");
            let params = operation
                .get("params")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = apply_op(&mut ex, op_name, &params);
            ex.results.push(result);
            if let Some(label) = operation.get("label").and_then(Value::as_str) {
                let last = ex.results.last().cloned().unwrap_or(Value::Null);
                ex.aliases.insert(label.to_string(), last);
            }
        }
    }

    // ── invariants ──
    // Pre-read every referenced file once (post-operation snapshot).
    let mut file_cache: BTreeMap<String, Result<String, String>> = BTreeMap::new();
    if let Some(invariants) = doc.get("invariants").and_then(Value::as_array) {
        for invariant in invariants {
            checks += 1;
            let kind = invariant
                .get("expect")
                .and_then(Value::as_str)
                .unwrap_or("");
            match kind {
                "fileContains" | "fileNotContains" => {
                    let file = invariant
                        .get("file")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let cached = file_cache.entry(file.to_string()).or_insert_with(|| {
                        let abs = join(&ex.active_home, file);
                        ex.read_virtual_file(&abs)
                    });
                    match cached {
                        Err(message) => {
                            fail(
                                &mut failures,
                                &name,
                                checks - 1,
                                invariant,
                                format!("file \"{file}\" unreadable: {message}"),
                            );
                        }
                        Ok(content) => {
                            let text = invariant.get("text").and_then(Value::as_str).unwrap_or("");
                            let has = content.contains(text);
                            if kind == "fileContains" && !has {
                                fail(
                                    &mut failures,
                                    &name,
                                    checks - 1,
                                    invariant,
                                    format!("\"{text}\" not in {file}"),
                                );
                            }
                            if kind == "fileNotContains" && has {
                                fail(
                                    &mut failures,
                                    &name,
                                    checks - 1,
                                    invariant,
                                    format!("\"{text}\" unexpectedly in {file}"),
                                );
                            }
                        }
                    }
                    continue;
                }
                "itemEvents" => {
                    let expected = mask_value(
                        &resolve_expected(invariant.get("value").unwrap_or(&Value::Null), &ex.vars),
                        &mask_keys,
                    );
                    let observed: Value = Value::Array(
                        ex.events
                            .borrow()
                            .iter()
                            .map(|(node_id, from, to)| json!([node_id, from, to]))
                            .collect(),
                    );
                    if !value_eq(&observed, &expected) {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "item events {} != {}",
                                stringify(&observed),
                                stringify(&expected)
                            ),
                        );
                    }
                    continue;
                }
                _ => {}
            }

            // Actual resolution + masking.
            let actual_raw: Option<Value> = match invariant.get("op") {
                Some(Value::Number(index)) => index
                    .as_u64()
                    .and_then(|i| ex.results.get(i as usize))
                    .cloned(),
                Some(Value::String(label)) => ex.aliases.get(label).cloned(),
                _ => None,
            };
            let actual_masked = actual_raw.as_ref().map(|raw| mask_value(raw, &mask_keys));
            let at = get_path(
                actual_masked.as_ref(),
                invariant.get("path").and_then(Value::as_str),
            );

            // Expected resolution: literal `value` wins; otherwise a
            // `valueFrom` reference points at another whole result with the
            // SAME path applied before comparison. Absent both → undefined
            // (only meaningful for defined/notDefined, which ignore it).
            let needs_expected = !matches!(kind, "defined" | "notDefined");
            let expected_source: Option<Value> = if !needs_expected {
                None
            } else if invariant.get("value").is_some() {
                invariant
                    .get("value")
                    .map(|value| resolve_expected(value, &ex.vars))
            } else if let Some(from) = invariant.get("valueFrom") {
                let source: Option<Value> = match from {
                    Value::Number(index) => index
                        .as_u64()
                        .and_then(|i| ex.results.get(i as usize))
                        .cloned(),
                    Value::String(label) => ex.aliases.get(label).cloned(),
                    _ => None,
                };
                source.map(|source| {
                    let picked =
                        get_path(Some(&source), invariant.get("path").and_then(Value::as_str));
                    picked.cloned().unwrap_or(Value::Null)
                })
            } else {
                None
            };
            let expected = expected_source
                .as_ref()
                .map(|source| mask_value(source, &mask_keys));

            match kind {
                "equals" => {
                    let Some(expected) = &expected else {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "missing value/valueFrom (actual: {})",
                                at.map(stringify).unwrap_or_else(|| "undefined".into())
                            ),
                        );
                        continue;
                    };
                    if !deep_eq(at, Some(expected)) {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "{} != {}",
                                at.map(stringify).unwrap_or_else(|| "undefined".into()),
                                stringify(expected)
                            ),
                        );
                    }
                }
                "matches" => {
                    let Some(expected) = &expected else {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            "missing value/valueFrom".into(),
                        );
                        continue;
                    };
                    if !subset_match(at, expected) {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "{} does not match {}",
                                at.map(stringify).unwrap_or_else(|| "undefined".into()),
                                stringify(expected)
                            ),
                        );
                    }
                }
                "contains" => {
                    let Some(expected) = &expected else {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            "missing value/valueFrom".into(),
                        );
                        continue;
                    };
                    let hit = match (at, expected) {
                        (Some(Value::String(haystack)), Value::String(needle)) => {
                            haystack.contains(needle.as_str())
                        }
                        (Some(Value::Array(entries)), _) => entries
                            .iter()
                            .any(|entry| subset_match(Some(entry), expected)),
                        _ => false,
                    };
                    if !hit {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "{} does not contain {}",
                                at.map(stringify).unwrap_or_else(|| "undefined".into()),
                                stringify(expected)
                            ),
                        );
                    }
                }
                "length" => {
                    let Some(expected) = &expected else {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            "missing value/valueFrom".into(),
                        );
                        continue;
                    };
                    let ok = match (at, expected.as_u64()) {
                        (Some(Value::Array(entries)), Some(expected_len)) => {
                            entries.len() as u64 == expected_len
                        }
                        _ => false,
                    };
                    if !ok {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "length of {} != {}",
                                at.map(stringify).unwrap_or_else(|| "undefined".into()),
                                stringify(expected)
                            ),
                        );
                    }
                }
                "gte" => {
                    let Some(expected) = &expected else {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            "missing value/valueFrom".into(),
                        );
                        continue;
                    };
                    let ok = match (at.and_then(Value::as_f64), expected.as_f64()) {
                        (Some(actual), Some(expected)) => actual >= expected,
                        _ => false,
                    };
                    if !ok {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "{} < {}",
                                at.map(stringify).unwrap_or_else(|| "undefined".into()),
                                stringify(expected)
                            ),
                        );
                    }
                }
                "defined" => {
                    if at.is_none() {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!("{:?} is undefined", invariant.get("path")),
                        );
                    }
                }
                "notDefined" => {
                    if at.is_some() {
                        fail(
                            &mut failures,
                            &name,
                            checks - 1,
                            invariant,
                            format!(
                                "{:?} is defined: {}",
                                invariant.get("path"),
                                at.map(stringify).unwrap_or_default()
                            ),
                        );
                    }
                }
                other => {
                    fail(
                        &mut failures,
                        &name,
                        checks - 1,
                        invariant,
                        format!("unknown expect kind \"{other}\""),
                    );
                }
            }
        }
    }

    let ok = failures.is_empty() && checks > 0;
    let debug_results = if ok {
        None
    } else {
        Some(
            ex.results
                .iter()
                .map(|result| mask_value(result, &mask_keys))
                .collect(),
        )
    };
    ScenarioSummary {
        ok,
        name,
        checks,
        failures,
        debug_results,
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
