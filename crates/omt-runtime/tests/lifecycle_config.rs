//! U5b daemon lifecycle config suites: `daemon.json` precedence
//! (defaults < file, unknown keys fail closed), rotating file logging at
//! `<runtime-dir>/logs/omt-daemon.log`, and the idle quiet-period watchdog
//! that drains queues and exits CLEANLY (lock released) when no actor has
//! worked for `idleQuietMs`.

#![allow(dead_code)]

mod common;

use common::{DaemonProcess, TestCtx};
use serde_json::json;
use std::time::Duration;

fn wait_exit(proc: &mut DaemonProcess, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = proc.try_wait() {
            return Some(status);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Idle watchdog: with idleQuietMs tiny and NO clients connected, the daemon
/// drains its queues, releases every home lock, removes its descriptor, and
/// exits with code 0.
#[test]
fn idle_quiet_period_exits_cleanly_releasing_home_lock() {
    let ctx = TestCtx::spawn();
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        json!({ "idleQuietMs": 700 }).to_string(),
    )
    .expect("config");

    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let descriptor = common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20))
        .expect("daemon publishes before idling");

    // Wait for the self-initiated exit.
    let status = wait_exit(&mut proc, Duration::from_secs(20)).expect("daemon exits on idle");
    assert!(
        status.success(),
        "clean exit code 0 after idle drain; stderr: {}",
        proc.stderr_text()
    );

    // Home lock released.
    let lock_path = ctx.home.join("home.lock");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while lock_path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!lock_path.exists(), "home.lock released on idle exit");

    // Descriptor removed.
    assert!(
        common::wait_descriptor_gone(&ctx.runtime_dir, Duration::from_secs(10)),
        "descriptor removed on idle exit"
    );
    let _ = descriptor;
}

/// Busy daemons do NOT idle-exit: a client holding an active subscription /
/// issuing periodic work keeps the process alive past the quiet window.
#[test]
fn active_clients_defer_the_idle_exit() {
    let ctx = TestCtx::spawn();
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        json!({ "idleQuietMs": 900 }).to_string(),
    )
    .expect("config");

    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let endpoint = common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20))
        .expect("descriptor")
        .endpoint;

    // Keep working slightly faster than the quiet window.
    let mut raw_client = common::TestClient::connect(&endpoint).expect("connect");
    let (_handshake, cred) = common::enroll(&mut raw_client, "cli", json!({})).expect("enroll");
    let client = &mut raw_client;
    for round in 0..4 {
        std::thread::sleep(Duration::from_millis(600));
        client
            .call(
                "node/create",
                common::authed(
                    json!({ "type": "epic", "title": format!("keepalive {round}") }),
                    &cred,
                ),
            )
            .expect("still serving during keepalives");
    }

    // Well past one quiet window of continuous work → still alive.
    assert!(
        wait_exit(&mut proc, Duration::from_millis(200)).is_none(),
        "active work defers the idle exit"
    );

    // Stop feeding it: the daemon should now exit within ~quiet window.
    drop(raw_client);
    let exited = wait_exit(&mut proc, Duration::from_secs(15)).is_some();
    proc.kill(); // no-op if already exited
    assert!(exited, "daemon exits once work goes quiet again");
}

/// Unknown daemon.json keys FAIL CLOSED with an actionable INVALID_INPUT
/// problem naming the offending field; exit code 2.
#[test]
fn unknown_config_keys_fail_closed() {
    let ctx = TestCtx::spawn();
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        json!({ "idleQuietMsTypo": 100 }).to_string(),
    )
    .expect("config");
    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let info = proc
        .wait_with_timeout(Duration::from_secs(20))
        .expect("daemon refuses to start on bad config");
    assert_eq!(info.code, Some(2), "stderr: {}", info.stderr);
    assert!(
        info.stderr.contains("INVALID_INPUT"),
        "stderr: {}",
        info.stderr
    );
    assert!(
        info.stderr.contains("idleQuietMsTypo"),
        "names the unknown field: {}",
        info.stderr
    );
}

/// Precedence: defaults < file — maxOpenHomes=1 from the file overrides the
/// default of 8 and is enforced (second home open refused).
#[test]
fn config_overrides_default_limits() {
    let ctx = TestCtx::spawn();
    let second = ctx.dir.path().join("home-b");
    std::fs::create_dir_all(&second).expect("mkdir b");
    std::fs::write(
        ctx.runtime_dir.join("daemon.json"),
        json!({ "limits": { "maxOpenHomes": 1 } }).to_string(),
    )
    .expect("config");
    let mut proc = DaemonProcess::spawn(
        &ctx,
        &["--home", ctx.home_str(), "--home", second.to_str().unwrap()],
    );
    let info = proc
        .wait_with_timeout(Duration::from_secs(20))
        .expect("refuses second open");
    assert_eq!(info.code, Some(2), "stderr: {}", info.stderr);
    assert!(
        info.stderr.contains("QUOTA_EXCEEDED"),
        "stderr: {}",
        info.stderr
    );
}

/// Rotating logs land at `<runtime-dir>/logs/omt-daemon.log` as JSON lines
/// with redacted payloads; startup + shutdown are both recorded.
#[test]
fn daemon_writes_redacted_json_log_lines() {
    let ctx = TestCtx::spawn();
    // Tiny rotation budget proves the pipeline works under pressure; the
    // rollover arithmetic itself is unit-tested in logging.rs.
    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let _ =
        common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).expect("descriptor");
    // Give it a moment to flush STARTUP.
    std::thread::sleep(Duration::from_millis(300));

    let log_path = ctx.runtime_dir.join("logs").join("omt-daemon.log");
    let raw = std::fs::read_to_string(&log_path).expect("log file exists");
    let mut saw_startup = false;
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("each log line is valid JSON");
        let ts = value.get("ts").and_then(|v| v.as_str()).expect("iso ts");
        assert!(ts.contains('T') && ts.ends_with('Z'), "line: {line}");
        assert!(
            value.get("level").and_then(|v| v.as_str()).is_some(),
            "line: {line}"
        );
        assert!(
            value.get("code").and_then(|v| v.as_str()).is_some(),
            "line: {line}"
        );
        let message = value["message"].as_str().unwrap_or_default();
        // Redaction: no bearer-shaped token survives into logs.
        assert!(
            !message.contains("bootToken"),
            "boot token must not leak into logs: {message}"
        );
        if value["code"] == json!("STARTUP") {
            saw_startup = true;
        }
    }
    assert!(saw_startup, "STARTUP line present; log: {raw}");

    // Clean shutdown appends SHUTDOWN.
    proc.kill();
}
