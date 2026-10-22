//! U5a event plane: seeded backlog resume (events written before the
//! subscriber attach are replayed from a cursor) + live notification
//! (`omt/event` pushed to an attached subscriber), and cursor keying.

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
fn events_resume_replays_seeded_backlog_then_streams_live() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut writer, w_cred) = connected_client(&endpoint, "cli").expect("writer");

    // Seed THREE committed node events before any subscription exists
    // (valid epic → story → ticket chain).
    let epic = writer
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "event epic" }), &w_cred),
        )
        .expect("seed epic");
    let story = writer
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "event story", "parentId": epic["node"]["nodeId"] }),
                &w_cred,
            ),
        )
        .expect("seed story");
    let parent_id = story["node"]["nodeId"].as_str().unwrap();
    let mut ids = Vec::new();
    for i in 0..3 {
        let result = writer
            .call(
                "node/create",
                authed(
                    json!({ "type": "ticket", "title": format!("event seed {i}"), "parentId": parent_id }),
                    &w_cred,
                ),
            )
            .expect("seed create");
        ids.push(result["node"]["nodeId"].as_str().unwrap().to_string());
    }

    // A second client resumes from cursor 0: the full backlog replays.
    let (mut reader, r_cred) = connected_client(&endpoint, "external").expect("reader");
    let page = reader
        .call(
            "events/resume",
            authed(json!({ "cursor": 0, "limit": 100 }), &r_cred),
        )
        .expect("resume");
    let events = page["events"].as_array().expect("events array");
    assert!(events.len() >= 3, "backlog carries the seeded creates");
    for event in events {
        assert_eq!(event["type"], "node.changed");
        assert!(event["cursor"].is_number(), "cursor keys every envelope");
    }
    // Strictly increasing cursors.
    let cursors: Vec<i64> = events
        .iter()
        .map(|e| e["cursor"].as_i64().unwrap())
        .collect();
    assert!(
        cursors.windows(2).all(|w| w[1] > w[0]),
        "cursors strictly increase: {cursors:?}"
    );

    // Live phase: subscribe (resume again registers the live channel),
    // then a THIRD client commits a new node; the notification arrives.
    reader
        .call(
            "events/resume",
            authed(
                json!({ "cursor": cursors.last().copied().unwrap_or(0) }),
                &r_cred,
            ),
        )
        .expect("subscription attach");

    let live_created = writer
        .call(
            "node/create",
            authed(
                json!({ "type": "ticket", "title": "live one", "parentId": parent_id }),
                &w_cred,
            ),
        )
        .expect("live-phase create");
    let live_id = live_created["node"]["nodeId"].as_str().unwrap();

    let notification = reader
        .wait_notification("omt/event", Duration::from_secs(10))
        .expect("live omt/event notification");
    assert_eq!(notification["type"], "node.changed");
    assert_eq!(
        notification["payload"]["change"], "created",
        "storage node.created maps onto protocol change discriminator"
    );
    assert_eq!(notification["payload"]["ref"]["nodeId"], json!(live_id));
    assert_eq!(notification["homeId"], notification["homeId"]); // present

    // The live envelope's cursor advances beyond the backlog.
    assert!(
        notification["cursor"].as_i64().unwrap() > *cursors.last().unwrap_or(&0),
        "live cursor advances past the backlog watermark"
    );

    proc.kill();
}

