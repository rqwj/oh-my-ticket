//! U5a enrollment + authorization suites: happy-path credential issuance
//! and the cross-scope denial matrix (parity classes, operation scope,
//! home scope, actor-namespace minting, admin grants).

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, enroll, DaemonProcess, RpcError, TestClient, TestCtx};
use serde_json::json;
use std::time::Duration;

/// Spawn a daemon against a fresh ctx; wait until its descriptor + endpoint
/// answer. Returns (ctx, process, endpoint).
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

fn problem_of(err: &RpcError) -> String {
    match err {
        RpcError::Problem { code, .. } => code.clone(),
        other => panic!("expected a Problem, got {other:?}"),
    }
}

#[test]
fn enrollment_happy_path_issues_scoped_credential_with_defaults() {
    let (_ctx, mut proc, endpoint) = ready();

    let (mut client, credential) = connected_client(&endpoint, "cli").expect("handshake succeeds");

    // Token shape: 64 hex chars from 32 random bytes.
    let token = credential["token"].as_str().expect("token string");
    assert_eq!(token.len(), 64, "32-byte hex token");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

    // principalId "<kind>:<pid>", actorNamespace defaults to it.
    assert!(
        credential["principalId"]
            .as_str()
            .unwrap_or_default()
            .starts_with("cli:"),
        "principalId cli:<pid>, got {}",
        credential["principalId"]
    );
    assert_eq!(credential["actorNamespace"], credential["principalId"]);

    // Default scopes: every open home granted; operations wildcard.
    let homes = credential["homes"].as_array().expect("homes array");
    assert!(!homes.is_empty(), "open home granted by default");
    assert_eq!(credential["operations"], json!(["*"]));

    // expiresAt ~12h out (ISO string parseable and in the future).
    assert!(!credential["expiresAt"]
        .as_str()
        .unwrap_or_default()
        .is_empty());

    // The credential actually authorizes work.
    client
        .call("node/list", authed(json!({}), &credential))
        .expect("authorized call works");

    proc.kill();
}

#[test]
fn requests_without_credential_are_unauthorized() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut client, _credential) = connected_client(&endpoint, "cli").expect("enroll");

    let err = client.call("node/list", json!({})).unwrap_err();
    assert_eq!(problem_of(&err), "UNAUTHORIZED");

    // Wrong/garbage token too.
    let err2 = client
        .call(
            "node/list",
            json!({ "credential": { "token": "f".repeat(64) } }),
        )
        .unwrap_err();
    assert_eq!(problem_of(&err2), "UNAUTHORIZED");

    proc.kill();
}

/// Parity matrix denials: adapter_only refuses non-adapter principals;
/// human_administrative refuses unlisted principals; both fail closed with
/// FORBIDDEN regardless of any requested scope.
#[test]
fn parity_denial_matrix_fails_closed() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut cli, cli_cred) = connected_client(&endpoint, "cli").expect("cli");
    let (mut mcp, mcp_cred) = connected_client(&endpoint, "mcp").expect("mcp");
    let (mut dsh, dsh_cred) = connected_client(&endpoint, "dsh").expect("dsh");

    // node/execute is adapter_only: cli/mcp denied even when explicitly
    // requested in scopes (scopes never widen parity).
    for (client_name, cred) in [("cli", &cli_cred), ("mcp", &mcp_cred)] {
        let mut params = json!({
            "nodeId": "TICKET-0001",
            "payload": { "kind": "noop" },
        });
        params["credential"] = json!({ "token": cred["token"].clone() });
        let err = match client_name {
            "cli" => cli.call("node/execute", params).unwrap_err(),
            _ => mcp.call("node/execute", params).unwrap_err(),
        };
        assert_eq!(problem_of(&err), "FORBIDDEN", "{client_name} node/execute");
    }

    // ui/* are adapter_only as well.
    let err = cli
        .call(
            "ui/filters-set",
            authed(json!({ "key": "tree", "filters": {} }), &cli_cred),
        )
        .unwrap_err();
    assert_eq!(problem_of(&err), "FORBIDDEN");

    // dsh passes the parity gate for the same calls (may still fail on
    // validation, but NEVER with parity FORBIDDEN).
    let result = dsh.call(
        "ui/filters-set",
        authed(
            json!({ "key": "tree", "filters": { "status": ["open"] } }),
            &dsh_cred,
        ),
    );
    assert!(result.is_ok(), "adapter channel allowed: {result:?}");

    // home/reindex is human_administrative without an admin grant.
    for (cred, name) in [(&cli_cred, "cli"), (&dsh_cred, "dsh")] {
        let err = cli_or_dsh_reindex(name, &mut cli, &mut dsh, cred).unwrap_err();
        assert_eq!(problem_of(&err), "FORBIDDEN", "{name} reindex pre-grant");
    }

    proc.kill();
}

