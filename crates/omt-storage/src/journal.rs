//! Phased operation journal (KTD8/R7/F2): every mutation is journaled
//! `prepared` BEFORE any file is touched, applied through an atomic
//! per-file plan, finalized in ONE SQLite transaction (node rows +
//! idempotent operations row + outbox events + phase=db_committed), then
//! acknowledged (recovery copies pruned). Crashes before db_committed replay
//! deterministically; crashes after it roll FORWARD — acknowledged results
//! never revert.
//!
//! The journal row carries the FULL deterministic plan (file contents, DB
//! changes, events, result envelope) so recovery replays from durable state
//! alone — no in-memory context survives a crash.

use crate::clock::MillisClock;
use crate::fault::{FaultGun, Step};
use crate::files::DiskFiles;
use crate::store;
use crate::{Problem, Result};
use omt_domain::error;
#[allow(unused_imports)]
use omt_domain::store::FileStore as _;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

// ── phases ──────────────────────────────────────────────────────────────

pub const PHASE_PREPARED: &str = "prepared";
pub const PHASE_FILES_APPLIED: &str = "files_applied";
pub const PHASE_DB_COMMITTED: &str = "db_committed";
pub const PHASE_ACKNOWLEDGED: &str = "acknowledged";

/// Problem code returned when a caller cancellation was honored at a
/// linearization-safe point (U5b; registered in schema/problems.schema.json).
/// details.rule = "client-canceled", details.at names the safe point.
pub const CANCELED_PROBLEM_CODE: &str = "CANCELED";

/// Build the canonical cancellation problem for one aborted command.
pub fn canceled_problem(command_id: &str, at: &str) -> Problem {
    Problem::with_details(
        CANCELED_PROBLEM_CODE,
        format!("operation {command_id} canceled before commit"),
        |d| {
            d.insert("rule".into(), "client-canceled".into());
            d.insert("commandId".into(), command_id.into());
            d.insert("at".into(), at.into());
        },
    )
}

/// Never-cancel probe used by the plain [`Storage::execute`] path.
pub fn no_cancel() -> impl Fn() -> bool + Send + Sync + Copy {
    || false
}

// ── plan vocabulary ─────────────────────────────────────────────────────

/// One file operation of a mutation plan. Content is ABSOLUTE (the final
/// bytes), so replays never depend on intermediate disk state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileOp {
    /// Write/overwrite one file. `before_sha` is `None` for creations.
    Write {
        path: String,
        content: String,
        before_sha: Option<String>,
        after_sha: String,
    },
    /// Move a directory subtree (`tickets/a` → `tickets/b/c`).
    MoveDir { from: String, to: String },
}

impl FileOp {
    /// Target paths whose on-disk state this op reads or writes.
    pub fn targets(&self) -> Vec<String> {
        match self {
            FileOp::Write { path, .. } => vec![path.clone()],
            FileOp::MoveDir { from, to } => vec![from.clone(), to.clone()],
        }
    }

    /// Injectable steps contributed to the fault ordinal sequence.
    pub fn step_count(&self) -> usize {
        match self {
            FileOp::Write { .. } => 5, // RecoveryCopy WriteTemp FsyncTemp RenameOver FsyncParentDir
            FileOp::MoveDir { .. } => 3, // SnapshotDir RenameDir FsyncDir
        }
    }

    #[allow(dead_code)] // diagnostic accessor for future recovery tooling
    fn before_sha(&self) -> Option<&str> {
        match self {
            FileOp::Write { before_sha, .. } => before_sha.as_deref(),
            FileOp::MoveDir { .. } => None,
        }
    }
}

/// One durable event destined for the outbox inside the finalize transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEvent {
    pub event_type: String,
    pub payload: Value,
}

/// Serializable node snapshot crossing the journal JSON boundary (domain rows
/// stay Serialize-only by design; storage owns the wire shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeDto {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub title: String,
    pub status: String,
    pub archived: bool,
    pub priority: i64,
    pub path: String,
    pub created_at: String,
    pub updated_at: String,
}

impl NodeDto {
    pub fn from_row(node: &omt_domain::types::NodeRow) -> Self {
        NodeDto {
            id: node.id.clone(),
            node_type: node.node_type.to_string(),
            title: node.title.clone(),
            status: node.status.to_string(),
            archived: node.archived,
            priority: node.priority,
            path: node.path.clone(),
            created_at: node.created_at.clone(),
            updated_at: node.updated_at.clone(),
        }
    }

    pub fn to_row(&self) -> Result<omt_domain::types::NodeRow> {
        Ok(omt_domain::types::NodeRow {
            id: self.id.clone(),
            node_type: parse(&self.node_type)?,
            title: self.title.clone(),
            status: parse(&self.status)?,
            archived: self.archived,
            priority: self.priority,
            path: self.path.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        })
    }
}

fn parse<E>(raw: &str) -> Result<E>
where
    E: std::str::FromStr,
    <E as std::str::FromStr>::Err: std::fmt::Display,
{
    raw.parse::<E>().map_err(|err| {
        Problem::with_details(
            error::INVALID_INPUT,
            format!("invalid enum value: {raw}"),
            |d| {
                d.insert("value".into(), raw.into());
                d.insert("parseError".into(), err.to_string().into());
            },
        )
    })
}

/// Serializable run snapshot crossing the journal JSON boundary (mirrors
/// [`omt_domain::types::RunRow`], which stays Serialize-only by design).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunDto {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: String,
    /// RunConfigValue wire shape {stopOnFailure, autoContinue, autoVerify, concurrency}.
    pub config: Value,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

impl RunDto {
    pub fn from_row(run: &omt_domain::types::RunRow) -> Self {
        RunDto {
            id: run.id.clone(),
            title: run.title.clone(),
            status: run.status.to_string(),
            config: serde_json::json!({
                "stopOnFailure": run.config.stop_on_failure,
                "autoContinue": run.config.auto_continue,
                "autoVerify": run.config.auto_verify,
                "concurrency": run.config.concurrency,
            }),
            created_at: run.created_at.clone(),
            finished_at: run.finished_at.clone(),
        }
    }

