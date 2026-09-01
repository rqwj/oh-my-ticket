//! Contract tests for the generated OMT protocol bindings (U1).
//!
//! These tests are the verification for the unit's test scenarios:
//!
//! - round-trip validation in Rust (serialization ↔ deserialization equality);
//! - unknown-field tolerance for additive changes (both schema-level and
//!   generated-struct level);
//! - unsupported-major negotiation yields `UNSUPPORTED_PROTOCOL`;
//! - malformed `homeId` rejected (schema validator and generated newtype);
//! - error assertions key on code/details only — `message` is diagnostic.
//!
//! Schema-level assertions re-read `schema/*.schema.json` from disk through the
//! `jsonschema` crate, so the tests fail if the hand-written protocol
//! constants in `src/lib.rs` drift from the schema source of truth.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use omt_contracts::protocol::*;
use omt_contracts::{
    ArchiveNodeParams, ClaimRunResult, CreateNodeParams, EventEnvelope, EventPayload, HomeId,
    Iso8601Time, JsonRpcResponse, NodeStatus, NodeType, NodeView, Problem, ProblemCode,
    ProblemDetails, Progress, RunItemState, RunLink, RunStatus, SnapshotResyncEvent,
};
use serde_json::{json, Value};

// ── harness ───────────────────────────────────────────────────────────────

/// Absolute path of the schema source directory.
fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema")
        .canonicalize()
        .expect("schema/ exists beside crates/")
}

