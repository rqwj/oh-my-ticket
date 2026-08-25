//! Quiescent takeover of a bridge-era home (U6 / TICKET-0124).
//!
//! Pipeline: quiescence preflight → bundle snapshot → exclusive open
//! (journal recovery + schema migrations under the daemon owner lock) →
//! generation fence → persistent daemon-owned marker. ANY failure after
//! the snapshot rolls the home back from that snapshot before the problem
//! surfaces, so an interrupted migration never leaves a half-converted
//! home behind.
//!
//! Refusal matrix (binding rulings, `ownership.rs`):
//! - a LIVE ts-bridge writer (alive pid + fresh heartbeat) refuses with
//!   actionable guidance — takeover requires quiescence;
//! - a stale/dead ts-bridge marker is the takeover target;
//! - a live daemon marker refuses as a second writer.

use crate::descriptor::pid_live;
use omt_domain::error;
use omt_storage::backup::{backup_home, restore_home};
use omt_storage::clock::{iso_from_ms, parse_iso_ms, MillisClock, SystemClock};
use omt_storage::home_lock::{
    LockBody, OwnerKind, DEFAULT_STALE_MS, LOCK_FILE_NAME, LOCK_SCHEMA_VERSION,
};
use omt_storage::journal::OpenConfig;
use omt_storage::{store, Problem, Result};
use std::path::Path;

/// Meta key fencing writers by capability generation (TICKET-0124).
pub const TAKEOVER_GENERATION_KEY: &str = "takeover.generation";
/// Generation written by this binary's takeover; legacy writers are
/// generation 1 and have no knowledge of the key.
pub const TAKEOVER_GENERATION: i64 = 2;

/// Test-only failure injection seam: where to abort the pipeline so the
/// rollback path can be exercised deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// Fail immediately after the bundle snapshot committed to disk.
    AfterSnapshot,
}

/// Take over one bridge-era home end-to-end. Offline verb: the caller must
/// ensure no live daemon serves the runtime dir (`refuse_if_served`).
pub fn takeover_home(
    runtime_dir: &Path,
    home: &Path,
    backups_root: &Path,
) -> Result<serde_json::Value> {
    run(runtime_dir, home, backups_root, None)
}

/// Same pipeline with a deterministic fault injection point (tests only).
pub fn takeover_home_with_fault(
    runtime_dir: &Path,
    home: &Path,
    backups_root: &Path,
    fault: FaultPoint,
) -> Result<serde_json::Value> {
    run(runtime_dir, home, backups_root, Some(fault))
}

fn run(
    runtime_dir: &Path,
    home: &Path,
    backups_root: &Path,
    fault: Option<FaultPoint>,
) -> Result<serde_json::Value> {
    crate::ownership::refuse_if_served(runtime_dir)?;
    let clock = SystemClock;

    // ── 1. quiescence preflight (marker-level, before any mutation) ────
    preflight(home)?;

    // ── 1b. the EXPLICIT clearing act ───────────────────────────────────
    // The storage layer NEVER auto-steals a ts-bridge marker (ruling):
    // quiescence having been proven above, takeover is the one sanctioned
    // point that removes it. A dead own-kind daemon marker is recovered
    // downstream by the normal boot path.
    // The marker file itself is RUNTIME STATE and never part of a bundle,
    // so capture its bytes now for exact restoration on rollback.
    let marker_before = std::fs::read_to_string(home.join(LOCK_FILE_NAME)).ok();
    clear_quiesced_bridge_marker(home);

    // ── 2. pristine snapshot + fenced conversion, rollback on failure ──
    let stamp = clock.now_ms();
    let bundle = backups_root.join(format!("takeover-{stamp}"));
    let attempt = || -> Result<serde_json::Value> {
        std::fs::create_dir_all(backups_root).map_err(|err| {
            Problem::with_details(error::IO, format!("create backups root: {err}"), |d| {
                d.insert("dir".into(), backups_root.display().to_string().into());
            })
        })?;
        backup_home(home, &bundle, &clock)?;

        if fault == Some(FaultPoint::AfterSnapshot) {
            return Err(Problem::with_details(
                error::IO,
                "injected fault after snapshot (test seam)",
                |d| {
                    d.insert("fault".into(), "after-snapshot".into());
                },
            ));
        }

        // Exclusive open: journal recovery runs first, then idempotent
        // migrations land the schema at this binary's KNOWN_SCHEMA_VERSION.
        // Acquiring the daemon owner lock transparently steals a STALE
        // ts-bridge marker (the storage refusal matrix already refuses a
        // LIVE one, and preflight refused it above).
        let mut config = OpenConfig::new(home);
        config.acquire_lock = true;
        config.owner_kind = OwnerKind::Daemon;
        config.recover_on_open = true;
        config.hostname = "omt-takeover".to_string();
        let mut storage = omt_storage::Storage::open(config)?;

        // Generation fence: future readers may refuse writers below the
        // floor; legacy writers simply ignore the key (and are fenced by
        // the marker left behind below).
        store::set_meta(
            storage.conn(),
            TAKEOVER_GENERATION_KEY,
            &TAKEOVER_GENERATION.to_string(),
        )?;
        storage.release_lock()?;

        // ── 3. persistent fence for legacy writers ──────────────────────
        // Leave a daemon-owned marker behind after the flock is released:
        // legacy bridges refuse ANY daemon marker outright (DAEMON_OWNS_
        // HOME) and cannot recover our dead pid, while a NEW daemon treats
        // it as its own dead predecessor and auto-recovers at boot.
        write_fence_marker(home, &clock)?;

        Ok(serde_json::json!({
            "home": home.display().to_string(),
            "bundle": bundle.display().to_string(),
            "generation": TAKEOVER_GENERATION,
            "fence": "daemon-marker",
        }))
    };

    match attempt() {
        Ok(report) => Ok(report),
        Err(mut problem) => {
            // Rollback: clear the possibly half-written database, then lay
            // the pristine snapshot back over the home. Best-effort: the
            // original problem is what the caller must see either way.
            let rollback = (|| -> Result<()> {
                for suffix in ["", "-wal", "-shm"] {
                    let db = home.join(format!("{}{suffix}", omt_storage::DB_FILE_NAME));
                    if db.exists() {
                        std::fs::remove_file(&db).map_err(|err| {
                            Problem::new(
                                error::IO,
                                format!("rollback remove {}: {err}", db.display()),
                            )
                        })?;
                    }
                }
                restore_home(&bundle, home)?;
                // Restore the pre-takeover marker verbatim (runtime state,
                // never captured by the bundle).
                match &marker_before {
                    Some(text) => {
                        std::fs::write(home.join(LOCK_FILE_NAME), text).map_err(|err| {
                            Problem::new(error::IO, format!("rollback restore marker: {err}"))
                        })?
                    }
                    None => {
                        let _ = std::fs::remove_file(home.join(LOCK_FILE_NAME));
                    }
                }
                Ok(())
            })();
            let mut extra = serde_json::Map::new();
            extra.insert("rolledBack".into(), serde_json::json!(rollback.is_ok()));
            extra.insert(
                "rollbackError".into(),
                serde_json::json!(rollback.err().map(|p| p.to_string())),
            );
            extra.insert("bundle".into(), bundle.display().to_string().into());
            match &mut problem.details {
                Some(serde_json::Value::Object(existing)) => existing.extend(extra),
                _ => problem.details = Some(serde_json::Value::Object(extra)),
            }
            Err(problem)
        }
    }
}

