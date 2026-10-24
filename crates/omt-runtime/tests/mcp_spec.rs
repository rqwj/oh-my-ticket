//! U14 (R18/R7/KD5): `omt mcp` stdio server against a REAL daemon.
//!
//! Coverage:
//! - tools/list equals the parity matrix's agent_available ∩
//!   non-human_administrative set MINUS home/declare (R7 home-family
//!   exclusion) UNION the 8 local tools (A6/Q-5: a top-level session
//!   always lists them; policy is checked at call time), asserted against
//!   the schema file itself;
//! - full CRUD + run claim/report flows through MCP tools;
//! - negative: a tool call outside the credential's operations (no
//!   home-family grant) surfaces a structured isError refusal;
//! - stderr carries no credential/token material (regex scan);
//! - features.homeDeclare does not add a declare tool to MCP;
//! - Q-5 / Q-12 / Q-3: injected bridges (`--actor exec:*`, `OMT_EXEC_ID`)
//!   get no local tools, `exec__start` is policy-checked at call time, and
//!   MCP cannot declare an unserved home through a `home` path argument.

#![allow(dead_code)]

mod common;

use common::{DaemonProcess, TestCtx};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

struct McpProcess {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    stderr_path: std::path::PathBuf,
    /// §49 worker-thread 验证：request() 跳过的行（乱序先到的其他响应）
    /// 记在这里供测试断言本地调用的响应确实另行写出。
    skipped: Vec<String>,
}

impl McpProcess {
    fn spawn(ctx: &TestCtx) -> McpProcess {
        McpProcess::spawn_with(ctx, &[], &[])
    }

