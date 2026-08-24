//! TICKET-0130 item 2: a terminal report on run A must passively advance the
//! matching item of every other ACTIVE run holding the same ticket
//! (observation-cross-run-broadcast corpus semantics), instead of leaving
//! sibling runs stale.

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

#[test]
fn terminal_report_broadcasts_to_sibling_active_run() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    let epic = client
        .call("node/create", authed(json!({ "type": "epic", "title": "obs epic" }), &cred))
        .expect("epic");
    let story = client
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "obs story", "parentId": epic["node"]["nodeId"] }),
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
                    "title": "shared ticket",
                    "parentId": story["node"]["nodeId"],
                }),
                &cred,
            ),
        )
        .expect("ticket");
    let ticket_id = ticket["node"]["nodeId"].as_str().unwrap().to_string();

    // Two active runs share the ticket; A executes it, B merely holds it.
    let run_a = client
        .call(
            "run/create",
            authed(json!({ "nodeIds": [&ticket_id], "config": {} }), &cred),
        )
        .expect("run A");
    let run_a_id = run_a["run"]["runId"].as_str().unwrap().to_string();
    let run_b = client
        .call(
            "run/create",
            authed(json!({ "nodeIds": [&ticket_id], "config": {} }), &cred),
        )
        .expect("run B");
    let run_b_id = run_b["run"]["runId"].as_str().unwrap().to_string();

    client
        .call(
            "run/control",
            authed(json!({ "runId": run_a_id, "action": "start" }), &cred),
        )
        .expect("start A");
    client
        .call(
            "run/control",
            authed(json!({ "runId": run_b_id, "action": "start" }), &cred),
        )
        .expect("start B");

    let claim = client
        .call("run/claim", authed(json!({ "runId": run_a_id }), &cred))
        .expect("claim in A");
    let lease_token = claim["lease"]["token"].as_str().unwrap().to_string();

    client
        .call(
            "run/report",
            authed(
                json!({
                    "runId": run_a_id,
                    "nodeId": ticket_id,
                    "outcome": "done",
                    "leaseToken": lease_token,
                }),
                &cred,
            ),
        )
        .expect("report done in A");

    // Sibling run B must have advanced its matching item (reported=true is
    // always trusted → straight to done for the same actor).
    let run_b_view = client
        .call("run/get", authed(json!({ "runId": run_b_id }), &cred))
        .expect("run B view");
    let b_state = run_b_view["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|it| it["nodeId"] == json!(ticket_id))
                .map(|it| it["state"].clone())
        })
        .expect("run B holds the shared ticket");
    assert_eq!(
        b_state,
        json!("done"),
        "terminal report must broadcast to sibling active runs"
    );

    // Run A derives to completed.
    let run_a_view = client
        .call("run/get", authed(json!({ "runId": run_a_id }), &cred))
        .expect("run A view");
    assert_eq!(run_a_view["run"]["status"], json!("completed"));

    proc.kill();
}