/// Load every schema document keyed by its file name (`common.schema.json`…).
fn load_docs() -> BTreeMap<String, Value> {
    let mut docs = BTreeMap::new();
    let mut entries: Vec<_> = fs::read_dir(schema_dir())
        .expect("schema dir is readable")
        .map(|entry| entry.expect("readdir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    for path in entries {
        let text = fs::read_to_string(&path).expect("schema file is UTF-8");
        let doc: Value = serde_json::from_str(&text).expect("schema file is valid JSON");
        docs.insert(
            path.file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned(),
            doc,
        );
    }
    assert_eq!(docs.len(), 7, "expected exactly seven schema documents");
    docs
}

fn document_uri(file: &str) -> String {
    format!("{SCHEMA_ID_BASE}{file}")
}

/// Build a registry that resolves every document under its published `$id`.
fn registry() -> jsonschema::Registry {
    let pairs = load_docs().into_iter().map(|(name, doc)| {
        (
            document_uri(&name),
            jsonschema::Resource::from_contents(doc).expect("resource from schema contents"),
        )
    });
    jsonschema::Registry::try_from_resources(pairs).expect("registry from schema documents")
}

/// Validator addressing one `$defs` entry of one document by absolute URI,
/// so cross-document `$ref`s resolve against the same registry.
fn validator_for(registry: &jsonschema::Registry, address: &str) -> jsonschema::Validator {
    let wrapper = json!({ "$ref": format!("{SCHEMA_ID_BASE}{address}") });
    jsonschema::options()
        .with_registry(registry.clone())
        .build(&wrapper)
        .expect("wrapper compiles into a validator")
}

fn assert_valid(validator: &jsonschema::Validator, instance: &Value, label: &str) {
    if let Err(error) = validator.validate(instance) {
        panic!("{label} should validate: {error}");
    }
}

fn assert_invalid(validator: &jsonschema::Validator, instance: &Value, label: &str) {
    assert!(
        !validator.is_valid(instance),
        "{label} should be REJECTED; instance = {instance}"
    );
}

// ── sample payloads shared across tests ───────────────────────────────────

const HOME: &str = "h_abc123";

fn sample_node_view() -> Value {
    json!({
        "homeId": HOME,
        "nodeId": "TICKET-0001",
        "type": "ticket",
        "title": "Ship the contract crate",
        "status": "in_progress",
        "archived": false,
        "priority": 0,
        "path": ".omt/tickets/TICKET-0001.md",
        "revision": 3,
        "createdAt": "2026-08-24T10:30:00Z",
        "updatedAt": "2026-08-24T11:00:00Z"
    })
}

fn sample_create_params() -> Value {
    json!({
        "homeId": HOME,
        "type": "story",
        "title": "Port the domain crate",
        "parentId": "EPIC-0001",
        "body": "# Story\n\nPort it.",
        "priority": 2
    })
}

// ── valid payloads pass their schemas ─────────────────────────────────────

#[test]
fn valid_create_node_params_passes_schema() {
    let reg = registry();
    let create = validator_for(&reg, "commands.schema.json#/$defs/CreateNodeParams");
    assert_valid(&create, &sample_create_params(), "valid node/create params");
    // Minimal variant too: only required fields.
    assert_valid(
        &create,
        &json!({ "homeId": HOME, "type": "epic", "title": "Root epic" }),
        "minimal node/create params",
    );
}

#[test]
fn valid_node_view_passes_schema() {
    let reg = registry();
    let view = validator_for(&reg, "common.schema.json#/$defs/NodeView");
    assert_valid(&view, &sample_node_view(), "valid NodeView");
}

#[test]
fn valid_problem_passes_schema() {
    let reg = registry();
    let problem = validator_for(&reg, "common.schema.json#/$defs/Problem");
    assert_valid(
        &problem,
        &json!({
            "code": "CONFLICT",
            "details": { "expectedRevision": 4, "actualRevision": 7 },
            "message": "diagnostic text only"
        }),
        "valid Problem with message",
    );
}

// ── malformed homeId is rejected ──────────────────────────────────────────

#[test]
fn malformed_home_id_rejected_by_schema() {
    let reg = registry();
    let home_id = validator_for(&reg, "common.schema.json#/$defs/HomeId");
    for bad in [
        "HOMEABC", "h_", "h_ABC123", "abc123", "h_upper!", "h_ abc12",
    ] {
        assert_invalid(&home_id, &json!(bad), &format!("malformed homeId {bad:?}"));
    }
    // The documented shape passes: h_ plus at least six lowercase alphanumerics.
    assert_valid(&home_id, &json!(HOME), "well-formed homeId");

    // And a command carrying a malformed homeId fails as a whole.
    let create = validator_for(&reg, "commands.schema.json#/$defs/CreateNodeParams");
    let mut bad_payload = sample_create_params();
    bad_payload["homeId"] = json!("NOT_QUALIFIED");
    assert_invalid(
        &create,
        &bad_payload,
        "node/create with bare non-qualified id",
    );
}

#[test]
fn malformed_home_id_rejected_by_generated_newtype() {
    // typify lowers pattern-constrained strings to newtypes whose FromStr and
    // Deserialize enforce the pattern via regress.
    assert!("NOT_QUALIFIED".parse::<HomeId>().is_err());
    assert!("h_UPPER".parse::<HomeId>().is_err());
    assert!(HOME.parse::<HomeId>().is_ok());

    let params = json!({
        "homeId": "bare-TICKET-0001-id",
        "type": "ticket",
        "title": "x"
    });
    let parsed: Result<CreateNodeParams, _> = serde_json::from_value(params);
    assert!(
        parsed.is_err(),
        "deserializing a malformed homeId must fail"
    );
}

// ── unknown-field tolerance (additive changes) ────────────────────────────

#[test]
fn unknown_fields_tolerated_at_schema_level() {
    let reg = registry();
    let view = validator_for(&reg, "common.schema.json#/$defs/NodeView");
    let mut future = sample_node_view();
    future["brandNewField"] = json!({ "anything": [1, 2, 3] });
    assert_valid(&view, &future, "NodeView with additive unknown field");

    let create = validator_for(&reg, "commands.schema.json#/$defs/CreateNodeParams");
    let mut future_create = sample_create_params();
    future_create["idempotencyKey"] = json!("opaque-key");
    assert_valid(
        &create,
        &future_create,
        "params with additive unknown field",
    );
}

#[test]
fn unknown_fields_ignored_by_generated_structs() {
    let mut future = sample_node_view();
    future["futureField"] = json!("added in v2");
    let view: NodeView = serde_json::from_value(future).expect("unknown fields are ignored");
    assert_eq!(view.node_id.as_str(), "TICKET-0001");
    assert_eq!(view.status, NodeStatus::InProgress);
    // Re-serializing drops what the struct does not know — the owned shape.
    let round: Value = serde_json::to_value(&view).expect("serialize NodeView");
    assert_eq!(round["nodeId"], json!("TICKET-0001"));
    assert!(
        round.get("futureField").is_none(),
        "unknowns are not adopted"
    );
}

// ── unsupported-major negotiation → UNSUPPORTED_PROTOCOL ──────────────────

#[test]
fn unsupported_major_constant_matches_registry() {
    // The semantic constant matches both the seed enum in problems.schema.json
    // and its registry entry.
    assert_eq!(UNSUPPORTED_PROTOCOL_CODE, "UNSUPPORTED_PROTOCOL");
    let docs = load_docs();

    let seeds = docs["problems.schema.json"]["$defs"]["SeedProblemCodes"]["enum"]
        .as_array()
        .expect("SeedProblemCodes enum");
    assert!(
        seeds.iter().any(|v| v == UNSUPPORTED_PROTOCOL_CODE),
        "UNSUPPORTED_PROTOCOL must be part of the seeded code set"
    );

    let descriptor = &docs["problems.schema.json"]["$defs"]["SeedProblemRegistry"]["properties"]
        ["UNSUPPORTED_PROTOCOL"];
    assert_eq!(
        descriptor["description"].as_str().unwrap_or_default(),
        "Handshake or request used a protocol MAJOR the peer does not support."
    );
    assert!(
        descriptor["$ref"].as_str().is_some(),
        "descriptor must reference the shared ProblemDescriptor shape"
    );

    // Every seeded code has a descriptor registered next to it.
    for code in SEED_PROBLEM_CODES {
        assert!(
            docs["problems.schema.json"]["$defs"]["SeedProblemRegistry"]["properties"]
                .get(code)
                .is_some(),
            "missing registry descriptor for {code}"
        );
    }
}

#[test]
fn unsupported_major_carries_problem_over_the_wire() {
    // Simulate the negotiation failure response a peer must produce for a
    // request advertising an unsupported MAJOR: a JSON-RPC error whose data is
    // a Problem carrying the stable code. Round-trips through generated types.
    let code: ProblemCode = UNSUPPORTED_PROTOCOL_CODE
        .parse()
        .expect("code matches pattern");
    let response = JsonRpcResponse::Variant1 {
        id: omt_contracts::RequestId::Variant0(7),
        jsonrpc: "2.0".parse().expect("jsonrpc literal"),
        error: omt_contracts::JsonRpcError {
            code: -32000,
            message: "diagnostic only".to_string(),
            data: Some(Problem {
                code,
                details: ProblemDetails(
                    [
                        ("supportedMajor".to_string(), json!(PROTOCOL_MAJOR)),
                        ("requestedVersion".to_string(), json!("9.0")),
                    ]
                    .into_iter()
                    .collect(),
                ),
                message: None,
            }),
        },
    };
    let value = serde_json::to_value(&response).expect("serialize response");
    assert_eq!(value["error"]["data"]["code"], UNSUPPORTED_PROTOCOL_CODE);
    let back: JsonRpcResponse = serde_json::from_value(value).expect("deserialize response");
    match back {
        JsonRpcResponse::Variant1 { error, .. } => {
            let data = error.data.expect("problem present");
            assert_eq!(data.code.as_str(), UNSUPPORTED_PROTOCOL_CODE);
            assert_eq!(
                data.details.0.get("supportedMajor"),
                Some(&json!(PROTOCOL_MAJOR))
            );
        }
        _ => panic!("wrong response variant"),
    }

    // A well-formed version string with an unshared MAJOR still parses as a
    // ProtocolVersion — rejection is a policy answer (the problem above), not
    // a syntax error.
    let reg = registry();
    let pv = validator_for(&reg, "common.schema.json#/$defs/ProtocolVersion");
    assert_valid(&pv, &json!("9.0"), "well-formed future major");
    assert_invalid(&pv, &json!("nine"), "non-numeric version rejected");
    assert_eq!(PROTOCOL_VERSION, "1.0");
}

// ── errors assert on code/details only ────────────────────────────────────

#[test]
fn problem_requires_code_and_details_but_not_message() {
    let reg = registry();
    let problem = validator_for(&reg, "common.schema.json#/$defs/Problem");

    assert_valid(
        &problem,
        &json!({ "code": "NOT_FOUND", "details": {} }),
        "message is optional (diagnostic only)",
    );
    assert_invalid(
        &problem,
        &json!({ "code": "NOT_FOUND", "message": "no details" }),
        "details are required",
    );
    assert_invalid(
        &problem,
        &json!({ "details": {}, "message": "no code" }),
        "code is required",
    );

    // Generated type mirrors this: message optional.
    let typed: Problem =
        serde_json::from_value(json!({ "code": "IO", "details": { "path": "/x" } }))
            .expect("problem without message");
    assert!(typed.message.is_none());
    assert_eq!(typed.code.as_str(), "IO");
}

// ── round-trip fidelity of generated types ────────────────────────────────

#[test]
fn node_view_round_trips_with_camel_case_wire_names() {
    let original: NodeView =
        serde_json::from_value(sample_node_view()).expect("sample NodeView parses");
    let serialized = serde_json::to_value(&original).expect("NodeView serializes");
    assert_eq!(
        serialized,
        sample_node_view(),
        "camelCase wire form preserved"
    );
    let back: NodeView = serde_json::from_value(serialized).expect("re-parse");
    assert_eq!(original.status, back.status);
    assert_eq!(*back.revision, *original.revision);
    assert_eq!(back.home_id.as_str(), HOME);
    assert_eq!(format!("{}", back.type_), "ticket");
}

#[test]
fn archive_command_round_trips() {
    let params = ArchiveNodeParams {
        home_id: HOME.parse().expect("home id"),
        node_id: "STORY-0012".parse().expect("node id"),
    };
    let value = serde_json::to_value(&params).expect("serialize");
    assert_eq!(value, json!({ "homeId": HOME, "nodeId": "STORY-0012" }));
    let back: ArchiveNodeParams = serde_json::from_value(value).expect("deserialize");
    assert_eq!(back.node_id.as_str(), "STORY-0012");
}

#[test]
fn event_envelope_round_trips_attention_payload() {
    let attention = json!({
        "cursor": 42,
        "homeId": HOME,
        "type": "attention.raised",
        "occurredAt": "2026-08-24T12:00:00Z",
        "payload": {
            "kind": "attention.raised",
            "action": "review.awaiting_confirmation",
            "ref": { "homeId": HOME, "runId": "RUN-0007" },
            "reason": "executor finished without a report",
            "deadline": "2026-08-25T09:00:00Z",
            "recoveryOptions": ["human confirm", "run/control retry"]
        }
    });

    let reg = registry();
    let envelope = validator_for(&reg, "events.schema.json#/$defs/EventEnvelope");
    assert_valid(&envelope, &attention, "attention envelope validates");

    let typed: EventEnvelope =
        serde_json::from_value(attention.clone()).expect("envelope deserializes");
    match &typed.payload {
        EventPayload::AttentionRaisedEvent(payload) => {
            assert_eq!(payload.action.as_str(), "review.awaiting_confirmation");
            assert!(matches!(
                payload.ref_,
                omt_contracts::AttentionRaisedEventRef::RunRef(_)
            ));
        }
        other => panic!("unexpected payload variant: {other:?}"),
    }
    let round: Value = serde_json::to_value(&typed).expect("envelope serializes");
    assert_eq!(round, attention, "attention envelope round-trips exactly");
}

#[test]
fn event_envelope_rejects_unknown_type_and_mismatched_kind_shape() {
    let reg = registry();
    let envelope = validator_for(&reg, "events.schema.json#/$defs/EventEnvelope");

    let base = json!({
        "cursor": 1,
        "homeId": HOME,
        "type": "node.changed",
        "occurredAt": "2026-08-24T12:00:00Z",
        "payload": {
            "kind": "node.changed",
            "ref": { "homeId": HOME, "nodeId": "EPIC-0002" },
            "change": "created"
        }
    });
    assert_valid(&envelope, &base, "node.changed envelope");

    let mut bad_type = base.clone();
    bad_type["type"] = json!("galaxy.brain");
    assert_invalid(&envelope, &bad_type, "unknown event type rejected");

    let mut bad_change = base;
    bad_change["payload"]["change"] = json!("teleported");
    assert_invalid(&envelope, &bad_change, "unknown change kind rejected");
}

#[test]
fn snapshot_resync_event_round_trips() {
    let payload = EventPayload::SnapshotResyncEvent(SnapshotResyncEvent {
        home_id: HOME.parse().unwrap(),
        kind: "snapshot.resync".parse().expect("kind literal"),
        reason: "retention expired".to_string(),
        // U5 additive keying fields (optional on the wire).
        pruned_through_seq: None,
        consumer_cursor: None,
    });
    let envelope = EventEnvelope {
        cursor: omt_contracts::EventCursor::from(9u64),
        home_id: HOME.parse().unwrap(),
        type_: omt_contracts::EventType::SnapshotResync,
        occurred_at: Iso8601Time::from("2026-08-24T00:00:00Z".to_string()),
        payload,
    };
    let value = serde_json::to_value(&envelope).expect("serialize");
    assert_eq!(value["type"], "snapshot.resync");
    assert_eq!(value["payload"]["kind"], "snapshot.resync");
    let back: EventEnvelope = serde_json::from_value(value).expect("deserialize");
    assert_eq!(back.cursor.0, 9u64);
}

// ── run vocabulary shapes ─────────────────────────────────────────────────

#[test]
fn claim_result_minimal_and_claimed_forms_validate() {
    let reg = registry();
    let claim = validator_for(&reg, "commands.schema.json#/$defs/ClaimRunResult");
    assert_valid(
        &claim,
        &json!({ "homeId": HOME, "runId": "RUN-0001", "claimed": false }),
        "empty-claim result",
    );

    let claimed = json!({
        "homeId": HOME,
        "runId": "RUN-0001",
        "claimed": true,
        "lease": {
            "token": "lease-token-value-0001",
            "attempt": 1,
            "principal": "adapter:dsh/session-42",
            "expiresAt": "2026-08-24T13:00:00Z"
        },
        "item": {
            "homeId": HOME,
            "runId": "RUN-0001",
            "nodeId": "TICKET-0009",
            "position": 2,
            "state": "running",
            "attempts": 0
        }
    });
    assert_valid(&claim, &claimed, "claimed result with lease");

    let typed: ClaimRunResult = serde_json::from_value(claimed).expect("parse claimed result");
    assert!(typed.claimed);
    assert_eq!(
        typed.lease.as_ref().expect("lease").attempt,
        std::num::NonZeroU64::new(1).unwrap()
    );
    assert_eq!(
        typed.item.as_ref().expect("item").state,
        RunItemState::Running
    );
}

#[test]
fn run_control_retry_requires_node_id_at_schema_level() {
    let reg = registry();
    let control = validator_for(&reg, "commands.schema.json#/$defs/RunControlParams");
    assert_valid(
        &control,
        &json!({ "homeId": HOME, "runId": "RUN-0001", "action": "pause" }),
        "pause needs no nodeId",
    );
    assert_valid(
        &control,
        &json!({ "homeId": HOME, "runId": "RUN-0001", "action": "retry", "nodeId": "TICKET-0002" }),
        "retry names its member",
    );
    assert_invalid(
        &control,
        &json!({ "homeId": HOME, "runId": "RUN-0001", "action": "retry" }),
        "retry without nodeId rejected",
    );
    assert_invalid(
        &control,
        &json!({ "homeId": HOME, "runId": "RUN-0001", "action": "warp" }),
        "unknown action rejected",
    );
}

// ── handshake + capabilities ──────────────────────────────────────────────

#[test]
fn handshake_round_trip_negotiates_shared_version() {
    let reg = registry();
    let params = validator_for(&reg, "capabilities.schema.json#/$defs/HandshakeParams");
    let result = validator_for(&reg, "capabilities.schema.json#/$defs/HandshakeResult");

    let request = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "client": { "kind": "cli", "name": "@omt/client-ts", "version": "0.1.0" },
        "requestedScopes": { "actorNamespace": "cli:4242" },
        "capabilities": ["events.resume"]
    });
    assert_valid(&params, &request, "handshake request");
    assert_invalid(
        &params,
        &json!({ "protocolVersion": "1.0", "client": {} }),
        "client kind+name required",
    );
    // U5b: kind is part of the principal identity (R22 parity classes).
    assert_invalid(
        &params,
        &json!({ "protocolVersion": "1.0", "client": { "name": "no-kind" } }),
        "client kind required",
    );

    let reply = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "daemon": { "name": "omt-daemon", "version": "0.1.0" },
        "homes": [
            { "homeId": HOME, "name": "workspace", "kind": "workspace" },
            { "homeId": "h_global", "name": "global", "kind": "global" }
        ],
        "limits": {
            "maxPayloadBytes": 1048576,
            "maxListLimit": 100,
            "maxEventBatch": 500,
            "runConcurrency": 1
        },
        "features": { "actionParityMatrix": true, "eventResume": true, "idempotencyKeys": false, "homeDeclare": true },
        "credential": {
            "token": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "principalId": "cli:4242",
            "actorNamespace": "cli:4242",
            "homes": [HOME],
            "operations": ["*"],
            "expiresAt": "2026-08-25T00:00:00.000Z"
        }
    });
    assert_valid(&result, &reply, "handshake result");
    // The server-derived credential block is REQUIRED in every result
    // ($defs/CredentialGrant, U5b): a reply without it is invalid.
    let mut stripped = reply.clone();
    stripped.as_object_mut().unwrap().remove("credential");
    assert_invalid(&result, &stripped, "handshake result requires credential");
    let homes = reply["homes"].as_array().unwrap();
    assert!(homes
        .iter()
        .all(|h| h["homeId"].as_str().unwrap().starts_with("h_")));
}