    /// A6：带全局参数（插在 `mcp` 之前，如 `--actor exec:<execId>`）与
    /// 追加环境变量（如 Q-12 的 `OMT_EXEC_ID`）的 spawn 变体。
    fn spawn_with(ctx: &TestCtx, extra_args: &[&str], envs: &[(&str, &str)]) -> McpProcess {
        let stderr_path = ctx.dir.path().join("mcp.stderr");
        let stderr = std::fs::File::create(&stderr_path).expect("create mcp stderr");
        let mut command = Command::new(std::path::PathBuf::from(env!("CARGO_BIN_EXE_omt")));
        command
            .arg("--runtime-dir")
            .arg(ctx.runtime_dir_str())
            .args(extra_args)
            .arg("mcp")
            .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str());
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command
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
            skipped: Vec::new(),
        }
    }

    /// F-ACC-9 harness rule: the test client mirrors the **MCP stdio spec**
    /// (newline-delimited JSON), never the server's implementation — the
    /// previous Content-Length harness silently agreed with a wrong server.
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let body =
            json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params })
                .to_string();
        writeln!(self.stdin, "{body}").expect("write line");
        self.stdin.flush().expect("flush line");
        // Read lines until the response with OUR id (skip notifications).
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read line");
            assert!(read > 0, "mcp stdout closed while waiting for {method}");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(trimmed)
                .unwrap_or_else(|err| panic!("line is not JSON ({err}): {trimmed}"));
            if value["id"].as_u64() == Some(self.next_id) {
                return value;
            }
            self.skipped.push(trimmed.to_string());
        }
    }

    /// §49 worker-thread 验证：不等应答地发一条请求（id 手工指定），让测试
    /// 在本地调用阻塞期间继续与 server 交互。
    fn send_raw(&mut self, body: &str) {
        writeln!(self.stdin, "{body}").expect("write raw line");
        self.stdin.flush().expect("flush raw line");
    }

    fn notify(&mut self, method: &str) {
        let body = json!({ "jsonrpc": "2.0", "method": method }).to_string();
        writeln!(self.stdin, "{body}").expect("write notify");
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

    // A6 / Q-5：顶层会话始终列出全部 8 个本地工具（`omt mcp` 不声明
    // listChanged，按 policy 隐藏会让中途开启派发的主控看不到 exec__start）；
    // `exec__start` 的 policy 在调用时检查。
    const LOCAL: &[&str] = &[
        "harness__list",
        "harness__models",
        "exec__start",
        "exec__stop",
        "exec__status",
        "exec__list",
        "exec__tail",
        "exec__policy",
    ];
    let mut expected_wire: Vec<String> = expected.iter().map(|a| a.replace('/', "__")).collect();
    expected_wire.extend(LOCAL.iter().map(|s| s.to_string()));
    expected_wire.sort();
    let mut actual = names.clone();
    actual.sort();
    assert_eq!(
        actual, expected_wire,
        "tools/list == daemon 子集 ∪ 全部本地工具"
    );
    assert!(
        !names.iter().any(|n| n.contains("declare")),
        "no declare tool despite features.homeDeclare: {names:?}"
    );

    // F-ACC-10: inputSchema must be projected from commands.schema.json —
    // an empty schema once left the model unable to pass `runId`.
    let tools_resp = mcp.request("tools/list", json!({}));
    let tools_arr = tools_resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .expect("tools array");
    for tool in tools_arr {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        let props = tool["inputSchema"]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{} 无 properties: {tool}", tool["name"]));
        assert!(!props.is_empty(), "{} properties 为空", tool["name"]);
    }
    let claim = tools_arr
        .iter()
        .find(|t| t["name"] == "run__claim")
        .expect("claim tool");
    let required: Vec<&str> = claim["inputSchema"]["required"]
        .as_array()
        .expect("required")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        required.contains(&"homeId") && required.contains(&"runId"),
        "claim 声明 homeId+runId: {claim}"
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

/// Q-3 / R7：MCP 不能借 `home` 路径让 daemon 声明一个未被服务的 home。
/// §49 测试加固：只传 `home`（不带 homeId——旧测试两个都传，homeId 优先，
/// 去掉防护也能通过，守不住 Q-3），断言 INVALID_INPUT 且未声明。
#[test]
fn mcp_exec_start_cannot_declare_an_unserved_home() {
    let (ctx, homes) = TestCtx::spawn_with_homes("mcp-local-home", 2);
    // 先用 home-01 起一次 daemon 再停掉 → home-01 已初始化（有 omt.db）但随后不被服务
    {
        let mut first = common::DaemonProcess::spawn(&ctx, &["--home", homes[1].to_str().unwrap()]);
        common::wait_for_descriptor(&ctx.runtime_dir, std::time::Duration::from_secs(20))
            .expect("daemon");
        first.kill();
        assert!(common::wait_descriptor_gone(
            &ctx.runtime_dir,
            std::time::Duration::from_secs(20)
        ));
    }
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", homes[0].to_str().unwrap()]);
    wait_endpoint(&ctx);
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(
        ctx.runtime_dir.join("exec/policy.json"),
        r#"{"allowFromMcp": true}"#,
    )
    .unwrap();
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    // 只传 home 路径（已初始化、未被服务——若防护失效即可被声明的最大威胁
    // 形态），不带 homeId：MCP 只认 homeId，home 键必须被拒绝为 INVALID_INPUT
    let result = call_tool(
        &mut mcp,
        "exec__start",
        serde_json::json!({
            "home": homes[1].to_str().unwrap(),
            "harness": "pi", "nodeId": "TICKET-1"
        }),
    );
    assert_eq!(result["result"]["isError"], true, "{result}");
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("INVALID_INPUT"),
        "MCP 只认 homeId；只给 home 路径必须是 INVALID_INPUT（不是借它查找/声明）: {result}"
    );
    // home-01 仍未被服务（没有被声明）
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_omt"))
        .args(["--runtime-dir", ctx.runtime_dir_str(), "daemon-status"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("home-01"));
    // 直接看 daemon 的握手 home 清单：只有 home-00，没有 home-01
    let descriptor =
        common::wait_for_descriptor(&ctx.runtime_dir, std::time::Duration::from_secs(20))
            .expect("live daemon descriptor");
    let mut client = common::TestClient::connect(&descriptor.endpoint).expect("connect");
    let handshake = client
        .call(
            "handshake/request",
            json!({
                "protocolVersion": "1.0",
                "client": { "kind": "cli", "name": "mcp-spec-home-check" },
            }),
        )
        .expect("helper handshake");
    let served = handshake["homes"].as_array().cloned().unwrap_or_default();
    assert_eq!(served.len(), 1, "home-01 未被声明: {served:?}");
    assert!(
        !serde_json::to_string(&served).unwrap().contains("home-01"),
        "daemon 只服务 home-00: {served:?}"
    );
    mcp.shutdown();
}

/// Q-12：进程环境带 OMT_EXEC_ID 的 omt mcp（被派发 harness 自己起的）同样不暴露本地工具。
#[test]
fn mcp_with_nested_exec_marker_exposes_no_local_tools() {
    let ctx = TestCtx::spawn_named("mcp-local-nested");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    let mut mcp = McpProcess::spawn_with(&ctx, &[], &[("OMT_EXEC_ID", "exec-parent")]);
    initialize(&mut mcp);
    let names = tool_names(&mut mcp);
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("harness__") || n.starts_with("exec__")),
        "{names:?}"
    );
    mcp.shutdown();
}

