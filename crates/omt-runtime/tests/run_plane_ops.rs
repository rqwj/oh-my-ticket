//! TICKET-0130 item 4: semantic pins for the run-plane additions —
//! run/add-members (pending-only gate), run/nudge-record (durable budget),
//! run/interrupt (two-pass demotion + terminal derivation).

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, DaemonProcess, RpcError};
use serde_json::json;
use std::time::Duration;

fn problem_of(err: &RpcError) -> String {
    match err {
        RpcError::Problem { code, .. } => code.clone(),
        other => panic!("expected a Problem, got {other:?}"),
    }
}

fn ready() -> (common::TestCtx, DaemonProcess, String) {
    let ctx = common::TestCtx::spawn();
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
fn add_members_nudge_and_interrupt_semantics() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    let epic = client
        .call("node/create", authed(json!({ "type": "epic", "title": "ops epic" }), &cred))
        .expect("epic");
    let story = client
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "ops story", "parentId": epic["node"]["nodeId"] }),
                &cred,
            ),
        )
        .expect("story");
    let parent_id = story["node"]["nodeId"].as_str().unwrap().to_string();
    let mk_ticket = |title: &str| json!({ "type": "ticket", "title": title, "parentId": parent_id });
    let t1 = client
        .call("node/create", authed(mk_ticket("ops one").clone(), &cred))
        .expect("t1");
    let t2 = client
        .call("node/create", authed(mk_ticket("ops two"), &cred))
        .expect("t2");
    let t1_id = t1["node"]["nodeId"].as_str().unwrap().to_string();
    let t2_id = t2["node"]["nodeId"].as_str().unwrap().to_string();

    // ── add-members: pending-only gate ──────────────────────────────────
    let run = client
        .call(
            "run/create",
            authed(json!({ "nodeIds": [&t1_id], "config": {} }), &cred),
        )
        .expect("run");
    let run_id = run["run"]["runId"].as_str().unwrap().to_string();
    let added = client
        .call(
            "run/add-members",
            authed(json!({ "runId": run_id, "nodeIds": [&t2_id] }), &cred),
        )
        .expect("add member while pending");
    assert_eq!(added["run"]["progress"]["total"], json!(2));
    assert_eq!(added["run"]["progress"]["pending"], json!(2));

    let dup = client
        .call(
            "run/add-members",
            authed(json!({ "runId": run_id, "nodeIds": [&t1_id] }), &cred),
        )
        .unwrap_err();
    assert_eq!(problem_of(&dup), "DUPLICATE_MEMBER");

    // After start the gate slams shut.
    client
        .call(
            "run/control",
            authed(json!({ "runId": run_id, "action": "start" }), &cred),
        )
        .expect("start");
    let late = client
        .call(
            "run/add-members",
            authed(json!({ "runId": run_id, "nodeIds": [epic["node"]["nodeId"]] }), &cred),
        )
        .unwrap_err();
    assert_eq!(problem_of(&late), "INVALID_INPUT");

    // ── nudge-record: durable count ─────────────────────────────────────
    for expected in 1..=2 {
        let nudged = client
            .call(
                "run/nudge-record",
                authed(json!({ "runId": run_id, "nodeId": t2_id }), &cred),
            )
            .expect("nudge");
        assert_eq!(
            nudged["nudged"][0]["nudgeCount"],
            json!(expected),
            "durable nudge budget"
        );
    }
    // Claimed item is not pending → nudge refuses.
    let claim = client
        .call("run/claim", authed(json!({ "runId": run_id }), &cred))
        .expect("claim first item");
    assert_eq!(claim["item"]["nodeId"], json!(t1_id));
    let refused = client
        .call(
            "run/nudge-record",
            authed(json!({ "runId": run_id, "nodeId": t1_id }), &cred),
        )
        .unwrap_err();
    assert_eq!(problem_of(&refused), "INVALID_INPUT");

    // ── interrupt: in-flight demotion + terminal derivation ─────────────
    let interrupted = client
        .call(
            "run/interrupt",
            authed(json!({ "runId": run_id }), &cred),
        )
        .expect("interrupt");
    assert_eq!(interrupted["interrupted"], json!([t1_id]));
    // Interrupted run pauses with one interrupted + one pending member.
    let progress = &interrupted["run"]["progress"];
    assert_eq!(interrupted["run"]["status"], json!("paused"));
    assert_eq!(progress["interrupted"], json!(1));
    assert_eq!(progress["pending"], json!(1));

    proc.kill();
}
