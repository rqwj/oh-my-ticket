//! U5c CLI e2e suites: the `omt` binary against REAL spawned daemons.
//!
//! Coverage:
//! - online verbs (list/show/create/update/move/archive, run create/get/
//!   list/control/claim/report) through the same handshake/enrollment
//!   protocol as the TypeScript client (actorNamespace "cli:<pid>");
//! - OFFLINE maintenance verbs refuse while a daemon serves (HOME_LOCKED
//!   with actionable guidance), then succeed offline under exclusive
//!   ownership and release the lock afterwards;
//! - doctor lists a seeded stale ts-bridge cohort instead of failing;
//! - daemon-start/status/stop lifecycle roundtrip;
//! - Ctrl-C during an in-flight call cancels it (exit 130).

#![allow(dead_code)]

mod common;

use common::{DaemonProcess, TestCtx};
use serde_json::json;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn omt_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_omt"))
}

fn run_cli(ctx: &TestCtx, args: &[&str], envs: &[(&str, String)]) -> Output {
    let mut command = Command::new(omt_path());
    command
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("run omt")
}

fn wait_ready(ctx: &TestCtx) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            return d.endpoint;
        }
        assert!(Instant::now() <= deadline, "no descriptor");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// U1 (R22/KTD1): both binaries expose `--version` reporting the unified
/// workspace product version — the anchor install smoke and brew formula
/// checks rely on.
#[test]
fn cli_and_daemon_version_flags_report_workspace_version() {
    let out = Command::new(omt_path())
        .arg("--version")
        .output()
        .expect("run omt --version");
    assert!(out.status.success(), "omt --version must exit 0");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("omt {}", env!("CARGO_PKG_VERSION")),
        "omt --version must print '<name> <workspace version>'"
    );

    let out = Command::new(std::path::PathBuf::from(env!("CARGO_BIN_EXE_omt-daemon")))
        .arg("--version")
        .output()
        .expect("run omt-daemon --version");
    assert!(out.status.success(), "omt-daemon --version must exit 0");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("omt-daemon {}", env!("CARGO_PKG_VERSION")),
        "omt-daemon --version must print '<name> <workspace version>'"
    );
}