#[test]
fn injected_harness_bridge_exposes_no_local_tools() {
    let ctx = TestCtx::spawn_named("mcp-local-bridge");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(
        ctx.runtime_dir.join("exec/policy.json"),
        r#"{"allowFromMcp": true}"#,
    )
    .unwrap();
    let mut mcp = McpProcess::spawn_with(&ctx, &["--actor", "exec:exec-1"], &[]);
    initialize(&mut mcp);
    let names = tool_names(&mut mcp);
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("harness__") || n.starts_with("exec__")),
        "{names:?}"
    );
    // 直接调用也拒绝（tools/list 之外的兜底）
    let reply = mcp.request(
        "tools/call",
        serde_json::json!({"name": "exec__list", "arguments": {}}),
    );
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
    mcp.shutdown();
}

#[test]
fn exec_start_via_mcp_is_policy_denied_by_default() {
    let ctx = TestCtx::spawn_named("mcp-local-deny");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    // 未列出但被直接调用：调用时再检一次 policy（Q-5）
    let result = call_tool(
        &mut mcp,
        "exec__start",
        serde_json::json!({
            "homeId": "h_x", "harness": "pi", "nodeId": "TICKET-1"
        }),
    );
    assert_eq!(result["result"]["isError"], true, "{result}");
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("POLICY_DENIED"),
        "{result}"
    );
    mcp.shutdown();
}

/// §49「MCP 的 homeId 也按路径匹配」：把**已服务 home 的路径**塞进 homeId
/// 不得被解析成功——「MCP 只认 homeId」，路径匹配是 CLI 专属。旧实现按
/// 规范路径匹配到已服务 home 后继续派发链（最后才因 ticket 不存在报
/// LAUNCH_FAILED），而不是 INVALID_INPUT。
#[test]
fn mcp_exec_start_rejects_a_path_shaped_home_id() {
    let ctx = TestCtx::spawn_named("mcp-local-homeid-path");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let endpoint = wait_endpoint(&ctx);
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(
        ctx.runtime_dir.join("exec/policy.json"),
        r#"{"allowFromMcp": true}"#,
    )
    .unwrap();
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    let result = call_tool(
        &mut mcp,
        "exec__start",
        serde_json::json!({
            "homeId": ctx.home_str(),
            "harness": "pi",
            "nodeId": "TICKET-1"
        }),
    );
    assert_eq!(result["result"]["isError"], true, "{result}");
    let text = result["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("INVALID_INPUT"),
        "路径形态的 homeId 必须是 INVALID_INPUT（field homeId），不是按路径解析: {result}"
    );
    assert!(text.contains("homeId"), "结构化 field：{text}");
    // daemon 侧该 home 仍被服务（没有触发任何按路径的副作用）
    let mut client = common::TestClient::connect(&endpoint).expect("connect");
    let handshake = client
        .call(
            "handshake/request",
            json!({
                "protocolVersion": "1.0",
                "client": { "kind": "cli", "name": "mcp-spec-path-homeid-check" },
            }),
        )
        .expect("helper handshake");
    assert_eq!(
        handshake["homes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .len(),
        1
    );
    mcp.shutdown();
}

/// §51 P3「MCP 对通知形式的 tools/call 也回包」：无 id 的 `tools/call`（本地
/// 工具路径，走 §49 工作线程）是 JSON-RPC 通知——不得有任何响应写出（旧实
/// 现无条件回包，输出 `{"id":null,…}` 违反协议）。手法：先发通知形式的
/// exec__list，再发一个哨兵 ping 请求；request() 按 id 配对，其余行全部落进
/// skipped——断言其中没有 id 为 null 的行（即通知没有产生响应）。
#[test]
fn mcp_notification_form_tools_call_gets_no_response() {
    let ctx = TestCtx::spawn_named("mcp-local-notify");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    // 通知形式：无 id 的 tools/call（本地工具 → 工作线程路径）
    mcp.send_raw(
        &json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": "exec__list", "arguments": {} }
        })
        .to_string(),
    );
    // 哨兵请求：等它应答的窗口里，通知的（非法）响应若存在必已到达
    let sentinel = mcp.request("ping", json!({}));
    assert!(sentinel.get("result").is_some(), "{sentinel}");
    // 工作线程的写出与哨兵应答无顺序保证——再泵几轮 ping 收集迟到行
    for _ in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = mcp.request("ping", json!({}));
    }
    assert!(
        mcp.skipped.iter().all(|line| !line.contains("\"id\":null")),
        "通知形式的 tools/call 不得产生响应（JSON-RPC 通知无响应）: {:?}",
        mcp.skipped
    );
    mcp.shutdown();
}

