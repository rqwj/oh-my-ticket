//! `omt-domain` — pure OMT domain model (plan U3).
//!
//! Port of hierarchy/status/run/lease-decision semantics from the TypeScript
//! core (`src/host/core.ts` + `src/host/store.ts`) with ZERO harness
//! dependencies: no filesystem access, no wall clock, no sockets. Every
//! side effect crosses a port ([`ports`]), every state-machine rule is a
//! total function over row snapshots ([`types`], [`runs`], [`janitor`],
//! [`hierarchy`]), and [`core::OmtCore`] sequences them exactly like the
//! behavioral source does — order-for-order, because the frozen 68-scenario
//! corpus (`corpus/scenarios/*.json`) pins those orderings.
//!
//! Module map (mirrors core.ts concerns):
//! | module        | TS concern |
//! |---------------|------------|
//! | [`types`]     | types.ts — enums (reused from omt-contracts), rows, transition tables |
//! | [`error`]     | OmtError/ProblemCode — structured `{code, details}` problems (R5) |
//! | [`markdown`]  | markdown.ts + files.ts layout helpers — frontmatter codec |
//! | [`store`]     | store.ts query semantics (ORDER BY clauses are normative) |
//! | [`hierarchy`] | create/move guards, ancestor/descendant walks |
//! | [`runs`]      | run/item transition legality, derivation, trust gate, lease-fenced report authorization |
//! | [`janitor`]   | two-pass startup sweep + the six ratified U3 semantics decisions |
//! | [`ports`]     | Clock / LeaseTable / FileStore traits + in-memory impls |
//! | [`core`]      | OmtCore dual-write orchestration |
//!
//! The executable spec lives in `tests/corpus.rs`: it loads every scenario
//! JSON and fails on any divergence from the invariants the TypeScript leg
//! also asserts.

pub mod core;
pub mod error;
pub mod hierarchy;
pub mod janitor;
pub mod markdown;
pub mod ports;
pub mod runs;
pub mod store;
pub mod types;

pub use error::{Problem, Result};
pub use ports::{Clock, FixedClock, LeaseGrant, LeaseTable, MemoryLeases};
pub use types::{
    is_run_active, is_run_item_failure, is_run_item_final, is_run_item_in_flight,
    is_run_item_stalled, item_transition_allowed, run_transition_allowed, EdgeRow, NodeRow,
    RunConfigValue, RunItemRow, RunRow, TreeNodeRow, NUDGE_BUDGET,
};