    pub fn to_row(&self) -> Result<omt_domain::types::RunRow> {
        Ok(omt_domain::types::RunRow {
            id: self.id.clone(),
            title: self.title.clone(),
            status: parse(&self.status)?,
            config: omt_domain::types::RunConfigValue {
                stop_on_failure: self.config["stopOnFailure"].as_bool().unwrap_or(false),
                auto_continue: self.config["autoContinue"].as_bool().unwrap_or(true),
                auto_verify: self.config["autoVerify"].as_bool().unwrap_or(false),
                concurrency: self.config["concurrency"].as_i64().unwrap_or(1),
            },
            created_at: self.created_at.clone(),
            finished_at: self.finished_at.clone(),
        })
    }
}

/// Serializable run-item snapshot crossing the journal JSON boundary
/// (mirrors [`omt_domain::types::RunItemRow`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemDto {
    pub run_id: String,
    pub node_id: String,
    pub position: i64,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_session_id: Option<String>,
    pub attempts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nudged_at: Option<String>,
    pub nudge_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

impl ItemDto {
    pub fn from_row(item: &RunItemRow) -> Self {
        ItemDto {
            run_id: item.run_id.clone(),
            node_id: item.node_id.clone(),
            position: item.position,
            state: item.state.to_string(),
            executor_session_id: item.executor_session_id.clone(),
            attempts: item.attempts,
            last_error: item.last_error.clone(),
            nudged_at: item.nudged_at.clone(),
            nudge_count: item.nudge_count,
            started_at: item.started_at.clone(),
            finished_at: item.finished_at.clone(),
        }
    }

    pub fn to_row(&self) -> Result<RunItemRow> {
        Ok(RunItemRow {
            run_id: self.run_id.clone(),
            node_id: self.node_id.clone(),
            position: self.position,
            state: parse(&self.state)?,
            executor_session_id: self.executor_session_id.clone(),
            attempts: self.attempts,
            last_error: self.last_error.clone(),
            nudged_at: self.nudged_at.clone(),
            nudge_count: self.nudge_count,
            started_at: self.started_at.clone(),
            finished_at: self.finished_at.clone(),
        })
    }
}

/// Explicit-set patch payload (`None` fields are skipped; dedicated clear
/// flags make NULL writes distinguishable from no-ops).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NodePatch {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_updated_at: Option<String>,
}

/// Run-row patch mirroring [`store::update_run`] semantics on the wire:
/// `clear_finished_at`/`clear_title` distinguish NULL writes from no-ops.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RunPatch {
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_finished_at: Option<String>,
    #[serde(default)]
    pub clear_finished_at: bool,
}

/// Run-item patch mirroring [`store::ItemPatchValues`] on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ItemPatch {
    pub run_id: String,
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_executor_session_id: Option<String>,
    #[serde(default)]
    pub clear_executor_session_id: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_attempts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_last_error: Option<String>,
    #[serde(default)]
    pub clear_last_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_nudged_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_nudge_count: Option<i64>,
    /// COALESCE(started_at, ?) semantics when true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_started_at_preserve: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_finished_at: Option<String>,
    #[serde(default)]
    pub clear_finished_at: bool,
}

/// One mechanical database side effect of a finalize transaction. Storage
/// applies these verbatim; ALL decision logic lives upstream (domain).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DbChange {
    NodeInsert {
        node: NodeDto,
    },
    NodePatch {
        patch: NodePatch,
    },
    EdgeInsert {
        parent_id: String,
        child_id: String,
        ord: i64,
    },
    EdgeDelete {
        parent_id: String,
        child_id: String,
    },
    SearchUpsert {
        id: String,
        title: String,
        body: String,
    },
    MetaSet {
        key: String,
        value: String,
    },
    CounterBump {
        prefix: String,
    },
    /// U5b run-plane vocabulary (additive): insert one run row. Applied in
    /// the first pass so later ItemInsert changes satisfy the FK.
    RunInsert {
        run: RunDto,
    },
    /// U5b run-plane vocabulary: status/finished_at patch of one run.
    RunPatch {
        patch: RunPatch,
    },
    /// U5b run-plane vocabulary: insert one pending item of a new run
    /// (requires its RunInsert earlier in the plan).
    ItemInsert {
        item: ItemDto,
    },
    ItemPatch {
        patch: ItemPatch,
    },
    ItemDelete {
        run_id: String,
        node_id: String,
    },
}

/// A fully specified recoverable mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedMutation {
    pub command_id: String,
    pub op_kind: String,
    /// Hash of the canonical input (idempotency fingerprint).
    pub input_hash: String,
    pub files: Vec<FileOp>,
    pub changes: Vec<DbChange>,
    pub events: Vec<OutboxEvent>,
    pub result: Value,
}

impl PreparedMutation {
    pub fn new(
        command_id: impl Into<String>,
        op_kind: impl Into<String>,
        input_hash: impl Into<String>,
    ) -> Self {
        PreparedMutation {
            command_id: command_id.into(),
            op_kind: op_kind.into(),
            input_hash: input_hash.into(),
            files: Vec::new(),
            changes: Vec::new(),
            events: Vec::new(),
            result: Value::Null,
        }
    }

    pub fn push_file(&mut self, op: FileOp) -> &mut Self {
        self.files.push(op);
        self
    }

    pub fn push_change(&mut self, change: DbChange) -> &mut Self {
        self.changes.push(change);
        self
    }

    pub fn push_event(&mut self, event_type: &str, payload: Value) -> &mut Self {
        self.events.push(OutboxEvent {
            event_type: event_type.to_string(),
            payload,
        });
        self
    }

    pub fn with_result(mut self, result: Value) -> Self {
        self.result = result;
        self
    }

    /// Deterministic step ordinal count for kill-grid iteration.
    pub fn step_count(&self) -> usize {
        self.files.iter().map(FileOp::step_count).sum::<usize>() + 2 // TxnCommit + Acknowledge
    }
}

/// Hash helper for canonical input fingerprints.
pub fn input_fingerprint(parts: &[&str]) -> String {
    let joined = parts.join("\u{1f}");
    crate::files::sha256_hex(&joined)
}

