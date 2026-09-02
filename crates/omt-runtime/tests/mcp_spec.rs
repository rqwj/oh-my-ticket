//! U14 (R18/R7/KD5): `omt mcp` stdio server against a REAL daemon.
//!
//! Coverage:
//! - tools/list equals the parity matrix's agent_available ∩
//!   non-human_administrative set MINUS home/declare (R7 home-family
//!   exclusion), asserted against the schema file itself;
//! - full CRUD + run claim/report flows through MCP tools;
//! - negative: a tool call outside the credential's operations (no
//!   home-family grant) surfaces a structured isError refusal;
//! - stderr carries no credential/token material (regex scan);
//! - features.homeDeclare does not add a declare tool to MCP.

#![allow(dead_code)]

mod common;

use common::{DaemonProcess, TestCtx};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};

struct McpProcess {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    stderr_path: std::path::PathBuf,
}

impl McpProcess {
    fn spawn(ctx: &TestCtx) -> McpProcess {
        let stderr_path = ctx.dir.path().join("mcp.stderr");
        let stderr = std::fs::File::create(&stderr_path).expect("create mcp stderr");
        let mut child = Command::new(std::path::PathBuf::from(env!("CARGO_BIN_EXE_omt")))
            .arg("--runtime-dir")
            .arg(ctx.runtime_dir_str())
            .arg("mcp")
            .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn omt mcp");
        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = BufReader::new(child.stdout.take().expect("mcp stdout"));
        McpProcess {
            child,
            stdin,
            stdout,
            next_id: 0,
            stderr_path,
        }
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let body =
            json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params })
                .to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).expect("write frame");
        self.stdin.flush().expect("flush frame");
        // Read frames until the response with OUR id (skip notifications).
        loop {
            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                let read = self.stdout.read_line(&mut header).expect("read header");
                assert!(read > 0, "mcp stdout closed while waiting for {method}");
                let trimmed = header.trim();
                if trimmed.is_empty() {
                    break;
                }
                if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                    content_length = value.trim().parse().expect("content-length");
                }
            }
            let mut body = vec![0u8; content_length];
            self.stdout.read_exact(&mut body).expect("read body");
            let value: serde_json::Value = serde_json::from_slice(&body).expect("frame json");
            if value["id"].as_u64() == Some(self.next_id) {
                return value;
            }
        }
    }

    fn notify(&mut self, method: &str) {
        let body = json!({ "jsonrpc": "2.0", "method": method }).to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).expect("write notify");
        self.stdin.flush().expect("flush notify");
    }

    fn shutdown(mut self) -> String {
        drop(self.stdin); // close stdin → server exits its loop
        let status = self.child.wait().expect("wait mcp");
        assert!(status.success(), "mcp exits 0 on stdin close: {status}");
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

fn wait_endpoint(ctx: &TestCtx) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            return d.endpoint;
        }
        assert!(std::time::Instant::now() <= deadline, "no descriptor");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn initialize(mcp: &mut McpProcess) {
    let response = mcp.request(
        "initialize",
        json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "mcp-spec", "version": "0" } }),
    );
    assert_eq!(
        response
            .pointer("/result/protocolVersion")
            .and_then(|v| v.as_str()),
        Some("2024-11-05"),
        "{response}"
    );
    assert_eq!(
        response
            .pointer("/result/serverInfo/version")
            .and_then(|v| v.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
        "server reports workspace product version: {response}"
    );
    mcp.notify("notifications/initialized");
}

fn tool_names(mcp: &mut McpProcess) -> Vec<String> {
    let response = mcp.request("tools/list", json!({}));
    response
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect()
}

