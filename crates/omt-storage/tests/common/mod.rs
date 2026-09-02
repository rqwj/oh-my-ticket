//! Shared fixtures for omt-storage suites: deterministic clock, temp homes,
//! baseline seeding through the journaled executor.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omt_domain::markdown;
use omt_domain::types::{NodeRow, RunConfigValue, RunItemRow, RunRow};
use omt_storage::clock::{FixedClock, MillisClock};
use omt_storage::home_lock::OwnerKind;
use omt_storage::journal::{OpenConfig, Storage};
use omt_storage::{FaultSchedule, Problem};

/// Deterministic epoch: 2026-08-24T05:00:00.000Z.
pub const T0_MS: i64 = 1_787_547_600_000; // 2026-08-24T05:00:00.000Z
/// One heartbeat cadence tick used when advancing the lock clock.
pub const HEARTBEAT_TICK_MS: i64 = 10_000;

pub fn fixed_clock() -> Arc<FixedClock> {
    Arc::new(FixedClock::at_ms(T0_MS))
}

pub fn temp_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    (dir, home)
}

pub fn open_config(home: &Path, clock: &Arc<dyn MillisClock>) -> OpenConfig {
    let mut config = OpenConfig::new(home);
    let cloned: Arc<dyn MillisClock> = Arc::clone(clock);
    config.clock = cloned;
    config.owner_kind = OwnerKind::Daemon;
    config.hostname = "grid-test".to_string();
    config.recover_on_open = true;
    config.acquire_lock = false;
    config.fault = FaultSchedule::never();
    config
}

pub fn open_storage(home: &Path, clock: &Arc<FixedClock>) -> Storage {
    let any_clock: Arc<dyn MillisClock> = clock.clone();
    Storage::open(open_config(home, &any_clock)).expect("storage open")
}

/// Open with a fault schedule armed (kill-point grid cells).
pub fn open_storage_armed(
    home: &Path,
    clock: &Arc<FixedClock>,
    schedule: FaultSchedule,
) -> Storage {
    let any_clock: Arc<dyn MillisClock> = clock.clone();
    let mut config = open_config(home, &any_clock);
    config.fault = schedule;
    Storage::open(config).expect("storage open (armed)")
}

/// Seed one node file+row through the journaled executor (suite-shared).
pub fn execute_simple(
    storage: &mut Storage,
    command_id: &str,
    node: &NodeRow,
    parent: Option<&NodeRow>,
    body: &str,
) {
    let plan = storage
        .plan_create(command_id, node, parent, body, false)
        .expect("plan");
    storage.execute(&plan).expect("execute");
}

pub fn expect_problem<T>(outcome: Result<T, Problem>, code: &str) -> Problem {
    match outcome {
        Ok(_) => panic!("expected problem {code}, got success"),
        Err(problem) => {
            assert_eq!(
                problem.code, code,
                "wrong problem code: {}",
                problem.message
            );
            problem
        }
    }
}

// ── row factories ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn node_row(id: &str, node_type: &str, title: &str, status: &str, path: &str) -> NodeRow {
    NodeRow {
        id: id.to_string(),
        node_type: node_type.parse().expect("node type"),
        title: title.to_string(),
        status: status.parse().expect("status"),
        archived: false,
        priority: 0,
        path: path.to_string(),
        created_at: iso_at(T0_MS),
        updated_at: iso_at(T0_MS),
    }
}

pub fn run_row(id: &str, status: &str) -> RunRow {
    RunRow {
        id: id.to_string(),
        title: Some(format!("run {id}")),
        status: status.parse().expect("run status"),
        config: RunConfigValue::default(),
        created_at: iso_at(T0_MS),
        finished_at: None,
    }
}

pub fn item_row(run_id: &str, node_id: &str, position: i64) -> RunItemRow {
    RunItemRow::new(run_id, node_id, position, "pending".parse().expect("state"))
}

pub fn iso_at(ms: i64) -> String {
    omt_storage::clock::iso_from_ms(ms)
}

pub fn slug(title: &str) -> String {
    markdown::slugify(title)
}

pub fn dir_of(path: &str) -> String {
    markdown::dirname(path)
}
