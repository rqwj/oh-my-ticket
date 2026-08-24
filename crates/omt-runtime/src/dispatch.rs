//! Method dispatch (U5a locked contract): every command in
//! `schema/commands.schema.json` plus `handshake/request`, dispatched on
//! the owning home's actor thread. Parity enforcement follows
//! `schema/parity.schema.json`: agent_available default, adapter_only
//! requires a dsh/desktop principal, human_administrative requires an
//! out-of-band administrator grant.
//!
//! Write paths: node/create|update|move|archive|execute and run/report go
//! through the U4a phased journal (storage planners); run/create|control|
//! claim use single immediate transactions gated by domain decisions —
//! the U4a journal vocabulary has no run-row ops yet (documented
//! deviation, README).

use omt_contracts::{NodeStatus, NodeType, RunItemState, RunStatus};
use omt_domain::error;
use omt_domain::types::{NodeRow, RunItemRow};
use omt_storage::clock::{iso_from_ms, MillisClock};
use omt_storage::journal::{report_item_patch, FileOp};
use omt_storage::store;
use omt_storage::{Problem, Result, Storage};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::mpsc::SyncSender;
use std::sync::{Mutex, OnceLock};

use crate::auth::{self, Credential};
use crate::events::Hub;

// ── parity matrix ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Parity {
    AgentAvailable,
    AdapterOnly,
    HumanAdministrative,
}

pub fn parity_of(method: &str) -> Parity {
    match method {
        "node/execute" | "ui/filters-get" | "ui/filters-set" | "ui/recent-get"
        | "ui/recent-set" => Parity::AdapterOnly,
        "home/reindex" => Parity::HumanAdministrative,
        _ => Parity::AgentAvailable,
    }
}

/// Every dispatchable business method (handshake excluded — it issues the
/// credential itself). Documentation mirror of the match below; the
/// protocol-level coverage test exercises each method through a live
/// daemon instead of importing this list.
#[allow(dead_code)]
pub const METHODS: &[&str] = &[
    "node/create",
    "node/get",
    "node/list",
    "node/tree",
    "node/search",
    "node/update",
    "node/move",
    "node/archive",
    "node/execute",
    "home/reindex",
    "run/create",
    "run/get",
    "run/list",
    "run/control",
    "run/claim",
    "run/report",
    "events/resume",
    "ui/filters-get",
    "ui/filters-set",
    "ui/recent-get",
    "ui/recent-set",
];

// ── entry ───────────────────────────────────────────────────────────────

pub struct Ctx<'a> {
    pub storage: &'a mut Storage,
    pub hub: &'a std::sync::Arc<Hub>,
    pub auth: &'a Credential,
    /// Production clock; deterministic injection rides the lock/journal
    /// layers (tests drive FixedClock there).
    clock: SystemClockBox,
    /// Connection writer channel when the caller asked for a live event
    /// subscription (events/resume registers it race-free on the actor).
    pub subscribe: Option<SyncSender<String>>,
}

struct SystemClockBox;
impl MillisClock for SystemClockBox {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

impl Ctx<'_> {
    pub fn home_id(&self) -> Result<&str> {
        self.storage
            .home_id()
            .ok_or_else(|| Problem::new(error::IO, "home identity missing"))
    }

    pub fn now_iso(&self) -> String {
        iso_from_ms(self.clock.now_ms())
    }

    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    #[allow(dead_code)]
    fn clock(&self) -> &dyn MillisClock {
        &self.clock
    }
}

/// Dispatch one authorized method against its opened home.
pub fn dispatch(
    storage: &mut Storage,
    hub: &std::sync::Arc<Hub>,
    method: &str,
    params: Value,
    auth_credential: &Credential,
    subscribe: Option<SyncSender<String>>,
) -> Result<Value> {
    // Parity gate (R22): every business method crosses the matrix before
    // touching a home.
    match parity_of(method) {
        Parity::AgentAvailable => {}
        Parity::AdapterOnly => {
            if !auth_credential.kind.is_adapter() {
                return Err(auth::forbidden(
                    "parity-adapter-only",
                    json!({
                        "method": method,
                        "requiredClientKinds": ["dsh", "desktop"],
                        "clientKind": auth_credential.kind.as_str(),
                    }),
                ));
            }
        }
        Parity::HumanAdministrative => {}
    }
    // Operation-family scope.
    if !auth_credential.operation_allowed(method) {
        return Err(auth::forbidden(
            "operation-not-granted",
            json!({ "method": method, "operations": auth_credential.operations }),
        ));
    }
    // Home scope: methods carrying homeId must address THIS opened home AND
    // a granted home. (recent-* are global-home scoped without a homeId.)
    if let Some(home_id) = params.get("homeId").and_then(|v| v.as_str()) {
        let opened_id = storage.home_id().unwrap_or_default();
        if opened_id != home_id {
            return Err(not_found_home(home_id));
        }
        if !auth_credential.home_allowed(home_id) {
            return Err(auth::forbidden(
                "home-not-scoped",
                json!({ "homeId": home_id }),
            ));
        }
    }

    let ctx = &mut Ctx {
        storage,
        hub,
        auth: auth_credential,
        clock: SystemClockBox,
        subscribe,
    };

    let outcome = match method {
        "node/create" => node_create(ctx, &params),
        "node/get" => node_get(ctx, &params),
        "node/list" => node_list(ctx, &params),
        "node/tree" => node_tree(ctx, &params),
        "node/search" => node_search(ctx, &params),
        "node/update" => node_update(ctx, &params),
        "node/move" => node_move(ctx, &params),
        "node/archive" => node_archive(ctx, &params),
        "node/execute" => node_execute(ctx, &params),
        "home/reindex" => home_reindex(ctx, &params),
        "run/create" => run_create(ctx, &params),
        "run/get" => run_get(ctx, &params),
        "run/list" => run_list(ctx, &params),
        "run/control" => run_control(ctx, &params),
        "run/claim" => run_claim(ctx, &params),
        "run/report" => run_report(ctx, &params),
        "events/resume" => events_resume(ctx, &params),
        "ui/filters-get" => filters_get(ctx, &params),
        "ui/filters-set" => filters_set(ctx, &params),
        "ui/recent-get" => recent_get(ctx, &params),
        "ui/recent-set" => recent_set(ctx, &params),
        other => Err(Problem::with_details(
            error::NOT_FOUND,
            format!("unknown method: {other}"),
            |d| {
                d.insert("kind".into(), "method".into());
                d.insert("id".into(), other.into());
            },
        )),
    };
    if outcome.is_ok() {
        // Fan out anything this job committed (no-op for pure reads).
        let _ = ctx
            .hub
            .publish_new(ctx.storage.conn(), ctx.storage.home_id().unwrap_or(""));
    }
    outcome
}

// ── param helpers ───────────────────────────────────────────────────────

fn require_str<'a>(params: &'a Value, field: &str) -> Result<&'a str> {
    params
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid_input(field, "must be a string"))
}

fn opt_str<'a>(params: &'a Value, field: &str) -> Option<&'a str> {
    params.get(field).and_then(|v| v.as_str())
}

fn opt_i64(params: &Value, field: &str) -> Result<Option<i64>> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| invalid_input(field, "must be an integer")),
        Some(_) => Err(invalid_input(field, "must be an integer")),
    }
}

fn invalid_input(field: &str, message: &str) -> Problem {
    Problem::with_details(error::INVALID_INPUT, format!("{field}: {message}"), |d| {
        d.insert("field".into(), field.into());
    })
}

fn not_found_home(home_id: &str) -> Problem {
    Problem::with_details(error::NOT_FOUND, format!("unknown home: {home_id}"), |d| {
        d.insert("kind".into(), "home".into());
        d.insert("id".into(), home_id.into());
    })
}

fn require_node(conn: &rusqlite::Connection, id: &str) -> Result<NodeRow> {
    store::get_node(conn, id)?.ok_or_else(|| error::not_found_node(id))
}

fn command_id(params: &Value) -> String {
    // Optional test/idempotency affordance (R9 seed): callers may pin the
    // journal commandId; identical replays return the stored result, reuse
    // with different input fails closed CONFLICT command-id-reuse.
    match params.get("commandId").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => crate::problem::entropy::short_id(),
    }
}

fn conflict_rule(rule: &str, message: impl Into<String>, details: Value) -> Problem {
    let mut map = Map::new();
    map.insert("rule".into(), json!(rule));
    if let Value::Object(extra) = details {
        for (key, value) in extra {
            map.insert(key, value);
        }
    }
    Problem {
        code: error::CONFLICT,
        message: message.into(),
        details: Some(Value::Object(map)),
    }
}

fn store_sql(context: &'static str) -> impl Fn(rusqlite::Error) -> Problem {
    move |err| Problem::new(error::IO, format!("{context}: {err}"))
}

/// NodeView fresh from the DB (revision included).
fn views_node(home_id: &str, conn: &rusqlite::Connection, node: &NodeRow) -> Value {
    let revision = store::current_revision(conn, &node.id).unwrap_or(0);
    crate::views::node_view(home_id, node, revision)
}

// ═══════════════════════════════ node plane ════════════════════════════