fn call_tool(mcp: &mut McpProcess, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    mcp.request(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

fn tool_payload(response: &serde_json::Value) -> serde_json::Value {
    let text = response
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    serde_json::from_str(text).unwrap_or(json!({ "raw": text }))
}

/// tools/list mirrors the parity matrix (agent_available ∩
/// non-human_administrative) minus the home-family actions (R7).
#[test]
fn mcp_tools_list_matches_parity_matrix_minus_home_family() {
    let ctx = TestCtx::spawn();
    let mut daemon = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);

    let names = tool_names(&mut mcp);

    // Ground truth: the schema file itself (same source coverage.rs reads).
    let matrix_text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schema/parity.schema.json"
    ))
    .expect("read parity matrix");
    let matrix: serde_json::Value = serde_json::from_str(&matrix_text).expect("parse matrix");
    let mut expected: Vec<String> = Vec::new();
    fn walk(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let (Some(action), Some(classification)) = (
                    map.get("action").and_then(|v| v.as_str()),
                    map.get("classification").and_then(|v| v.as_str()),
                ) {
                    if classification == "agent_available" {
                        out.push(action.to_string());
                    }
                }
                for v in map.values() {
                    walk(v, out);
                }
            }
            serde_json::Value::Array(items) => {
                for v in items {
                    walk(v, out);
                }
            }
            _ => {}
        }
    }
    walk(&matrix, &mut expected);
    // R7: home-family actions are agent-available in the matrix but the
    // MCP credential excludes the home family — the tools omit them.
    expected.retain(|action| !action.starts_with("home/"));

    let mut expected_wire: Vec<String> = expected.iter().map(|a| a.replace('/', "__")).collect();
    expected_wire.sort();
    let mut actual = names.clone();
    actual.sort();
    assert_eq!(
        actual, expected_wire,
        "tools/list == matrix agent_available − home family"
    );
    assert!(
        !names.iter().any(|n| n.contains("declare")),
        "no declare tool despite features.homeDeclare: {names:?}"
    );

    let stderr = mcp.shutdown();
    daemon.kill();
    assert!(!stderr.contains("token"), "stderr token-free: {stderr}");
}

/// Full CRUD + run claim/report through MCP tools, then clean cancel.
#[test]
fn mcp_crud_and_run_flows_work_end_to_end() {
    let ctx = TestCtx::spawn();
    let mut daemon = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);

    // Discover the home id via a direct client (MCP tools need homeId).
    let endpoint = wait_endpoint(&ctx);
    let (mut client, _cred) = common::connected_client(&endpoint, "cli").expect("helper client");
    let handshake = client
        .call(
            "handshake/request",
            json!({
                "protocolVersion": "1.0",
                "client": { "kind": "cli", "name": "mcp-spec-helper" },
            }),
        )
        .expect("helper handshake");
    let home_id = handshake["homes"][0]["homeId"]
        .as_str()
        .expect("home id")
        .to_string();

    // create epic → story → ticket through MCP.
    let response = call_tool(
        &mut mcp,
        "node__create",
        json!({ "homeId": home_id, "type": "epic", "title": "MCP 通道" }),
    );
    assert_eq!(
        response
            .pointer("/result/isError")
            .and_then(|v| v.as_bool()),
        Some(false),
        "{response}"
    );
    let epic = tool_payload(&response);
    let epic_id = epic
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .expect("epic id")
        .to_string();
    assert!(epic_id.starts_with("EPIC-"), "{epic}");

    let response = call_tool(
        &mut mcp,
        "node__create",
        json!({ "homeId": home_id, "type": "story", "title": "子故事", "parentId": epic_id }),
    );
    let story = tool_payload(&response);
    let story_id = story
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .expect("story id")
        .to_string();

    let response = call_tool(
        &mut mcp,
        "node__create",
        json!({ "homeId": home_id, "type": "ticket", "title": "执行项", "parentId": story_id }),
    );
    let ticket = tool_payload(&response);
    let ticket_id = ticket
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .expect("ticket id")
        .to_string();

    // get/list/tree/search round out the read surface.
    let response = call_tool(
        &mut mcp,
        "node__get",
        json!({ "homeId": home_id, "nodeId": ticket_id }),
    );
    assert_eq!(
        tool_payload(&response)
            .pointer("/node/title")
            .and_then(|v| v.as_str()),
        Some("执行项")
    );
    let response = call_tool(
        &mut mcp,
        "node__search",
        json!({ "homeId": home_id, "query": "执行项" }),
    );
    assert!(
        tool_payload(&response).to_string().contains(&ticket_id),
        "search finds the ticket"
    );

    // update through MCP.
    let response = call_tool(
        &mut mcp,
        "node__update",
        json!({ "homeId": home_id, "nodeId": ticket_id, "changes": { "status": "in_progress" } }),
    );
    assert_eq!(
        response
            .pointer("/result/isError")
            .and_then(|v| v.as_bool()),
        Some(false),
        "{response}"
    );

    // run create → claim → report → control through MCP.
    let response = call_tool(
        &mut mcp,
        "run__create",
        json!({ "homeId": home_id, "title": "mcp run", "nodeIds": [ticket_id] }),
    );
    let run = tool_payload(&response);
    let run_id = run
        .pointer("/run/runId")
        .and_then(|v| v.as_str())
        .expect("run id")
        .to_string();

    // Runs start pending; claim requires running (status gate).
    let response = call_tool(
        &mut mcp,
        "run__control",
        json!({ "homeId": home_id, "runId": run_id, "action": "start" }),
    );
    assert_eq!(
        response
            .pointer("/result/isError")
            .and_then(|v| v.as_bool()),
        Some(false),
        "start: {response}"
    );

    let response = call_tool(
        &mut mcp,
        "run__claim",
        json!({ "homeId": home_id, "runId": run_id }),
    );
    let claim = tool_payload(&response);
    assert_eq!(
        response
            .pointer("/result/isError")
            .and_then(|v| v.as_bool()),
        Some(false),
        "claim: {response}"
    );
    let claim_text = claim.to_string();
    assert!(
        claim_text.contains(&ticket_id),
        "claim returns the item: {claim_text}"
    );
    let lease_token = claim
        .pointer("/lease/token")
        .and_then(|v| v.as_str())
        .expect("lease token")
        .to_string();

    let response = call_tool(
        &mut mcp,
        "run__report",
        json!({
            "homeId": home_id, "runId": run_id, "nodeId": ticket_id, "outcome": "done", "note": "via mcp",
            "leaseToken": lease_token,
        }),
    );
    assert_eq!(
        response
            .pointer("/result/isError")
            .and_then(|v| v.as_bool()),
        Some(false),
        "report: {response}"
    );

    let response = call_tool(&mut mcp, "run__list", json!({ "homeId": home_id }));
    assert!(
        tool_payload(&response).to_string().contains(&run_id),
        "run/list sees the run"
    );

    let response = call_tool(
        &mut mcp,
        "run__control",
        json!({ "homeId": home_id, "runId": run_id, "action": "cancel" }),
    );
    assert_eq!(
        response
            .pointer("/result/isError")
            .and_then(|v| v.as_bool()),
        Some(false),
        "cancel: {response}"
    );

    let stderr = mcp.shutdown();
    daemon.kill();
    let lowered = stderr.to_lowercase();
    assert!(
        !lowered.contains("token") && !lowered.contains("credential"),
        "stderr carries no credential material: {stderr}"
    );
}