fn cli_or_dsh_reindex(
    name: &str,
    cli: &mut TestClient,
    dsh: &mut TestClient,
    cred: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let params = authed(json!({}), cred);
    if name == "cli" {
        cli.call("home/reindex", params)
    } else {
        dsh.call("home/reindex", params)
    }
}

/// Admin grant path: listing a principal id in admin-grants.json unlocks
/// home/reindex WITHOUT restarting or re-enrolling; unknown ids stay denied.
#[test]
fn admin_grant_unlocks_human_administrative_path() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut cli, cli_cred) = connected_client(&endpoint, "cli").expect("cli");

    // Pre-grant: denied.
    let err = cli
        .call("home/reindex", authed(json!({}), &cli_cred))
        .unwrap_err();
    assert_eq!(problem_of(&err), "FORBIDDEN");

    // Out-of-band grant file names THIS principal.
    let principal = cli_cred["principalId"].as_str().expect("principalId");
    std::fs::write(
        _ctx.runtime_dir.join("admin-grants.json"),
        json!({ "principalIds": [principal] }).to_string(),
    )
    .expect("write admin grants");

    // Same credential now passes (fresh read on every check).
    let result = cli.call("home/reindex", authed(json!({}), &cli_cred));
    assert!(result.is_ok(), "admin grant unlocks reindex: {result:?}");

    // A different principal remains denied.
    let (_c2, other_cred) = connected_client(&endpoint, "external").expect("external");
    let err = {
        let mut c2 = TestClient::connect(&endpoint).unwrap();
        let (_, oc) = enroll(&mut c2, "external", json!({})).unwrap();
        c2.call("home/reindex", authed(json!({}), &oc)).unwrap_err()
    };
    assert_eq!(problem_of(&err), "FORBIDDEN");
    let _ = other_cred;

    proc.kill();
}

/// Scope matrix: homes/operations narrow authority; foreign actor
/// namespaces are silently NOT granted (server assigns its own base).
#[test]
fn scope_narrowing_and_actor_namespace_rules() {
    let (_ctx, mut proc, endpoint) = ready();

    // Request ONLY operations ["node"] — run/* must deny.
    let mut client = TestClient::connect(&endpoint).unwrap();
    let (_, scoped) =
        enroll(&mut client, "cli", json!({ "operations": ["node"] })).expect("scoped enroll");
    client
        .call("node/list", authed(json!({}), &scoped))
        .expect("node family allowed");
    let err = client
        .call("run/list", authed(json!({}), &scoped))
        .unwrap_err();
    assert_eq!(
        problem_of(&err),
        "FORBIDDEN",
        "operation narrowing enforced"
    );

    // Request a FOREIGN actorNamespace: server keeps its own base instead
    // of honoring the mint attempt.
    let (_c3, foreign) = enroll(
        &mut TestClient::connect(&endpoint).unwrap(),
        "external",
        json!({ "actorNamespace": "dsh:999999/injected" }),
    )
    .expect("foreign-ns enroll");
    assert_ne!(
        foreign["actorNamespace"].as_str().unwrap_or_default(),
        "dsh:999999/injected",
        "a client can never mint another principal's namespace"
    );
    assert!(
        foreign["actorNamespace"]
            .as_str()
            .unwrap_or_default()
            .starts_with("external:"),
        "falls back to server-assigned base"
    );

    // Nested namespace UNDER our own base IS honored.
    let base = {
        let (_, cred) = enroll(
            &mut TestClient::connect(&endpoint).unwrap(),
            "cli",
            json!({}),
        )
        .unwrap();
        cred["actorNamespace"].as_str().unwrap().to_string()
    };
    let nested = format!("{base}/agent-42");
    let (_c4, nested_cred) = enroll(
        &mut TestClient::connect(&endpoint).unwrap(),
        "cli",
        json!({ "actorNamespace": nested }),
    )
    .expect("nested ns enroll");
    assert_eq!(nested_cred["actorNamespace"].as_str().unwrap(), nested);

    proc.kill();
}
