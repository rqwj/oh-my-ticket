//! U5a multi-client correctness: two clients share one temp home through
//! the per-home queue — no lost updates, no duplicate ids, idempotency-key
//! reuse fails closed CONFLICT — plus optimistic-revision gating.

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

/// Two clients interleave creates and revision-checked updates on ONE home.
/// The per-home actor serializes every job, so:
/// - all 2×N creates land with distinct node ids,
/// - interleaved expectedRevision updates never lose a write (a stale
///   expectation CONFLICTs instead of overwriting),
/// - final state equals exactly the applied writes.
#[test]
fn two_clients_interleaved_creates_and_updates_never_lose_writes() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut a, a_cred) = connected_client(&endpoint, "cli").expect("client A");
    let (mut b, b_cred) = connected_client(&endpoint, "dsh").expect("client B");

    // Shared epic → story parent chain.
    let epic = a
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "shared epic" }), &a_cred),
        )
        .expect("epic");
    let story = b
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "shared story", "parentId": epic["node"]["nodeId"] }),
                &b_cred,
            ),
        )
        .expect("story");
    let parent_id = story["node"]["nodeId"]
        .as_str()
        .expect("nodeId")
        .to_string();

    // Interleaved creates: A, B, A, B...
    let mut created = Vec::new();
    for i in 0..6 {
        let client = if i % 2 == 0 { &mut a } else { &mut b };
        let cred = if i % 2 == 0 { &a_cred } else { &b_cred };
        let result = client
            .call(
                "node/create",
                authed(
                    json!({
                        "type": "ticket",
                        "title": format!("shared ticket {i}"),
                        "parentId": parent_id,
                    }),
                    cred,
                ),
            )
            .expect("create succeeds on shared home");
        created.push(
            result["node"]["nodeId"]
                .as_str()
                .expect("nodeId")
                .to_string(),
        );
    }
    let unique: std::collections::HashSet<&String> = created.iter().collect();
    assert_eq!(
        unique.len(),
        created.len(),
        "no duplicate node ids across clients"
    );

    // Interleaved revision-checked updates on ONE node: each client must
    // read-modify-write through the current revision.
    let target = &created[0];
    for round in 0..4 {
        let client = if round % 2 == 0 { &mut a } else { &mut b };
        let cred = if round % 2 == 0 { &a_cred } else { &b_cred };
        let view = client
            .call("node/get", authed(json!({ "nodeId": target }), cred))
            .expect("get");
        let rev = view["node"]["revision"].as_i64().expect("revision");
        let result = client.call(
            "node/update",
            authed(
                json!({
                    "nodeId": target,
                    "expectedRevision": rev,
                    "changes": { "priority": round + 1 },
                }),
                cred,
            ),
        );
        assert!(
            result.is_ok(),
            "round {round} update with fresh revision lands"
        );
    }

    // A stale expectation conflicts instead of clobbering.
    let err = a
        .call(
            "node/update",
            authed(
                json!({
                    "nodeId": target,
                    "expectedRevision": 1, // long stale
                    "changes": { "priority": 99 },
                }),
                &a_cred,
            ),
        )
        .unwrap_err();
    match err {
        common::RpcError::Problem { code, details } => {
            assert_eq!(code, "CONFLICT");
            assert_eq!(details["rule"], "revision-mismatch");
        }
        other => panic!("expected CONFLICT, got {other:?}"),
    }

    // Final state: priority 4 (last accepted write), nothing lost.
    let final_view = b
        .call("node/get", authed(json!({ "nodeId": target }), &b_cred))
        .expect("final get");
    assert_eq!(final_view["node"]["priority"].as_i64(), Some(4));

    proc.kill();
}

/// Idempotency keys (R9 seed): replaying the SAME commandId returns the
/// stored result; reusing it with DIFFERENT input fails closed CONFLICT
/// command-id-reuse. Duplicate commandIds therefore never double-apply.
#[test]
fn same_command_id_replays_prior_result_and_reuse_conflicts() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");

    let command_id = "IDEMPOTENT-CMD-0001";
    // Valid epic → story parent chain.
    let epic = client
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "idem epic" }), &cred),
        )
        .expect("epic");
    let story = client
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "idem story", "parentId": epic["node"]["nodeId"] }),
                &cred,
            ),
        )
        .expect("story");
    let parent_id = story["node"]["nodeId"].as_str().unwrap().to_string();
    let first = client
        .call(
            "node/create",
            authed(
                json!({
                    "commandId": command_id,
                    "type": "ticket",
                    "title": "idempotent create",
                    "parentId": parent_id,
                }),
                &cred,
            ),
        )
        .expect("first apply");
    let first_id = first["node"]["nodeId"]
        .as_str()
        .expect("nodeId")
        .to_string();

    // Identical replay: prior result, NOT a second node.
    let replay = client
        .call(
            "node/create",
            authed(
                json!({
                    "commandId": command_id,
                    "type": "ticket",
                    "title": "idempotent create",
                    "parentId": parent_id,
                }),
                &cred,
            ),
        )
        .expect("replay");
    assert_eq!(replay["node"]["nodeId"].as_str(), Some(first_id.as_str()));

    // Same key, different input: fail closed.
    let err = client
        .call(
            "node/create",
            authed(
                json!({
                    "commandId": command_id,
                    "type": "ticket",
                    "title": "DIFFERENT input",
                }),
                &cred,
            ),
        )
        .unwrap_err();
    match err {
        common::RpcError::Problem { code, details } => {
            assert_eq!(code, "CONFLICT");
            assert_eq!(details["rule"], "command-id-reuse");
        }
        other => panic!("expected CONFLICT command-id-reuse, got {other:?}"),
    }

    // Exactly one node exists for that title.
    let list = client
        .call(
            "node/list",
            authed(json!({ "filter": { "query": "idempotent" } }), &cred),
        )
        .expect("list");
    assert_eq!(
        list["nodes"].as_array().map(|n| n.len()),
        Some(1),
        "no duplicate application behind one commandId"
    );

    proc.kill();
}
