//! U5 home/declare acceptance: the route-seam command that idempotently
//! registers an existing on-disk home into a RUNNING daemon (R6-R9, KTD2,
//! KTD3), plus the two review-mandated registry fixes it leans on — actor
//! self-eviction on exit (quota counts only live homes) and subscriber
//! keep-alive during the idle quiet window.
//!
//! Every daemon here is spawned with an explicit --home pointing INTO the
//! suite's temp dir so nothing ever touches the real ~/.omt.

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, enroll, DaemonProcess, TestClient, TestCtx};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Asserts a Problem-shaped refusal and hands the parts to `check`.
fn expect_problem(err: common::RpcError, context: &str) -> (String, serde_json::Value) {
    match err {
        common::RpcError::Problem { code, details } => (code, details),
        other => panic!("{context}: expected structured problem, got {other:?}"),
    }
}

fn wait_descriptor(ctx: &TestCtx, proc: &DaemonProcess) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            return d.endpoint;
        }
        assert!(
            proc.is_alive(),
            "daemon died during startup: {}",
            proc.stderr_text()
        );
        assert!(Instant::now() <= deadline, "no descriptor within 20s");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Spawn with an explicit home + optional daemon.json written first.
fn ready_with(
    ctx: &TestCtx,
    config: Option<Value>,
    envs: &[(&str, String)],
) -> (DaemonProcess, String) {
    if let Some(config) = config {
        std::fs::write(ctx.runtime_dir.join("daemon.json"), config.to_string())
            .expect("write daemon.json");
    }
    let extra: Vec<String> = vec!["--home".into(), ctx.home_str().into()];
    let refs: Vec<&str> = extra.iter().map(String::as_str).collect();
    let proc = DaemonProcess::spawn_with_env(ctx, &refs, envs);
    let endpoint = wait_descriptor(ctx, &proc);
    (proc, endpoint)
}

/// Fresh handshake; returns the full handshake result (homes list etc.).
fn handshake(endpoint: &str) -> Value {
    let mut client = TestClient::connect(endpoint).expect("connect");
    let (result, _) = enroll(&mut client, "cli", json!({})).expect("handshake");
    result
}

/// Declare via a dedicated connection; returns raw result value.
fn declare(
    endpoint: &str,
    cred: &Value,
    path: &std::path::Path,
) -> Result<Value, common::RpcError> {
    let mut client = TestClient::connect(endpoint).expect("connect for declare");
    client.call(
        "home/declare",
        authed(json!({ "path": path.to_string_lossy() }), cred),
    )
}

fn make_home_dir(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir declared home");
    dir
}

// ── 1. happy path ────────────────────────────────────────────────────────

#[test]
fn declare_happy_path_makes_home_usable() {
    let ctx = TestCtx::spawn_named("declare-happy");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    let target = make_home_dir(ctx.dir.path(), "workspace-late");
    let result = declare(&endpoint, &cred, &target).expect("declare ok");
    let home_id = result["homeId"].as_str().expect("homeId").to_string();
    assert!(!home_id.is_empty());
    assert_eq!(result["requiresRehandshake"], json!(true), "R8 flag");
    assert_eq!(result["name"], json!("workspace-late"));
    assert_eq!(result["kind"], json!("workspace"));

    // The pre-declare credential is scoped to the homes that existed at
    // handshake time: addressing the new home refuses, WITH the KTD3 hint.
    let stale = client
        .call("node/list", authed(json!({ "homeId": home_id }), &cred))
        .expect_err("stale credential lacks the declared home");
    let (code, details) = expect_problem(stale, "stale credential");
    assert_eq!(code, "FORBIDDEN", "{details}");
    assert_eq!(details["reason"], json!("home-not-scoped"), "{details}");
    assert_eq!(details["requiresRehandshake"], json!(true));

    // Rehandshake (the R8/KTD3 self-heal): a fresh credential now carries
    // the declared home and real work flows.
    let (mut fresh, fresh_cred) = connected_client(&endpoint, "cli").expect("fresh client");
    let created = fresh
        .call(
            "node/create",
            authed(
                json!({ "type": "epic", "title": "late arrival", "homeId": home_id }),
                &fresh_cred,
            ),
        )
        .expect("create in declared home after rehandshake");
    let node_id = created["node"]["nodeId"].as_str().unwrap().to_string();
    let list = fresh
        .call(
            "node/list",
            authed(json!({ "homeId": home_id }), &fresh_cred),
        )
        .expect("node/list usable on declared home");
    let ids: Vec<&str> = list["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .filter_map(|n| n["nodeId"].as_str())
        .collect();
    assert!(ids.contains(&node_id.as_str()), "declared home serves data");

    // And it appears in fresh handshakes.
    let homes = handshake(&endpoint)["homes"].clone();
    assert!(
        homes
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["homeId"] == json!(home_id)),
        "declared home listed: {homes}"
    );
}