/// Online verb happy paths against a real daemon.
#[test]
fn cli_online_node_and_run_verbs_happy_paths() {
    let ctx = TestCtx::spawn();

    // Start the daemon THROUGH the CLI itself (--home so the spawned
    // daemon serves this test's workspace home).
    let out = run_cli(&ctx, &["--home", ctx.home_str(), "daemon-start"], &[]);
    assert!(
        out.status.success(),
        "daemon-start stdout: {} | stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    wait_ready(&ctx);

    // status reports running.
    let out = run_cli(&ctx, &["daemon-status"], &[]);
    let status_text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "status: {status_text}");
    let status_json: serde_json::Value =
        serde_json::from_str(status_text.trim()).expect("status json");
    assert_eq!(
        status_json.get("running"),
        Some(&json!(true)),
        "{status_json}"
    );

    // create → show → list → update → move → archive.
    let out = run_cli(
        &ctx,
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "CLI epic",
            "--body",
            "made by cli",
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "create stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let created: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("create json");
    let epic_id = created
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .expect("nodeId")
        .to_string();
    assert!(epic_id.starts_with("EPIC-"));

    let out = run_cli(&ctx, &["show", &epic_id], &[]);
    assert!(out.status.success(), "show failed");
    let shown: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("show json");
    assert_eq!(
        shown.pointer("/node/title").and_then(|v| v.as_str()),
        Some("CLI epic")
    );

    let out = run_cli(&ctx, &["list"], &[]);
    assert!(out.status.success(), "list failed");
    let listed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("list json");
    assert!(
        listed.to_string().contains(&epic_id),
        "listed output contains the epic: {listed}"
    );

    // Optimistic concurrency: pin the CURRENT revision from `show`.
    let shown_rev = shown
        .pointer("/node/revision")
        .and_then(|v| v.as_i64())
        .expect("revision present");
    let rev_text = shown_rev.to_string();
    let out = run_cli(
        &ctx,
        &[
            "update",
            &epic_id,
            "--title",
            "CLI epic renamed",
            "--expected-revision",
            &rev_text,
            "--append",
            "\nappended",
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "update stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A story child for move.
    let out = run_cli(
        &ctx,
        &[
            "create", "--type", "story", "--title", "mover", "--parent", &epic_id,
        ],
        &[],
    );
    assert!(out.status.success());
    let story_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("story json");
    let story_id = story_json
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    // Second epic as move target.
    let out = run_cli(
        &ctx,
        &["create", "--type", "epic", "--title", "target"],
        &[],
    );
    assert!(out.status.success());
    let target_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("target json");
    let target_id = target_json
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    let out = run_cli(&ctx, &["move", &story_id, "--to", &target_id], &[]);
    assert!(
        out.status.success(),
        "move stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_cli(&ctx, &["archive", &story_id], &[]);
    assert!(
        out.status.success(),
        "archive stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Run-plane verbs: two TICKET members under a fresh STORY (hierarchy:
    // epic → story → ticket; epics/stories are context-only members).
    let out = run_cli(
        &ctx,
        &[
            "create", "--type", "story", "--title", "run lane", "--parent", &target_id,
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "story create stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lane_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("lane json");
    let lane_id = lane_json
        .pointer("/node/nodeId")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    let mut member_ids = Vec::new();
    for n in 0..2 {
        let out = run_cli(
            &ctx,
            &[
                "create",
                "--type",
                "ticket",
                "--title",
                &format!("work item {n}"),
                "--parent",
                &lane_id,
            ],
            &[],
        );
        assert!(
            out.status.success(),
            "ticket create stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let t_json: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("ticket json");
        member_ids.push(
            t_json
                .pointer("/node/nodeId")
                .and_then(|v| v.as_str())
                .unwrap()
                .to_string(),
        );
    }
    let members = member_ids.join(",");
    let out = run_cli(&ctx, &["run-create", &members, "--title", "cli batch"], &[]);
    assert!(
        out.status.success(),
        "run-create stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("run json");
    let run_id = run_json
        .pointer("/run/runId")
        .or_else(|| run_json.get("runId"))
        .and_then(|v| v.as_str())
        .expect("runId present in run/create result")
        .to_string();

    let out = run_cli(&ctx, &["run-get", &run_id], &[]);
    assert!(
        out.status.success(),
        "run-get stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_cli(&ctx, &["run-list"], &[]);
    assert!(out.status.success());

    let out = run_cli(&ctx, &["run-control", &run_id, "--action", "start"], &[]);
    assert!(
        out.status.success(),
        "start stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Claim and report run in SEPARATE PROCESSES: persisted enrollment
    // gives both the same actor identity, so the attempt-fenced lease
    // accepts the follow-up report.
    let out = run_cli(&ctx, &["run-claim", &run_id], &[]);
    assert!(
        out.status.success(),
        "claim stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let claim_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("claim json");
    let claimed_node = claim_json
        .pointer("/item/nodeId")
        .and_then(|v| v.as_str())
        .expect("claimed nodeId")
        .to_string();
    // The attempt-fenced lease token must be presented back on report.
    let lease_token = claim_json
        .pointer("/lease/token")
        .and_then(|v| v.as_str())
        .expect("lease token")
        .to_string();

    let out = run_cli(
        &ctx,
        &[
            "run-report",
            &run_id,
            &claimed_node,
            "--outcome",
            "done",
            "--note",
            "cli did it",
            "--lease-token",
            &lease_token,
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "report stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Problem path: unknown node id → exit 3 + NOT_FOUND on stderr.
    let out = run_cli(&ctx, &["show", "EPIC-9999"], &[]);
    assert_eq!(out.status.code(), Some(3), "problem exit code");
    let err_text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err_text.contains("NOT_FOUND"), "{err_text}");

    // Usage error → exit 2.
    let out = run_cli(&ctx, &["update", &epic_id], &[]);
    assert_eq!(out.status.code(), Some(2), "usage exit code");

    // Stop via CLI; lock released.
    let out = run_cli(&ctx, &["daemon-stop"], &[]);
    assert!(
        out.status.success(),
        "daemon-stop stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while common::Descriptor::read(&ctx.runtime_dir).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !ctx.home.join("home.lock").exists(),
        "lock released after CLI stop"
    );
}

/// Offline reindex: refuses ONLINE with HOME_LOCKED guidance; succeeds
/// OFFLINE under exclusive ownership and releases the lock.
#[test]
fn cli_reindex_denied_online_succeeds_offline() {
    let ctx = TestCtx::spawn();

    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let _endpoint =
        common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).expect("descriptor");

    // Seed content so reindex has something to rebuild.
    let mut client =
        common::TestClient::connect(&common::Descriptor::read(&ctx.runtime_dir).unwrap().endpoint)
            .expect("connect");
    let (_hs, cred) = common::enroll(&mut client, "dsh", json!({})).expect("enroll");
    client
        .call(
            "node/create",
            common::authed(json!({ "type": "epic", "title": "reindex me" }), &cred),
        )
        .expect("seed");

    // ONLINE refusal: exit 3, HOME_LOCKED, stop-the-daemon guidance.
    let out = run_cli(&ctx, &["reindex", ctx.home_str()], &[]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "offline verb must refuse online"
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("HOME_LOCKED"), "{err}");
    assert!(err.contains("daemon-stop"), "actionable guidance: {err}");

    proc.kill();
    let deadline = Instant::now() + Duration::from_secs(10);
    while ctx.home.join("home.lock").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    // OFFLINE success: exit 0, human summary on stdout.
    let out = run_cli(&ctx, &["reindex", ctx.home_str()], &[]);
    assert!(
        out.status.success(),
        "offline reindex stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("\"nodes\": 1"),
        "rebuilt index reports nodes: {text}"
    );
    assert!(
        !ctx.home.join("home.lock").exists(),
        "lock released after reindex"
    );

    // A second offline run still works (no leaked ownership).
    let out = run_cli(&ctx, &["reindex", ctx.home_str()], &[]);
    assert!(out.status.success(), "repeat offline reindex");
}

/// Doctor reports a seeded stale ts-bridge marker as a named cohort
/// WITHOUT stealing it, and reports healthy:false; clean homes pass.
#[test]
fn cli_doctor_reports_stale_ts_bridge_cohort() {
    let ctx = TestCtx::spawn();

    // Seed the cohort BEFORE any daemon ever ran here: a ts-bridge marker
    // with a dead pid + old heartbeat.
    std::fs::write(
        ctx.home.join("home.lock"),
        json!({
            "schemaVersion": 1,
            "ownerKind": "ts-bridge",
            "pid": 2_000_000_000i64,
            "hostname": "ghost",
            "acquiredAt": "2026-08-20T00:00:00.000Z",
            "heartbeatAt": "2026-08-20T00:00:00.000Z",
            "token": "deadbeefdeadbeefdeadbeefdeadbeef"
        })
        .to_string(),
    )
    .expect("seed stale bridge marker");

    let out = run_cli(&ctx, &["doctor", ctx.home_str()], &[]);
    assert!(
        out.status.success(),
        "doctor runs diagnostically: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("doctor json");
    assert_eq!(
        report.pointer("/healthy").and_then(|v| v.as_bool()),
        Some(false),
        "unhealthy due to cohort: {report}"
    );
    let markers = report
        .pointer("/cohorts/tsBridgeMarkers")
        .cloned()
        .unwrap_or(json!([]));
    let markers_text = markers.to_string();
    assert!(markers_text.contains("ts-bridge"), "{markers_text}");
    assert!(
        markers_text.contains("stale"),
        "state classified: {markers_text}"
    );
    assert!(
        markers_text.contains("takeover") || report.to_string().contains("takeover"),
        "explicit-takeover note present"
    );

    // The marker was NOT stolen by the scan.
    let raw = std::fs::read_to_string(ctx.home.join("home.lock")).expect("marker intact");
    assert!(
        raw.contains("ts-bridge"),
        "cohort preserved, not auto-stolen"
    );

    // Clean home (marker removed) → healthy:true with deep probes.
    let _ = std::fs::remove_file(ctx.home.join("home.lock"));
    let out = run_cli(&ctx, &["doctor", ctx.home_str()], &[]);
    assert!(out.status.success());
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("doctor json 2");
    assert_eq!(
        report.pointer("/healthy").and_then(|v| v.as_bool()),
        Some(true),
        "clean home is healthy: {report}"
    );
    assert!(
        report.get("nodes").is_some(),
        "deep probe ran (node count present): {report}"
    );
}

/// Ctrl-C during an in-flight call: the CLI aborts with exit 130 and sends
/// cancellation intent; the daemon-side delay hook gives a wide window.
#[test]
fn cli_sigint_cancels_inflight_call() {
    let ctx = TestCtx::spawn();
    let mut daemon = DaemonProcess::spawn_with_env(
        &ctx,
        &["--home", ctx.home_str()],
        &[("OMT_DELAY_BEFORE_METHOD", "node/create:4000".to_string())],
    );
    let endpoint = loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            break d.endpoint;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let _ = endpoint;

    let mut child = Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .args(["create", "--type", "epic", "--title", "slow"])
        .spawn()
        .expect("spawn omt");

    // Wait until the call is in flight, then SIGINT it.
    std::thread::sleep(Duration::from_millis(800));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let status = child.wait().expect("wait after SIGINT");
    use std::os::unix::process::ExitStatusExt;
    // 128 + SIGINT(2) = 130 by convention.
    assert!(
        status.code() == Some(130) || status.signal() == Some(libc::SIGINT),
        "Ctrl-C exits 130 or dies by SIGINT, got {status:?}"
    );

    daemon.kill();
}

/// U7 (R10/KTD4): doctor's online preamble — installed-binary vs
/// running-daemon version consistency, observation-only against a live
/// daemon (deep probes skip with a note instead of refusing).
#[test]
fn cli_doctor_online_preamble_reports_version_match() {
    let ctx = TestCtx::spawn();
    let out = run_cli(&ctx, &["--home", ctx.home_str(), "daemon-start"], &[]);
    assert!(
        out.status.success(),
        "daemon-start: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait_ready(&ctx);

    // Same-version daemon → match:true, exit 0, deep probes skipped with
    // the runtime fields present.
    let out = run_cli(&ctx, &["doctor", ctx.home_str()], &[]);
    assert!(
        out.status.success(),
        "doctor against live daemon exits 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("doctor json");
    assert_eq!(
        report
            .pointer("/runtime/descriptorFound")
            .and_then(|v| v.as_bool()),
        Some(true),
        "{report}"
    );
    assert_eq!(
        report.pointer("/runtime/match").and_then(|v| v.as_bool()),
        Some(true),
        "same-version daemon matches: {report}"
    );
    assert_eq!(
        report
            .pointer("/runtime/daemonVersion")
            .and_then(|v| v.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
        "{report}"
    );
    assert!(
        report
            .pointer("/runtime/generation")
            .and_then(|v| v.as_i64())
            .is_some(),
        "{report}"
    );
    assert!(
        report.to_string().contains("deep probes skipped"),
        "deep probes deferred to the live daemon: {report}"
    );

    // Simulated version drift (test seam overrides the CLI side of the
    // comparison — the comparison logic is the unit under test).
    let out = run_cli(
        &ctx,
        &["doctor", ctx.home_str()],
        &[("OMT_DOCTOR_CLI_VERSION_OVERRIDE", "9.9.9-drift".to_string())],
    );
    assert!(out.status.success());
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("doctor json drift");
    assert_eq!(
        report.pointer("/runtime/match").and_then(|v| v.as_bool()),
        Some(false),
        "drifted version reports mismatch: {report}"
    );
    assert_eq!(
        report
            .pointer("/runtime/cliVersion")
            .and_then(|v| v.as_str()),
        Some("9.9.9-drift"),
        "{report}"
    );

    let out = run_cli(&ctx, &["daemon-stop"], &[]);
    assert!(
        out.status.success(),
        "daemon-stop: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// U7: no daemon → preamble says not-running and offline checks proceed
/// exactly as before (exit 0, deep probe fields present).
#[test]
fn cli_doctor_preamble_degrades_without_daemon() {
    let ctx = TestCtx::spawn();
    let out = run_cli(&ctx, &["doctor", ctx.home_str()], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("doctor json");
    assert_eq!(
        report
            .pointer("/runtime/descriptorFound")
            .and_then(|v| v.as_bool()),
        Some(false),
        "{report}"
    );
    assert_eq!(
        report.pointer("/runtime/match").and_then(|v| v.as_str()),
        Some("unknown"),
        "{report}"
    );
    // Offline deep probes still ran (node count present).
    assert!(
        report.get("nodes").is_some(),
        "offline checks proceed: {report}"
    );
    assert_eq!(
        report.pointer("/healthy").and_then(|v| v.as_bool()),
        Some(true),
        "{report}"
    );
}

/// U7: admin grants with a dead embedded pid surface in deadPidEntries.
#[test]
fn cli_doctor_admin_grants_surfaces_dead_pids() {
    let ctx = TestCtx::spawn();
    std::fs::write(
        ctx.runtime_dir.join("admin-grants.json"),
        json!({
            "schemaVersion": 1,
            "principalIds": [
                "cli:2000000001",
                "cli:2000000002",
                "dsh:alive-no-numeric-suffix"
            ]
        })
        .to_string(),
    )
    .expect("seed admin grants");

    let out = run_cli(&ctx, &["doctor", ctx.home_str()], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("doctor json");
    assert_eq!(
        report
            .pointer("/adminGrants/totalEntries")
            .and_then(as_count),
        Some(3),
        "{report}"
    );
    let dead = report
        .pointer("/adminGrants/deadPidEntries")
        .cloned()
        .unwrap_or(json!([]));
    let dead_text = dead.to_string();
    assert!(dead_text.contains("2000000001"), "{dead_text}");
    assert!(dead_text.contains("2000000002"), "{dead_text}");
    assert!(
        !dead_text.contains("alive-no-numeric-suffix"),
        "non-pid principals never classify as dead: {dead_text}"
    );
}

fn as_count(v: &serde_json::Value) -> Option<usize> {
    v.as_u64().map(|n| n as usize)
}
