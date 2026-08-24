//! TICKET-0130 item 1: claim-context ancestor budget must truncate on UTF-8
//! char boundaries. A CJK ancestor body cut mid-character panicked the home
//! thread (`parsed.body[..take]` byte slicing).

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, DaemonProcess, TestCtx};
use serde_json::json;
use std::time::Duration;

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
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Ancestor story body is pure 3-byte chars ("用"×6000 = 18_000 B). The
/// child body is sized so remaining budget = 16_384 − 3 = 16_381, and
/// 16_381 % 3 == 1: a naive byte slice cuts INSIDE a multi-byte char.
#[test]
fn cjk_ancestor_body_truncates_on_char_boundary_without_panicking() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    let epic = client
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "cjk epic" }), &cred),
        )
        .expect("epic");
    let big_body = "用".repeat(6000);
    let story = client
        .call(
            "node/create",
            authed(
                json!({
                    "type": "story",
                    "title": "cjk story",
                    "parentId": epic["node"]["nodeId"],
                    "body": big_body,
                }),
                &cred,
            ),
        )
        .expect("story");
    let ticket = client
        .call(
            "node/create",
            authed(
                json!({
                    "type": "ticket",
                    "title": "cjk item",
                    "parentId": story["node"]["nodeId"],
                    "body": "abc",
                }),
                &cred,
            ),
        )
        .expect("ticket");
    let ticket_id = ticket["node"]["nodeId"].as_str().unwrap().to_string();

    let run = client
        .call(
            "run/create",
            authed(json!({ "nodeIds": [&ticket_id], "config": {} }), &cred),
        )
        .expect("run create");
    let run_id = run["run"]["runId"].as_str().unwrap().to_string();
    client
        .call(
            "run/control",
            authed(json!({ "runId": run_id, "action": "start" }), &cred),
        )
        .expect("run start");

    let claim = client
        .call("run/claim", authed(json!({ "runId": run_id }), &cred))
        .expect("claim must not panic on CJK ancestor truncation");

    // The home thread survived: daemon still answers.
    assert!(
        proc.is_alive(),
        "daemon died: {}",
        proc.stderr_text()
    );
    let context = &claim["context"];
    let ancestors = context["ancestors"]
        .as_array()
        .expect("ancestors array");
    let entry = ancestors
        .iter()
        .find(|a| a["truncated"] == json!(true))
        .expect("the oversized CJK ancestor is present and truncated");
    let body = entry["body"].as_str().expect("ancestor body string");
    // Included bytes must sit on a char boundary: re-encode round-trips.
    assert_eq!(body.len(), entry["includedBytes"].as_u64().unwrap() as usize);
    assert!(std::str::from_utf8(body.as_bytes()).is_ok());
    assert!(entry["includedBytes"].as_u64().unwrap() <= 16 * 1024);

    proc.kill();
}