/// Read a committed operation row (idempotency probe). Read-only SQL on the
/// operations table — the storage layer keeps `stored_result` private, but
/// retry-after-lost-ack needs the probe BEFORE auto-allocating an id.
fn stored_operation(
    conn: &rusqlite::Connection,
    command_id: &str,
) -> Result<Option<(String, String)>> {
    let mut stmt = conn
        .prepare("SELECT input_hash, result_json FROM operations WHERE command_id = ?1")
        .map_err(store_sql("operation lookup"))?;
    let mut rows = stmt
        .query([command_id])
        .map_err(store_sql("operation query"))?;
    match rows.next().map_err(store_sql("operation read"))? {
        Some(row) => {
            let hash: String = row.get(0).map_err(store_sql("operation hash"))?;
            let result: String = row.get(1).map_err(store_sql("operation result"))?;
            Ok(Some((hash, result)))
        }
        None => Ok(None),
    }
}

fn node_create(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let title_raw = require_str(params, "title")?.trim().to_string();
    if title_raw.is_empty() {
        return Err(invalid_input("title", "must not be empty"));
    }
    // Idempotency replay probe BEFORE auto-allocation (R9): a committed
    // commandId must return its stored result even though a naive retry
    // would derive a DIFFERENT id from the advanced counter. The fingerprint
    // reconstruction mirrors plan_create's exact formula [nodeId, title].
    let command = command_id(params);
    if let Some((stored_hash, stored_result)) = stored_operation(ctx.storage.conn(), &command)? {
        let original_id = serde_json::from_str::<Value>(&stored_result)
            .ok()
            .and_then(|result| {
                // Stored results appear in two shapes depending on the write
                // path: the dispatch wire view (/node/nodeId) or the raw
                // finalized row (top-level "id"). Both carry the original id.
                result
                    .pointer("/node/nodeId")
                    .or_else(|| result.pointer("/node/id"))
                    .or_else(|| result.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        let replay_matches = original_id
            .as_ref()
            .map(|original_id| {
                omt_storage::journal::input_fingerprint(&[original_id, &title_raw]) == stored_hash
            })
            .unwrap_or(false);
        if replay_matches {
            // Re-render through the normal view path so a replay returns the
            // SAME shape as first apply (the raw stored row is snake_case).
            if let Some(original_id) = original_id {
                if let Some(node_row) = store::get_node(ctx.storage.conn(), &original_id)? {
                    return Ok(json!({
                        "node": views_node(&home_id, ctx.storage.conn(), &node_row),
                    }));
                }
            }
            return serde_json::from_str(&stored_result).map_err(|err| {
                Problem::new(error::IO, format!("stored result unreadable: {err}"))
            });
        }
        return Err(conflict_rule(
            "command-id-reuse",
            format!("command id {command} reused with different input"),
            json!({ "commandId": command }),
        ));
    }
    let node_type_text = require_str(params, "type")?;
    let node_type: NodeType = node_type_text.parse().map_err(|_| {
        Problem::with_details(
            error::INVALID_INPUT,
            format!("unknown node type: {node_type_text}"),
            |d| {
                d.insert("field".into(), "type".into());
                d.insert("value".into(), node_type_text.into());
            },
        )
    })?;
    let conn = ctx.storage.conn();
    let parent: Option<NodeRow> = match opt_str(params, "parentId") {
        None => {
            omt_domain::hierarchy::check_root_allowed(node_type)?;
            None
        }
        Some(parent_id) => {
            let parent = require_node(conn, parent_id)?;
            omt_domain::hierarchy::check_child_type(&parent, node_type)?;
            Some(parent)
        }
    };

    // Scope is advisory in U5a: clients address explicitly authorized
    // homes; creation lands in the addressed home regardless.
    let _ = opt_str(params, "scope");

    let prefix = omt_domain::types::type_prefix(node_type);
    let next_number = store::counter_value_of(ctx.storage.conn(), prefix)? + 1;
    let id = format!("{prefix}-{next_number:04}");
    let exists = store::get_node(ctx.storage.conn(), &id)?.is_some();
    if exists {
        return Err(conflict_rule(
            "duplicate-node-id",
            format!("duplicate node id: {id}"),
            json!({ "nodeId": id }),
        ));
    }

    let now = ctx.now_iso();
    let path = omt_domain::markdown::path_for(
        &node_type.to_string(),
        &id,
        &title_raw,
        parent.as_ref().map(|p| p.path.as_str()),
    );
    let node = NodeRow {
        id: id.clone(),
        node_type,
        title: title_raw,
        status: NodeStatus::Open,
        archived: false,
        priority: opt_i64(params, "priority")?.unwrap_or(0),
        path: path.clone(),
        created_at: now.clone(),
        updated_at: now,
    };
    let body = match opt_str(params, "body") {
        Some(body) => body.to_string(),
        None => omt_domain::core::default_body(&node.node_type),
    };

    let mutation =
        ctx.storage
            .plan_create(&command_id(params), &node, parent.as_ref(), &body, true)?;
    ctx.storage.execute(&mutation)?;

    let node_row = require_node(ctx.storage.conn(), &id)?;
    Ok(json!({ "node": views_node(&home_id, ctx.storage.conn(), &node_row) }))
}

fn node_get(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let conn = ctx.storage.conn();
    let node = require_node(conn, require_str(params, "nodeId")?)?;
    let file = ctx
        .storage
        .files()
        .read_optional(&node.path)?
        .ok_or_else(|| {
            Problem::with_details(error::IO, "node file missing", |d| {
                d.insert("path".into(), node.path.clone().into());
            })
        })?;
    let parsed = omt_domain::markdown::parse_node_file(&file)?;
    let children = store::children_of(conn, &node.id)?;
    let parent = store::parent_of(conn, &node.id)?;
    let runs = holding_runs(conn, &home_id, &node.id)?;
    Ok(json!({
        "node": views_node(&home_id, conn, &node),
        "parent": parent.map(|p| crate::views::node_summary(&home_id, &p)),
        "children": children.iter().map(|c| crate::views::node_summary(&home_id, c)).collect::<Vec<_>>(),
        "body": parsed.body,
        "runs": runs,
    }))
}

/// Non-terminal runs holding one node (RunLink projection).
fn holding_runs(conn: &rusqlite::Connection, home_id: &str, node_id: &str) -> Result<Vec<Value>> {
    let active = [
        RunStatus::Pending,
        RunStatus::Running,
        RunStatus::Paused,
        RunStatus::Interrupted,
    ];
    let items = items_for_node(conn, node_id, &active)?;
    let mut links = Vec::new();
    for (item, _run_status) in items {
        let run = store::get_run(conn, &item.run_id)?
            .ok_or_else(|| error::not_found_run(&item.run_id))?;
        let counts = state_counts(conn, &run.id)?;
        links.push(crate::views::run_link(home_id, &run, item.state, &counts));
    }
    Ok(links)
}

/// Items of one node across runs in `statuses`, ordered by run_id.
fn items_for_node(
    conn: &rusqlite::Connection,
    node_id: &str,
    statuses: &[RunStatus],
) -> Result<Vec<(RunItemRow, RunStatus)>> {
    let placeholders = statuses
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {}, r.status FROM run_items i JOIN runs r ON r.id = i.run_id \
         WHERE i.node_id = ?1 AND r.status IN ({placeholders}) ORDER BY i.run_id",
        ITEM_COLS_WITH_PREFIX
    );
    let mut stmt = conn.prepare(&sql).map_err(store_sql("items for node"))?;
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(node_id.to_string())];
    for status in statuses {
        bind.push(Box::new(status.to_string()));
    }
    let refs: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), read_item_prefixed)
        .map_err(store_sql("items for node query"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_sql("items for node read"))?;
    Ok(rows)
}

const ITEM_COLS_WITH_PREFIX: &str = "i.run_id, i.node_id, i.position, i.state, i.executor_session_id, i.attempts, i.last_error, i.nudged_at, i.nudge_count, i.started_at, i.finished_at";

fn read_item_prefixed(row: &rusqlite::Row<'_>) -> rusqlite::Result<(RunItemRow, RunStatus)> {
    Ok((
        RunItemRow {
            run_id: row.get(0)?,
            node_id: row.get(1)?,
            position: row.get(2)?,
            state: parse_state_column(row.get::<_, String>(3)?)?,
            executor_session_id: row.get(4)?,
            attempts: row.get(5)?,
            last_error: row.get(6)?,
            nudged_at: row.get(7)?,
            nudge_count: row.get(8)?,
            started_at: row.get(9)?,
            finished_at: row.get(10)?,
        },
        row.get::<_, String>(11)?.parse().map_err(|_| {
            rusqlite::Error::InvalidColumnType(11, "status".into(), rusqlite::types::Type::Text)
        })?,
    ))
}

fn parse_state_column(text: String) -> rusqlite::Result<RunItemState> {
    text.parse().map_err(|_| {
        rusqlite::Error::InvalidColumnType(3, "state".into(), rusqlite::types::Type::Text)
    })
}

fn state_counts(conn: &rusqlite::Connection, run_id: &str) -> Result<Vec<(RunItemState, i64)>> {
    let mut map: BTreeMap<String, i64> = BTreeMap::new();
    for item in store::list_run_items(conn, run_id)? {
        *map.entry(item.state.to_string()).or_insert(0) += 1;
    }
    Ok(map
        .into_iter()
        .filter_map(|(text, count)| text.parse().ok().map(|state| (state, count)))
        .collect())
}

