//! Crash recovery (R7/F2): scan unacknowledged journal commands and converge
//! the home.
//!
//! Policy (documented resolution between "restore from recovery copies" and
//! "manual drift fails closed"):
//! - `phase == db_committed` → roll FORWARD to acknowledged + prune (never
//!   revert user-visible state).
//! - `phase < db_committed` → CUMULATIVE drift simulation of the whole plan:
//!     * a step whose target already carries its after-content is skipped;
//!     * a step whose target still carries its before-content replays fresh;
//!     * a target MISSING entirely (and not produced by an earlier plan step)
//!       is restored from its recovery copy, then replayed — the recovery-
//!       copy restore path;
//!     * any other observed content is MANUAL DRIFT: REINDEX_REQUIRED, fail
//!       closed, byte-identical refusal (never overwrite unknown edits).
//! - Replay then applies every non-applied step with absolute contents and
//!   finalizes exactly like a live execute() (single transaction), then rolls
//!   forward to acknowledged.

use crate::files::{sha256_hex, DiskFiles};
use crate::journal::{
    finalize_mutation, FileOp, JournalRow, PreparedMutation, PHASE_ACKNOWLEDGED, PHASE_DB_COMMITTED,
};
use crate::{Problem, Result};
use omt_domain::error;
use omt_domain::store::FileStore as _;
use std::collections::BTreeMap;

/// What one recovery pass did (assertion surface for the kill grid).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecoveryReport {
    /// Target paths restored from recovery copies before replay.
    pub restored: Vec<String>,
    /// Commands replayed from phase < db_committed to full convergence.
    pub replayed: Vec<String>,
    /// Commands rolled forward (db_committed but not yet acknowledged).
    pub rolled_forward: Vec<String>,
    /// File steps skipped because their after-state was already on disk.
    pub steps_skipped_applied: usize,
}

/// Marker value for directory presence in the virtual overlay.
const DIR_PRESENT: &str = "\u{0}dir";

/// Virtual disk overlay used by the drift gate: path → current content sha
/// (or [`DIR_PRESENT`]). Seeded from REAL disk state, then advanced step by
/// step so later steps see earlier steps' effects even when those steps never
/// reached disk before the crash.
struct Overlay {
    entries: BTreeMap<String, String>,
}

impl Overlay {
    fn get(&self, path: &str) -> Option<&str> {
        self.entries.get(path).map(String::as_str)
    }

    #[allow(dead_code)]
    fn set(&mut self, path: &str, sha: Option<&str>) {
        match sha {
            Some(sha) => {
                self.entries.insert(path.to_string(), sha.to_string());
            }
            None => {
                self.entries.remove(path);
            }
        }
    }

    /// Apply a MoveDir effect: relocate every key under `from/`, flip markers.
    fn apply_move(&mut self, from: &str, to: &str) {
        let moved: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|(path, _)| path.starts_with(&format!("{from}/")))
            .map(|(path, sha)| (path.clone(), sha.clone()))
            .collect();
        for (old_path, sha) in moved {
            let new_path = format!("{to}/{}", &old_path[from.len() + 1..]);
            self.entries.remove(&old_path);
            self.entries.insert(new_path, sha);
        }
        self.entries.remove(from);
        self.entries.insert(to.to_string(), DIR_PRESENT.to_string());
    }
}

pub(crate) fn recover_pending(storage: &mut crate::journal::Storage) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();
    let rows = pending_commands(storage.conn())?;
    for row in rows {
        if row.phase == PHASE_DB_COMMITTED {
            // Crash after commit, before acknowledge: roll FORWARD only.
            roll_forward(storage, &row.command_id)?;
            report.rolled_forward.push(row.command_id.clone());
            continue;
        }
        // Phase < db_committed: gate first (may fail closed), then restore +
        // replay + finalize exactly once.
        let actions = drift_gate(storage, &row.plan)?;
        execute_restore_actions(storage, &row.plan.command_id, &actions)?;
        report.restored.extend(actions.restored);

        // Replay the non-applied steps deterministically (absolute contents).
        replay_files(storage, &row.plan, &actions.applied_steps)?;

        // Finalize in ONE transaction; then acknowledge + prune.
        finalize_mutation(
            storage.conn(),
            storage.home_id().map(str::to_string).as_deref(),
            &row.plan,
            &storage.now_iso(),
        )?;
        roll_forward(storage, &row.command_id)?;
        report.replayed.push(row.command_id.clone());
        report.steps_skipped_applied += actions.skipped_applied;
    }
    Ok(report)
}

fn roll_forward(storage: &mut crate::journal::Storage, command_id: &str) -> Result<()> {
    storage.journal_set_phase(command_id, PHASE_ACKNOWLEDGED)?;
    storage.files_mut().prune_recovery(command_id)
}

// ── pending journal scan ────────────────────────────────────────────────

