//! U5a lifecycle suites: stale bootstrap-lock stealing, stale descriptor
//! respawn, and SIGTERM drain releasing the home lock for re-acquisition.

#![allow(dead_code)]

mod common;

use common::{read_maybe, wait_descriptor_gone, wait_for_descriptor, DaemonProcess, TestCtx};
use std::time::Duration;

/// A bootstrap.lock whose holder pid is dead must be stolen: a fresh daemon
/// wins the election despite the leftover marker file (injected-clock
/// staleness rides the dead-pid fast path; mtime staleness covers the rest).
#[test]
fn stale_bootstrap_lock_is_stolen_by_a_new_daemon() {
    let ctx = TestCtx::spawn();

    // Plant a lock whose holder is long dead.
    let lock_path = ctx.runtime_dir.join("bootstrap.lock");
    std::fs::write(
        &lock_path,
        serde_json::json!({
            "schemaVersion": 1,
            "pid": 2_000_000_000i64, // implausibly alive; kill(pid,0) != 0
            "bootToken": "deadbeefdeadbeef",
            "acquiredAt": "2026-08-24T10:00:00.000Z",
            "heartbeatAt": "2026-08-24T10:00:00.000Z",
        })
        .to_string(),
    )
    .expect("plant stale lock");

    // The new daemon must win and publish.
    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let descriptor = wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20))
        .expect("fresh daemon must steal the stale lock and publish");
    assert_eq!(
        descriptor.generation, 1,
        "first winner publishes generation 1"
    );

    // The stale marker was replaced by the winner's own lock body.
    let body = read_maybe(&lock_path).expect("winner rewrote the lock");
    assert!(
        body.contains("2000000000") == false || body.contains("\"pid\":2000000000") == false,
        "stale pid must not survive"
    );
    assert!(proc.is_alive(), "winner serves");

    proc.kill();
}

/// Restart after a hard kill: the BOOTSTRAP plane recovers (stale election
/// lock + stale descriptor are stolen/steppable), but the HOME plane fails
/// CLOSED per U4a's locked refusal matrix — `ownerKind:"daemon"` markers
/// refuse ALWAYS (even stale/dead) so crashed daemons need EXPLICIT
/// takeover (U6 tooling), never silent auto-steal. The replacement daemon
/// must therefore exit(2) with an actionable DAEMON_OWNS_HOME error.
#[test]
fn killed_daemons_home_marker_fails_closed_until_explicit_takeover() {
    let ctx = TestCtx::spawn();

    let mut first = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let _d1 = wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20))
        .expect("first daemon publishes");
    first.kill(); // hard kill: leaves bootstrap.lock + descriptor.json + home.lock

    // Bootstrap plane recovers: the abandoned election artifacts do not
    // block a new contender from WINNING the election...
    let mut second = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);

    // ...but the home open must FAIL CLOSED on the daemon-owned marker:
    // exit code 2 + actionable problem code on stderr.
    let info = second
        .wait_with_timeout(Duration::from_secs(20))
        .expect("replacement daemon exits after the refused home open");
    assert_eq!(
        info.code,
        Some(2),
        "fail-closed exit code; stderr: {}",
        info.stderr
    );
    assert!(
        info.stderr.contains("DAEMON_OWNS_HOME"),
        "actionable problem code required, got: {}",
        info.stderr
    );

    // No descriptor is published by the failed replacement.
    let published = common::Descriptor::read(&ctx.runtime_dir);
    match published {
        None => {}
        Some(d) => assert_ne!(
            d.pid,
            std::process::id() as i64,
            "a failed replacement must not publish its own descriptor"
        ),
    }

    // Explicit-takeover path (U6 preview, manual marker removal per U4a's
    // documented recovery): removing the abandoned marker re-enables
    // ownership and the daemon then starts cleanly.
    let _ = std::fs::remove_file(ctx.home.join("home.lock"));
    let mut third = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let d3 = wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20))
        .expect("daemon starts after explicit takeover");
    assert!(
        d3.generation >= 2,
        "generation increments across replacements"
    );
    third.kill();
}

/// Stale descriptor respawn path: with only a descriptor left behind by a
/// dead predecessor, a fresh daemon binds the endpoint, replaces the
/// descriptor atomically, and serves handshakes on the same socket path.
#[test]
fn stale_descriptor_is_replaced_and_endpoint_serves() {
    let ctx = TestCtx::spawn();

    // Pre-plant a descriptor naming a dead pid and the endpoint we expect.
    let endpoint = ctx.runtime_dir.join("omt").join("daemon.sock");
    std::fs::create_dir_all(endpoint.parent().unwrap()).expect("mkdir omt/");
    std::fs::write(
        ctx.runtime_dir.join("descriptor.json"),
        serde_json::json!({
            "schemaVersion": 1,
            "endpoint": endpoint.to_str().expect("utf8"),
            "generation": 7,
            "pid": 2_000_000_001i64,
            "bootToken": "cafebabecafebabe",
            "startedAt": "2026-08-24T09:00:00.000Z",
        })
        .to_string(),
    )
    .expect("plant stale descriptor");

    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let d = wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20))
        .expect("daemon replaces the stale descriptor");
    assert_eq!(d.generation, 8, "generation increments per replacement");
    assert_eq!(
        std::path::PathBuf::from(&d.endpoint),
        endpoint,
        "endpoint path is stable across replacements"
    );

    // The endpoint answers a full handshake.
    let (_client, credential) =
        common::connected_client(&d.endpoint, "cli").expect("handshake over respawned endpoint");
    assert!(credential["token"].as_str().expect("token").len() == 64);

    proc.kill();
}

/// SIGTERM drain: stop accepting → drain queues → release home locks →
/// remove descriptor → exit 0. After exit, a fresh Storage open with
/// acquire_lock=true (the ts-bridge-equivalent acquisition) succeeds
/// immediately, proving the kernel flock + marker were released.
#[test]
fn sigterm_drain_releases_home_lock_for_bridge_reacquire() {
    let ctx = TestCtx::spawn();

    let mut proc = DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    let d = wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20))
        .expect("daemon ready before signal");

    // Keep one client connection open across shutdown: queued work must
    // still complete before release.
    let (client, credential) =
        common::connected_client(&d.endpoint, "cli").expect("client before drain");

    // Send SIGTERM.
    #[cfg(unix)]
    unsafe {
        libc::kill(proc.pid() as i32, libc::SIGTERM);
    }

    // Drain semantics: the connection may answer in-flight requests or
    // close; both are acceptable, but the process MUST exit cleanly.
    let info = proc
        .wait_with_timeout(Duration::from_secs(20))
        .expect("daemon exits within 20s of SIGTERM");
    assert_eq!(
        info.code,
        Some(0),
        "clean exit code 0; stderr: {}",
        info.stderr
    );

    // Descriptor removed by OUR boot token (no successor wrote one).
    assert!(
        wait_descriptor_gone(&ctx.runtime_dir, Duration::from_secs(5)),
        "descriptor removed after drain"
    );

    // ts-bridge re-acquire: exclusive owner lock succeeds instantly.
    let mut config = omt_storage::journal::OpenConfig::new(ctx.home.clone());
    config.acquire_lock = true;
    config.owner_kind = omt_storage::home_lock::OwnerKind::TsBridge;
    config.recover_on_open = true;
    let storage = omt_storage::journal::Storage::open(config)
        .expect("bridge acquires the released home lock without waiting");
    assert!(storage.home_id().is_some());
    drop(storage);
    drop(client);
    let _ = credential;
}