/// Open configuration. Lock acquisition defaults OFF in U4a (the daemon owns
/// wiring in U5); `use_lock` turns on marker+flock ownership for integration
/// tests and the future composition helper.
#[derive(Clone)]
pub struct OpenConfig {
    pub home: std::path::PathBuf,
    pub clock: std::sync::Arc<dyn MillisClock>,
    /// Refuse homes whose schema is newer than this binary (R8 preflight).
    pub fail_on_too_new: bool,
    pub recover_on_open: bool,
    pub acquire_lock: bool,
    pub owner_kind: crate::home_lock::OwnerKind,
    pub hostname: String,
    pub lock_stale_ms: i64,
    pub lock_heartbeat_ms: i64,
    /// Fixed home id override (tests); generated when absent.
    pub home_id: Option<String>,
    pub fault: FaultScheduleAlias,
}

pub type FaultScheduleAlias = crate::fault::FaultSchedule;

impl OpenConfig {
    pub fn new(home: impl Into<std::path::PathBuf>) -> Self {
        OpenConfig {
            home: home.into(),
            clock: std::sync::Arc::new(crate::clock::SystemClock),
            fail_on_too_new: true,
            recover_on_open: true,
            acquire_lock: false,
            owner_kind: crate::home_lock::OwnerKind::Daemon,
            hostname: "localhost".to_string(),
            lock_stale_ms: crate::home_lock::DEFAULT_STALE_MS,
            lock_heartbeat_ms: crate::home_lock::HEARTBEAT_INTERVAL_MS,
            home_id: None,
            fault: crate::fault::FaultSchedule::never(),
        }
    }
}

/// One journaled command as read back from disk.
#[derive(Debug, Clone)]
pub struct JournalRow {
    pub op_id: i64,
    pub command_id: String,
    pub op_kind: String,
    pub phase: String,
    pub input_hash: String,
    pub plan: PreparedMutation,
}

/// The storage engine bound to one home: SQLite connection + file operator +
/// fault gun + optional kernel lock.
pub struct Storage {
    pub(crate) conn: Connection,
    pub(crate) files: DiskFiles,
    pub(crate) home: std::path::PathBuf,
    pub(crate) clock: std::sync::Arc<dyn MillisClock>,
    pub(crate) gun: FaultGun,
    pub(crate) lock: Option<crate::home_lock::HomeLockHandle>,
    pub(crate) home_id: Option<String>,
}

impl Storage {
    /// Open a home: future-schema preflight (strictly read-only, R8) →
    /// optional kernel lock → SQLite (WAL/busy_timeout/FULL) → schema
    /// ensure or ledgered migration → crash recovery.
    pub fn open(config: OpenConfig) -> Result<Storage> {
        use crate::home_lock::LockConfig;
        let db_path = config.home.join(crate::DB_FILE_NAME);

        // 1. Future-schema preflight BEFORE any write-capable handle exists.
        if config.fail_on_too_new && db_path.exists() {
            crate::migrate::preflight_read_only(&db_path)?;
        }

        std::fs::create_dir_all(&config.home).map_err(|err| {
            Problem::with_details(error::IO, format!("cannot create home: {err}"), |d| {
                d.insert("home".into(), config.home.display().to_string().into());
            })
        })?;

        // 2. Kernel ownership (marker + advisory flock on <home>/home.lock).
        let lock = if config.acquire_lock {
            Some(crate::home_lock::acquire(
                &config.home,
                &LockConfig {
                    owner_kind: config.owner_kind,
                    hostname: config.hostname.clone(),
                    stale_ms: config.lock_stale_ms,
                    heartbeat_ms: config.lock_heartbeat_ms,
                },
                Arc::clone(&config.clock),
            )?)
        } else {
            None
        };

        // 3. Read-write connection + durability pragmas.
        let conn = Connection::open(&db_path)
            .map_err(|err| Problem::new(error::IO, format!("sqlite open failed: {err}")))?;
        store::apply_open_pragmas(&conn)?;

        // 4. Schema ensure / ledgered migration.
        let now_iso = crate::clock::iso_from_ms(config.clock.now_ms());
        let mut conn = conn;
        match crate::migrate::detect_version(&conn)? {
            crate::migrate::DetectedVersion::Current => {}
            crate::migrate::DetectedVersion::Fresh | crate::migrate::DetectedVersion::Legacy(_) => {
                crate::migrate::migrate_to_current(
                    &mut conn,
                    &config.home.to_string_lossy(),
                    &*config.clock,
                    config.home_id.as_deref(),
                )?;
            }
            crate::migrate::DetectedVersion::TooNew(found) => {
                return Err(Problem::with_details(
                    error::SCHEMA_TOO_NEW,
                    format!(
                        "home schema v{found} is newer than this binary understands (v{})",
                        store::KNOWN_SCHEMA_VERSION
                    ),
                    |d| {
                        d.insert("foundSchemaVersion".into(), found.into());
                        d.insert(
                            "knownSchemaVersion".into(),
                            store::KNOWN_SCHEMA_VERSION.into(),
                        );
                    },
                ));
            }
        }
        let _ = now_iso;

        // 5. Assemble the engine and run crash recovery.
        let home_id = store::get_home_id(&conn)?;
        let mut storage = Storage {
            conn,
            files: DiskFiles::new(&config.home),
            home: config.home.clone(),
            clock: Arc::clone(&config.clock),
            gun: FaultGun::new(config.fault),
            lock,
            home_id,
        };
        if config.recover_on_open {
            crate::recovery::recover_pending(&mut storage)?;
        }
        Ok(storage)
    }

    /// Execute one journaled mutation end-to-end:
    /// prepared journal row → atomic per-file plan (recovery copies first) →
    /// single finalize transaction → acknowledge + prune.
    /// A previously committed `command_id` returns its stored result without
    /// re-applying anything (retry-after-lost-ack idempotency, R9).
    pub fn execute(&mut self, mutation: &PreparedMutation) -> Result<Value> {
        self.execute_cancellable(mutation, &no_cancel())
    }