pub(crate) fn pending_commands(conn: &rusqlite::Connection) -> Result<Vec<JournalRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT op_id, command_id, op_kind, phase, input_hash, plan_json
             FROM journal WHERE phase != 'acknowledged' ORDER BY op_id",
        )
        .map_err(|err| sql_problem("pending select", err))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|err| sql_problem("pending query", err))?;
    let mut out = Vec::new();
    for row in rows {
        let (op_id, command_id, op_kind, phase, input_hash, plan_json) =
            row.map_err(|err| sql_problem("pending read", err))?;
        let plan: PreparedMutation = serde_json::from_str(&plan_json).map_err(|err| {
            Problem::with_details(
                error::IO,
                format!("corrupt journal plan for {command_id}"),
                |d| {
                    d.insert("commandId".into(), command_id.clone().into());
                    d.insert("error".into(), err.to_string().into());
                },
            )
        })?;
        out.push(JournalRow {
            op_id,
            command_id,
            op_kind,
            phase,
            input_hash,
            plan,
        });
    }
    Ok(out)
}

fn sql_problem(context: &str, err: rusqlite::Error) -> Problem {
    Problem::with_details(
        error::IO,
        format!("recovery sqlite {context}: {err}"),
        |d| {
            d.insert("sqliteContext".into(), context.into());
        },
    )
}

// ── cumulative drift gate ───────────────────────────────────────────────

struct GateActions {
    /// Steps (by index) that must be applied during replay.
    applied_steps: Vec<usize>,
    skipped_applied: usize,
    /// Targets restored from recovery copies before replay.
    restored: Vec<String>,
}

/// Simulate every file step against real-disk-seeded state, deciding per
/// step: skip / replay / restore-then-replay / DRIFT-FAIL-CLOSED.
///
/// Manual-drift detection: BEFORE simulation, every Write target whose
/// current content matches NEITHER its recorded before-sha NOR its planned
/// after-sha — and which no earlier MoveDir of this plan could have produced
/// — is user drift; the whole recovery fails closed without touching bytes.
fn drift_gate(storage: &crate::journal::Storage, plan: &PreparedMutation) -> Result<GateActions> {
    verify_no_manual_drift(storage.files(), plan)?;
    let mut overlay = seed_overlay(storage.files(), plan)?;
    let mut actions = GateActions {
        applied_steps: Vec::new(),
        skipped_applied: 0,
        restored: Vec::new(),
    };

    for (index, step) in plan.files.iter().enumerate() {
        match step {
            FileOp::Write { path, content, .. } => {
                let after_sha = sha256_hex(content);
                match overlay.get(path) {
                    Some(existing) if existing == after_sha => {
                        // Already applied before the crash.
                        actions.skipped_applied += 1;
                    }
                    Some(_) | None => {
                        let will_exist_after_move = plan.files[..index].iter().any(|prior| {
                            matches!(prior, FileOp::MoveDir { to, .. } if path_starts_with(path, to))
                        });
                        if overlay.get(path).is_none()
                            && !will_exist_after_move
                            && plan_has_before(plan, path)
                        {
                            // Missing target of an UPDATE-shaped write:
                            // restore from the recovery copy.
                            actions.restored.push(path.clone());
                            // Overlay now reflects restored original; replay
                            // overwrites it right after.
                            let copy_rel = DiskFiles::recovery_copy_rel(&plan.command_id, path);
                            let sha = storage.files().hash_current(&copy_rel)?;
                            if sha.is_none() {
                                return Err(drift_problem(plan, path));
                            }
                        }
                        actions.applied_steps.push(index);
                        let new_sha = after_sha.to_string();
                        overlay.set(path, Some(&new_sha));
                    }
                }
            }
            FileOp::MoveDir { from, to } => {
                let from_present = matches!(overlay.get(from), Some(DIR_PRESENT));
                let to_present = matches!(overlay.get(to), Some(DIR_PRESENT));
                if !from_present && to_present {
                    actions.skipped_applied += 1;
                } else if from_present && !to_present {
                    actions.applied_steps.push(index);
                    overlay.apply_move(from, to);
                } else if from_present && to_present {
                    // Both present: ambiguous collision — refuse closed.
                    return Err(Problem::with_details(
                        error::REINDEX_REQUIRED,
                        "move target already exists while source remains",
                        |d| {
                            d.insert("commandId".into(), plan.command_id.clone().into());
                            d.insert("from".into(), from.clone().into());
                            d.insert("to".into(), to.clone().into());
                            d.insert("requiresReindex".into(), true.into());
                        },
                    ));
                } else {
                    return Err(drift_problem_move(plan, from));
                }
            }
        }
    }
    Ok(actions)
}

fn plan_has_before(plan: &PreparedMutation, path: &str) -> bool {
    plan.files
        .iter()
        .any(|op| matches!(op, FileOp::Write { path: p, before_sha: Some(_), .. } if p == path))
}

fn path_starts_with(path: &str, dir: &str) -> bool {
    path.starts_with(&format!("{dir}/"))
}