/// §51 P3「MCP exec__list 仍接受路径形状 homeId」：§50 的守卫只在 exec_start
/// ——只读动词同样守卫（MCP 只认 homeId，路径查找是 CLI 专属）。
#[test]
fn mcp_exec_list_rejects_a_path_shaped_home_id() {
    let ctx = TestCtx::spawn_named("mcp-local-list-homeid-path");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    let result = call_tool(
        &mut mcp,
        "exec__list",
        serde_json::json!({ "homeId": ctx.home_str() }),
    );
    assert_eq!(result["result"]["isError"], true, "{result}");
    let text = result["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("INVALID_INPUT"),
        "路径形态的 homeId 必须是 INVALID_INPUT（field homeId），不是按路径解析: {result}"
    );
    assert!(text.contains("homeId"), "结构化 field：{text}");
    mcp.shutdown();
}

/// §49「MCP stdio 循环单线程阻塞」：本地工具调用（exec__start 阻塞在跨进程
/// 派发锁上，获取超时 60s）期间 ping 必须保持应答——旧实现里 serve 循环被
/// 本地调用占死，客户端超时放弃后 shim 已拉起、调用方拿不到 execId。
/// 手法：测试进程持有 exec/launch 的 flock，先发 exec__start（id 1），再
/// ping——ping（id 2）必须在界内应答；随后放锁，exec__start 的响应（乱序、
/// 按 id 配对）另行到达。
#[test]
fn mcp_ping_stays_responsive_while_a_local_tool_call_blocks() {
    let ctx = TestCtx::spawn_named("mcp-local-worker");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let endpoint = wait_endpoint(&ctx);
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(
        ctx.runtime_dir.join("exec/policy.json"),
        r#"{"allowFromMcp": true}"#,
    )
    .unwrap();
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    // 已服务 home 的 homeId（daemon handshake）
    let mut helper = common::TestClient::connect(&endpoint).expect("connect");
    let handshake = helper
        .call(
            "handshake/request",
            json!({
                "protocolVersion": "1.0",
                "client": { "kind": "cli", "name": "mcp-spec-worker-home" },
            }),
        )
        .expect("helper handshake");
    let home_id = handshake["homes"][0]["homeId"]
        .as_str()
        .unwrap()
        .to_string();

    // 持有跨进程派发锁：exec__start 的工作线程阻塞在 lock_exclusive 上
    let runtime = std::path::PathBuf::from(ctx.runtime_dir_str());
    let launch_lock = omt_harness::atomic_json::lock_exclusive(
        &omt_exec::layout::launch_lock_file(&runtime),
        std::time::Duration::from_secs(30),
    )
    .expect("test holds the launch lock");

    // 先发 exec__start（不等应答），再发 ping——ping 必须在本地调用
    // 阻塞期间立刻应答（旧实现：ping 要等 exec__start 拿到锁并走完派发链）
    // §51 测试卫生：id 用 9001（不与 initialize 的 id 1 复用——复用会与
    // 乱序配对逻辑相混，响应归属不清）
    mcp.send_raw(
        &json!({
            "jsonrpc": "2.0", "id": 9001, "method": "tools/call",
            "params": { "name": "exec__start",
                        "arguments": { "homeId": home_id, "harness": "pi", "nodeId": "TICKET-1" } }
        })
        .to_string(),
    );
    let (ping_tx, ping_rx) = std::sync::mpsc::channel();
    let reader = {
        let mut mcp_for_ping = mcp; // 所有权转进线程，应答后经 join 取回
        std::thread::spawn(move || {
            let reply = mcp_for_ping.request("ping", json!({}));
            let _ = ping_tx.send(reply);
            mcp_for_ping
        })
    };
    let started = std::time::Instant::now();
    let ping = ping_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("ping 必须在本地工具调用阻塞期间保持应答（serve 循环不得被本地调用占死）");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "ping 应答耗时 {:?}",
        started.elapsed()
    );
    assert!(
        ping.get("result").is_some(),
        "ping 是 result 不是 error: {ping}"
    );

    // 放锁：exec__start 继续走链（ticket 不存在 → LAUNCH_FAILED），其响应
    // 乱序到达——被后续 request() 当作「跳过行」读到（id == 9001）
    drop(launch_lock);
    let mut mcp = reader.join().expect("ping reader thread");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !mcp.skipped.iter().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v["id"].as_u64())
            == Some(9001)
    }) {
        assert!(
            std::time::Instant::now() < deadline,
            "exec__start 的响应（id 9001）应在放锁后 20s 内到达: {:?}",
            mcp.skipped
        );
        let _ = mcp.request("ping", json!({})); // 泵一行，顺带收集跳过行
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let exec_reply = mcp
        .skipped
        .iter()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| v["id"].as_u64() == Some(9001))
        .expect("id 9001 的响应");
    assert_eq!(
        exec_reply["result"]["isError"], true,
        "ticket 不存在 → exec__start 报错（证明响应确实来自被阻塞的那个调用）: {exec_reply}"
    );
    mcp.shutdown();
}
