//! Known-homes catalog: runtime-dir SQLite persistence of every directory
//! the daemon has opened or declared, surviving generations — the data
//! behind the desktop home picker's "known but closed" section.
//!
//! Coverage:
//! - startup opens AND declares are both recorded;
//! - a daemon RESTART preserves the catalog and refreshes
//!   last_home_id/last_seen_at (first_seen_at preserved);
//! - `home/list-known` annotates open/missing per entry;
//! - deleted directories surface missing:true and are NOT pruned;
//! - mcp-scoped credentials are refused (home operation family, R7).

#![allow(dead_code)]

mod common;

use common::{connected_client, DaemonProcess, TestCtx};
use serde_json::json;
use std::time::{Duration, Instant};

fn wait_endpoint(ctx: &TestCtx, proc: &DaemonProcess) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(d) = common::Descriptor::read(&ctx.runtime_dir) {
            // Descriptor write precedes listener accept in the boot window —
            // require a successful connect before returning the endpoint.
            if std::os::unix::net::UnixStream::connect(&d.endpoint).is_ok() {
                return d.endpoint;
            }
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

fn list_known(endpoint: &str) -> Vec<serde_json::Value> {
    let (mut client, cred) = connected_client(endpoint, "dsh").expect("adapter client");
    let result = client
        .call("home/list-known", common::authed(json!({}), &cred))
        .expect("list-known");
    result["homes"].as_array().expect("homes array").clone()
}

#[test]
fn opens_and_declares_are_recorded_and_survive_restart() {
    let ctx = TestCtx::spawn_named("known-homes");
    let declared = ctx.dir.path().join("declared-home");
    std::fs::create_dir_all(&declared).expect("mkdir declared");

    // Generation 1: startup open (workspace home from TestCtx) + declare.
    let mut proc = DaemonProcess::spawn_with_env(
        &ctx,
        &["--home", ctx.home_str()],
        &[("OMT_HOME", ctx.global_home_str().to_string())],
    );
    let endpoint = wait_endpoint(&ctx, &proc);
    {
        let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");
        client
            .call(
                "home/declare",
                common::authed(json!({ "path": declared.to_string_lossy() }), &cred),
            )
            .expect("declare");
    }

    let first = list_known(&endpoint);
    let paths: Vec<&str> = first.iter().filter_map(|h| h["path"].as_str()).collect();
    let canonical_declared = std::fs::canonicalize(&declared).expect("canonicalize");
    assert!(
        paths
            .iter()
            .any(|p| *p == canonical_declared.to_string_lossy()),
        "declared home recorded: {paths:?}"
    );
    let canonical_home = std::fs::canonicalize(&ctx.home).expect("canonicalize home");
    let startup_entry = first
        .iter()
        .find(|h| h["path"].as_str() == Some(canonical_home.to_string_lossy().as_ref()))
        .expect("startup home recorded");
    assert_eq!(startup_entry["open"].as_bool(), Some(true));
    assert_eq!(startup_entry["missing"].as_bool(), Some(false));
    let first_seen = startup_entry["firstSeenAt"]
        .as_str()
        .expect("firstSeenAt")
        .to_string();
    let gen1_seen = startup_entry["lastSeenAt"]
        .as_str()
        .expect("lastSeenAt")
        .to_string();

    proc.kill();
    // Wait for the descriptor to go stale before respawning.
    let deadline = Instant::now() + Duration::from_secs(5);
    while common::Descriptor::read(&ctx.runtime_dir).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    // Generation 2: catalog persists; restarted open refreshes
    // last_home_id/last_seen_at, preserves first_seen_at.
    let mut proc2 = DaemonProcess::spawn_with_env(
        &ctx,
        &["--home", ctx.home_str()],
        &[("OMT_HOME", ctx.global_home_str().to_string())],
    );
    let endpoint2 = wait_endpoint(&ctx, &proc2);
    let second = list_known(&endpoint2);
    let restarted = second
        .iter()
        .find(|h| h["path"].as_str() == Some(canonical_home.to_string_lossy().as_ref()))
        .expect("startup home still cataloged after restart");
    assert_eq!(
        restarted["firstSeenAt"].as_str(),
        Some(first_seen.as_str()),
        "first_seen preserved across generations"
    );
    assert_ne!(
        restarted["lastSeenAt"].as_str(),
        Some(gen1_seen.as_str()),
        "last_seen_at refreshes on reopen"
    );
    // The declared home was never opened in generation 2 → open:false.
    let declared_entry = second
        .iter()
        .find(|h| h["path"].as_str() == Some(canonical_declared.to_string_lossy().as_ref()))
        .expect("declared home persists in catalog");
    assert_eq!(
        declared_entry["open"].as_bool(),
        Some(false),
        "declared-but-unopened entry"
    );

    proc2.kill();
}

#[test]
fn missing_directories_are_flagged_not_pruned() {
    let ctx = TestCtx::spawn_named("known-missing");
    let gone = ctx.dir.path().join("gone-home");
    std::fs::create_dir_all(&gone).expect("mkdir gone");

    let mut proc = DaemonProcess::spawn_with_env(
        &ctx,
        &["--home", ctx.global_home_str()],
        &[("OMT_HOME", ctx.global_home_str().to_string())],
    );
    let endpoint = wait_endpoint(&ctx, &proc);
    {
        let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");
        client
            .call(
                "home/declare",
                common::authed(json!({ "path": gone.to_string_lossy() }), &cred),
            )
            .expect("declare");
    }
    // Remove the directory AFTER a successful declare.
    std::fs::remove_dir_all(&gone).expect("remove home dir");

    let entries = list_known(&endpoint);
    let canonical_gone = std::fs::canonicalize(ctx.dir.path())
        .expect("canon")
        .join("gone-home");
    let entry = entries
        .iter()
        .find(|h| h["path"].as_str() == Some(canonical_gone.to_string_lossy().as_ref()))
        .expect("deleted home still cataloged");
    assert_eq!(entry["missing"].as_bool(), Some(true), "missing flagged");
    proc.kill();
}

#[test]
fn mcp_scoped_credentials_are_refused() {
    let ctx = TestCtx::spawn_named("known-mcp");
    let mut proc = DaemonProcess::spawn_with_env(
        &ctx,
        &["--home", ctx.global_home_str()],
        &[("OMT_HOME", ctx.global_home_str().to_string())],
    );
    let endpoint = wait_endpoint(&ctx, &proc);

    let mut client = common::TestClient::connect(&endpoint).expect("connect");
    let handshake = client
        .call(
            "handshake/request",
            json!({
                "protocolVersion": "1.0",
                "client": { "kind": "mcp", "name": "known-homes-spec" },
                "requestedScopes": { "operations": ["node", "run", "events"] },
            }),
        )
        .expect("scoped handshake");
    let token = handshake["credential"]["token"]
        .as_str()
        .expect("token")
        .to_string();

    let denial = client
        .call(
            "home/list-known",
            json!({ "credential": { "token": token } }),
        )
        .expect_err("mcp credential must be refused");
    let text = format!("{denial:?}");
    assert!(text.contains("FORBIDDEN"), "{text}");
    assert!(text.contains("operation-not-granted"), "{text}");
    proc.kill();
}