/// Fail closed when ANY Write target's CURRENT bytes match neither its
/// before-sha nor its planned after-sha and no earlier MoveDir explains the
/// state. Byte-identical refusal: nothing is written before this passes.
fn verify_no_manual_drift(files: &DiskFiles, plan: &PreparedMutation) -> Result<()> {
    for (index, step) in plan.files.iter().enumerate() {
        let FileOp::Write {
            path,
            content: _,
            before_sha,
            after_sha,
        } = step
        else {
            continue;
        };
        let Some(before) = before_sha else { continue }; // creates may be absent
        let current = files.hash_current(path)?;
        let Some(current) = current else { continue }; // missing handled by gate
        if current == before.as_str() || current == after_sha.as_str() {
            continue;
        }
        // A prior MoveDir in THIS plan relocates content here legitimately.
        let explained_by_move = plan.files[..index]
            .iter()
            .any(|prior| matches!(prior, FileOp::MoveDir { to, .. } if path_starts_with(path, to)));
        if !explained_by_move {
            return Err(drift_problem(plan, path));
        }
    }
    // MoveDir endpoints: both-present or both-absent collisions refuse.
    for step in &plan.files {
        if let FileOp::MoveDir { from, to } = step {
            let from_exists = files.is_dir(from);
            let to_exists = files.is_dir(to);
            if from_exists == to_exists {
                return Err(Problem::with_details(
                    error::REINDEX_REQUIRED,
                    "pending move is ambiguous on disk",
                    |d| {
                        d.insert("commandId".into(), plan.command_id.clone().into());
                        d.insert("from".into(), from.clone().into());
                        d.insert("to".into(), to.clone().into());
                        d.insert("requiresReindex".into(), true.into());
                    },
                ));
            }
        }
    }
    Ok(())
}

/// Seed the overlay from REAL disk: hash every Write target, both endpoints
/// of every MoveDir, and every file contained in each source/target dir.
fn seed_overlay(files: &DiskFiles, plan: &PreparedMutation) -> Result<Overlay> {
    let mut overlay = Overlay {
        entries: BTreeMap::new(),
    };
    for step in &plan.files {
        match step {
            FileOp::Write { path, .. } => {
                let sha = files.hash_current(path)?;
                if let Some(sha) = sha {
                    overlay.set(path, Some(&sha));
                }
            }
            FileOp::MoveDir { from, to } => {
                for dir in [from, to] {
                    for contained in files.list_files_under(dir) {
                        let sha = files.hash_current(&contained)?;
                        if let Some(sha) = sha {
                            overlay.set(&contained, Some(&sha));
                        }
                    }
                }
                overlay.set(from, files.is_dir(from).then_some(DIR_PRESENT));
                overlay.set(to, files.is_dir(to).then_some(DIR_PRESENT));
            }
        }
    }
    Ok(overlay)
}

fn drift_problem(plan: &PreparedMutation, path: &str) -> Problem {
    Problem::with_details(
        error::REINDEX_REQUIRED,
        format!(
            "file drifted during a pending plan; refusing to overwrite (command {})",
            plan.command_id
        ),
        |d| {
            d.insert("commandId".into(), plan.command_id.clone().into());
            d.insert("driftedPath".into(), path.into());
            d.insert("requiresReindex".into(), true.into());
        },
    )
}

fn drift_problem_move(plan: &PreparedMutation, from: &str) -> Problem {
    Problem::with_details(
        error::REINDEX_REQUIRED,
        "moved-away directory vanished during a pending plan",
        |d| {
            d.insert("commandId".into(), plan.command_id.clone().into());
            d.insert("from".into(), from.into());
            d.insert("requiresReindex".into(), true.into());
        },
    )
}

/// Restore missing targets from their recovery copies (BEFORE replay).
fn execute_restore_actions(
    storage: &mut crate::journal::Storage,
    command_id: &str,
    actions: &GateActions,
) -> Result<()> {
    if actions.restored.is_empty() {
        return Ok(());
    }
    let token = crate::files::entropy_token();
    for target in &actions.restored {
        let copy_rel = DiskFiles::recovery_copy_rel(command_id, target);
        let restored = storage
            .files_mut()
            .restore_from_recovery(&copy_rel, target, &token)?;
        if !restored {
            return Err(drift_problem_by_id(command_id, target));
        }
    }
    Ok(())
}

fn drift_problem_by_id(command_id: &str, path: &str) -> Problem {
    Problem::with_details(
        error::REINDEX_REQUIRED,
        "recovery copy vanished mid-restore",
        |d| {
            d.insert("commandId".into(), command_id.into());
            d.insert("driftedPath".into(), path.into());
            d.insert("requiresReindex".into(), true.into());
        },
    )
}

/// Apply the decided steps with absolute contents (no fault injection —
/// replay is deterministic).
fn replay_files(
    storage: &mut crate::journal::Storage,
    plan: &PreparedMutation,
    applied_steps: &[usize],
) -> Result<()> {
    for index in applied_steps {
        match &plan.files[*index] {
            FileOp::Write { path, content, .. } => {
                let token = crate::files::entropy_token();
                storage.files_mut().atomic_write(path, content, &token)?;
            }
            FileOp::MoveDir { from, to } => {
                let token = crate::files::entropy_token();
                DiskFiles::move_dir(storage.files_mut(), from, to, &token)?;
            }
        }
    }
    Ok(())
}