fn node_list(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let filter = params.get("filter").cloned().unwrap_or(json!({}));
    let node_type: Option<NodeType> = match filter.get("type").and_then(|v| v.as_str()) {
        Some(text) => Some(
            text.parse()
                .map_err(|_| invalid_input("filter.type", "unknown type"))?,
        ),
        None => None,
    };
    let status: Option<NodeStatus> = match filter.get("status").and_then(|v| v.as_str()) {
        Some(text) => Some(
            text.parse()
                .map_err(|_| invalid_input("filter.status", "unknown status"))?,
        ),
        None => None,
    };
    let archived = filter.get("archived").and_then(|v| v.as_bool());
    let query = filter.get("query").and_then(|v| v.as_str()).map(str::trim);

    let nodes: Vec<NodeRow> = if let Some(query) = query.filter(|q| !q.is_empty()) {
        let conn = ctx.storage.conn();
        store::search(conn, query, 20)?
            .into_iter()
            .filter_map(|id| store::get_node(conn, &id).ok().flatten())
            .collect()
    } else {
        store::list_nodes(ctx.storage.conn(), node_type, status)?
    };
    let nodes: Vec<NodeRow> = nodes
        .into_iter()
        .filter(|node| archived.is_none_or(|wanted| node.archived == wanted))
        .collect();
    Ok(json!({
        "nodes": nodes.iter().map(|n| crate::views::node_summary(&home_id, n)).collect::<Vec<_>>(),
    }))
}

fn node_tree(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let conn = ctx.storage.conn();
    let root_id = opt_str(params, "rootId");
    let mut children_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in store::all_edges(conn)? {
        children_of
            .entry(edge.parent_id)
            .or_default()
            .push(edge.child_id);
    }
    fn build(
        conn: &rusqlite::Connection,
        home_id: &str,
        id: &str,
        children_of: &BTreeMap<String, Vec<String>>,
    ) -> Result<Value> {
        let node = store::get_node(conn, id)?.ok_or_else(|| error::not_found_node(id))?;
        let children = children_of
            .get(id)
            .map(|kids| {
                kids.iter()
                    .filter_map(|kid| build(conn, home_id, kid, children_of).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(crate::views::tree_node(home_id, &node, children))
    }
    let trees = match root_id {
        None => {
            let mut roots = Vec::new();
            for node in store::list_nodes(conn, None, None)? {
                if store::parent_of(conn, &node.id)?.is_none() {
                    roots.push(build(conn, &home_id, &node.id, &children_of)?);
                }
            }
            roots
        }
        Some(root_id) => {
            require_node(conn, root_id)?;
            vec![build(conn, &home_id, root_id, &children_of)?]
        }
    };
    Ok(json!({ "trees": trees }))
}

fn node_search(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let query = opt_str(params, "query")
        .unwrap_or_default()
        .trim()
        .to_string();
    let limit = opt_i64(params, "limit")?.unwrap_or(20).clamp(1, 100) as usize;
    let conn = ctx.storage.conn();
    let ids: Vec<String> = if query.is_empty() {
        // Empty query yields latest nodes first (@-trigger convention).
        let mut nodes = store::list_nodes(conn, None, None)?;
        nodes.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        nodes.into_iter().take(limit).map(|n| n.id).collect()
    } else {
        store::search(conn, &query, limit)?
    };
    let nodes = ids
        .into_iter()
        .filter_map(|id| store::get_node(conn, &id).ok().flatten())
        .map(|n| crate::views::node_summary(&home_id, &n))
        .collect::<Vec<_>>();
    Ok(json!({ "nodes": nodes }))
}

// ── update pipeline (shared by update/archive/execute/observation) ──────

fn node_update(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let id = require_str(params, "nodeId")?.to_string();
    let changes = params
        .get("changes")
        .cloned()
        .ok_or_else(|| invalid_input("changes", "required"))?;
    if changes.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return Err(invalid_input("changes", "at least one property required"));
    }
    if changes.get("body").is_some() && changes.get("append").is_some() {
        return Err(invalid_input(
            "changes",
            "body and append are mutually exclusive",
        ));
    }
    // Optimistic concurrency gate BEFORE planning (R9).
    if let Some(expected) = opt_i64(params, "expectedRevision")? {
        let current = store::current_revision(ctx.storage.conn(), &id)?;
        if expected != current {
            return Err(revision_mismatch(&id, expected, current));
        }
    }
    let command = command_id(params);
    apply_update_and_activate(
        ctx,
        &id,
        &changes,
        Some(&ctx.auth.actor_namespace),
        false,
        command,
    )
}

fn revision_mismatch(id: &str, expected: i64, current: i64) -> Problem {
    Problem::with_details(
        error::CONFLICT,
        format!("revision mismatch for {id}: expected {expected}, current {current}"),
        |d| {
            d.insert("rule".into(), "revision-mismatch".into());
            d.insert("nodeId".into(), id.into());
            d.insert("expectedRevision".into(), json!(expected));
            d.insert("currentRevision".into(), json!(current));
        },
    )
}

/// One journaled update + passive observation; then, when the change lit a
/// node to in_progress, the ancestor chain lights up (STORY-0022) exactly
/// like core's recursive update().
fn apply_update_and_activate(
    ctx: &mut Ctx,
    id: &str,
    changes: &Value,
    observer_actor: Option<&str>,
    reported: bool,
    command: String,
) -> Result<Value> {
    let outcome = apply_update(ctx, id, changes, observer_actor, reported, command)?;
    if changes.get("status").and_then(|v| v.as_str()) == Some("in_progress") {
        let _ = activate_ancestors(ctx, id);
    }
    Ok(outcome)
}

/// Core of every node write: gates, planner, journal execute, observation.
fn apply_update(
    ctx: &mut Ctx,
    id: &str,
    changes: &Value,
    observer_actor: Option<&str>,
    reported: bool,
    command: String,
) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let conn = ctx.storage.conn();
    let before = require_node(conn, id)?;

    let title = changes.get("title").and_then(|v| v.as_str());
    let status_text = changes.get("status").and_then(|v| v.as_str());
    let priority = match changes.get("priority") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(
            n.as_i64()
                .ok_or_else(|| invalid_input("changes.priority", "integer"))?,
        ),
        Some(_) => return Err(invalid_input("changes.priority", "integer")),
    };
    let archived_change = changes.get("archived").and_then(|v| v.as_bool());
    let replace_body = changes.get("body").and_then(|v| v.as_str());
    let append_note = changes.get("append").and_then(|v| v.as_str());

    let status: Option<NodeStatus> = match status_text {
        Some(text) => Some(
            text.parse()
                .map_err(|_| invalid_input("changes.status", "unknown status"))?,
        ),
        None => None,
    };
    if let Some(title) = title {
        if title.trim().is_empty() {
            return Err(invalid_input("changes.title", "must not be empty"));
        }
    }

    // Archived gate (ARCHIVED_READONLY subdivision): only restoring passes.
    if before.archived && archived_change != Some(false) {
        let touches_content = title.is_some()
            || status.is_some()
            || priority.is_some()
            || replace_body.is_some()
            || append_note.is_some();
        if touches_content || archived_change == Some(true) {
            return Err(Problem::with_details(
                error::ARCHIVED_READONLY,
                format!(
                    "{} is archived; restore it (archived: false) before mutating",
                    before.id
                ),
                |d| {
                    d.insert("nodeId".into(), before.id.clone().into());
                    d.insert("operation".into(), "update".into());
                },
            ));
        }
    }

    let parent = store::parent_of(ctx.storage.conn(), &before.id)?;
    let unarchiving = before.archived && archived_change == Some(false);
    let events_json = json!({
        "nodeId": before.id,
        "status": status.as_ref().map(|s| s.to_string()),
        "archived": archived_change,
        "unarchived": unarchiving.then_some(true),
    });

    let mut mutation = ctx.storage.plan_update(
        &command,
        &before,
        title.map(str::to_string),
        status.map(|s| s.to_string()),
        archived_change,
        priority,
        replace_body.map(str::to_string),
        append_note.map(str::to_string),
        parent.as_ref(),
        vec![],
        events_json,
    )?;

    // Parent managed-children block refresh when title/status moved.
    if title.is_some() || status.is_some() {
        if let Some(parent_node) = &parent {
            append_children_block_rewrite(ctx.storage, &mut mutation.files, parent_node)?;
        }
    }
    ctx.storage.execute(&mutation)?;

    // Passive observation mirrors core.update: broadcast to active runs.
    observe_node_status(
        ctx,
        &before.id,
        status,
        archived_change,
        observer_actor,
        reported,
    )?;

    let updated = require_node(ctx.storage.conn(), id)?;
    Ok(json!({ "node": views_node(&home_id, ctx.storage.conn(), &updated) }))
}

/// Ancestor activation walk (STORY-0022): open ancestors flip to
/// in_progress via journaled updates; done/blocked/skipped never reopen;
/// archived skipped silently; failures are cosmetic.
fn activate_ancestors(ctx: &mut Ctx, child_id: &str) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    seen.insert(child_id.to_string());
    let mut current = store::parent_of(ctx.storage.conn(), child_id)?;
    while let Some(parent) = current {
        if seen.contains(&parent.id) {
            break;
        }
        seen.insert(parent.id.clone());
        if !parent.archived && parent.status == NodeStatus::Open {
            let _ = apply_update(
                ctx,
                &parent.id,
                &json!({ "status": "in_progress" }),
                None,
                false,
                crate::problem::entropy::short_id(),
            );
        }
        current = store::parent_of(ctx.storage.conn(), &parent.id)?;
    }
    Ok(())
}

