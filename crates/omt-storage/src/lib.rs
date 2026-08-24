//! `omt-storage` — crash-recoverable dual-store persistence (plan U4a).
//!
//! Preserves the on-disk formats: SQLite metadata index (WAL, busy_timeout),
//! Markdown tree written through atomic file plans (temp → fsync → rename →
//! directory fsync), phased operation journal
//! (`prepared → files_applied → db_committed → acknowledged`), single-
//! transaction finalize of node rows + idempotent operations row + outbox
//! events, ledgered schema migration v3→v4 behind a read-only future-schema
//! preflight, and kernel home ownership layered over the U2b marker contract.
//!
//! Division of labor (R1/U3 boundary): [`omt-domain`] owns decision functions
//! and ports ([`omt_domain::store::FileStore`], [`omt_domain::ports`]) and the
//! [`omt_domain::error::Problem`] shape; this crate implements the IO side and
//! never duplicates domain decisions.

pub mod backup;
pub mod clock;
pub mod fault;
pub mod files;
pub mod home_lock;
pub mod invariants;
pub mod journal;
pub mod migrate;
pub mod outbox;
pub mod recovery;
pub mod reindex;
pub mod store;

pub use clock::{FixedClock, MillisClock, SystemClock};
pub use fault::{FaultSchedule, FilePhase, Step};
pub use journal::Storage as _StorageAlias;
pub use journal::{
    report_item_patch, DbChange, FileOp, ItemPatch, NodeDto, NodePatch, OpenConfig, OutboxEvent,
    PreparedMutation, Storage,
};
pub use omt_domain::error::Problem;
pub use omt_domain::Result;
pub use recovery::RecoveryReport;

/// SQLite database file name inside a home (unchanged TS layout).
pub const DB_FILE_NAME: &str = "omt.db";

/// Recovery-copy root inside a home: `<home>/.omt/recovery/<commandId>/`.
pub const RECOVERY_ROOT: &str = ".omt/recovery";
