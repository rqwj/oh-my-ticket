//! U5b limits negative matrix (R21): every bounded quantity is tripped ONCE
//! through a real daemon, each refusal carries its registered Problem code
//! with structured details, and degradation is FAIR — cheap checks run
//! before expensive ones (payload before auth; auth before durable quota).
//!
//! Config overrides arrive via `<runtime-dir>/daemon.json` (precedence:
//! defaults < file), proving the config wiring end-to-end.

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, enroll, DaemonProcess, TestClient, TestCtx};
use serde_json::json;
use std::time::Duration;

fn problem_code(err: &common::RpcError) -> String {
    match err {
        common::RpcError::Problem { code, .. } => code.clone(),
        other => panic!("expected a Problem, got {other:?}"),
    }
}

fn problem_details(err: &common::RpcError) -> serde_json::Value {
    match err {
        common::RpcError::Problem { details, .. } => details.clone(),
        other => panic!("expected a Problem, got {other:?}"),
    }
}

/// Spawn a daemon with a `daemon.json` written first; wait ready.
fn ready_with_config(ctx: &TestCtx, config: serde_json::Value) -> (DaemonProcess, String) {
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        serde_json::to_string(&config).expect("config json"),
    )
    .expect("write daemon.json");
    let proc = DaemonProcess::spawn(ctx, &["--home", ctx.home_str()]);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            return (proc, d.endpoint);
        }
        assert!(proc.is_alive(), "daemon died: {}", proc.stderr_text());
        if std::time::Instant::now() > deadline {
            panic!("no descriptor within 20s");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// 1. Payload bound (wire framing stage): oversized line refused with
///    INVALID_INPUT naming maxPayloadBytes — and FAIRNESS: the refusal wins
///    over UNAUTHORIZED even though no credential was presented.
#[test]
fn payload_bound_trips_before_credential_check() {
    let ctx = TestCtx::spawn();
    let (_proc, endpoint) = ready_with_config(&ctx, json!({ "limits": { "maxPayloadBytes": 64 } }));
    let mut client = TestClient::connect(&endpoint).expect("connect");
    let oversized = format!("{{\"padding\":\"{}\"}}", "x".repeat(200));
    client.send_line(&oversized).expect("send");
    // Reader loop answers with an id-null error response.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let answer;
    loop {
        assert!(
            std::time::Instant::now() <= deadline,
            "no refusal for oversized payload"
        );
        let mut line = String::new();

        client.reader_line(&mut line).expect("read refusal line");
        if !line.trim().is_empty() {
            answer = line;
            break;
        }
    }
    let value: serde_json::Value = serde_json::from_str(answer.trim()).expect("json");
    assert_eq!(
        value.pointer("/error/data/code").and_then(|v| v.as_str()),
        Some("INVALID_INPUT")
    );
    assert_eq!(
        value.pointer("/error/data/details/maxPayloadBytes"),
        Some(&json!(64))
    );

    // Fairness proof at equal size: an undersized request WITHOUT any
    // handshake gets UNAUTHORIZED (stage 3 runs only when stage 1 passed).
    let err = client.call("node/list", json!({})).unwrap_err();
    assert_eq!(problem_code(&err), "UNAUTHORIZED");
}

/// 2. Concurrent-connections cap: excess connect receives a RATE_LIMITED
///    line and is closed, while the admitted connection keeps serving.
#[test]
fn connection_cap_degrades_with_rate_limited() {
    let ctx = TestCtx::spawn();
    let (_proc, endpoint) =
        ready_with_config(&ctx, json!({ "limits": { "maxConcurrentConnections": 1 } }));
    let (mut first, first_cred) = connected_client(&endpoint, "cli").expect("first client");
    // First connection still works.
    first
        .call("events/resume", authed(json!({ "cursor": 0 }), &first_cred))
        .expect("serving");

    let mut second = TestClient::connect(&endpoint).expect("tcp connect ok");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let answer;
    loop {
        assert!(
            std::time::Instant::now() <= deadline,
            "no RATE_LIMITED refusal line"
        );
        let mut line = String::new();
        match second.reader_line(&mut line) {
            Ok(0) => panic!("closed without refusal line"),
            Ok(_) => {}
            Err(_) => continue,
        }
        if !line.trim().is_empty() {
            answer = line;
            break;
        }
    }
    let value: serde_json::Value = serde_json::from_str(answer.trim()).expect("json");
    assert_eq!(
        value.pointer("/error/data/code").and_then(|v| v.as_str()),
        Some("RATE_LIMITED")
    );
    assert_eq!(
        value
            .pointer("/error/data/details/reason")
            .and_then(|v| v.as_str()),
        Some("concurrent-connections")
    );
    assert_eq!(value.pointer("/error/data/details/limit"), Some(&json!(1)));

    // The first connection remains healthy after the refusal.
    first
        .call("events/resume", authed(json!({ "cursor": 0 }), &first_cred))
        .expect("still serving");
}

/// 3. Opened-homes quota: exceeding it refuses the SECOND home open with
///    QUOTA_EXCEEDED (rule=open-homes) and the daemon exits fail-closed.
#[test]
fn open_homes_quota_refuses_second_home() {
    let ctx = TestCtx::spawn();
    let second_home = ctx.dir.path().join("home-two");
    std::fs::create_dir_all(&second_home).expect("second home");
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        json!({ "limits": { "maxOpenHomes": 1 } }).to_string(),
    )
    .expect("config");
    let mut proc = DaemonProcess::spawn(
        &ctx,
        &[
            "--home",
            ctx.home_str(),
            "--home",
            second_home.to_str().unwrap(),
        ],
    );
    let info = proc
        .wait_with_timeout(Duration::from_secs(20))
        .expect("daemon exits on quota refusal");
    assert_eq!(
        info.code,
        Some(2),
        "fail-closed exit; stderr {}",
        info.stderr
    );
    assert!(
        info.stderr.contains("QUOTA_EXCEEDED"),
        "stderr: {}",
        info.stderr
    );
    assert!(
        info.stderr.contains("open-homes"),
        "details.rule names the quota: {}",
        info.stderr
    );
}