/// Rewrite one node's managed children block as an extra journaled file op.
fn append_children_block_rewrite(
    storage: &mut Storage,
    files: &mut Vec<FileOp>,
    node: &NodeRow,
) -> Result<()> {
    let entries: Vec<omt_domain::markdown::ChildEntry> =
        store::children_of(storage.conn(), &node.id)?
            .iter()
            .map(|child| omt_domain::markdown::ChildEntry {
                id: child.id.clone(),
                title: child.title.clone(),
                dir_name: omt_domain::markdown::node_dir_name(&child.id, &child.title),
                node_type: child.node_type.to_string(),
                status: child.status.to_string(),
            })
            .collect();
    let block = omt_domain::markdown::render_children_entries(&entries);
    let raw = storage.files().read_optional(&node.path)?.ok_or_else(|| {
        Problem::with_details(error::IO, "parent file missing", |d| {
            d.insert("path".into(), node.path.clone().into());
        })
    })?;
    let parsed = omt_domain::markdown::parse_node_file(&raw)?;
    let content = omt_domain::store::serialize_from_row(
        node,
        None,
        &omt_domain::markdown::replace_children_block(&parsed.body, &block),
    );
    files.push(FileOp::Write {
        path: node.path.clone(),
        before_sha: Some(omt_storage::files::sha256_hex(&raw)),
        after_sha: omt_storage::files::sha256_hex(&content),
        content,
    });
    Ok(())
}

/// Passive observation (TICKET-0061) over SQLite: advance matching items of
/// every ACTIVE run after one node change. Decision logic mirrors
/// core.observe_node_status verbatim; effects land transactionally.
fn observe_node_status(
    ctx: &mut Ctx,
    node_id: &str,
    status_change: Option<NodeStatus>,
    archived_change: Option<bool>,
    observer_session: Option<&str>,
    reported: bool,
) -> Result<Vec<RunItemRow>> {
    if archived_change != Some(true) && status_change.is_none() {
        return Ok(vec![]);
    }
    let advanced = std::cell::RefCell::new(Vec::new());
    let snapshot = items_for_node(
        ctx.storage.conn(),
        node_id,
        &[RunStatus::Running, RunStatus::Paused],
    )?;
    for (item, run_status) in snapshot {
        let in_flight = omt_domain::types::is_run_item_in_flight(item.state);
        let advance =
            in_flight || (item.state == RunItemState::Pending && run_status == RunStatus::Running);

        if archived_change == Some(true) {
            if advance {
                let transitioned = transition_item_sql(
                    ctx,
                    &item.run_id,
                    node_id,
                    RunItemState::Skipped,
                    None,
                    None,
                )?;
                advanced.borrow_mut().push(transitioned);
            }
            continue;
        }
        let Some(change) = status_change else {
            continue;
        };
        match change {
            NodeStatus::InProgress => {
                if item.state == RunItemState::Pending && run_status == RunStatus::Running {
                    let transitioned = transition_item_sql(
                        ctx,
                        &item.run_id,
                        node_id,
                        RunItemState::Running,
                        observer_session.map(str::to_string),
                        None,
                    )?;
                    advanced.borrow_mut().push(transitioned);
                }
            }
            NodeStatus::Done => {
                if !advance {
                    continue;
                }
                let auto_verify = store::get_run(ctx.storage.conn(), &item.run_id)?
                    .map(|r| r.config.auto_verify)
                    .unwrap_or(false);
                let gated = omt_domain::runs::trust_gate_gates(
                    item.state,
                    reported,
                    observer_session,
                    item.executor_session_id.as_deref(),
                    auto_verify,
                );
                let target = if gated {
                    RunItemState::AwaitingConfirmation
                } else {
                    RunItemState::Done
                };
                let transitioned =
                    transition_item_sql(ctx, &item.run_id, node_id, target, None, None)?;
                advanced.borrow_mut().push(transitioned);
            }
            NodeStatus::Blocked | NodeStatus::Skipped => {
                if advance {
                    let target = match change {
                        NodeStatus::Blocked => RunItemState::Blocked,
                        _ => RunItemState::Skipped,
                    };
                    let transitioned =
                        transition_item_sql(ctx, &item.run_id, node_id, target, None, None)?;
                    advanced.borrow_mut().push(transitioned);
                }
            }
            NodeStatus::Open => {
                if matches!(
                    item.state,
                    RunItemState::Done | RunItemState::Blocked | RunItemState::Skipped
                ) {
                    let replayed = replay_item_sql(ctx, &item.run_id, node_id)?;
                    advanced.borrow_mut().push(replayed);
                } else if item.state == RunItemState::AwaitingConfirmation {
                    let transitioned = transition_item_sql(
                        ctx,
                        &item.run_id,
                        node_id,
                        RunItemState::Interrupted,
                        None,
                        None,
                    )?;
                    advanced.borrow_mut().push(transitioned);
                }
            }
        }
    }
    Ok(std::cell::RefCell::into_inner(advanced))
}

/// Direct-transition engine mirroring core.transition_item: gates, patch,
/// stop-on-failure pause, terminal derivation, item event — one transaction.
fn transition_item_sql(
    ctx: &mut Ctx,
    run_id: &str,
    node_id: &str,
    to: RunItemState,
    executor: Option<String>,
    failure_error: Option<String>,
) -> Result<RunItemRow> {
    let home_id = ctx.home_id()?.to_string();
    let conn = ctx.storage.conn();
    let run = store::get_run(conn, run_id)?.ok_or_else(|| error::not_found_run(run_id))?;
    let item = store::get_run_item(conn, run_id, node_id)?
        .ok_or_else(|| error::not_found_run_item(run_id, node_id))?;

    if run.status == RunStatus::Paused {
        if !omt_domain::types::is_run_item_in_flight(item.state) {
            return Err(conflict_rule(
                "dispatch-paused",
                format!("run {run_id} is paused; dispatch is stopped"),
                json!({
                    "runId": run_id,
                    "nodeId": node_id,
                    "runStatus": run.status.to_string(),
                    "itemState": item.state.to_string(),
                }),
            ));
        }
    } else if run.status != RunStatus::Running {
        return Err(conflict_rule(
            "items-frozen",
            format!("run {run_id} is {}; items are frozen", run.status),
            json!({
                "runId": run_id,
                "runStatus": run.status.to_string(),
                "nodeId": node_id,
                "itemState": item.state.to_string(),
            }),
        ));
    }
    if !omt_domain::types::item_transition_allowed(item.state, to) {
        return Err(conflict_rule(
            "item-transition",
            format!(
                "illegal item transition for {node_id}: {} → {}",
                item.state, to
            ),
            json!({
                "runId": run_id,
                "nodeId": node_id,
                "from": item.state.to_string(),
                "to": to.to_string(),
            }),
        ));
    }

    let now = ctx.now_iso();
    let entering_running = to == RunItemState::Running;
    let final_state = omt_domain::types::is_run_item_final(to);
    let last_error = if to == RunItemState::Failed {
        failure_error.clone()
    } else {
        None
    };
    let patch = store::ItemPatchValues {
        state: Some(to),
        position: None,
        executor_session_id: executor,
        clear_executor: false,
        attempts: None,
        last_error,
        clear_last_error: false,
        nudged_at: None,
        nudge_count: None,
        started_at: item.started_at.clone().or_else(|| Some(now.clone())),
        preserve_started_at: entering_running,
        finished_at: final_state.then(|| now.clone()),
        clear_finished_at: false,
    };
    let stop_on_failure_hit = to == RunItemState::Failed
        && run.config.stop_on_failure
        && run.status == RunStatus::Running;

    let home_id_ref = home_id.clone();
    store::in_transaction(conn, move |tx| {
        store::update_run_item(tx, run_id, node_id, &patch)?;
        append_item_event_tx(tx, &home_id_ref, run_id, node_id, to, &now)?;
        if stop_on_failure_hit {
            let refreshed =
                store::get_run(tx, run_id)?.ok_or_else(|| error::not_found_run(run_id))?;
            set_run_status_tx(
                tx,
                &home_id_ref,
                run_id,
                RunStatus::Paused,
                &SYSTEM_CLOCK,
                &refreshed,
            )?;
        }
        derive_terminal_tx(tx, &home_id_ref, run_id, &SYSTEM_CLOCK)?;
        Ok(())
    })?;

    store::get_run_item(ctx.storage.conn(), run_id, node_id)?
        .ok_or_else(|| error::not_found_run_item(run_id, node_id))
}

static SYSTEM_CLOCK: SystemClockBox = SystemClockBox;

