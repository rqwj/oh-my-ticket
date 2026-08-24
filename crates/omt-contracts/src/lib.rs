//! `omt-contracts` — Rust bindings for the OMT JSON-RPC protocol (KTD2, R3).
//!
//! All types in [`generated`] are produced at build time from
//! `schema/*.schema.json` by typify; do not edit them by hand. The constants
//! in [`protocol`] mirror values declared in the schema documents and are
//! cross-checked against them by `tests/contract.rs`.
//!
//! Wire conventions: JSON fields are camelCase; enum values keep the domain's
//! snake_case spelling (`in_progress`, `awaiting_confirmation`, …). Generated
//! serde structs tolerate unknown fields so additive schema changes stay
//! compatible for older peers.

mod generated {
    include!(concat!(env!("OUT_DIR"), "/contracts_generate.rs"));
}

pub use generated::*;

/// Protocol-level constants mirrored from the schema source of truth.
///
/// These live outside codegen because they are semantic anchors (negotiation
/// rules, problem-code assertions) rather than data shapes. `tests/contract.rs`
/// re-reads the schema documents and fails if these drift.
pub mod protocol {
    /// Current protocol version, MAJOR.MINOR (see common.schema.json).
    pub const PROTOCOL_VERSION: &str = "1.0";
    /// Major component of [`PROTOCOL_VERSION`]. Peers must agree on MAJOR.
    pub const PROTOCOL_MAJOR: u32 = 1;
    /// Minor component of [`PROTOCOL_VERSION`].
    pub const PROTOCOL_MINOR: u32 = 0;

    /// Problem code returned when a request or handshake uses a protocol
    /// MAJOR this peer does not support (problems.schema.json seed set).
    pub const UNSUPPORTED_PROTOCOL_CODE: &str = "UNSUPPORTED_PROTOCOL";

    /// Problem codes seeded by v1, carried forward from the TypeScript core's
    /// coarse taxonomy. U2 subdivides additively; clients must not exhaustively
    /// match this list.
    pub const SEED_PROBLEM_CODES: [&str; 7] = [
        "CONFLICT",
        "INVALID_HIERARCHY",
        "INVALID_INPUT",
        "NOT_FOUND",
        "IO",
        "UNSUPPORTED_PROTOCOL",
        "SCHEMA_TOO_NEW",
    ];

    /// URI base under which every schema document publishes its `$id`.
    pub const SCHEMA_ID_BASE: &str = "https://omt.dev/schemas/v1/";
}

#[cfg(test)]
mod tests {
    use super::protocol::*;

    #[test]
    fn protocol_version_components_match() {
        assert_eq!(
            PROTOCOL_VERSION,
            format!("{PROTOCOL_MAJOR}.{PROTOCOL_MINOR}")
        );
    }
}