/// 4. Per-home queue depth: while a delayed job executes, queued jobs fill
///    the bounded channel and the NEXT submit degrades with RATE_LIMITED
///    (reason=home-queue-depth) instead of growing memory unboundedly.
#[test]
fn queue_depth_overflow_degrades_with_rate_limited() {
    let ctx = TestCtx::spawn();
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        json!({ "limits": { "maxHomeQueueDepth": 1 }, "lockHeartbeatMs": 1000 }).to_string(),
    )
    .expect("config");
    let mut proc = DaemonProcess::spawn_with_env(
        &ctx,
        &["--home", ctx.home_str()],
        &[("OMT_DELAY_BEFORE_METHOD", "node/update:1200".to_string())],
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let endpoint = loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            break d.endpoint;
        }
        assert!(std::time::Instant::now() <= deadline, "no descriptor");
        std::thread::sleep(Duration::from_millis(25));
    };
    let _ = &mut proc;
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    // Seed one node so updates have a target.
    let node = client
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "queue target" }), &cred),
        )
        .expect("seed node");
    let node_id = node
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .expect("id")
        .to_string();

    // Occupancy probe: events/resume rides the SAME actor queue; when the
    // actor is busy executing the delayed create, our probe occupies the
    // single queue slot and the next job must be refused.
    let busy = {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut slow_client = TestClient::connect(&endpoint).expect("slow conn");
        let (_, slow_cred) = {
            let c = &mut slow_client;
            enroll(c, "cli", json!({})).expect("enroll")
        };
        std::thread::spawn(move || {
            let outcome = slow_client.call(
                "node/update",
                authed(
                    json!({ "nodeId": node_id, "changes": { "priority": 9 } }),
                    &slow_cred,
                ),
            );
            let _ = tx.send(outcome);
        });
        // Wait until the actor is provably busy: the delay hook holds it
        // ~1200ms; submit the queue-filling probe right away.
        rx
    };

    // Give the busy job a moment to reach the actor, then occupy the slot.
    std::thread::sleep(Duration::from_millis(250));
    let filler = {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut filler_client = TestClient::connect(&endpoint).expect("filler conn");
        let (_, filler_cred) = {
            let c = &mut filler_client;
            enroll(c, "cli", json!({})).expect("enroll")
        };
        std::thread::spawn(move || {
            let outcome = filler_client.call("node/list", authed(json!({}), &filler_cred));
            let _ = tx.send(outcome);
        });
        rx
    };
    std::thread::sleep(Duration::from_millis(100));

    // Third submit: queue full → RATE_LIMITED.
    let err = client
        .call("node/list", authed(json!({}), &cred))
        .unwrap_err();
    assert_eq!(problem_code(&err), "RATE_LIMITED", "err: {err:?}");
    assert_eq!(
        problem_details(&err)["reason"].as_str(),
        Some("home-queue-depth")
    );

    // Everything drains afterwards: both background jobs complete.
    busy.recv_timeout(Duration::from_secs(15))
        .expect("busy job completes")
        .expect("busy update ok");
    filler
        .recv_timeout(Duration::from_secs(15))
        .expect("filler completes")
        .expect("filler list ok");
    client
        .call("node/list", authed(json!({}), &cred))
        .expect("queue healthy again");
}