/// Replay (decision 11) over SQLite, mirroring core.replay_item.
fn replay_item_sql(ctx: &mut Ctx, run_id: &str, node_id: &str) -> Result<RunItemRow> {
    let conn = ctx.storage.conn();
    let run = store::get_run(conn, run_id)?.ok_or_else(|| error::not_found_run(run_id))?;
    if !matches!(
        run.status,
        RunStatus::Pending | RunStatus::Running | RunStatus::Paused | RunStatus::Interrupted
    ) {
        return Err(conflict_rule(
            "replay-run-gate",
            format!(
                "run {run_id} is {}; replay requires an in-progress run",
                run.status
            ),
            json!({ "runId": run_id, "runStatus": run.status.to_string() }),
        ));
    }
    let item = store::get_run_item(conn, run_id, node_id)?
        .ok_or_else(|| error::not_found_run_item(run_id, node_id))?;
    if !matches!(
        item.state,
        RunItemState::Done | RunItemState::Blocked | RunItemState::Skipped
    ) {
        return Err(conflict_rule(
            "replay-state-gate",
            format!(
                "only done/blocked/skipped items can replay ({node_id} is {})",
                item.state
            ),
            json!({
                "runId": run_id,
                "nodeId": node_id,
                "itemState": item.state.to_string(),
                "required": ["done", "blocked", "skipped"],
            }),
        ));
    }
    let now = ctx.now_iso();
    let home_id = ctx.home_id()?.to_string();
    let patch = store::ItemPatchValues {
        state: Some(RunItemState::Pending),
        position: None,
        executor_session_id: None,
        clear_executor: true,
        attempts: None,
        last_error: None,
        clear_last_error: false,
        nudged_at: None,
        nudge_count: Some(0),
        started_at: None,
        preserve_started_at: false,
        finished_at: None,
        clear_finished_at: true,
    };
    let home_id_ref = home_id.clone();
    store::in_transaction(conn, move |tx| {
        store::update_run_item(tx, run_id, node_id, &patch)?;
        append_item_event_tx(
            tx,
            &home_id_ref,
            run_id,
            node_id,
            RunItemState::Pending,
            &now,
        )?;
        Ok(())
    })?;
    store::get_run_item(ctx.storage.conn(), run_id, node_id)?
        .ok_or_else(|| error::not_found_run_item(run_id, node_id))
}

fn node_move(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let id = require_str(params, "nodeId")?;
    let new_parent_id = require_str(params, "newParentId")?;
    let conn = ctx.storage.conn();
    let node = require_node(conn, id)?;
    let new_parent = require_node(conn, new_parent_id)?;
    omt_domain::hierarchy::check_self_parent(id, new_parent_id)?;
    omt_domain::hierarchy::check_child_type(&new_parent, node.node_type)?;
    check_descendant_cycle(conn, id, new_parent_id)?;

    let old_parent = store::parent_of(conn, id)?;
    let old_path = node.path.clone();
    let new_path = omt_domain::markdown::path_for(
        &node.node_type.to_string(),
        id,
        &node.title,
        Some(&new_parent.path),
    );
    if old_path == new_path {
        return Err(conflict_rule(
            "already-at-target",
            "node is already at the target location",
            json!({ "nodeId": id }),
        ));
    }

    let subtree = subtree_of(conn, id)?;
    let old_dir = omt_domain::markdown::dirname(&old_path);
    let new_dir = omt_domain::markdown::dirname(&new_path);
    let relocate = |member_path: &str| -> String {
        member_path.replacen(old_dir.as_str(), new_dir.as_str(), 1)
    };

    let mutation = ctx.storage.plan_move(
        &command_id(params),
        &node,
        &new_parent,
        old_parent.as_ref(),
        &subtree,
        &|member_id: &str| -> Option<String> {
            subtree
                .iter()
                .find(|m| m.id == member_id)
                .map(|m| relocate(&m.path))
        },
        vec![],
    )?;
    ctx.storage.execute(&mutation)?;

    let moved = require_node(ctx.storage.conn(), id)?;
    Ok(json!({ "node": views_node(&home_id, ctx.storage.conn(), &moved) }))
}

fn check_descendant_cycle(
    conn: &rusqlite::Connection,
    id: &str,
    new_parent_id: &str,
) -> Result<()> {
    if id == new_parent_id {
        return Err(conflict_rule(
            "self-parent",
            "a node cannot become its own parent",
            json!({ "nodeId": id }),
        ));
    }
    let mut current = Some(new_parent_id.to_string());
    let mut hops = 0;
    while let Some(node_id) = current {
        if node_id == id {
            return Err(conflict_rule(
                "descendant-cycle",
                format!("{new_parent_id} is inside {id}'s subtree"),
                json!({ "nodeId": id, "newParentId": new_parent_id }),
            ));
        }
        hops += 1;
        if hops > 10_000 {
            return Err(Problem::new(error::CONFLICT, "edge cycle detected"));
        }
        current = store::parent_of(conn, &node_id)?.map(|p| p.id);
    }
    Ok(())
}

fn subtree_of(conn: &rusqlite::Connection, id: &str) -> Result<Vec<NodeRow>> {
    let mut out = Vec::new();
    out.push(require_node(conn, id)?);
    let mut frontier = vec![id.to_string()];
    while let Some(current) = frontier.pop() {
        for child in store::children_of(conn, &current)? {
            frontier.push(child.id.clone());
            out.push(child);
        }
    }
    Ok(out)
}

fn node_archive(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let id = require_str(params, "nodeId")?.to_string();
    let changes = json!({ "archived": true });
    let command = command_id(params);
    apply_update_and_activate(
        ctx,
        &id,
        &changes,
        Some(&ctx.auth.actor_namespace),
        false,
        command,
    )
}

fn node_execute(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    // adapter_only parity enforced at dispatch entry. Executor identity
    // derives from the CONNECTION credential — never payload fields (R12) —
    // and the execution binding is recorded as an auditable meta row.
    let home_id = ctx.home_id()?.to_string();
    let id = require_str(params, "nodeId")?.to_string();
    let node = require_node(ctx.storage.conn(), &id)?;
    if node.archived {
        return Err(Problem::with_details(
            error::ARCHIVED_READONLY,
            format!("{id} is archived; restore before executing"),
            |d| {
                d.insert("nodeId".into(), id.clone().into());
                d.insert("operation".into(), "update".into());
            },
        ));
    }
    let binding_key = format!("exec:{id}");
    let binding = json!({
        "principalId": ctx.auth.principal_id,
        "actorNamespace": ctx.auth.actor_namespace,
        "clientKind": ctx.auth.kind.as_str(),
        "at": ctx.now_iso(),
    })
    .to_string();

    let changes = json!({ "status": "in_progress" });
    let command = command_id(params);
    apply_update_and_activate(
        ctx,
        &id,
        &changes,
        Some(&ctx.auth.actor_namespace),
        false,
        command,
    )?;

    store::set_meta(ctx.storage.conn(), &binding_key, &binding)?;
    let updated = require_node(ctx.storage.conn(), &id)?;
    let _ = &home_id;
    Ok(json!({ "node": views_node(&home_id, ctx.storage.conn(), &updated) }))
}

// ═══════════════════════════════ reindex ═══════════════════════════════

static RUNTIME_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

pub fn set_runtime_dir(path: std::path::PathBuf) {
    let _ = RUNTIME_DIR.set(path);
}

fn runtime_dir_handle() -> std::path::PathBuf {
    RUNTIME_DIR
        .get()
        .cloned()
        .unwrap_or_else(std::env::temp_dir)
}

fn home_reindex(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    // human_administrative: admin grant OR (U5b) offline exclusive CLI.
    if !auth::is_administrator(&runtime_dir_handle(), ctx.auth) {
        return Err(auth::forbidden(
            "admin-required",
            json!({
                "method": "home/reindex",
                "hint": "add the principalId to <runtime-dir>/admin-grants.json",
            }),
        ));
    }
    let _ = params;
    // Dedicated short-lived connection (reindex::execute wants &mut
    // Connection; the daemon owns this home exclusively so no second
    // writer can race the actor's main handle).
    let db_path = ctx.storage.home_path().join(omt_storage::DB_FILE_NAME);
    let mut conn = rusqlite::Connection::open(&db_path)
        .map_err(|err| Problem::new(error::IO, format!("reindex open: {err}")))?;
    store::apply_open_pragmas(&conn)?;
    let plan = omt_storage::reindex::dry_run(&conn, ctx.storage.files())?;
    let executed =
        omt_storage::reindex::execute(&mut conn, ctx.storage.files(), &plan, &ctx.clock)?;
    let nodes: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
        .map_err(store_sql("count nodes"))?;
    let edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .map_err(store_sql("count edges"))?;
    Ok(json!({
        "nodes": nodes,
        "edges": edges,
        "skipped": executed.quarantined.len(),
    }))
}

// ═══════════════════════════════ run plane ═════════════════════════════