// ── 2. idempotency + alias paths ─────────────────────────────────────────

#[test]
fn declare_is_idempotent_and_alias_paths_collapse() {
    let ctx = TestCtx::spawn_named("declare-idem");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let target = make_home_dir(ctx.dir.path(), "idem-home");
    let first = declare(&endpoint, &cred, &target).expect("first declare");
    let id1 = first["homeId"].as_str().unwrap().to_string();

    // Exact repeat → same homeId.
    let second = declare(&endpoint, &cred, &target).expect("repeat declare");
    assert_eq!(second["homeId"], json!(id1), "idempotent replay");

    // Alias through a symlink canonicalizes to the same path.
    let alias = ctx.dir.path().join("alias-link");
    std::os::unix::fs::symlink(&target, &alias).expect("symlink alias");
    let third = declare(&endpoint, &cred, &alias).expect("alias declare");
    assert_eq!(
        third["homeId"],
        json!(id1),
        "alias path collapses to one home"
    );
}

// ── 3. two-connection concurrent declare race ────────────────────────────

#[test]
fn concurrent_declare_race_is_one_opener_no_ownership_leak() {
    let ctx = TestCtx::spawn_named("declare-race");
    let (_proc, endpoint) = ready_with(
        &ctx,
        None,
        // Hold the placeholder long enough that the second connection is
        // provably a WAITER when the opener completes.
        &[("OMT_TEST_DECLARE_DELAY_MS", "800".to_string())],
    );

    let target = make_home_dir(ctx.dir.path(), "race-home");
    let endpoint_for_a = endpoint.clone();
    let endpoint_for_b = endpoint.clone();
    let path_a = target.to_string_lossy().to_string();
    let path_b = target.to_string_lossy().to_string();

    let handle_a = std::thread::spawn(move || {
        let (_, cred) = connected_client(&endpoint_for_a, "cli").unwrap();
        let mut client = TestClient::connect(&endpoint_for_a).unwrap();
        client.call("home/declare", authed(json!({ "path": path_a }), &cred))
    });
    // Tiny stagger so A inserts the placeholder first; B must wait.
    std::thread::sleep(Duration::from_millis(120));
    let handle_b = std::thread::spawn(move || {
        let (_, cred) = connected_client(&endpoint_for_b, "cli").unwrap();
        let mut client = TestClient::connect(&endpoint_for_b).unwrap();
        client.call("home/declare", authed(json!({ "path": path_b }), &cred))
    });

    let result_a = handle_a.join().unwrap().expect("A declares ok");
    let result_b = handle_b
        .join()
        .unwrap()
        .expect("B declares ok (idempotent)");
    let id_a = result_a["homeId"].as_str().expect("a homeId");
    let id_b = result_b["homeId"].as_str().expect("b homeId");
    assert_eq!(id_a, id_b, "both connections get the SAME homeId");
}

// ── 4. starvation ────────────────────────────────────────────────────────

#[test]
fn inflight_declare_does_not_starve_other_homes() {
    let ctx = TestCtx::spawn_named("declare-starve");
    let (_proc, endpoint) = ready_with(
        &ctx,
        None,
        &[("OMT_TEST_DECLARE_DELAY_MS", "2000".to_string())],
    );
    let (mut other, cred) = connected_client(&endpoint, "cli").expect("client");

    let target = make_home_dir(ctx.dir.path(), "slow-home");
    let declare_endpoint = endpoint.clone();
    let declare_path = target.to_string_lossy().to_string();
    let opener = std::thread::spawn(move || {
        let (_, cred) = connected_client(&declare_endpoint, "cli").unwrap();
        let mut client = TestClient::connect(&declare_endpoint).unwrap();
        client.call(
            "home/declare",
            authed(json!({ "path": declare_path }), &cred),
        )
    });
    std::thread::sleep(Duration::from_millis(150)); // opener holds the placeholder

    // A DIFFERENT home's hot path must complete inside a bounded window —
    // no global lock is held across the opener's filesystem work.
    let started = Instant::now();
    other
        .call("node/list", authed(json!({}), &cred))
        .expect("node/list while a declare is in flight");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(1500),
        "node/list starved behind in-flight declare: {elapsed:?}"
    );
    opener.join().unwrap().expect("opener finishes");
}

