//! `omt mcp` (U14/R18, KD5): MCP server over stdio exposing the
//! agent-available action subset to external harnesses. One MCP process
//! connects to the RUNNING daemon (installed CLI → shared runtime, never
//! an embedded copy) and enrolls as kind:"mcp" with a deliberately
//! RESTRICTED credential: operations exclude the home and ui families
//! (R7 — no home administration, no adapter UI bags through MCP).
//!
//! Secrets hygiene (KTD8-adjacent): the credential token lives only in
//! process memory and on the wire to the local daemon — never in argv,
//! never on stderr, never in logs. stderr carries lifecycle log lines
//! only; every protocol error maps onto an MCP structured error.
//!
//! Transport: JSON-RPC 2.0 over stdio with `Content-Length` framed
//! messages (the MCP stdio transport contract). Tools are generated from
//! the SAME parity matrix the coverage suite enforces (agent_available ∩
//! non-human_administrative), so tools/list can never drift from the
//! protocol contract.

use std::io::{BufRead, BufReader, Write};

use omt_domain::error::Problem;

use crate::cli::GlobalArgs;

/// Agent-available, MCP-exposed actions. MUST stay in lockstep with
/// schema/parity.schema.json (agent_available ∩ non-human_administrative)
/// — mcp_spec.rs asserts exact equality against the matrix file itself.
pub const MCP_TOOLS: &[&str] = &[
    "node/create",
    "node/get",
    "node/list",
    "node/tree",
    "node/search",
    "node/update",
    "node/move",
    "node/archive",
    "run/create",
    "run/get",
    "run/list",
    "run/control",
    "run/claim",
    "run/report",
    "run/add-members",
    "run/nudge-record",
    "run/interrupt",
    "events/resume",
    // home/declare is agent_available in the matrix but excluded here: the
    // mcp credential lacks the home operation family (R7), so the tool
    // would be dead weight — and omitting it keeps home administration
    // visibly out of the agent surface.
];

/// MCP tool name mapping: wire names replace '/' with '__' (MCP tool
/// names match [a-zA-Z0-9_-] in common client implementations).
fn wire_name(action: &str) -> String {
    action.replace('/', "__")
}

fn action_of(wire: &str) -> Option<&'static str> {
    MCP_TOOLS.iter().copied().find(|a| wire_name(a) == wire)
}

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the stdio MCP server loop until stdin closes.
pub fn serve(globals: &GlobalArgs) -> Result<i32, Problem> {
    let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());
    // Bounded readiness wait: harnesses legitimately launch the mcp
    // process immediately after daemon-start, while the winner may still
    // be inside its boot window (descriptor written, listener not yet
    // accepting). Retry the live probe for up to 5s before refusing.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let descriptor = loop {
        let candidate = omt_client::read_descriptor(&runtime_dir).filter(|d| {
            crate::descriptor::pid_live(d.pid) && omt_client::endpoint_live(&d.endpoint)
        });
        if let Some(descriptor) = candidate {
            break descriptor;
        }
        if std::time::Instant::now() >= deadline {
            return Err(Problem::new(
                "NOT_FOUND",
                format!(
                    "no live daemon under {}; start one with `omt daemon-start`",
                    runtime_dir.display()
                ),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    // Restricted enrollment: node/run/events only — home administration
    // and adapter UI bags stay outside the agent surface (R7).
    let options = omt_client::EnrollOptions {
        kind: "mcp".into(),
        name: Some("omt-mcp".into()),
        actor_namespace: Some(format!("mcp:{}", std::process::id())),
        operations: None,
        credential_path: None,
    };
    let mut enrollment = omt_client::Client::connect_and_enroll_with_operations(
        &descriptor,
        &options,
        &["node", "run", "events"],
    )?;

    log_line("mcp server ready (scoped grant: node/run/events)");

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    while let Some(message) = read_frame(&mut reader)? {
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&message) else {
            continue;
        };
        let method = request["method"].as_str().unwrap_or_default();
        let id = request["id"].clone();

        // Notifications (no id) need no response.
        let is_notification = id.is_null();

        let response = match method {
            "initialize" => Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "omt-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                },
            })),
            "notifications/initialized" | "notifications/cancelled" => None,
            "ping" => Some(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
            "tools/list" => Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_descriptors() },
            })),
            "tools/call" => {
                let name = request["params"]["name"].as_str().unwrap_or_default();
                let arguments = request["params"]["arguments"].clone();
                Some(match action_of(name) {
                    None => error_reply(&id, -32602, &format!("unknown tool: {name}")),
                    Some(action) => call_tool(&mut enrollment.client, action, arguments, &id),
                })
            }
            _ => {
                if is_notification {
                    None
                } else {
                    Some(error_reply(
                        &id,
                        -32601,
                        &format!("method not found: {method}"),
                    ))
                }
            }
        };

        if let Some(response) = response {
            if !is_notification {
                write_frame(&mut writer, &response.to_string())
                    .map_err(|err| Problem::new("IO", format!("stdout write: {err}")))?;
            }
        }
    }
    log_line("mcp server exiting (stdin closed)");
    Ok(0)
}

/// tools/list descriptors: one MCP tool per exposed action, named with
/// the '__' mapping, carrying a permissive object input schema (params
/// pass through to the daemon verbatim — the daemon's own schema
/// validation remains the authority).
fn tool_descriptors() -> Vec<serde_json::Value> {
    MCP_TOOLS
        .iter()
        .map(|action| {
            serde_json::json!({
                "name": wire_name(action),
                "description": format!("OMT daemon action {action} (agent-available subset, parity-matrix governed)"),
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": true,
                },
            })
        })
        .collect()
}

/// One tool invocation → one daemon call; the daemon Problem maps onto an
/// MCP structured error (isError content + code in the message), never a
/// transport-level failure.
fn call_tool(
    client: &mut omt_client::Client,
    action: &str,
    arguments: serde_json::Value,
    id: &serde_json::Value,
) -> serde_json::Value {
    let params = if arguments.is_object() {
        arguments
    } else {
        serde_json::json!({})
    };
    match client.call(action, params) {
        Ok(result) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_default() }],
                "isError": false,
            },
        }),
        Err(problem) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": format!("{}: {}", problem.code, problem.message),
                }],
                "isError": true,
            },
        }),
    }
}

fn error_reply(id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// stderr lifecycle line — secrets hygiene: never log anything beyond
/// static text (no params, no tokens, no paths with user data).
fn log_line(text: &str) {
    eprintln!("[omt-mcp] {text}");
}

/// Read one Content-Length framed message; None at clean EOF.
fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, Problem> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut header = String::new();
        let read = reader
            .read_line(&mut header)
            .map_err(|err| Problem::new("IO", format!("stdin read: {err}")))?;
        if read == 0 {
            return Ok(None); // EOF
        }
        let trimmed = header.trim();
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue; // tolerate stray blank lines between frames
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
        // Content-Type and unknown headers are ignored per the spec.
    }
    let length = content_length
        .ok_or_else(|| Problem::new("INVALID_INPUT", "frame without Content-Length"))?;
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|err| Problem::new("IO", format!("stdin body read: {err}")))?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|err| Problem::new("INVALID_INPUT", format!("frame not utf-8: {err}")))
}

fn write_frame(writer: &mut impl Write, body: &str) -> std::io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()
}