// ── action-parity classification (R22) ────────────────────────────────────

#[test]
fn parity_matrix_seed_covers_whole_vocabulary() {
    const AGENT_AVAILABLE_ACTIONS: [&str; 14] = [
        "node/create",
        "node/get",
        "node/list",
        "node/tree",
        "node/search",
        "node/update",
        "node/move",
        "node/archive",
        "run/create",
        "run/get",
        "run/list",
        "run/control",
        "run/claim",
        "run/report",
        // events/resume makes 15? No — see below: counted separately.
    ];
    let _ = AGENT_AVAILABLE_ACTIONS;

    let matrix = &load_docs()["parity.schema.json"]["$defs"]["SeedActionParityMatrix"]["default"];
    let entries = matrix["entries"].as_array().expect("seed entries");

    let expected_agent: &[&str] = &[
        "node/create",
        "node/get",
        "node/list",
        "node/tree",
        "node/search",
        "node/update",
        "node/move",
        "node/archive",
        "run/create",
        "run/get",
        "run/list",
        "run/control",
        "run/claim",
        "run/report",
        "run/add-members",
        "run/nudge-record",
        "run/interrupt",
        "events/resume",
        "home/declare",
    ];
    let expected_adapter: &[&str] = &[
        "node/execute",
        "ui/filters-get",
        "ui/filters-set",
        "ui/recent-get",
        "ui/recent-set",
        "home/list-known",
    ];
    let expected_human: &[&str] = &["home/reindex"];

    let mut seen = BTreeMap::new();
    for entry in entries {
        let action = entry["action"].as_str().expect("action name");
        let class = entry["classification"].as_str().expect("classification");
        assert!(
            matches!(
                class,
                "agent_available" | "adapter_only" | "human_administrative"
            ),
            "classification outside the R22 enum: {class}"
        );
        assert_eq!(entry["since"], "v1", "seed entries all land in v1");
        seen.insert(action.to_string(), class.to_string());
    }
    assert_eq!(entries.len(), 26, "seed matrix covers all v1 methods");

    for action in expected_agent {
        assert_eq!(seen[*action], "agent_available", "{action}");
    }
    for action in expected_adapter {
        assert_eq!(seen[*action], "adapter_only", "{action}");
    }
    for action in expected_human {
        assert_eq!(seen[*action], "human_administrative", "{action}");
    }
    assert_eq!(seen.len(), 26, "no duplicate actions in the matrix");

    // The classification enum itself is the R22 three-way split.
    let reg = registry();
    let parity = validator_for(&reg, "common.schema.json#/$defs/ActionParity");
    assert_valid(&parity, &json!("agent_available"), "enum value 1");
    assert_invalid(&parity, &json!("sometimes"), "no fourth class");
}