/// Refuse anything that is not quiescent, with actionable guidance.
fn preflight(home: &Path) -> Result<()> {
    let Some(body) = read_marker(home) else {
        return Ok(()); // no marker: nothing to take over, plain open works
    };
    let owner = body["ownerKind"].as_str().unwrap_or_default();
    if owner != "ts-bridge" {
        // Daemon markers: a live pid means another daemon owns the home.
        let pid = body["pid"].as_i64();
        if owner == "daemon" && pid.map(pid_live).unwrap_or(false) {
            return Err(Problem::with_details(
                error::DAEMON_OWNS_HOME,
                format!(
                    "home {} is owned by a live omt-daemon (pid {:?}); stop it before takeover",
                    home.display(),
                    body["pid"]
                ),
                |d| {
                    d.insert("reason".into(), "active-daemon-writer".into());
                    d.insert(
                        "hint".into(),
                        "stop the serving daemon, then rerun `omt takeover`".into(),
                    );
                },
            ));
        }
        return Ok(()); // dead daemon marker: recoverable downstream
    }
    // ts-bridge marker: refuse ONLY when genuinely active.
    let pid = body["pid"].as_i64();
    let heartbeat_age = body["heartbeatAt"]
        .as_str()
        .and_then(parse_iso_ms)
        .map(|hb| SystemClock.now_ms().saturating_sub(hb))
        .unwrap_or(i64::MAX);
    let alive = pid.map(pid_live).unwrap_or(false);
    if alive && heartbeat_age < DEFAULT_STALE_MS {
        return Err(Problem::with_details(
            error::HOME_LOCKED,
            format!(
                "home {} has an ACTIVE legacy writer (ts-bridge pid {:?}); takeover requires quiescence",
                home.display(),
                body["pid"]
            ),
            |d| {
                d.insert("reason".into(), "active-legacy-writer".into());
                d.insert("hint".into(), "stop the legacy bridge process, confirm with `omt doctor`, then rerun `omt takeover`".into());
            },
        ));
    }
    Ok(())
}

fn read_marker(home: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(home.join(LOCK_FILE_NAME)).ok()?;
    serde_json::from_str(raw.trim()).ok()
}

/// Remove a ts-bridge marker already proven quiescent by [`preflight`].
fn clear_quiesced_bridge_marker(home: &Path) {
    let Some(body) = read_marker(home) else {
        return;
    };
    if body["ownerKind"].as_str() == Some("ts-bridge") {
        let _ = std::fs::remove_file(home.join(LOCK_FILE_NAME));
    }
}

/// Publish a daemon-owned marker WITHOUT holding its flock: a tombstone
/// that fences legacy writers but is auto-recoverable by any new daemon.
fn write_fence_marker(home: &Path, clock: &SystemClock) -> Result<()> {
    let now = iso_from_ms(clock.now_ms());
    let body = LockBody {
        schema_version: LOCK_SCHEMA_VERSION,
        owner_kind: OwnerKind::Daemon.as_str().to_string(),
        pid: Some(std::process::id() as i64),
        hostname: Some("omt-takeover".to_string()),
        acquired_at: now.clone(),
        heartbeat_at: now,
        token: omt_storage::files::sha256_hex(&format!(
            "{}:{}",
            home.display(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        )),
    };
    let text = serde_json::to_string(&body)
        .map_err(|err| Problem::new(error::IO, format!("serialize fence marker: {err}")))?;
    std::fs::write(home.join(LOCK_FILE_NAME), text).map_err(|err| {
        Problem::with_details(error::IO, format!("write fence marker: {err}"), |d| {
            d.insert("home".into(), home.display().to_string().into());
        })
    })
}