// ── 5. waiter timeout (KTD2 completion test b) ───────────────────────────

#[test]
fn same_path_waiter_times_out_with_retryable_problem() {
    let ctx = TestCtx::spawn_named("declare-wait-timeout");
    let (_proc, endpoint) = ready_with(
        &ctx,
        None,
        &[
            ("OMT_TEST_DECLARE_DELAY_MS", "2500".to_string()),
            ("OMT_DECLARE_WAIT_TIMEOUT_MS", "500".to_string()),
        ],
    );

    let target = make_home_dir(ctx.dir.path(), "waited-home");
    let opener_endpoint = endpoint.clone();
    let opener_path = target.to_string_lossy().to_string();
    let opener = std::thread::spawn(move || {
        let (_, cred) = connected_client(&opener_endpoint, "cli").unwrap();
        let mut client = TestClient::connect(&opener_endpoint).unwrap();
        client.call(
            "home/declare",
            authed(json!({ "path": opener_path }), &cred),
        )
    });
    std::thread::sleep(Duration::from_millis(150));

    let (_, cred) = connected_client(&endpoint, "cli").expect("waiter client");
    let waited = declare(&endpoint, &cred, &target).expect_err("bounded wait trips");
    match &waited {
        common::RpcError::Problem { code, details } => {
            assert_eq!(
                code, "RATE_LIMITED",
                "structured retryable problem: {waited:?}"
            );
            assert_eq!(
                details["reason"],
                json!("home-declare-in-progress"),
                "{details}"
            );
            assert_eq!(details["retryable"], json!(true));
        }
        other => panic!("expected problem, got {other:?}"),
    }

    // The opener still completes; its result is authoritative.
    let opened = opener.join().unwrap().expect("opener ok");
    let home_id = opened["homeId"].as_str().unwrap().to_string();

    // Retry after backoff succeeds idempotently (placeholder cleaned up).
    let retry = declare(&endpoint, &cred, &target).expect("retry after settle");
    assert_eq!(retry["homeId"], json!(home_id));
}

// ── 6. failed declare is immediately retryable (KTD2 completion test a) ──

#[test]
fn failed_declare_frees_placeholder_for_immediate_retry() {
    let ctx = TestCtx::spawn_named("declare-retry");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let target = make_home_dir(ctx.dir.path(), "retry-home");
    write_bridge_marker(&target);

    let failed = declare(&endpoint, &cred, &target).expect_err("bridge marker refuses");
    match &failed {
        common::RpcError::Problem { code, .. } => assert_eq!(code, "HOME_LOCKED"),
        other => panic!("expected problem, got {other:?}"),
    }

    // Remove the obstacle; the SAME path declares successfully right away —
    // proof the failure removed the opening placeholder and woke nobody
    // into a stuck state.
    std::fs::remove_file(target.join("home.lock")).expect("remove marker");
    let retried = declare(&endpoint, &cred, &target).expect("immediate retry ok");
    assert!(!retried["homeId"].as_str().unwrap_or_default().is_empty());
}

// ── 7. eviction regression: quota freed by idle exit ─────────────────────

