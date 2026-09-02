//! U5b retention suite: the daemon-owned outbox pruner keeps at most
//! `limits.maxRetainedEvents` events per open home, and every pruning pass
//! that overtakes a live consumer emits a keyed `snapshot.resync` event
//! (prunedThroughSeq / consumerCursor keying). Live subscribers see the
//! resync pushed; a later resume from an ancient cursor reports resync:true
//! (gap detection).

#![allow(dead_code)]

mod common;

use common::{authed, enroll, DaemonProcess, TestClient, TestCtx};
use serde_json::json;
use std::time::{Duration, Instant};

fn ready(ctx: &TestCtx) -> (DaemonProcess, String) {
    let proc = DaemonProcess::spawn(ctx, &["--home", ctx.home_str()]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            return (proc, d.endpoint);
        }
        assert!(proc.is_alive(), "daemon died: {}", proc.stderr_text());
        assert!(Instant::now() <= deadline, "no descriptor");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn retention_prunes_and_emits_keyed_snapshot_resync() {
    let ctx = TestCtx::spawn();
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        json!({
            "lockHeartbeatMs": 150,
            "limits": { "maxRetainedEvents": 3 }
        })
        .to_string(),
    )
    .expect("config");
    let (_proc, endpoint) = ready(&ctx);

    // Watcher connects; its SUBSCRIPTION lands below at a nonzero lagging
    // cursor so retention can key the overtaken-consumer signal on it.
    let mut watcher = TestClient::connect(&endpoint).expect("connect");
    let (_hs, watch_cred) = enroll(&mut watcher, "cli", json!({})).expect("enroll");

    // Worker seeds one event, giving the watcher a frontier to pin.
    let mut worker = TestClient::connect(&endpoint).expect("connect");
    let (_hs, cred) = enroll(&mut worker, "cli", json!({})).expect("enroll");
    worker
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "seed" }), &cred),
        )
        .expect("seed create");

    // Subscribe AT THE FRONTIER (seq 1): everything after is live stream.
    worker
        .call("events/resume", authed(json!({ "cursor": 1 }), &cred))
        .expect("worker subscribes too");
    watcher
        .call("events/resume", authed(json!({ "cursor": 1 }), &watch_cred))
        .expect("watcher subscribes at frontier");
    std::thread::sleep(Duration::from_millis(200));

    // Generate well past the 3-event ceiling: eviction runs far past the
    // watchers' pinned cursor 1 → overtaken-consumer resync must fire.
    for round in 0..8 {
        worker
            .call(
                "node/create",
                authed(
                    json!({ "type": "epic", "title": format!("retention {round}") }),
                    &cred,
                ),
            )
            .expect("create");
    }

    // Drain watcher notifications until the snapshot.resync one arrives
    // (interleaved node.changed backlog events are expected).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut payload_text = String::new();
    let mut found = false;
    while Instant::now() < deadline {
        match watcher.wait_notification("omt/event", Duration::from_millis(500)) {
            Ok(params) => {
                let candidate = serde_json::to_string(&params).expect("serialize");
                if candidate.contains("snapshot.resync") {
                    payload_text = candidate;
                    found = true;
                    break;
                }
            }
            Err(_) => continue,
        }
    }
    assert!(
        found,
        "snapshot.resync fanned out to subscribers; saw only unrelated events"
    );
    assert!(
        payload_text.contains("snapshot.resync"),
        "event type present: {payload_text}"
    );
    let normalized = payload_text.replace('_', "");
    assert!(
        normalized.contains("prunedThroughSeq"),
        "keyed by prunedThroughSeq: {payload_text}"
    );
    assert!(
        normalized.contains("consumerCursor"),
        "keyed by consumerCursor: {payload_text}"
    );

    // Resume from seq 0 after pruning: the gap detector reports resync.
    let resumed = worker
        .call("events/resume", authed(json!({ "cursor": 0 }), &cred))
        .expect("resume answers");
    assert_eq!(
        resumed.get("resync").and_then(|v| v.as_bool()),
        Some(true),
        "ancient cursor over pruned history must resync: {resumed}"
    );
}
