//! U5a dispatch coverage: EVERY action in schema/parity.schema.json's
//! canonical matrix (1:1 with schema/commands.schema.json) is driven
//! through a live daemon and must route to its handler — the only failure
//! modes allowed are semantic (validation / not-found of a fixture /
//! parity), never "unknown method".

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, enroll, DaemonProcess, TestClient, TestCtx};
use serde_json::{json, Value};
use std::time::Duration;

/// The canonical 21-action list, read from the published matrix.
fn canonical_actions() -> Vec<(String, String)> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/parity.schema.json");
    let raw = std::fs::read_to_string(path).expect("read parity.schema.json");
    let value: Value = serde_json::from_str(&raw).expect("parse parity schema");
    let entries = value["$defs"]["SeedActionParityMatrix"]["default"]["entries"]
        .as_array()
        .expect("seed matrix default")
        .clone();
    entries
        .iter()
        .map(|e| {
            (
                e["action"].as_str().expect("action").to_string(),
                e["classification"]
                    .as_str()
                    .expect("classification")
                    .to_string(),
            )
        })
        .collect()
}

fn ready() -> (TestCtx, DaemonProcess, String) {
    let ctx = TestCtx::spawn();
    let proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            return (ctx, proc, d.endpoint);
        }
        if !proc.is_alive() {
            panic!("daemon died during startup: {}", proc.stderr_text());
        }
        if std::time::Instant::now() > deadline {
            panic!("no descriptor within 20s");
        }
    }
}