// ── commands ↔ parity coherence ───────────────────────────────────────────

#[test]
fn every_documented_method_has_params_and_result_defs() {
    let methods: &[(&str, &str, &str)] = &[
        ("node/create", "CreateNodeParams", "CreateNodeResult"),
        ("node/get", "GetNodeParams", "GetNodeResult"),
        ("node/list", "ListNodeParams", "ListNodeResult"),
        ("node/tree", "TreeParams", "TreeResult"),
        ("node/search", "SearchNodesParams", "SearchNodesResult"),
        ("node/update", "UpdateNodeParams", "UpdateNodeResult"),
        ("node/move", "MoveNodeParams", "MoveNodeResult"),
        ("node/archive", "ArchiveNodeParams", "ArchiveNodeResult"),
        ("node/execute", "ExecuteNodeParams", "ExecuteNodeResult"),
        ("home/reindex", "ReindexHomeParams", "ReindexHomeResult"),
        ("home/declare", "DeclareHomeParams", "DeclareHomeResult"),
        (
            "home/list-known",
            "ListKnownHomesParams",
            "ListKnownHomesResult",
        ),
        ("run/create", "RunCreateParams", "RunCreateResult"),
        ("run/get", "RunGetParams", "RunGetResult"),
        ("run/list", "RunListParams", "RunListResult"),
        ("run/control", "RunControlParams", "RunControlResult"),
        ("run/claim", "ClaimRunParams", "ClaimRunResult"),
        ("run/report", "ReportRunParams", "ReportRunResult"),
        (
            "run/add-members",
            "RunAddMembersParams",
            "RunAddMembersResult",
        ),
        (
            "run/nudge-record",
            "RunNudgeRecordParams",
            "RunNudgeRecordResult",
        ),
        ("run/interrupt", "RunInterruptParams", "RunInterruptResult"),
        ("events/resume", "EventsResumeParams", "EventsResumeResult"),
        ("ui/filters-get", "FiltersGetParams", "FiltersGetResult"),
        ("ui/filters-set", "FiltersSetParams", "FiltersSetResult"),
        ("ui/recent-get", "RecentGetParams", "RecentGetResult"),
        ("ui/recent-set", "RecentSetParams", "RecentSetResult"),
    ];

    let docs = load_docs();
    let commands = &docs["commands.schema.json"]["$defs"];
    for (method, params_def, result_def) in methods {
        assert!(
            commands.get(*params_def).is_some(),
            "{method}: missing {params_def}"
        );
        assert!(
            commands.get(*result_def).is_some(),
            "{method}: missing {result_def}"
        );
        assert!(
            method.split('/').count() == 2 && method.chars().next().is_some_and(char::is_lowercase),
            "method name {method} violates the namespace/verb convention"
        );
    }

    // The parity matrix references exactly these methods.
    let parity_actions: Vec<&str> = docs["parity.schema.json"]["$defs"]["SeedActionParityMatrix"]
        ["default"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(parity_actions.len(), methods.len());
    for (method, _, _) in methods {
        assert!(
            parity_actions.contains(method),
            "parity matrix missing {method}"
        );
    }
}

// ── qualified refs everywhere (R4) ────────────────────────────────────────

#[test]
fn public_result_views_carry_home_id() {
    // Spot-check the shared views: every public reference shape requires homeId.
    let reg = registry();
    for def in [
        "common.schema.json#/$defs/NodeSummary",
        "common.schema.json#/$defs/NodeView",
        "common.schema.json#/$defs/TreeNode",
        "common.schema.json#/$defs/RunView",
        "common.schema.json#/$defs/RunItemView",
        "common.schema.json#/$defs/QualifiedNodeRef",
    ] {
        let validator = validator_for(&reg, def);
        let empty = json!({});
        assert_invalid(&validator, &empty, "{def} must require homeId (and more)");
    }

    // Generated GetNodeResult embeds qualified run links.
    let link = RunLink {
        home_id: HOME.parse().unwrap(),
        run_id: "RUN-0002".parse().unwrap(),
        title: Some("batch".to_string()),
        status: RunStatus::Running,
        item_state: RunItemState::AwaitingConfirmation,
        progress: Progress {
            total: 4,
            pending: 1,
            running: 1,
            done: 1,
            failed: 0,
            blocked: 0,
            skipped: 0,
            interrupted: 0,
            awaiting_confirmation: 1,
        },
    };
    let value = serde_json::to_value(&link).expect("serialize RunLink");
    assert_eq!(value["homeId"], HOME);
    assert_eq!(value["itemState"], "awaiting_confirmation");
}

#[test]
fn node_type_enum_preserves_domain_vocabulary() {
    assert!(
        NodeType::try_from("subticket").is_ok(),
        "subticket is a valid type"
    );
    let summary = json!({
        "homeId": HOME, "nodeId": "X-1", "type": "mystery",
        "title": "t", "status": "open", "archived": false, "priority": 0
    });
    let reg = registry();
    let summary_validator = validator_for(&reg, "common.schema.json#/$defs/NodeSummary");
    assert_invalid(&summary_validator, &summary, "unknown node type rejected");

    // Statuses keep snake_case on the wire but map to idiomatic variants.
    let typed: Result<omt_contracts::NodeSummary, _> = serde_json::from_value(json!({
        "homeId": HOME, "nodeId": "TICKET-0005", "type": "ticket",
        "title": "t", "status": "blocked", "archived": true, "priority": 1
    }));
    let summary = typed.expect("valid summary");
    assert!(matches!(summary.status, NodeStatus::Blocked));
}
