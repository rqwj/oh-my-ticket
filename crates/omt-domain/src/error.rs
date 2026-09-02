//! Structured problems (R5): stable machine-readable codes + structured
//! details. Message text is diagnostic only — corpus assertions and protocol
//! consumers must key on `code` + `details`.
//!
//! The code registry lives in `schema/problems.schema.json`. This crate only
//! emits the coarse v1 codes (`CONFLICT`, `INVALID_HIERARCHY`,
//! `INVALID_INPUT`, `NOT_FOUND`, `IO`) plus the U2 subdivisions
//! (`ARCHIVED_READONLY`, `DUPLICATE_MEMBER`, `INVALID_CONCURRENCY`); every
//! state-gate nuance travels as a `rule` entry inside `details`, matching the
//! TypeScript throw sites of `src/host/core.ts`.

use serde_json::{Map, Value};

/// One assertion-ready problem: `{code, message, details?}`.
///
/// Serialization mirrors the harness error envelope: `details` is omitted
/// when absent (never serialized as `null`), so `deepEqual` treats the key
/// as missing exactly like the TypeScript spread
/// `{ code, message, ...(details !== undefined ? { details } : {}) }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl Problem {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Problem {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(
        code: &'static str,
        message: impl Into<String>,
        build: impl FnOnce(&mut Map<String, Value>),
    ) -> Self {
        let mut map = Map::new();
        build(&mut map);
        Problem {
            code,
            message: message.into(),
            details: Some(Value::Object(map)),
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Problem {}

/// Domain result alias: every fallible decision returns a [`Problem`].
pub type Result<T> = std::result::Result<T, Problem>;

// ── coarse codes (v1 seed set) ──────────────────────────────────────────

pub const CONFLICT: &str = "CONFLICT";
pub const INVALID_HIERARCHY: &str = "INVALID_HIERARCHY";
pub const INVALID_INPUT: &str = "INVALID_INPUT";
pub const NOT_FOUND: &str = "NOT_FOUND";
pub const IO: &str = "IO";

// ── U2 subdivisions (each falls back to a coarse ancestor) ──────────────

pub const ARCHIVED_READONLY: &str = "ARCHIVED_READONLY";
pub const DUPLICATE_MEMBER: &str = "DUPLICATE_MEMBER";
pub const INVALID_CONCURRENCY: &str = "INVALID_CONCURRENCY";

// ── U2b/U4 storage-plane codes (additive; registered in problems.schema.json
//    for HOME_LOCKED / DAEMON_OWNS_HOME / SCHEMA_TOO_NEW; REINDEX_REQUIRED is
//    emitted by the U4 recovery layer and still needs its schema registration) ──

/// Owner-lock refusal (U2b/R2): another live writer holds `<home>/home.lock`.
pub const HOME_LOCKED: &str = "HOME_LOCKED";
/// Daemon-marker refusal (U2b/R2/F5): ownerKind "daemon" always refuses.
pub const DAEMON_OWNS_HOME: &str = "DAEMON_OWNS_HOME";
/// Future-schema preflight (R8): on-disk schema newer than this binary knows.
pub const SCHEMA_TOO_NEW: &str = "SCHEMA_TOO_NEW";
/// A pending journal plan's target files were hand-edited while the writer was
/// down (U4): fail closed, never overwrite, reindex required.
pub const REINDEX_REQUIRED: &str = "REINDEX_REQUIRED";

// ── structured-details constructors mirroring core.ts throw sites ───────

/// `NOT_FOUND` for one referent: `kind` ∈ node | run | run-item.
pub fn not_found_node(id: &str) -> Problem {
    Problem::with_details(NOT_FOUND, format!("unknown node: {id}"), |d| {
        d.insert("kind".into(), "node".into());
        d.insert("id".into(), id.into());
    })
}

pub fn not_found_run(id: &str) -> Problem {
    Problem::with_details(NOT_FOUND, format!("unknown run: {id}"), |d| {
        d.insert("kind".into(), "run".into());
        d.insert("id".into(), id.into());
    })
}

pub fn not_found_run_item(run_id: &str, node_id: &str) -> Problem {
    Problem::with_details(
        NOT_FOUND,
        format!("run {run_id} has no item for node: {node_id}"),
        |d| {
            d.insert("kind".into(), "run-item".into());
            d.insert("runId".into(), run_id.into());
            d.insert("nodeId".into(), node_id.into());
        },
    )
}