    /// [`Self::execute`] with a client-cancellation probe (U5b). The probe
    /// is honored ONLY at linearization-safe points:
    /// - before the journal row exists: clean abort, nothing durable;
    /// - after `prepared` / after `files_applied`: the op aborts and the
    ///   caller sees [`CANCELED_PROBLEM_CODE`], while the journal row stays
    ///   PENDING-RECOVERY — cancellation is exactly a crash at that instant
    ///   and the next recovery rolls it forward deterministically (R7);
    /// - after `db_committed`: the mutation completes; the cancel flag is
    ///   ignored and the normal result returns.
    pub fn execute_cancellable(
        &mut self,
        mutation: &PreparedMutation,
        canceled: &dyn Fn() -> bool,
    ) -> Result<Value> {
        // Idempotency fast path: same commandId must carry the same input
        // fingerprint; a reuse with different input fails closed.
        let existing = self.stored_result(&mutation.command_id)?;
        if let Some((stored_hash, stored_result)) = existing {
            if stored_hash != mutation.input_hash {
                return Err(Problem::with_details(
                    error::CONFLICT,
                    "command id reused with different input",
                    |d| {
                        d.insert("rule".into(), "command-id-reuse".into());
                        d.insert("commandId".into(), mutation.command_id.clone().into());
                    },
                ));
            }
            return serde_json::from_str(&stored_result).map_err(|err| {
                Problem::new(error::IO, format!("stored result unreadable: {err}"))
            });
        }

        // Safe point 1: nothing durable exists yet — clean abort.
        if canceled() {
            return Err(canceled_problem(&mutation.command_id, "before-journal"));
        }

        // Phase PREPARED — durable BEFORE any file is touched.
        self.journal_insert(mutation)?;

        // Safe point 2: journal row stays pending-recovery (replay converges).
        if canceled() {
            return Err(canceled_problem(&mutation.command_id, "after-prepared"));
        }

        // File plan application with per-phase kill points.
        self.apply_file_plan(mutation)?;
        self.journal_set_phase(&mutation.command_id, PHASE_FILES_APPLIED)?;

        // Safe point 3: files applied but DB untouched — still recoverable.
        if canceled() {
            return Err(canceled_problem(
                &mutation.command_id,
                "after-files-applied",
            ));
        }

        // Finalize: ONE transaction committing node rows + operations +
        // outbox events + journal phase=db_committed.
        self.gun.check(Step::TxnCommit)?;
        finalize_mutation(
            &self.conn,
            self.home_id.as_deref(),
            mutation,
            &self.now_iso(),
        )?;

        // Acknowledge: mark + prune recovery copies.
        self.gun.check(Step::Acknowledge)?;
        self.journal_set_phase(&mutation.command_id, PHASE_ACKNOWLEDGED)?;
        self.files.prune_recovery(&mutation.command_id)?;
        Ok(mutation.result.clone())
    }

    pub(crate) fn apply_file_plan(&mut self, mutation: &PreparedMutation) -> Result<()> {
        for (index, file_op) in mutation.files.iter().enumerate() {
            match file_op {
                FileOp::Write {
                    path,
                    content,
                    before_sha,
                    ..
                } => {
                    // Recovery copy of the CURRENT original (only when an
                    // original exists — creates have nothing to preserve).
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::RecoveryCopy,
                    })?;
                    if before_sha.is_some() {
                        self.files
                            .make_recovery_copy(&mutation.command_id, path, &|rel| {
                                DiskFiles::recovery_copy_rel(&mutation.command_id, rel)
                            })?;
                    }
                    let token = crate::files::entropy_token();
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::WriteTemp,
                    })?;
                    let tmp = self.files.stage_write(path, content, &token)?;
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::FsyncTemp,
                    })?;
                    self.files.fsync_staged(&tmp)?;
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::RenameOver,
                    })?;
                    self.files.promote_staged(&tmp, path)?;
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::FsyncParentDir,
                    })?;
                    self.files.fsync_parent_of(path)?;
                }
                FileOp::MoveDir { from, to } => {
                    // Snapshot every contained file for later restore.
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::RecoveryCopy,
                    })?;
                    for contained in self.files.list_files_under(from) {
                        self.files.make_recovery_copy(
                            &mutation.command_id,
                            &contained,
                            &|rel| DiskFiles::recovery_copy_rel(&mutation.command_id, rel),
                        )?;
                    }
                    let token = crate::files::entropy_token();
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::RenameOver,
                    })?;
                    self.files.move_dir(from, to, &token)?;
                    self.gun.check(Step::FileOp {
                        index,
                        phase: crate::fault::FilePhase::FsyncParentDir,
                    })?;
                    self.files.fsync_parent_of(to)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn stored_result(&self, command_id: &str) -> Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT input_hash, result_json FROM operations WHERE command_id = ?1",
                [command_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|err| Problem::new(error::IO, format!("operations lookup failed: {err}")))
    }

    pub(crate) fn journal_insert(&self, mutation: &PreparedMutation) -> Result<()> {
        let plan_json = serde_json::to_string(mutation)
            .map_err(|err| Problem::new(error::IO, format!("plan serialize failed: {err}")))?;
        let now_iso = self.now_iso();
        let inserted = self.conn
            .execute(
                "INSERT INTO journal (command_id, op_kind, phase, input_hash, plan_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    mutation.command_id,
                    mutation.op_kind,
                    PHASE_PREPARED,
                    mutation.input_hash,
                    plan_json,
                    now_iso,
                    now_iso,
                ],
            )
            .map_err(|err| {
                if err.to_string().contains("UNIQUE") {
                    Problem::with_details(error::CONFLICT, "command already pending in journal", |d| {
                        d.insert("rule".into(), "journal-duplicate".into());
                        d.insert("commandId".into(), mutation.command_id.clone().into());
                    })
                } else {
                    Problem::new(error::IO, format!("journal insert failed: {err}"))
                }
            })?;
        let _ = inserted;
        Ok(())
    }

    pub(crate) fn journal_set_phase(&self, command_id: &str, phase: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE journal SET phase = ?2, updated_at = ?3 WHERE command_id = ?1",
                rusqlite::params![command_id, phase, self.now_iso()],
            )
            .map_err(|err| {
                Problem::new(error::IO, format!("journal phase update failed: {err}"))
            })?;
        Ok(())
    }

    /// Recover pending journal commands (replay / roll-forward / drift gate).
    pub fn recover_pending(&mut self) -> Result<crate::recovery::RecoveryReport> {
        crate::recovery::recover_pending(self)
    }
}