#[test]
fn idle_exited_homes_free_quota_and_leave_handshake_listing() {
    let ctx = TestCtx::spawn_named("declare-evict");
    let (proc, endpoint) = ready_with(
        &ctx,
        Some(json!({ "idleQuietMs": 700, "limits": { "maxOpenHomes": 2 } })),
        &[],
    );
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let home_a = make_home_dir(ctx.dir.path(), "evict-a");
    let a = declare(&endpoint, &cred, &home_a).expect("declare fills quota to 2");
    let id_a = a["homeId"].as_str().unwrap().to_string();

    // Keep ONLY the global home busy; evict-a goes quiet and its actor must
    // exit AND remove its own registry entry.
    let (mut poke, poke_cred) = connected_client(&endpoint, "cli").expect("poke client");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut gone = false;
    while Instant::now() < deadline {
        // Activity keeps the GLOBAL actor alive so the process itself stays up.
        poke.call("node/list", authed(json!({}), &poke_cred))
            .expect("global home stays alive");
        if !handshake(&endpoint)["homes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["homeId"] == json!(id_a))
        {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(gone, "idle-exited home vanished from handshake listing");
    assert!(
        proc.is_alive(),
        "daemon survives while another home is busy"
    );

    // Quota freed by eviction: a NEW declare succeeds despite maxOpenHomes=2.
    let home_b = make_home_dir(ctx.dir.path(), "evict-b");
    let b = declare(&endpoint, &cred, &home_b).expect("quota no longer counts corpses");
    let id_b = b["homeId"].as_str().unwrap().to_string();
    assert_ne!(id_a, id_b);
    let listed = handshake(&endpoint)["homes"].clone();
    let ids: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["homeId"].as_str())
        .collect();
    assert!(ids.contains(&id_b.as_str()), "new home listed: {ids:?}");
    assert!(!ids.contains(&id_a.as_str()), "exited home absent: {ids:?}");
}

// ── 8. subscriber keep-alive ─────────────────────────────────────────────

#[test]
fn live_subscriber_suppresses_idle_quiet_exit() {
    let ctx = TestCtx::spawn_named("declare-keepalive");
    let (proc, endpoint) = ready_with(&ctx, Some(json!({ "idleQuietMs": 900 })), &[]);
    let (mut sub, cred) = connected_client(&endpoint, "external").expect("subscriber");

    // Attach a live subscription; then send NOTHING from anyone.
    sub.call("events/resume", authed(json!({ "cursor": 0 }), &cred))
        .expect("subscription attach");

    // Well past one quiet window: the home must still be alive because the
    // hub holds a live subscriber (pre-fix this daemon IDLE_EXITs entirely).
    std::thread::sleep(Duration::from_millis(2300));
    assert!(
        proc.is_alive(),
        "subscriber keeps the daemon past the quiet window"
    );
    let homes = handshake(&endpoint)["homes"].clone();
    assert_eq!(
        homes.as_array().map(Vec::len),
        Some(1),
        "subscribed home still listed: {homes}"
    );
}

// ── 9. foreign-daemon ownership → DAEMON_OWNS_HOME ──────────────────────

#[test]
fn foreign_daemon_owned_home_reports_daemon_owns_home() {
    let owner_ctx = TestCtx::spawn_named("declare-owner");
    let (owner_proc, _owner_endpoint) = ready_with(&owner_ctx, None, &[]);

    let ctx = TestCtx::spawn_named("declare-foreign");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    // Declaring a home held by ANOTHER runtime-dir daemon refuses with the
    // owner pid in details (R9).
    let err = declare(&endpoint, &cred, &owner_ctx.home).expect_err("foreign-owned refuses");
    let (code, details) = expect_problem(err, "foreign daemon home");
    assert_eq!(code, "DAEMON_OWNS_HOME", "{details}");
    assert_eq!(
        details["owner"]["pid"],
        json!(owner_proc.pid()),
        "details carry the owner pid"
    );
    assert_eq!(details["owner"]["ownerKind"], json!("daemon"));
}

// ── 10. ts-bridge marker → HOME_LOCKED with takeover guidance ───────────

fn write_bridge_marker(home: &std::path::Path) {
    let body = json!({
        "schemaVersion": 1,
        "ownerKind": "ts-bridge",
        "pid": std::process::id() as i64,
        "hostname": "declare-test",
        "acquiredAt": "2026-08-25T00:00:00.000Z",
        "heartbeatAt": "2026-08-25T00:00:00.000Z",
        "token": "bridge-marker",
    });
    std::fs::write(home.join("home.lock"), body.to_string()).expect("marker");
}

#[test]
fn ts_bridge_marked_home_refuses_with_takeover_guidance() {
    let ctx = TestCtx::spawn_named("declare-bridged");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let target = make_home_dir(ctx.dir.path(), "bridge-home");
    write_bridge_marker(&target);

    let err = declare(&endpoint, &cred, &target).expect_err("bridge marker refuses");
    let (code, details) = expect_problem(err, "bridge marker");
    // HOME_LOCKED, never an automatic takeover (R9).
    assert_eq!(code, "HOME_LOCKED", "{details}");
    assert_eq!(
        details["reason"],
        json!("ts-bridge-requires-takeover"),
        "{details}"
    );
    assert_eq!(details["owner"]["ownerKind"], json!("ts-bridge"));
}

// ── 11. invalid paths fail closed before any open ────────────────────────

#[test]
fn missing_or_file_paths_are_rejected_structured() {
    let ctx = TestCtx::spawn_named("declare-invalid");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let missing = ctx.dir.path().join("does-not-exist");
    let err = declare(&endpoint, &cred, &missing).expect_err("missing path refuses");
    let (code, details) = expect_problem(err, "missing path");
    assert_eq!(code, "INVALID_INPUT", "{details}");
    assert_eq!(details["field"], json!("path"));

    let file_path = ctx.dir.path().join("plain-file");
    std::fs::write(&file_path, "not a directory").expect("seed file");
    let err = declare(&endpoint, &cred, &file_path).expect_err("file path refuses");
    let (code, details) = expect_problem(err, "file path");
    assert_eq!(code, "INVALID_INPUT", "{details}");
    assert_eq!(details["reason"], json!("not-a-directory"), "{details}");

    // Non-string path param is a validation failure too.
    let mut client = TestClient::connect(&endpoint).unwrap();
    let err = client
        .call("home/declare", authed(json!({ "path": 42 }), &cred))
        .expect_err("non-string path refuses");
    let (code, _) = expect_problem(err, "non-string path");
    assert_eq!(code, "INVALID_INPUT");
}

// ── 12. quota ────────────────────────────────────────────────────────────

#[test]
fn declaring_past_max_open_homes_hits_quota_exceeded() {
    let ctx = TestCtx::spawn_named("declare-quota");
    let (_proc, endpoint) = ready_with(&ctx, Some(json!({ "limits": { "maxOpenHomes": 1 } })), &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let target = make_home_dir(ctx.dir.path(), "over-quota-home");
    let err = declare(&endpoint, &cred, &target).expect_err("quota refuses");
    let (code, details) = expect_problem(err, "quota");
    assert_eq!(code, "QUOTA_EXCEEDED", "{details}");
    assert_eq!(details["rule"], json!("open-homes"), "{details}");
}

// ── 13. capability advertisement + unknown-method routing integrity ─────

#[test]
fn features_advertise_home_declare_and_unknown_methods_stay_not_found() {
    let ctx = TestCtx::spawn_named("declare-features");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);

    let result = handshake(&endpoint);
    assert_eq!(
        result.pointer("/features/homeDeclare"),
        Some(&json!(true)),
        "features map advertises homeDeclare: {}",
        result["features"]
    );

    // Old clients / typos still hit the generic unknown-method problem —
    // the seam intercept did not swallow routing integrity.
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");
    let err = client
        .call("home/declar", authed(json!({}), &cred))
        .expect_err("unknown method");
    let (code, details) = expect_problem(err, "unknown method");
    assert_eq!(code, "NOT_FOUND");
    assert_eq!(details["kind"], json!("method"));
}

// ── 14. authorization at the seam (KTD3 amendment) ───────────────────────

#[test]
fn operation_family_denials_have_no_hint_but_home_scope_ones_do() {
    let ctx = TestCtx::spawn_named("declare-authz");
    // Two open homes so home-scope denials are reachable. OMT_HOME pins the
    // global-home identity so the first home gets kind "global".
    let args = ["--home", ctx.global_home_str(), "--home", ctx.home_str()];
    let mut proc = DaemonProcess::spawn_with_env(
        &ctx,
        &args,
        &[("OMT_HOME", ctx.global_home_str().to_string())],
    );
    let endpoint = wait_descriptor(&ctx, &proc);

    // Discover both home ids.
    let listing = handshake(&endpoint)["homes"].clone();
    let homes_arr = listing.as_array().expect("homes array");
    let workspace_id = homes_arr
        .iter()
        .find(|h| h["kind"] == json!("workspace"))
        .and_then(|h| h["homeId"].as_str())
        .expect("workspace id")
        .to_string();
    let global_id = homes_arr
        .iter()
        .find(|h| h["kind"] == json!("global"))
        .and_then(|h| h["homeId"].as_str())
        .expect("global id")
        .to_string();

    // (a) MCP minimum-privilege credential WITHOUT the home family:
    //     plain FORBIDDEN, NO requiresRehandshake hint (KTD3 amendment —
    //     re-enrollment cannot grant an excluded op family).
    let mut mcp = TestClient::connect(&endpoint).unwrap();
    let (_, mcp_cred) =
        enroll(&mut mcp, "mcp", json!({ "operations": ["node"] })).expect("mcp credential");
    let target = make_home_dir(ctx.dir.path(), "authz-home");
    let err = declare(&endpoint, &mcp_cred, &target).expect_err("no home family → refuse");
    let (code, details) = expect_problem(err, "op-family denial");
    assert_eq!(code, "FORBIDDEN", "{details}");
    assert_eq!(
        details["reason"],
        json!("operation-not-granted"),
        "{details}"
    );
    assert!(
        details.get("requiresRehandshake").is_none(),
        "operation-family denial must NOT carry the rehandshake hint: {details}"
    );

    // (b) Home-scope FORBIDDEN (credential scoped away from global):
    //     WITH requiresRehandshake hint (KTD3 daemon half).
    let mut scoped = TestClient::connect(&endpoint).unwrap();
    let (_, scoped_cred) =
        enroll(&mut scoped, "cli", json!({ "homes": [workspace_id] })).expect("scoped credential");
    let err = scoped
        .call(
            "node/list",
            authed(json!({ "homeId": global_id }), &scoped_cred),
        )
        .expect_err("home not scoped → refuse");
    let (code, details) = expect_problem(err, "home-scope denial");
    assert_eq!(code, "FORBIDDEN", "{details}");
    assert_eq!(details["reason"], json!("home-not-scoped"), "{details}");
    assert_eq!(
        details["requiresRehandshake"],
        json!(true),
        "home-scope denial carries the hint: {details}"
    );

    // (c) NOT_FOUND kind:home also carries the hint (stale credential
    //     referencing a home enrolled before a declare).
    let err = scoped
        .call(
            "node/list",
            authed(json!({ "homeId": "h_missing" }), &scoped_cred),
        )
        .expect_err("unknown home");
    let (code, details) = expect_problem(err, "unknown home");
    assert_eq!(code, "NOT_FOUND", "{details}");
    assert_eq!(details["kind"], json!("home"), "{details}");
    assert_eq!(details["requiresRehandshake"], json!(true), "{details}");

    proc.kill();
}

// ── 15. DECLARE / DECLARE_FAILED log lines ───────────────────────────────

#[test]
fn declare_outcomes_log_declare_lines() {
    let ctx = TestCtx::spawn_named("declare-log");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let good = make_home_dir(ctx.dir.path(), "logged-home");
    declare(&endpoint, &cred, &good).expect("successful declare");

    let bad = ctx.dir.path().join("never-existed");
    declare(&endpoint, &cred, &bad).expect_err("failed declare");

    let log_path = ctx.runtime_dir.join("logs").join("omt-daemon.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    let log = loop {
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        if text.contains("\"code\":\"DECLARE_FAILED\"") || Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        log.contains("\"code\":\"DECLARE\""),
        "success line present: {log}"
    );
    assert!(
        log.contains("\"code\":\"DECLARE_FAILED\""),
        "failure line present"
    );
    // The failure line carries the problem code and the path (and nothing
    // else beyond them).
    assert!(
        log.contains("\"message\":\"INVALID_INPUT"),
        "failure line names the problem code: {log}"
    );
    // Nothing beyond codes and the path: no credential-shaped material.
    assert!(
        !log.contains("[redacted]"),
        "nothing token-shaped ever reached a declare log line"
    );
}

// ── 16. hostile store fails closed, no partial actor start ───────────────

#[test]
fn corrupt_store_fails_closed_without_partial_start() {
    let ctx = TestCtx::spawn_named("declare-corrupt");
    let (_proc, endpoint) = ready_with(&ctx, None, &[]);
    let (_, cred) = connected_client(&endpoint, "cli").expect("client");

    let target = make_home_dir(ctx.dir.path(), "corrupt-home");
    std::fs::write(
        target.join(omt_storage::DB_FILE_NAME),
        b"definitely not sqlite",
    )
    .expect("garbage db");

    let err = declare(&endpoint, &cred, &target).expect_err("hostile store refuses");
    let (code, details) = expect_problem(err, "hostile store");
    assert!(
        matches!(code.as_str(), "IO" | "SCHEMA_TOO_NEW" | "REINDEX_REQUIRED"),
        "structured storage-plane refusal, got {code} ({details})"
    );

    // No partial state: the registry shows exactly the startup home, and
    // ordinary traffic is unaffected.
    let homes = handshake(&endpoint)["homes"].clone();
    assert_eq!(homes.as_array().map(Vec::len), Some(1), "{homes}");

    // Repair the store; the same path declares immediately (the failed
    // opener removed its placeholder — no wedged RATE_LIMITED limbo).
    std::fs::remove_file(target.join(omt_storage::DB_FILE_NAME)).expect("remove garbage");
    let retried = declare(&endpoint, &cred, &target).expect("post-repair declare ok");
    assert_eq!(retried["kind"], json!("workspace"));
}
