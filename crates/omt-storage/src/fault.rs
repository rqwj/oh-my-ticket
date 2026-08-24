//! Kill-point injection (U4 evidence strategy): every durable step of a
//! mutation — recovery copy, temp write, file fsync, rename-over, directory
//! fsync, transaction commit, acknowledge — is an injectable failure point.
//! A [`FaultSchedule`] names ONE ordinal; the executor checks each step in a
//! deterministic order and returns an injected IO problem at the scheduled
//! one. Tests then drop the [`Storage`](crate::journal::Storage) handle
//! (crash simulation), reopen over the same home, and assert convergence.

use crate::Problem;
use omt_domain::error;

/// One injectable executor step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A phase of the `index`-th file operation of the plan.
    FileOp { index: usize, phase: FilePhase },
    /// Immediately before the finalize transaction COMMITs.
    TxnCommit,
    /// Immediately before the acknowledge mark + recovery prune.
    Acknowledge,
}

/// Phases of a journaled file operation. `Write` ops run all five; `MoveDir`
/// ops run the last three (snapshot → rename → dir fsync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePhase {
    /// Copy the original content into `.omt/recovery/<commandId>/`.
    RecoveryCopy,
    /// Write the new content to a temp file in the same directory.
    WriteTemp,
    /// fsync the temp file before it becomes visible under its real name.
    FsyncTemp,
    /// Rename the temp file over the target.
    RenameOver,
    /// fsync the parent directory so the rename is durable.
    FsyncParentDir,
}

/// Deterministic fault schedule: fire exactly once at plan-step `ordinal`
/// (`None` = never; production default). The ordinal counts every
/// [`Step`] check in execution order — per-file phases first, then
/// `TxnCommit`, then `Acknowledge`.
#[derive(Debug, Clone, Default)]
pub struct FaultSchedule {
    pub fail_at_ordinal: Option<usize>,
}

impl FaultSchedule {
    pub fn never() -> Self {
        FaultSchedule {
            fail_at_ordinal: None,
        }
    }

    pub fn at(ordinal: usize) -> Self {
        FaultSchedule {
            fail_at_ordinal: Some(ordinal),
        }
    }
}

/// Per-open fault gun: counts checked steps and fires the scheduled one.
/// Recovery runs always use a never-firing gun (replay must be deterministic).
#[derive(Debug)]
pub(crate) struct FaultGun {
    schedule: FaultSchedule,
    next_ordinal: usize,
}

impl FaultGun {
    pub fn new(schedule: FaultSchedule) -> Self {
        FaultGun {
            schedule,
            next_ordinal: 0,
        }
    }

    /// Check one step: fires the injected problem when this check's ordinal
    /// matches the schedule, otherwise records the step and continues.
    pub(crate) fn check(&mut self, step: Step) -> Result<(), Problem> {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        if self.schedule.fail_at_ordinal == Some(ordinal) {
            return Err(Problem::with_details(
                error::IO,
                format!("injected kill-point at ordinal {ordinal}: {step:?}"),
                |d| {
                    d.insert("injected".into(), "kill-point".into());
                    d.insert("ordinal".into(), ordinal.into());
                    d.insert("step".into(), format!("{step:?}").into());
                },
            ));
        }
        Ok(())
    }

    /// Total steps a plan will check — the grid iterates `0..count`.
    #[allow(dead_code)] // exercised by tests/kill_grid.rs planning
    pub(crate) fn static_plan_step_count(files: &[crate::journal::FileOp]) -> usize {
        files.iter().map(|op| op.step_count()).sum::<usize>() + 2 // TxnCommit + Acknowledge
    }
}