/// Shared finalize transaction: node rows + idempotent operations row +
/// outbox events + journal phase=db_committed — atomic, exactly once.
pub(crate) fn finalize_mutation(
    conn: &Connection,
    home_id: Option<&str>,
    mutation: &PreparedMutation,
    now_iso: &str,
) -> Result<()> {
    let home_id = home_id.ok_or_else(|| Problem::new(error::IO, "home identity missing"))?;
    store::in_transaction(conn, |tx| {
        apply_db_changes(tx, &mutation.changes)?;
        // Idempotent result registry FIRST: a duplicate command id aborts the
        // whole transaction (nothing half-commits).
        tx.execute(
            "INSERT INTO operations (command_id, op_kind, input_hash, result_json, committed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                mutation.command_id,
                mutation.op_kind,
                mutation.input_hash,
                serde_json::to_string(&mutation.result).map_err(|err| Problem::new(
                    error::IO,
                    format!("result serialize failed: {err}")
                ))?,
                now_iso,
            ],
        )
        .map_err(|err| {
            Problem::with_details(error::CONFLICT, "operation already committed", |d| {
                d.insert("rule".into(), "operations-duplicate".into());
                d.insert("commandId".into(), mutation.command_id.clone().into());
                d.insert("detail".into(), err.to_string().into());
            })
        })?;
        for event in &mutation.events {
            crate::outbox::append(tx, home_id, &event.event_type, &event.payload, now_iso)?;
        }
        tx.execute(
            "UPDATE journal SET phase = ?2, updated_at = ?3 WHERE command_id = ?1",
            rusqlite::params![mutation.command_id, PHASE_DB_COMMITTED, now_iso],
        )
        .map_err(|err| Problem::new(error::IO, format!("journal phase failed: {err}")))?;
        Ok(())
    })
}

pub(crate) fn apply_db_changes(tx: &Connection, changes: &[DbChange]) -> Result<()> {
    // Three-pass application: (1) every NodeInsert AND RunInsert lands
    // before anything that can reference it (edges/run_items carry FK
    // constraints), preserving relative order within the pass; (2) ItemInsert
    // rows (FK → runs) before generic patches; (3) everything else in plan
    // order.
    for change in changes
        .iter()
        .filter(|c| matches!(c, DbChange::NodeInsert { .. } | DbChange::RunInsert { .. }))
    {
        match change {
            DbChange::NodeInsert { node } => store::insert_node(tx, &node.to_row()?, 1)?,
            DbChange::RunInsert { run } => store::insert_run(tx, &run.to_row()?)?,
            _ => unreachable!("filtered into first pass"),
        }
    }
    for change in changes
        .iter()
        .filter(|c| matches!(c, DbChange::ItemInsert { .. }))
    {
        if let DbChange::ItemInsert { item } = change {
            store::insert_run_item(tx, &item.to_row()?)?;
        }
    }
    for change in changes.iter().filter(|c| {
        !matches!(
            c,
            DbChange::NodeInsert { .. } | DbChange::RunInsert { .. } | DbChange::ItemInsert { .. }
        )
    }) {
        match change {
            DbChange::NodeInsert { .. }
            | DbChange::RunInsert { .. }
            | DbChange::ItemInsert { .. } => {
                unreachable!("filtered into earlier passes")
            }
            DbChange::NodePatch { patch } => {
                let values = store::NodePatchValues {
                    title: patch.set_title.clone(),
                    status: patch.set_status.clone(),
                    archived: patch.set_archived,
                    priority: patch.set_priority,
                    path: patch.set_path.clone(),
                    updated_at: patch.set_updated_at.clone(),
                };
                store::update_node(tx, &patch.id, &values)?;
            }
            DbChange::EdgeInsert {
                parent_id,
                child_id,
                ord,
            } => {
                store::insert_edge(tx, parent_id, child_id, *ord)?;
            }
            DbChange::EdgeDelete {
                parent_id,
                child_id,
            } => {
                store::delete_edge(tx, parent_id, child_id)?;
            }
            DbChange::SearchUpsert { id, title, body } => {
                store::index_node(tx, id, title, body)?;
            }
            DbChange::MetaSet { key, value } => {
                store::set_meta(tx, key, value)?;
            }
            DbChange::CounterBump { prefix } => {
                store::bump_counter(tx, prefix)?;
            }
            DbChange::RunPatch { patch } => {
                let status = patch.set_status.as_deref().map(parse).transpose()?;
                let finished_at: Option<Option<String>> = if patch.clear_finished_at {
                    Some(None)
                } else {
                    patch.set_finished_at.clone().map(Some)
                };
                store::update_run(tx, &patch.run_id, status, finished_at, None)?;
            }
            DbChange::ItemPatch { patch } => {
                let values = store::ItemPatchValues {
                    state: patch.set_state.as_deref().map(parse).transpose()?,
                    position: None,
                    executor_session_id: patch.set_executor_session_id.clone(),
                    clear_executor: patch.clear_executor_session_id,
                    attempts: patch.set_attempts,
                    last_error: patch.set_last_error.clone(),
                    clear_last_error: patch.clear_last_error,
                    nudged_at: patch.set_nudged_at.clone(),
                    nudge_count: patch.set_nudge_count,
                    started_at: patch.set_started_at.clone(),
                    preserve_started_at: patch.set_started_at_preserve.is_some(),
                    finished_at: patch.set_finished_at.clone(),
                    clear_finished_at: patch.clear_finished_at,
                };
                // COALESCE semantics need the candidate under started_at.
                let values = store::ItemPatchValues {
                    started_at: patch.set_started_at_preserve.clone(),
                    ..values
                };
                store::update_run_item(tx, &patch.run_id, &patch.node_id, &values)?;
            }
            DbChange::ItemDelete { run_id, node_id } => {
                store::delete_run_item(tx, run_id, node_id)?;
            }
        }
    }
    Ok(())
}