/// 5. Search-term bound: over-long queries fail INVALID_INPUT with
///    field/maxLength/observed details.
#[test]
fn search_term_bound_trips() {
    let ctx = TestCtx::spawn();
    let (_proc, endpoint) =
        ready_with_config(&ctx, json!({ "limits": { "maxSearchTermBytes": 8 } }));
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");
    let err = client
        .call(
            "node/search",
            authed(json!({ "query": "way-too-long-search-term" }), &cred),
        )
        .unwrap_err();
    assert_eq!(problem_code(&err), "INVALID_INPUT");
    assert_eq!(problem_details(&err)["field"], json!("query"));
    assert_eq!(problem_details(&err)["maxLength"], json!(8));

    // node/list filter.query carries the same bound.
    let err = client
        .call(
            "node/list",
            authed(json!({ "filter": { "query": "0123456789" } }), &cred),
        )
        .unwrap_err();
    assert_eq!(problem_code(&err), "INVALID_INPUT");
    assert_eq!(problem_details(&err)["field"], json!("filter.query"));

    // A term inside the bound works.
    client
        .call("node/search", authed(json!({ "query": "ok" }), &cred))
        .expect("short term accepted");
}

/// 6. Event page bound: limit above maxEventBatch fails INVALID_INPUT
///    instead of silently clamping (clients must know they asked too much).
#[test]
fn event_page_bound_trips() {
    let ctx = TestCtx::spawn();
    let (_proc, endpoint) = ready_with_config(&ctx, json!({}));
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");
    let err = client
        .call(
            "events/resume",
            authed(json!({ "cursor": 0, "limit": 5000 }), &cred),
        )
        .unwrap_err();
    assert_eq!(problem_code(&err), "INVALID_INPUT");
    assert_eq!(problem_details(&err)["field"], json!("limit"));
    assert_eq!(problem_details(&err)["limit"], json!(1000));
    assert_eq!(problem_details(&err)["observed"], json!(5000));

    // Within the bound works and returns the documented batch shape.
    let batch = client
        .call(
            "events/resume",
            authed(json!({ "cursor": 0, "limit": 10 }), &cred),
        )
        .expect("page accepted");
    assert!(batch.get("resync").and_then(|v| v.as_bool()).is_some());
}

/// 7. Idempotency-table quota: NEW commands beyond the registry cap degrade
///    with QUOTA_EXCEEDED (rule=idempotency-table); committed replays stay
///    allowed regardless of table pressure (fairness of the bound).
#[test]
fn idempotency_table_quota_trips_but_replays_stay_allowed() {
    let ctx = TestCtx::spawn();
    let (_proc, endpoint) =
        ready_with_config(&ctx, json!({ "limits": { "maxIdempotencyEntries": 1 } }));
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    // First explicit command consumes the single registry slot.
    let first = client
        .call(
            "node/create",
            authed(
                json!({
                    "commandId": "LIMITS-CMD-1",
                    "type": "epic",
                    "title": "quota seed",
                }),
                &cred,
            ),
        )
        .expect("first command commits");
    let epic_id = first
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .expect("id")
        .to_string();

    // Second NEW command: table full → QUOTA_EXCEEDED.
    let err = client
        .call(
            "node/create",
            authed(
                json!({
                    "commandId": "LIMITS-CMD-2",
                    "type": "story",
                    "title": "should be refused",
                    "parentId": epic_id,
                }),
                &cred,
            ),
        )
        .unwrap_err();
    assert_eq!(problem_code(&err), "QUOTA_EXCEEDED");
    assert_eq!(problem_details(&err)["rule"], json!("idempotency-table"));
    assert_eq!(problem_details(&err)["limit"], json!(1));
    // And nothing partial landed.
    client
        .call("node/get", authed(json!({ "nodeId": "STORY-0001" }), &cred))
        .unwrap_err();

    // Replay of the committed command still returns its stored result.
    let replay = client
        .call(
            "node/create",
            authed(
                json!({
                    "commandId": "LIMITS-CMD-1",
                    "type": "epic",
                    "title": "quota seed",
                }),
                &cred,
            ),
        )
        .expect("committed replay always allowed");
    assert_eq!(
        replay.pointer("/node/nodeId").and_then(|v| v.as_str()),
        Some(epic_id.as_str())
    );

    // Fair order: auth precedes the quota probe — a garbage token gets
    // UNAUTHORIZED even on the full table.
    let mut anon = TestClient::connect(&endpoint).expect("anon conn");
    let err = anon
        .call(
            "node/create",
            json!({
                "credential": { "token": format!("{:064}", 'f') },
                "commandId": "LIMITS-CMD-3",
                "type": "epic",
                "title": "nope",
            }),
        )
        .unwrap_err();
    assert_eq!(problem_code(&err), "UNAUTHORIZED");
}