/// Process-wide fenced lease registry (R10). Keys (runId, nodeId); dies
/// with the generation like credentials.
fn leases() -> &'static Mutex<BTreeMap<(String, String), omt_domain::ports::LeaseGrant>> {
    static LEASES: OnceLock<Mutex<BTreeMap<(String, String), omt_domain::ports::LeaseGrant>>> =
        OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Lease TTL for claims (heartbeat renewal wiring lands with the janitor
/// in U5b; claims stay attempt-fenced regardless).
pub const LEASE_TTL_MS: i64 = 15 * 60 * 1000;

fn run_view_full(ctx: &mut Ctx, home_id: &str, run_id: &str) -> Result<Value> {
    let conn = ctx.storage.conn();
    let run = store::get_run(conn, run_id)?.ok_or_else(|| error::not_found_run(run_id))?;
    let items = store::list_run_items(conn, run_id)?;
    let counts = state_counts(conn, run_id)?;
    let mut item_views = Vec::new();
    for item in &items {
        let title = store::get_node(conn, &item.node_id)?.map(|n| n.title);
        item_views.push(crate::views::run_item_view(home_id, item, title.as_deref()));
    }
    Ok(json!({
        "run": crate::views::run_view(home_id, &run, &counts, &items),
        "items": item_views,
    }))
}

fn run_create(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let conn = ctx.storage.conn();
    let node_ids: Vec<String> = params
        .get("nodeIds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect()
        })
        .ok_or_else(|| invalid_input("nodeIds", "required array"))?;
    if node_ids.is_empty() {
        return Err(invalid_input("nodeIds", "minItems 1"));
    }

    // Domain decisions: duplicates, member types, archive gate.
    let mut seen = std::collections::BTreeSet::new();
    for node_id in &node_ids {
        if !seen.insert(node_id.clone()) {
            return Err(Problem::with_details(
                error::DUPLICATE_MEMBER,
                format!("duplicate run member: {node_id}"),
                |d| {
                    d.insert("nodeId".into(), node_id.clone().into());
                },
            ));
        }
        let node = require_node(conn, node_id)?;
        if !omt_domain::types::is_run_member_node_type(node.node_type) {
            return Err(Problem::with_details(
                error::INVALID_INPUT,
                format!(
                    "run member {node_id} must be a ticket/subticket ({} is context only)",
                    node.node_type
                ),
                |d| {
                    d.insert("rule".into(), "member-type".into());
                    d.insert("nodeId".into(), node_id.clone().into());
                    d.insert("nodeType".into(), node.node_type.to_string().into());
                },
            ));
        }
        if node.archived {
            return Err(Problem::with_details(
                error::ARCHIVED_READONLY,
                format!("run member {node_id} is archived"),
                |d| {
                    d.insert("nodeId".into(), node_id.clone().into());
                    d.insert("operation".into(), "run-membership".into());
                },
            ));
        }
    }

    // Config merge + concurrency validation (domain decision).
    let merged = omt_domain::core::merge_run_config(params.get("config"));
    let concurrency =
        omt_domain::runs::validate_concurrency(merged.get("concurrency").unwrap_or(&Value::Null))?;
    let config = omt_domain::types::RunConfigValue {
        stop_on_failure: merged["stopOnFailure"].as_bool().unwrap_or(false),
        auto_continue: merged["autoContinue"].as_bool().unwrap_or(true),
        auto_verify: merged["autoVerify"].as_bool().unwrap_or(false),
        concurrency,
    };

    let now = ctx.now_iso();
    let next_run_number = store::counter_value_of(conn, "RUN")? + 1;
    let run_id = format!("RUN-{next_run_number:04}");
    let title = opt_str(params, "title")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let run = omt_domain::types::RunRow {
        id: run_id.clone(),
        title,
        status: RunStatus::Pending,
        config: config.clone(),
        created_at: now.clone(),
        finished_at: None,
    };

    let home_id_ref = home_id.clone();
    let run_id_items = run_id.clone();
    store::in_transaction(conn, move |tx| {
        store::bump_counter(tx, "RUN")?;
        store::insert_run(tx, &run)?;
        for (position, node_id) in node_ids.iter().enumerate() {
            store::insert_run_item(
                tx,
                &RunItemRow::new(
                    &run_id_items,
                    node_id,
                    position as i64,
                    RunItemState::Pending,
                ),
            )?;
        }
        omt_storage::outbox::append(
            tx,
            &home_id_ref,
            "run.changed",
            &json!({ "kind": "run.changed", "ref": { "homeId": home_id_ref, "runId": run_id_items } }),
            &now,
        )?;
        Ok(())
    })?;

    run_view_full(ctx, &home_id, &run_id)
}

fn run_get(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let run_id = require_str(params, "runId")?;
    if store::get_run(ctx.storage.conn(), run_id)?.is_none() {
        return Err(error::not_found_run(run_id));
    }
    run_view_full(ctx, &home_id, run_id)
}

fn run_list(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let status: Option<RunStatus> = match opt_str(params, "status") {
        Some(text) => Some(
            text.parse()
                .map_err(|_| invalid_input("status", "unknown run status"))?,
        ),
        None => None,
    };
    let statuses: Vec<RunStatus> = status.into_iter().collect();
    let runs = store::list_runs(ctx.storage.conn(), &statuses)?;
    let conn = ctx.storage.conn();
    let mut views = Vec::new();
    for run in &runs {
        let items = store::list_run_items(conn, &run.id).unwrap_or_default();
        let counts = state_counts(conn, &run.id).unwrap_or_default();
        views.push(crate::views::run_view(&home_id, run, &counts, &items));
    }
    Ok(json!({ "runs": views }))
}

/// Validated run status change (finished_at semantics mirror
/// core.set_run_status); emits the run.changed event inside the tx.
fn set_run_status_tx(
    tx: &rusqlite::Connection,
    home_id: &str,
    run_id: &str,
    to: RunStatus,
    clock: &dyn MillisClock,
    before: &omt_domain::types::RunRow,
) -> Result<()> {
    if !omt_domain::types::run_transition_allowed(before.status, to) {
        return Err(conflict_rule(
            "run-transition",
            format!(
                "illegal run transition for {run_id}: {} → {}",
                before.status, to
            ),
            json!({
                "runId": run_id,
                "from": before.status.to_string(),
                "to": to.to_string(),
            }),
        ));
    }
    let now = iso_from_ms(clock.now_ms());
    let finished_at: Option<Option<String>> = match to {
        RunStatus::Running => None,
        RunStatus::Completed
        | RunStatus::CompletedWithFailures
        | RunStatus::Canceled
        | RunStatus::Interrupted => Some(Some(now.clone())),
        RunStatus::Pending | RunStatus::Paused => Some(before.finished_at.clone()),
    };
    store::update_run(tx, run_id, Some(to), finished_at, None)?;
    omt_storage::outbox::append(
        tx,
        home_id,
        "run.changed",
        &json!({ "kind": "run.changed", "ref": { "homeId": home_id, "runId": run_id } }),
        &now,
    )?;
    Ok(())
}

fn run_control(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let run_id = require_str(params, "runId")?.to_string();
    let action_text = require_str(params, "action")?;
    let action: omt_contracts::RunControlAction = action_text
        .parse()
        .map_err(|_| invalid_input("action", "unknown control action"))?;
    let conn = ctx.storage.conn();
    let run = store::get_run(conn, &run_id)?.ok_or_else(|| error::not_found_run(&run_id))?;
    let now = ctx.now_iso();

    match action {
        omt_contracts::RunControlAction::Start => {
            if run.status != RunStatus::Pending {
                return Err(status_gate(&run_id, run.status, &[RunStatus::Pending]));
            }
            store::in_transaction(conn, |tx| {
                set_run_status_tx(tx, &home_id, &run_id, RunStatus::Running, &ctx.clock, &run)?;
                derive_terminal_tx(tx, &home_id, &run_id, &ctx.clock)?;
                Ok(())
            })?;
        }
        omt_contracts::RunControlAction::Pause => {
            if run.status != RunStatus::Running {
                return Err(status_gate(&run_id, run.status, &[RunStatus::Running]));
            }
            store::in_transaction(conn, |tx| {
                set_run_status_tx(tx, &home_id, &run_id, RunStatus::Paused, &ctx.clock, &run)
            })?;
        }
        omt_contracts::RunControlAction::Resume => {
            if !matches!(run.status, RunStatus::Paused | RunStatus::Interrupted) {
                return Err(status_gate(
                    &run_id,
                    run.status,
                    &[RunStatus::Paused, RunStatus::Interrupted],
                ));
            }
            store::in_transaction(conn, |tx| {
                set_run_status_tx(tx, &home_id, &run_id, RunStatus::Running, &ctx.clock, &run)
            })?;
        }
        omt_contracts::RunControlAction::Cancel => {
            store::in_transaction(conn, |tx| {
                set_run_status_tx(tx, &home_id, &run_id, RunStatus::Canceled, &ctx.clock, &run)
            })?;
        }
        omt_contracts::RunControlAction::Retry => {
            let node_id = require_str(params, "nodeId")?;
            retry_item(ctx, &run_id, node_id, &run, &now)?;
        }
        omt_contracts::RunControlAction::Remove => {
            let node_id = require_str(params, "nodeId")?;
            remove_item(ctx, &run_id, node_id)?;
        }
    }
    run_view_full(ctx, &home_id, &run_id)
}

fn status_gate(run_id: &str, current: RunStatus, required: &[RunStatus]) -> Problem {
    conflict_rule(
        "run-status-gate",
        format!("run-status-gate violated for {run_id}"),
        json!({
            "runId": run_id,
            "current": current.to_string(),
            "required": required.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
        }),
    )
}