impl Storage {
    // ── accessors ───────────────────────────────────────────────────────

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn files(&self) -> &DiskFiles {
        &self.files
    }

    pub(crate) fn files_mut(&mut self) -> &mut DiskFiles {
        &mut self.files
    }

    pub fn home_path(&self) -> &std::path::Path {
        &self.home
    }

    pub fn home_id(&self) -> Option<&str> {
        self.home_id.as_deref()
    }

    pub fn now_iso(&self) -> String {
        crate::clock::iso_from_ms(self.clock.now_ms())
    }

    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    pub fn lock_handle(&mut self) -> Option<&mut crate::home_lock::HomeLockHandle> {
        self.lock.as_mut()
    }

    /// Release the kernel lock (idempotent).
    pub fn release_lock(&mut self) -> Result<()> {
        match self.lock.take() {
            Some(handle) => handle.release(),
            None => Ok(()),
        }
    }
}

// ── plan builders ───────────────────────────────────────────────────────
//
// IO-side composition of deterministic mutation plans. Decision logic
// (hierarchy rules, transitions, trust gates) stays in omt-domain; callers
// hand validated rows/patches and these builders snapshot CURRENT file state
// into before-hashes + absolute after-contents so replays never depend on
// intermediate disk state.

use omt_domain::markdown;
use omt_domain::store::serialize_from_row;
use omt_domain::types::{NodeRow, RunItemRow};

impl Storage {
    /// CREATE: write the node file (+ parent managed-children rewrite),
    /// insert node + edge + search mirror, optionally bump the type counter.
    /// Mirrors core.ts create()'s dual-write shape minus decision gates.
    pub fn plan_create(
        &self,
        command_id: &str,
        node: &NodeRow,
        parent: Option<&NodeRow>,
        body: &str,
        auto_allocated_id: bool,
    ) -> Result<PreparedMutation> {
        let mut plan = PreparedMutation::new(
            command_id.to_string(),
            "create",
            input_fingerprint(&[&node.id, &node.title]),
        );

        let full_body =
            markdown::replace_children_block(body, &markdown::render_children_entries(&[]));
        let content = serialize_from_row(node, parent.map(|p| p.id.as_str()), &full_body);
        plan.push_file(FileOp::Write {
            path: node.path.clone(),
            before_sha: None,
            after_sha: crate::files::sha256_hex(&content),
            content,
        });

        if let Some(parent_node) = parent {
            let block = self.render_children_block_for(parent_node)?;
            let Some(existing) = self.files.read_optional(&parent_node.path)? else {
                return Err(Problem::with_details(
                    error::IO,
                    "parent file missing during create",
                    |d| {
                        d.insert("path".into(), parent_node.path.clone().into());
                    },
                ));
            };
            let parsed = markdown::parse_node_file(&existing)?;
            let new_body = markdown::replace_children_block(&parsed.body, &block);
            let parent_content = serialize_from_row(parent_node, None, &new_body);
            plan.push_file(FileOp::Write {
                path: parent_node.path.clone(),
                before_sha: Some(crate::files::sha256_hex(&existing)),
                after_sha: crate::files::sha256_hex(&parent_content),
                content: parent_content,
            });
            let ord = store::children_of(&self.conn, &parent_node.id)?.len() as i64;
            plan.push_change(DbChange::EdgeInsert {
                parent_id: parent_node.id.clone(),
                child_id: node.id.clone(),
                ord,
            });
        }

        if auto_allocated_id {
            plan.push_change(DbChange::CounterBump {
                prefix: prefix_of(&node.id),
            });
        }
        plan.push_change(DbChange::NodeInsert {
            node: NodeDto::from_row(node),
        });
        plan.push_change(DbChange::SearchUpsert {
            id: node.id.clone(),
            title: node.title.clone(),
            body: markdown::strip_children_block(&full_body),
        });
        plan.push_event(
            "node.created",
            serde_json::json!({ "nodeId": node.id, "path": node.path }),
        );
        plan.result = serde_json::to_value(NodeDto::from_row(node)).unwrap_or(Value::Null);
        Ok(plan)
    }

