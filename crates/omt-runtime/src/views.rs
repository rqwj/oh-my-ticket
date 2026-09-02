//! Row → protocol view mapping (common.schema.json shapes, camelCase wire
//! fields). Built as plain serde_json values so additive schema changes
//! never break the daemon build.

use omt_contracts::RunStatus;
use omt_domain::types::{NodeRow, RunItemRow, RunRow};
use serde_json::{json, Map, Value};

#[allow(dead_code)]
pub fn home_id(home_id: &str) -> Value {
    Value::String(home_id.to_string())
}

pub fn node_summary(home_id: &str, node: &NodeRow) -> Value {
    json!({
        "homeId": home_id,
        "nodeId": node.id,
        "type": node.node_type.to_string(),
        "title": node.title,
        "status": node.status.to_string(),
        "archived": node.archived,
        "priority": node.priority,
    })
}

pub fn node_view(home_id: &str, node: &NodeRow, revision: i64) -> Value {
    json!({
        "homeId": home_id,
        "nodeId": node.id,
        "type": node.node_type.to_string(),
        "title": node.title,
        "status": node.status.to_string(),
        "archived": node.archived,
        "priority": node.priority,
        "path": node.path,
        "revision": revision,
        "createdAt": node.created_at,
        "updatedAt": node.updated_at,
    })
}

pub fn tree_node(home_id: &str, node: &NodeRow, children: Vec<Value>) -> Value {
    json!({
        "homeId": home_id,
        "nodeId": node.id,
        "type": node.node_type.to_string(),
        "title": node.title,
        "status": node.status.to_string(),
        "archived": node.archived,
        "priority": node.priority,
        "children": children,
    })
}

pub fn progress(counts: &[(omt_contracts::RunItemState, i64)]) -> Value {
    let mut map = Map::new();
    for (state, count) in counts {
        map.insert(state.to_string(), json!(count));
    }
    let total: i64 = counts.iter().map(|(_, c)| *c).sum();
    json!({
        "total": total,
        "pending": map.get("pending").cloned().unwrap_or(json!(0)),
        "running": map.get("running").cloned().unwrap_or(json!(0)),
        "done": map.get("done").cloned().unwrap_or(json!(0)),
        "failed": map.get("failed").cloned().unwrap_or(json!(0)),
        "blocked": map.get("blocked").cloned().unwrap_or(json!(0)),
        "skipped": map.get("skipped").cloned().unwrap_or(json!(0)),
        "interrupted": map.get("interrupted").cloned().unwrap_or(json!(0)),
        "awaitingConfirmation": map.get("awaiting_confirmation").cloned().unwrap_or(json!(0)),
    })
}

pub const NUDGE_BUDGET: i64 = 3;

pub fn stalled_count(items: &[RunItemRow]) -> i64 {
    items
        .iter()
        .filter(|item| {
            item.state == omt_contracts::RunItemState::Pending && item.nudge_count >= NUDGE_BUDGET
        })
        .count() as i64
}

pub fn run_view(
    home_id: &str,
    run: &RunRow,
    counts: &[(omt_contracts::RunItemState, i64)],
    items: &[RunItemRow],
) -> Value {
    let mut view = json!({
        "homeId": home_id,
        "runId": run.id,
        "status": run.status.to_string(),
        "config": run_config(&run.config),
        "progress": progress(counts),
        "stalledCount": stalled_count(items),
        "createdAt": run.created_at,
    });
    if let Some(title) = &run.title {
        view["title"] = json!(title);
    }
    if let Some(finished_at) = &run.finished_at {
        view["finishedAt"] = json!(finished_at);
    }
    view
}

pub fn run_config(config: &omt_domain::types::RunConfigValue) -> Value {
    json!({
        "stopOnFailure": config.stop_on_failure,
        "autoContinue": config.auto_continue,
        "autoVerify": config.auto_verify,
        "concurrency": config.concurrency,
    })
}

pub fn run_item_view(home_id: &str, item: &RunItemRow, title: Option<&str>) -> Value {
    let mut view = json!({
        "homeId": home_id,
        "runId": item.run_id,
        "nodeId": item.node_id,
        "position": item.position,
        "state": item.state.to_string(),
        "attempts": item.attempts,
        "stalled": item.state == omt_contracts::RunItemState::Pending
            && item.nudge_count >= NUDGE_BUDGET,
    });
    if let Some(executor) = &item.executor_session_id {
        view["executorActor"] = json!(executor);
    }
    if let Some(last_error) = &item.last_error {
        view["lastError"] = json!(last_error);
    }
    if let Some(started_at) = &item.started_at {
        view["startedAt"] = json!(started_at);
    }
    if let Some(finished_at) = &item.finished_at {
        view["finishedAt"] = json!(finished_at);
    }
    if let Some(title) = title {
        view["title"] = json!(title);
    }
    view
}

pub fn run_link(
    home_id: &str,
    run: &RunRow,
    item_state: omt_contracts::RunItemState,
    counts: &[(omt_contracts::RunItemState, i64)],
) -> Value {
    let mut link = json!({
        "homeId": home_id,
        "runId": run.id,
        "status": run.status.to_string(),
        "itemState": item_state.to_string(),
        "progress": progress(counts),
    });
    if let Some(title) = &run.title {
        link["title"] = json!(title);
    }
    link
}

#[allow(dead_code)] // used by U5b janitor demotion; pinned beside terminal derivation
pub fn is_terminal_run(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed | RunStatus::CompletedWithFailures | RunStatus::Canceled
    )
}