fn retry_item(
    ctx: &mut Ctx,
    run_id: &str,
    node_id: &str,
    run: &omt_domain::types::RunRow,
    now: &str,
) -> Result<()> {
    let conn = ctx.storage.conn();
    if matches!(run.status, RunStatus::Canceled | RunStatus::Completed) {
        return Err(conflict_rule(
            "retry-run-gate",
            format!("run {run_id} is {}; retry is unavailable", run.status),
            json!({ "runId": run_id, "runStatus": run.status.to_string() }),
        ));
    }
    let item = store::get_run_item(conn, run_id, node_id)?
        .ok_or_else(|| error::not_found_run_item(run_id, node_id))?;
    if !matches!(
        item.state,
        RunItemState::Failed | RunItemState::Interrupted | RunItemState::Pending
    ) {
        return Err(conflict_rule(
            "retry-state-gate",
            format!(
                "only failed/interrupted/pending items can retry ({node_id} is {})",
                item.state
            ),
            json!({
                "runId": run_id,
                "nodeId": node_id,
                "itemState": item.state.to_string(),
                "required": ["failed", "interrupted", "pending"],
            }),
        ));
    }
    let home_id = ctx.home_id()?.to_string();
    let was_terminal_failure = run.status == RunStatus::CompletedWithFailures;
    let home_id_ref = home_id.clone();
    store::in_transaction(conn, move |tx| {
        store::update_run_item(
            tx,
            run_id,
            node_id,
            &store::ItemPatchValues {
                state: Some(RunItemState::Pending),
                position: None,
                executor_session_id: None,
                clear_executor: true,
                attempts: Some(item.attempts + 1),
                last_error: None,
                clear_last_error: false, // kept across retries
                nudged_at: None,
                nudge_count: Some(0),
                started_at: None,
                preserve_started_at: false,
                finished_at: None,
                clear_finished_at: true,
            },
        )?;
        append_item_event_tx(
            tx,
            &home_id_ref,
            run_id,
            node_id,
            RunItemState::Pending,
            now,
        )?;
        if was_terminal_failure {
            let refreshed =
                store::get_run(tx, run_id)?.ok_or_else(|| error::not_found_run(run_id))?;
            set_run_status_tx(
                tx,
                &home_id_ref,
                run_id,
                RunStatus::Running,
                &SYSTEM_CLOCK,
                &refreshed,
            )?;
        }
        Ok(())
    })?;
    leases()
        .lock()
        .expect("leases")
        .remove(&(run_id.to_string(), node_id.to_string()));
    Ok(())
}

fn remove_item(ctx: &mut Ctx, run_id: &str, node_id: &str) -> Result<()> {
    let conn = ctx.storage.conn();
    let item = store::get_run_item(conn, run_id, node_id)?
        .ok_or_else(|| error::not_found_run_item(run_id, node_id))?;
    if omt_domain::types::is_run_item_in_flight(item.state) {
        let force_allowed = match store::get_node(conn, node_id)? {
            Some(node) => node.archived,
            None => true, // dangling row: nothing left to protect
        };
        if !force_allowed {
            return Err(conflict_rule(
                "remove-in-flight",
                format!(
                    "item {node_id} is {} (in-flight); it cannot be removed",
                    item.state
                ),
                json!({
                    "runId": run_id,
                    "nodeId": node_id,
                    "itemState": item.state.to_string(),
                }),
            ));
        }
    }
    let home_id = ctx.home_id()?.to_string();
    store::in_transaction(conn, |tx| {
        store::delete_run_item(tx, run_id, node_id)?;
        derive_terminal_tx(tx, &home_id, run_id, &ctx.clock)?;
        Ok(())
    })?;
    leases()
        .lock()
        .expect("leases")
        .remove(&(run_id.to_string(), node_id.to_string()));
    Ok(())
}

/// Terminal derivation over SQL rows (domain decision + status write +
/// event) inside an open transaction.
fn derive_terminal_tx(
    tx: &rusqlite::Connection,
    home_id: &str,
    run_id: &str,
    clock: &dyn MillisClock,
) -> Result<()> {
    let run = match store::get_run(tx, run_id)? {
        Some(run) => run,
        None => return Ok(()),
    };
    let items = store::list_run_items(tx, run_id)?;
    let Some(terminal) = omt_domain::runs::derive_terminal(run.status, &items) else {
        return Ok(());
    };
    if terminal == run.status {
        return Ok(());
    }
    set_run_status_tx(tx, home_id, run_id, terminal, clock, &run)?;
    Ok(())
}

fn run_claim(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let run_id = require_str(params, "runId")?.to_string();
    let conn = ctx.storage.conn();
    let run = store::get_run(conn, &run_id)?.ok_or_else(|| error::not_found_run(&run_id))?;
    if run.status != RunStatus::Running {
        return Err(status_gate(&run_id, run.status, &[RunStatus::Running]));
    }
    // R12: executor identity derives from the connection credential.
    let executor = ctx.auth.actor_namespace.clone();

    let now_iso = ctx.now_iso();
    let (claimed, drained) = store::claim_next_run_item(conn, &run_id, &executor, &now_iso)?;

    // Durable item events: drained members first, then the claim (ratified
    // decision 3 ordering).
    for item in &drained {
        append_item_event_tx(
            conn,
            &home_id,
            &run_id,
            &item.node_id,
            RunItemState::Skipped,
            &now_iso,
        )?;
    }
    if let Some(item) = &claimed {
        append_item_event_tx(
            conn,
            &home_id,
            &run_id,
            &item.node_id,
            RunItemState::Running,
            &now_iso,
        )?;
    }

    match claimed {
        None => {
            // Queue empty: derivation may close the run.
            store::in_transaction(conn, |tx| {
                derive_terminal_tx(tx, &home_id, &run_id, &ctx.clock)
            })?;
            Ok(json!({
                "homeId": home_id,
                "runId": run_id,
                "claimed": false,
            }))
        }
        Some(item) => {
            // Attempt-fenced lease bound to this actor.
            let grant = omt_domain::ports::LeaseGrant {
                token: crate::problem::entropy::token_hex(),
                attempt: item.attempts,
                principal: executor.clone(),
                expires_at: ctx.now_ms() + LEASE_TTL_MS,
            };
            leases()
                .lock()
                .expect("leases")
                .insert((run_id.clone(), item.node_id.clone()), grant.clone());
            // Ancestor activation is best-effort on the claim path.
            let _ = activate_ancestors(ctx, &item.node_id);

            let ticket = require_node(ctx.storage.conn(), &item.node_id)?;
            let ticket_file = ctx.storage.files().read_optional(&ticket.path)?;
            let current_body = ticket_file
                .as_deref()
                .and_then(|raw| omt_domain::markdown::parse_node_file(raw).ok())
                .map(|parsed| parsed.body)
                .unwrap_or_default();
            let context = build_claim_context(ctx, &home_id, &ticket.id, &current_body)?;

            let conn = ctx.storage.conn();
            let fresh_item = store::get_run_item(conn, &run_id, &item.node_id)?
                .ok_or_else(|| error::not_found_run_item(&run_id, &item.node_id))?;
            Ok(json!({
                "homeId": home_id,
                "runId": run_id,
                "claimed": true,
                "lease": {
                    "token": grant.token,
                    "attempt": grant.attempt,
                    "principal": grant.principal,
                    "expiresAt": iso_from_ms(grant.expires_at),
                },
                "item": crate::views::run_item_view(&home_id, &fresh_item, Some(&ticket.title)),
                "ticket": views_node(&home_id, conn, &ticket),
                "context": context,
            }))
        }
    }
}

fn append_item_event_tx(
    tx: &rusqlite::Connection,
    home_id: &str,
    run_id: &str,
    node_id: &str,
    state: RunItemState,
    now: &str,
) -> Result<()> {
    omt_storage::outbox::append(
        tx,
        home_id,
        "run.item_changed",
        &json!({
            "kind": "run.item_changed",
            "ref": { "homeId": home_id, "runId": run_id, "nodeId": node_id },
            "state": state.to_string(),
        }),
        now,
    )?;
    Ok(())
}

/// Budgeted ancestor context (ClaimContext): current item body plus
/// read-only ancestor bodies under ANCESTOR_BUDGET_BYTES total.
const ANCESTOR_BUDGET_BYTES: usize = 16 * 1024;

fn build_claim_context(
    ctx: &mut Ctx,
    home_id: &str,
    node_id: &str,
    current_body: &str,
) -> Result<Value> {
    let conn = ctx.storage.conn();
    let mut used = current_body.len().min(ANCESTOR_BUDGET_BYTES);
    let mut truncated_total = false;
    let mut ancestors_json = Vec::new();
    let mut read_errors = Vec::new();

    let current_summary = store::get_node(conn, node_id)?
        .map(|n| crate::views::node_summary(home_id, &n))
        .unwrap_or(json!({}));

    let mut current = store::parent_of(conn, node_id)?;
    let mut hops = 0;
    while let Some(ancestor) = current {
        hops += 1;
        if hops > 64 {
            break;
        }
        let summary = crate::views::node_summary(home_id, &ancestor);
        match ctx.storage.files().read_optional(&ancestor.path) {
            Ok(Some(raw)) => match omt_domain::markdown::parse_node_file(&raw) {
                Ok(parsed) => {
                    let remaining = ANCESTOR_BUDGET_BYTES.saturating_sub(used);
                    let take = remaining.min(parsed.body.len());
                    let truncated_entry = take < parsed.body.len();
                    if truncated_entry {
                        truncated_total = true;
                    }
                    used += take;
                    ancestors_json.push(json!({
                        "node": summary,
                        "body": parsed.body[..take].to_string(),
                        "truncated": truncated_entry,
                        "originalBytes": parsed.body.len(),
                        "includedBytes": take,
                    }));
                }
                Err(err) => read_errors.push(json!({ "node": summary, "error": err.message })),
            },
            Ok(None) => read_errors.push(json!({ "node": summary, "error": "file missing" })),
            Err(err) => read_errors.push(json!({ "node": summary, "error": err.message })),
        }
        current = store::parent_of(conn, &ancestor.id)?;
    }
    Ok(json!({
        "ancestorBudgetBytes": ANCESTOR_BUDGET_BYTES,
        "ancestorUsedBytes": used.min(ANCESTOR_BUDGET_BYTES),
        "truncated": truncated_total,
        "ancestors": ancestors_json,
        "readErrors": read_errors,
        "current": {
            "node": current_summary,
            "body": current_body,
        },
    }))
}