    /// UPDATE (also serves ARCHIVE via `set_archived`): rewrite the node file
    /// from patched metadata + new body, refresh search mirror, bump revision.
    /// Caller-supplied `extra_changes` carry downstream domain effects (passive
    /// observation item transitions, ancestor activation patches, terminal run
    /// derivation) so decision logic stays upstream.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_update(
        &self,
        command_id: &str,
        before: &NodeRow,
        set_title: Option<String>,
        set_status: Option<String>,
        set_archived: Option<bool>,
        set_priority: Option<i64>,
        replace_body: Option<String>,
        append_note: Option<String>,
        parent: Option<&NodeRow>,
        extra_changes: Vec<DbChange>,
        events_json: Value,
    ) -> Result<PreparedMutation> {
        // Fingerprint covers the SEMANTIC inputs (not just row identity) so
        // command-id reuse with different intent fails closed (R9).
        let mut plan = PreparedMutation::new(
            command_id.to_string(),
            "update",
            input_fingerprint(&[
                &before.id,
                &before.updated_at,
                &set_title.clone().unwrap_or_default(),
                &set_status.clone().unwrap_or_default(),
                &format!("{:?}", set_archived),
                &format!("{:?}", set_priority),
                &replace_body.clone().unwrap_or_default(),
                &append_note.clone().unwrap_or_default(),
            ]),
        );

        let existing = self.files.read_optional(&before.path)?;
        let Some(raw_existing) = &existing else {
            return Err(Problem::with_details(
                error::IO,
                "node file missing during update",
                |d| {
                    d.insert("path".into(), before.path.clone().into());
                },
            ));
        };
        let parsed = markdown::parse_node_file(raw_existing)?;
        let mut body = parsed.body;
        if let Some(replacement) = &replace_body {
            body = replacement.clone();
        } else if let Some(note) = &append_note {
            body = format!("{}\n\n{}\n", body.trim_end(), note);
        }

        let now = self.now_iso();
        let mut updated = before.clone();
        if let Some(title) = &set_title {
            updated.title = title.clone();
        }
        if let Some(status_text) = &set_status {
            updated.status = parse(status_text)?;
        }
        if let Some(archived) = set_archived {
            updated.archived = archived;
        }
        if let Some(priority) = set_priority {
            updated.priority = priority;
        }
        updated.updated_at = now.clone();

        // Managed children block survives untouched across updates.
        let content = serialize_from_row(&updated, parent.map(|p| p.id.as_str()), &body);
        plan.push_file(FileOp::Write {
            path: before.path.clone(),
            before_sha: Some(crate::files::sha256_hex(raw_existing)),
            after_sha: crate::files::sha256_hex(&content),
            content,
        });
        plan.push_change(DbChange::NodePatch {
            patch: NodePatch {
                id: before.id.clone(),
                set_title,
                set_status,
                set_archived,
                set_priority,
                set_path: None,
                set_updated_at: Some(now),
            },
        });
        plan.push_change(DbChange::SearchUpsert {
            id: before.id.clone(),
            title: updated.title.clone(),
            body: markdown::strip_children_block(&body),
        });
        for change in extra_changes {
            plan.push_change(change);
        }
        plan.push_event("node.updated", events_json);
        plan.result = serde_json::to_value(NodeDto::from_row(&updated)).unwrap_or(Value::Null);
        Ok(plan)
    }

    /// MOVE (multi-file): relocate the subtree directory, rewrite every
    /// relocated path row, re-point the moved node's frontmatter `parent`,
    /// refresh BOTH managed children blocks.
    #[allow(clippy::too_many_arguments)] // planner surface mirrors domain patch shape
    pub fn plan_move(
        &self,
        command_id: &str,
        moved_before: &NodeRow,
        new_parent: &NodeRow,
        old_parent: Option<&NodeRow>,
        subtree_before: &[NodeRow],
        new_path_by_id: &dyn Fn(&str) -> Option<String>,
        extra_changes: Vec<DbChange>,
    ) -> Result<PreparedMutation> {
        let now = self.now_iso();
        let old_path = &moved_before.path;
        let new_path = new_path_by_id(&moved_before.id).ok_or_else(|| {
            Problem::with_details(error::IO, "move produced no destination path", |d| {
                d.insert("nodeId".into(), moved_before.id.clone().into());
            })
        })?;
        let old_dir = markdown::dirname(old_path);
        let new_dir = markdown::dirname(&new_path);

        let mut plan = PreparedMutation::new(
            command_id.to_string(),
            "move",
            input_fingerprint(&[&moved_before.id, &new_parent.id]),
        );
        plan.push_file(FileOp::MoveDir {
            from: old_dir.clone(),
            to: new_dir.clone(),
        });

        // Rewritten frontmatter for the moved root. At PLAN time the file
        // still lives at its OLD path (the MoveDir applies later), so we
        // snapshot from there and target the NEW path with absolute content;
        // execution order (MoveDir first) makes the target valid, and the
        // cumulative drift gate models the rename.
        let existing = self.files.read_optional(old_path)?;
        let Some(raw_existing) = &existing else {
            return Err(Problem::with_details(
                error::IO,
                "moved file missing at source",
                |d| {
                    d.insert("path".into(), old_path.clone().into());
                },
            ));
        };
        let parsed = markdown::parse_node_file(raw_existing)?;
        let mut root_after = moved_before.clone();
        root_after.path = new_path.clone();
        root_after.updated_at = now.clone();
        let root_content = serialize_from_row(&root_after, Some(&new_parent.id), &parsed.body);
        plan.push_file(FileOp::Write {
            path: new_path.clone(),
            before_sha: Some(crate::files::sha256_hex(raw_existing)),
            after_sha: crate::files::sha256_hex(&root_content),
            content: root_content,
        });

        // Parent managed-children blocks: old parent drops the entry, new
        // parent gains it.
        if let Some(old_parent_node) = old_parent {
            if old_parent_node.id != new_parent.id {
                let block = self.render_children_block_for(old_parent_node)?;
                let raw = self
                    .files
                    .read_optional(&old_parent_node.path)?
                    .ok_or_else(|| {
                        Problem::with_details(
                            error::IO,
                            "old parent file missing during move",
                            |d| {
                                d.insert("path".into(), old_parent_node.path.clone().into());
                            },
                        )
                    })?;
                let parsed_old = markdown::parse_node_file(&raw)?;
                let content = serialize_from_row(
                    old_parent_node,
                    None,
                    &markdown::replace_children_block(&parsed_old.body, &block),
                );
                plan.push_file(FileOp::Write {
                    path: old_parent_node.path.clone(),
                    before_sha: Some(crate::files::sha256_hex(&raw)),
                    after_sha: crate::files::sha256_hex(&content),
                    content,
                });
            }
        }
        {
            let block = self.render_children_block_for(new_parent)?;
            let raw = self.files.read_optional(&new_parent.path)?.ok_or_else(|| {
                Problem::with_details(error::IO, "new parent file missing during move", |d| {
                    d.insert("path".into(), new_parent.path.clone().into());
                })
            })?;
            let parsed_new = markdown::parse_node_file(&raw)?;
            let content = serialize_from_row(
                new_parent,
                None,
                &markdown::replace_children_block(&parsed_new.body, &block),
            );
            plan.push_file(FileOp::Write {
                path: new_parent.path.clone(),
                before_sha: Some(crate::files::sha256_hex(&raw)),
                after_sha: crate::files::sha256_hex(&content),
                content,
            });
        }

        // DB side: relocate stored paths (subtree), swap the edge, stamp the
        // moved root's updated_at.
        for member in subtree_before {
            let Some(relocated) = new_path_by_id(&member.id) else {
                continue;
            };
            plan.push_change(DbChange::NodePatch {
                patch: NodePatch {
                    id: member.id.clone(),
                    set_title: None,
                    set_status: None,
                    set_archived: None,
                    set_priority: None,
                    set_path: Some(relocated),
                    set_updated_at: (member.id == moved_before.id).then(|| now.clone()),
                },
            });
        }
        if let Some(old_parent_node) = old_parent {
            plan.push_change(DbChange::EdgeDelete {
                parent_id: old_parent_node.id.clone(),
                child_id: moved_before.id.clone(),
            });
        }
        let ord = store::children_of(&self.conn, &new_parent.id)?.len() as i64;
        plan.push_change(DbChange::EdgeInsert {
            parent_id: new_parent.id.clone(),
            child_id: moved_before.id.clone(),
            ord,
        });
        for change in extra_changes {
            plan.push_change(change);
        }
        plan.push_event(
            "node.moved",
            serde_json::json!({ "nodeId": moved_before.id, "from": old_path, "to": new_path }),
        );
        plan.result = serde_json::to_value(NodeDto::from_row(&root_after)).unwrap_or(Value::Null);
        Ok(plan)
    }

    /// REPORT: transition the item, double-write the ticket (status + note
    /// append), refresh the search mirror. Trust-gate/lease authorization is
    /// upstream (domain runs module); this composes the durable effect.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_report(
        &self,
        command_id: &str,
        node_before: &NodeRow,
        parent: Option<&NodeRow>,
        outcome_status: Option<String>,
        note: Option<String>,
        item_patch: ItemPatch,
        extra_changes: Vec<DbChange>,
    ) -> Result<PreparedMutation> {
        let mut plan = self.plan_update(
            command_id,
            node_before,
            None,
            outcome_status.clone(),
            None,
            None,
            None,
            note,
            parent,
            vec![DbChange::ItemPatch { patch: item_patch }],
            serde_json::json!({ "nodeId": node_before.id, "outcome": outcome_status }),
        )?;
        plan.op_kind = "report".to_string();
        plan.input_hash = input_fingerprint(&[&node_before.id, command_id]);
        for change in extra_changes {
            plan.changes.push(change);
        }
        Ok(plan)
    }

    /// RUN CREATE (U5b): insert the run row, its pending items, and the RUN
    /// counter bump as ONE journaled mutation. Runs are DB-only (no file
    /// ops); decision gates (duplicates, member types, archive) stay
    /// upstream. Fingerprint pins [runId, every memberId] so command-id
    /// reuse with different membership fails closed (R9).
    pub fn plan_run_create(
        &self,
        command_id: &str,
        run: &omt_domain::types::RunRow,
        items: &[RunItemRow],
        home_id: &str,
    ) -> Result<PreparedMutation> {
        let mut fingerprint: Vec<&str> = vec![&run.id];
        let item_ids: Vec<String> = items.iter().map(|item| item.node_id.clone()).collect();
        let item_refs: Vec<&str> = item_ids.iter().map(String::as_str).collect();
        fingerprint.extend(item_refs);
        let mut plan = PreparedMutation::new(
            command_id.to_string(),
            "run_create",
            input_fingerprint(&fingerprint),
        );
        plan.push_change(DbChange::CounterBump {
            prefix: "RUN".into(),
        });
        plan.push_change(DbChange::RunInsert {
            run: RunDto::from_row(run),
        });
        for item in items {
            plan.push_change(DbChange::ItemInsert {
                item: ItemDto::from_row(item),
            });
        }
        plan.push_event(
            "run.changed",
            serde_json::json!({
                "kind": "run.changed",
                "ref": { "homeId": home_id, "runId": run.id },
            }),
        );
        plan.result = serde_json::to_value(RunDto::from_row(run)).unwrap_or(Value::Null);
        Ok(plan)
    }

    /// Generic run-plane mutation builder (U5b): control transitions,
    /// retry/remove, and report follow-ups (stop-on-failure pause, terminal
    /// derivation) compose validated [`DbChange`] lists + stream events +
    /// the result envelope into one journaled mutation. ALL decision logic
    /// stays upstream; this owns only durable shape + idempotency.
    pub fn plan_run_mutation(
        &self,
        command_id: &str,
        op_kind: &str,
        fingerprint_parts: &[&str],
        changes: Vec<DbChange>,
        events: Vec<(&str, Value)>,
        result: Value,
    ) -> PreparedMutation {
        let mut plan = PreparedMutation::new(
            command_id.to_string(),
            op_kind.to_string(),
            input_fingerprint(fingerprint_parts),
        );
        for change in changes {
            plan.push_change(change);
        }
        for (event_type, payload) in events {
            plan.push_event(event_type, payload);
        }
        plan.result = result;
        plan
    }

    /// Render the managed children block of one node from live edges.
    fn render_children_block_for(&self, node: &NodeRow) -> Result<String> {
        let children = store::children_of(&self.conn, &node.id)?;
        let entries: Vec<markdown::ChildEntry> = children
            .iter()
            .map(|child| markdown::ChildEntry {
                id: child.id.clone(),
                title: child.title.clone(),
                dir_name: markdown::node_dir_name(&child.id, &child.title),
                node_type: child.node_type.to_string(),
                status: child.status.to_string(),
            })
            .collect();
        Ok(markdown::render_children_entries(&entries))
    }
}

fn prefix_of(id: &str) -> String {
    id.split('-').next().unwrap_or_default().to_string()
}

/// Convenience used by tests/U5: build an [`ItemPatch`] for a report-style
/// transition (kept beside the planners for symmetry).
pub fn report_item_patch(
    item: &RunItemRow,
    outcome_state: &str,
    finished_at: String,
    last_error: Option<String>,
) -> ItemPatch {
    ItemPatch {
        run_id: item.run_id.clone(),
        node_id: item.node_id.clone(),
        set_state: Some(outcome_state.to_string()),
        set_executor_session_id: None,
        clear_executor_session_id: false,
        set_attempts: None,
        set_last_error: last_error,
        clear_last_error: false,
        set_nudged_at: None,
        set_nudge_count: None,
        set_started_at_preserve: None,
        set_started_at: None,
        set_finished_at: Some(finished_at),
        clear_finished_at: false,
    }
}