#[test]
fn every_schema_action_routes_to_a_handler() {
    let actions = canonical_actions();
    assert_eq!(actions.len(), 21, "canonical v1 matrix size");

    let (_ctx, mut proc, endpoint) = ready();
    let (mut agent, agent_cred) = connected_client(&endpoint, "cli").expect("agent client");
    let (mut adapter, adapter_cred) = connected_client(&endpoint, "dsh").expect("adapter client");
    let mut admin_client = TestClient::connect(&endpoint).unwrap();
    let (_, admin_cred) = enroll(&mut admin_client, "cli", json!({})).unwrap();
    // Promote ONE principal to administrator out-of-band.
    let principal = admin_cred["principalId"].as_str().unwrap().to_string();
    std::fs::write(
        _ctx.runtime_dir.join("admin-grants.json"),
        json!({ "principalIds": [principal] }).to_string(),
    )
    .expect("admin grants");

    // Fixture hierarchy: epic → story → ticket.
    let epic = agent
        .call(
            "node/create",
            authed(
                json!({ "type": "epic", "title": "coverage epic" }),
                &agent_cred,
            ),
        )
        .expect("fixture epic");
    let story = agent
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "coverage story", "parentId": epic["node"]["nodeId"] }),
                &agent_cred,
            ),
        )
        .expect("fixture story");
    let created = agent
        .call(
            "node/create",
            authed(
                json!({ "type": "ticket", "title": "coverage fixture", "parentId": story["node"]["nodeId"] }),
                &agent_cred,
            ),
        )
        .expect("fixture create");
    let node_id = created["node"]["nodeId"].as_str().unwrap().to_string();
    let story_id = story["node"]["nodeId"].as_str().unwrap().to_string();

    // Second story as a valid move TARGET.
    let other_story = agent
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "coverage story two", "parentId": epic["node"]["nodeId"] }),
                &agent_cred,
            ),
        )
        .expect("fixture second story");
    let move_target = other_story["node"]["nodeId"].as_str().unwrap().to_string();

    // A run over the fixture for run-plane coverage.
    let run = agent
        .call(
            "run/create",
            authed(json!({ "nodeIds": [&node_id], "config": {} }), &agent_cred),
        )
        .expect("run create");
    let run_id = run["run"]["runId"].as_str().unwrap().to_string();

    // (method, classification, params, which client)
    let cases: Vec<(&str, &str, Value, &str)> = vec![
        (
            "node/create",
            "agent_available",
            json!({ "type": "ticket", "title": "coverage second", "parentId": story_id }),
            "agent",
        ),
        (
            "node/get",
            "agent_available",
            json!({ "nodeId": node_id }),
            "agent",
        ),
        ("node/list", "agent_available", json!({}), "agent"),
        ("node/tree", "agent_available", json!({}), "agent"),
        (
            "node/search",
            "agent_available",
            json!({ "query": "coverage" }),
            "agent",
        ),
        (
            "node/update",
            "agent_available",
            json!({ "nodeId": node_id, "changes": { "priority": 2 } }),
            "agent",
        ),
        (
            "node/move",
            "agent_available",
            json!({ "nodeId": node_id, "newParentId": move_target }),
            "agent",
        ),
        (
            "node/execute",
            "adapter_only",
            json!({ "nodeId": node_id, "payload": { "kind": "noop" } }),
            "adapter",
        ),
        ("home/reindex", "human_administrative", json!({}), "admin"),
        (
            "run/create",
            "agent_available",
            json!({ "nodeIds": [node_id], "config": {} }),
            "agent",
        ),
        (
            "run/get",
            "agent_available",
            json!({ "runId": run_id }),
            "agent",
        ),
        ("run/list", "agent_available", json!({}), "agent"),
        (
            "run/control",
            "agent_available",
            json!({ "runId": run_id, "action": "start" }),
            "agent",
        ),
        (
            "run/claim",
            "agent_available",
            json!({ "runId": run_id }),
            "agent",
        ),
        (
            "run/report",
            "agent_available",
            json!({ "runId": run_id, "nodeId": node_id, "outcome": "done" }),
            "agent",
        ),
        (
            "events/resume",
            "agent_available",
            json!({ "cursor": 0 }),
            "agent",
        ),
        (
            "ui/filters-get",
            "adapter_only",
            json!({ "key": "tree" }),
            "adapter",
        ),
        (
            "ui/filters-set",
            "adapter_only",
            json!({ "key": "tree", "filters": { "q": "x" } }),
            "adapter",
        ),
        (
            "ui/recent-get",
            "adapter_only",
            json!({ "key": "recent" }),
            "adapter",
        ),
        (
            "ui/recent-set",
            "adapter_only",
            json!({ "key": "recent", "refs": [{ "homeId": "h", "nodeId": "n" }] }),
            "adapter",
        ),
        // Archive LAST: it flips the fixture read-only, so every other
        // node/run case must already have run against live state.
        (
            "node/archive",
            "agent_available",
            json!({ "nodeId": node_id }),
            "agent",
        ),
    ];

    // The case list itself must equal the canonical matrix exactly.
    let case_names: Vec<&str> = cases.iter().map(|(m, ..)| *m).collect();
    let canonical_names: Vec<String> = actions.iter().map(|(a, _)| a.clone()).collect();
    assert_eq!(
        case_names.len(),
        canonical_names.len(),
        "case list covers every canonical action"
    );
    for name in &canonical_names {
        assert!(
            case_names.contains(&name.as_str()),
            "missing coverage case: {name}"
        );
    }

    let mut report = String::from("| method | classification | routed |\n|---|---|---|\n");
    for (method, classification, params, who) in cases {
        let params = match who {
            "agent" => authed(params, &agent_cred),
            "adapter" => authed(params, &adapter_cred),
            _ => authed(params, &admin_cred),
        };
        let outcome = match who {
            "agent" => agent.call(method, params),
            "adapter" => adapter.call(method, params),
            _ => admin_client.call(method, params),
        };
        match outcome {
            Ok(_) => report.push_str(&format!("| {method} | {classification} | ok |\n")),
            Err(common::RpcError::Problem { code, details }) => {
                // Semantic refusals are fine; unknown-method routing is NOT.
                let is_unknown_method = code == "NOT_FOUND" && details["kind"] == "method";
                assert!(!is_unknown_method, "{method} did not route to a handler");
                report.push_str(&format!(
                    "| {method} | {classification} | semantic ({code}) |\n"
                ));
            }
            Err(other) => panic!("{method}: unexpected transport error {other:?}"),
        }
    }
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/u5a-coverage.md"),
        &report,
    )
    .ok(); // evidence artifact; best-effort

    proc.kill();
}