fn run_report(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let run_id = require_str(params, "runId")?.to_string();
    let node_id = require_str(params, "nodeId")?.to_string();
    let outcome_text = require_str(params, "outcome")?;
    const OUTCOMES: [&str; 4] = ["done", "failed", "blocked", "skipped"];
    if !OUTCOMES.contains(&outcome_text) {
        return Err(invalid_input(
            "outcome",
            "unknown report outcome (done/failed/blocked/skipped)",
        ));
    }
    let note = opt_str(params, "note")
        .map(str::trim)
        .filter(|n| !n.is_empty());

    let conn = ctx.storage.conn();
    let _run = store::get_run(conn, &run_id)?.ok_or_else(|| error::not_found_run(&run_id))?;
    let node = require_node(conn, &node_id)?;
    let item = store::get_run_item(conn, &run_id, &node_id)?
        .ok_or_else(|| error::not_found_run_item(&run_id, &node_id))?;

    // Trust gate / lease authorization (domain decision, R10): presented
    // token fences against the stored grant; no lease without either a
    // matching lease or audited administration.
    let stored_grant = leases()
        .lock()
        .expect("leases")
        .get(&(run_id.clone(), node_id.clone()))
        .cloned();
    let authority = match params.get("leaseToken").and_then(|v| v.as_str()) {
        Some(presented) => omt_domain::runs::ReportAuthority::ExecutorLease {
            token: presented.to_string(),
            actor: ctx.auth.actor_namespace.clone(),
        },
        None => {
            if auth::is_administrator(&runtime_dir_handle(), ctx.auth) {
                omt_domain::runs::ReportAuthority::Administrator {
                    reason: "admin-grant".to_string(),
                }
            } else {
                omt_domain::runs::ReportAuthority::ExecutorLease {
                    token: String::new(),
                    actor: ctx.auth.actor_namespace.clone(),
                }
            }
        }
    };
    let verdict = omt_domain::runs::authorize_report(
        item.state,
        item.attempts,
        stored_grant.as_ref(),
        &authority,
        ctx.now_ms(),
        stored_grant.as_ref().map(|grant| grant.token.as_str()),
    )?;
    let _ = verdict;

    // Archived executable tickets reject reports (ARCHIVED_READONLY).
    let executable = omt_domain::types::is_run_member_node_type(node.node_type);
    if node.archived && executable {
        return Err(Problem::with_details(
            error::ARCHIVED_READONLY,
            format!("{node_id} is archived; restore before reporting"),
            |d| {
                d.insert("nodeId".into(), node_id.clone().into());
                d.insert("operation".into(), "report".into());
            },
        ));
    }

    let failed = outcome_text == "failed";
    let outcome_state: Option<RunItemState> = match outcome_text {
        "done" => Some(RunItemState::Done),
        "blocked" => Some(RunItemState::Blocked),
        "skipped" => Some(RunItemState::Skipped),
        _ => None,
    };

    let now = ctx.now_iso();
    let patch = report_item_patch(
        &item,
        if failed { "failed" } else { outcome_text },
        now.clone(),
        failed.then(|| note.unwrap_or_default().to_string()),
    );
    let parent = store::parent_of(conn, &node_id)?;
    let command = command_id(params);
    let mut mutation = ctx.storage.plan_report(
        &command,
        &node,
        parent.as_ref(),
        outcome_state.map(|s| s.to_string()),
        if failed {
            None
        } else {
            note.map(str::to_string)
        },
        patch,
        vec![],
    )?;
    mutation.push_event(
        "run.item_changed",
        json!({
            "kind": "run.item_changed",
            "ref": { "homeId": home_id, "runId": run_id, "nodeId": node_id },
            "state": if failed { "failed" } else { outcome_text },
        }),
    );
    ctx.storage.execute(&mutation)?;
    leases()
        .lock()
        .expect("leases")
        .remove(&(run_id.clone(), node_id.clone()));

    // Stop-on-failure + terminal derivation mirror the TS report flow.
    let conn = ctx.storage.conn();
    let run_after = store::get_run(conn, &run_id)?.ok_or_else(|| error::not_found_run(&run_id))?;
    let stop_hit =
        failed && run_after.config.stop_on_failure && run_after.status == RunStatus::Running;
    let home_id_ref = home_id.clone();
    let run_id_tx = run_id.clone();
    store::in_transaction(conn, move |tx| {
        if stop_hit {
            set_run_status_tx(
                tx,
                &home_id_ref,
                &run_id_tx,
                RunStatus::Paused,
                &SYSTEM_CLOCK,
                &run_after,
            )?;
        }
        derive_terminal_tx(tx, &home_id_ref, &run_id_tx, &SYSTEM_CLOCK)?;
        Ok(())
    })?;

    let final_run = store::get_run(conn, &run_id)?.ok_or_else(|| error::not_found_run(&run_id))?;
    let final_item = store::get_run_item(conn, &run_id, &node_id)?
        .ok_or_else(|| error::not_found_run_item(&run_id, &node_id))?;
    let final_node = require_node(conn, &node_id)?;
    let counts = state_counts(conn, &run_id)?;
    let items = store::list_run_items(conn, &run_id)?;
    Ok(json!({
        "run": crate::views::run_view(&home_id, &final_run, &counts, &items),
        "item": crate::views::run_item_view(&home_id, &final_item, Some(&final_node.title)),
        "node": views_node(&home_id, conn, &final_node),
    }))
}

// ═══════════════════════════════ events ════════════════════════════════

pub const MAX_EVENT_BATCH: i64 = 1000;

fn events_resume(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let home_id = ctx.home_id()?.to_string();
    let cursor = opt_i64(params, "cursor")?.unwrap_or(0).max(0);
    let limit = opt_i64(params, "limit")?
        .unwrap_or(500)
        .clamp(1, MAX_EVENT_BATCH) as usize;
    let batch = crate::events::backlog(ctx.storage.conn(), &home_id, cursor, limit)?;
    // Register the live subscription AFTER the backlog read, ON THE ACTOR:
    // commits cannot interleave with the actor, so nothing falls into the
    // gap between backlog page and subscription. An envelope may arrive
    // twice (page boundary vs live push); clients dedupe by cursor.
    if let Some(sender) = ctx.subscribe.take() {
        ctx.hub.subscribe(sender);
    }
    let new_cursor = batch.last().map(|e| e.cursor).unwrap_or(cursor);
    Ok(json!({
        "cursor": new_cursor,
        "resync": false,
        "events": batch.iter().map(|e| e.value.clone()).collect::<Vec<_>>(),
    }))
}

// ═══════════════════════════════ ui bags ═══════════════════════════════

const FILTERS_PREFIX: &str = "uifilters:";
const RECENT_PREFIX: &str = "uirecent:";

fn filters_get(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let key = require_str(params, "key")?;
    let meta_key = format!("{FILTERS_PREFIX}{key}");
    let raw = store::get_meta(ctx.storage.conn(), meta_key.as_str())?;
    let filters = raw
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    Ok(json!({ "filters": filters }))
}

fn filters_set(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let key = require_str(params, "key")?;
    let patch = params
        .get("filters")
        .cloned()
        .ok_or_else(|| invalid_input("filters", "required object"))?;
    if !patch.is_object() {
        return Err(invalid_input("filters", "must be an object"));
    }
    let meta_key = format!("{FILTERS_PREFIX}{key}");
    let existing = store::get_meta(ctx.storage.conn(), meta_key.as_str())?
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    let mut merged = existing.as_object().expect("object").clone();
    for (key, value) in patch.as_object().expect("object") {
        merged.insert(key.clone(), value.clone());
    }
    let merged_value = Value::Object(merged);
    store::set_meta(
        ctx.storage.conn(),
        meta_key.as_str(),
        &merged_value.to_string(),
    )?;
    Ok(json!({ "filters": merged_value }))
}

/// Recent bags are GLOBAL-home scoped (schema description): callers route
/// this job onto the global home's actor.
fn recent_get(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let key = require_str(params, "key")?;
    let meta_key = format!("{RECENT_PREFIX}{key}");
    let raw = store::get_meta(ctx.storage.conn(), meta_key.as_str())?;
    let refs = raw
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(|v| v.is_array())
        .unwrap_or_else(|| json!([]));
    Ok(json!({ "refs": refs }))
}

fn recent_set(ctx: &mut Ctx, params: &Value) -> Result<Value> {
    let key = require_str(params, "key")?;
    let refs = params
        .get("refs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| invalid_input("refs", "required array"))?;
    if refs.len() > 100 {
        return Err(invalid_input("refs", "maxItems 100"));
    }
    for reference in refs {
        let ok = reference.get("homeId").and_then(|v| v.as_str()).is_some()
            && reference.get("nodeId").and_then(|v| v.as_str()).is_some();
        if !ok {
            return Err(invalid_input(
                "refs",
                "entries must be QualifiedNodeRef objects",
            ));
        }
    }
    let meta_key = format!("{RECENT_PREFIX}{key}");
    store::set_meta(
        ctx.storage.conn(),
        meta_key.as_str(),
        &Value::Array(refs.clone()).to_string(),
    )?;
    Ok(json!({ "refs": Value::Array(refs.clone()) }))
}