/// Negative: the restricted credential refuses home-family actions and
/// ui bags structurally; unknown tools refuse at the MCP layer.
#[test]
fn mcp_restricted_credential_refuses_out_of_scope_actions() {
    let ctx = TestCtx::spawn();
    let mut daemon = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);

    // No home-family tool exists at all — unknown tool refusal.
    let response = call_tool(&mut mcp, "home__declare", json!({ "path": "/tmp/nowhere" }));
    assert!(
        response.get("error").is_some(),
        "unknown tool refused at MCP layer: {response}"
    );

    // ui bags are also outside the mcp credential — but ui tools are not
    // exposed either, so the MCP layer refuses first (unknown tool).
    let response = call_tool(
        &mut mcp,
        "ui__filters_get",
        json!({ "homeId": "h", "key": "k" }),
    );
    assert!(response.get("error").is_some(), "{response}");

    // Direct daemon-level proof of the restricted grant: the mcp
    // credential's operations exclude home/ui — verify via a second
    // handshake with the same requested scope shape and an attempted
    // home-scoped call that the server FORBIDs with the family reason.
    let endpoint = wait_endpoint(&ctx);
    let mut client = common::TestClient::connect(&endpoint).expect("connect");
    let handshake = client
        .call(
            "handshake/request",
            json!({
                "protocolVersion": "1.0",
                "client": { "kind": "mcp", "name": "mcp-spec-negative" },
                "requestedScopes": { "operations": ["node", "run", "events"] },
            }),
        )
        .expect("scoped handshake");
    let token = handshake["credential"]["token"]
        .as_str()
        .expect("token")
        .to_string();
    assert_eq!(
        handshake["credential"]["operations"]
            .as_array()
            .expect("operations")
            .len(),
        3,
        "restricted operations honored: {handshake}"
    );
    let denial = client
        .call(
            "home/declare",
            json!({ "path": "/tmp/nowhere", "credential": { "token": token } }),
        )
        .expect_err("home family must be forbidden");
    let denial_text = format!("{denial:?}");
    assert!(
        denial_text.contains("FORBIDDEN"),
        "home family forbidden: {denial_text}"
    );
    assert!(
        denial_text.contains("operation-not-granted"),
        "family reason, NOT a rehandshake hint (KTD3): {denial_text}"
    );
    assert!(
        !denial_text.contains("requiresRehandshake"),
        "op-family FORBIDDEN carries no hint: {denial_text}"
    );

    let _stderr = mcp.shutdown();
    daemon.kill();
}