/// Cursor resume does not replay already-seen events: resuming AT the last
/// delivered cursor yields only strictly-new envelopes.
#[test]
fn events_resume_from_cursor_is_strictly_new() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    let epic = client
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "cursor epic" }), &cred),
        )
        .expect("epic");
    let story = client
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "cursor story", "parentId": epic["node"]["nodeId"] }),
                &cred,
            ),
        )
        .expect("story");
    let parent_id = story["node"]["nodeId"].as_str().unwrap();
    client
        .call(
            "node/create",
            authed(
                json!({ "type": "ticket", "title": "first", "parentId": parent_id }),
                &cred,
            ),
        )
        .expect("create 1");

    let first_page = client
        .call("events/resume", authed(json!({ "cursor": 0 }), &cred))
        .expect("first page");
    let first_cursors: Vec<i64> = first_page["events"]
        .as_array()
        .expect("array")
        .iter()
        .map(|e| e["cursor"].as_i64().unwrap())
        .collect();
    let watermark = *first_cursors.last().expect("at least one event");

    // Nothing new yet.
    let empty_page = client
        .call(
            "events/resume",
            authed(json!({ "cursor": watermark }), &cred),
        )
        .expect("empty page");
    assert_eq!(
        empty_page["events"].as_array().map(|a| a.len()),
        Some(0),
        "no replay of consumed events"
    );

    // One new commit → exactly the new envelope on the next resume.
    client
        .call(
            "node/create",
            authed(
                json!({ "type": "ticket", "title": "second", "parentId": parent_id }),
                &cred,
            ),
        )
        .expect("create 2");
    let next = client
        .call(
            "events/resume",
            authed(json!({ "cursor": watermark }), &cred),
        )
        .expect("next page");
    let events = next["events"].as_array().expect("array");
    assert!(!events.is_empty(), "new commit appears");
    assert!(
        events
            .iter()
            .all(|e| e["cursor"].as_i64().unwrap() > watermark),
        "strictly after cursor"
    );

    proc.kill();
}

/// Live thread count of a process: `/proc/<pid>/task` on Linux, `ps -M`
/// rows (minus the header) on macOS.
fn thread_count(pid: i64) -> usize {
    if let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        return entries.count();
    }
    let out = std::process::Command::new("ps")
        .args(["-M", "-p", &pid.to_string()])
        .output()
        .expect("ps -M");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .count()
        .saturating_sub(1)
}

/// F-DAEMON-1 regression: a client that polls `events/resume` on ONE
/// connection (the desktop pump does so every 250ms) keeps exactly one live
/// subscription for that home — no forwarder thread per call, and a commit
/// is pushed once, not once per past resume.
#[test]
fn repeated_events_resume_on_one_connection_keeps_one_subscription() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut writer, w_cred) = connected_client(&endpoint, "cli").expect("writer");
    let epic = writer
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "poll epic" }), &w_cred),
        )
        .expect("epic");
    let epic_id = epic["node"]["nodeId"].as_str().unwrap().to_string();

    let (mut reader, r_cred) = connected_client(&endpoint, "desktop").expect("reader");
    let mut cursor = 0;
    let poll = |reader: &mut common::TestClient, cursor: &mut i64| {
        let page = reader
            .call(
                "events/resume",
                authed(json!({ "cursor": *cursor, "limit": 500 }), &r_cred),
            )
            .expect("poll");
        *cursor = page["cursor"].as_i64().unwrap_or(*cursor);
    };
    poll(&mut reader, &mut cursor);
    std::thread::sleep(Duration::from_millis(300));
    let baseline = thread_count(proc.pid());

    for _ in 0..40 {
        poll(&mut reader, &mut cursor);
    }
    // Superseded forwarders exit on channel disconnect; give them a beat.
    std::thread::sleep(Duration::from_millis(600));
    let after = thread_count(proc.pid());
    assert!(
        after <= baseline + 2,
        "40 polls on one connection must not grow daemon threads: {baseline} → {after}"
    );

    writer
        .call(
            "node/update",
            authed(
                json!({ "nodeId": epic_id, "changes": { "title": "poll epic v2" } }),
                &w_cred,
            ),
        )
        .expect("live commit");
    reader
        .wait_notification("omt/event", Duration::from_secs(10))
        .expect("live omt/event");
    // Duplicates (one per stale subscription) would land within the same
    // burst; drain them through a round-trip before counting.
    std::thread::sleep(Duration::from_millis(500));
    reader
        .call("node/get", authed(json!({ "nodeId": epic_id }), &r_cred))
        .expect("drain round-trip");
    let duplicates = reader
        .notifications
        .iter()
        .filter(|line| line.contains("\"omt/event\""))
        .count();
    assert_eq!(duplicates, 0, "the commit is pushed exactly once");

    proc.kill();
}
