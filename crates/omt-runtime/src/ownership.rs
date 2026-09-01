//! Exclusive home-ownership helpers (U5b/U5c) shared by daemon startup and
//! the CLI's offline maintenance verbs.
//!
//! Binding rulings implemented here (orchestrator, 2026-08-24):
//! - **Daemon restart may auto-recover ONLY its own homes:** a `home.lock`
//!   whose `ownerKind` is `"daemon"` with a DEAD pid is removable by a new
//!   daemon/CLI taking the home (it can only be our own predecessor; the
//!   kernel flock is probed first so a living lease never loses the file).
//! - **Any ts-bridge marker requires explicit takeover (U6):** live or
//!   stale, a `ts-bridge` marker refuses daemon/CLI writers with
//!   actionable guidance — never auto-stealed above the storage layer.
//! - A LIVE daemon marker refuses with `DAEMON_OWNS_HOME` (second writer),
//!   matching the U2b cross-language matrix.
//!
//! Offline maintenance additionally requires that NO live daemon serves
//! the runtime dir ([`refuse_if_served`]).

use crate::descriptor;
use omt_domain::error;
use omt_storage::{Problem, Result};

#[derive(Debug, Clone, serde::Deserialize)]
struct MarkerProbe {
    #[serde(rename = "ownerKind")]
    owner_kind: String,
    #[serde(default)]
    pid: Option<i64>,
}

/// Read `<home>/home.lock`, tolerating absence/corruption (the storage
/// layer owns the full matrix downstream; this preflight only rules on the
/// two cases the RUNTIME must decide above it).
fn read_marker(home: &std::path::Path) -> Result<Option<MarkerProbe>> {
    let path = home.join(omt_storage::home_lock::LOCK_FILE_NAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None), // unreadable: defer to the lock layer
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    Ok(serde_json::from_str(&raw).ok())
}

/// Auto-recover OUR OWN dead predecessor's daemon marker (ruling 1).
/// Returns Ok when the caller may proceed to acquire; refuses otherwise.
pub fn recover_own_dead_daemon_marker(home: &std::path::Path) -> Result<()> {
    let Some(marker) = read_marker(home)? else {
        return Ok(());
    };
    match marker.owner_kind.as_str() {
        "ts-bridge" => Err(Problem::with_details(
            error::HOME_LOCKED,
            format!(
                "home {} carries a TypeScript-bridge writer marker; explicit takeover is required (see `omt doctor`, docs/runtime/takeover.md)",
                home.display()
            ),
            |d| {
                d.insert("reason".into(), "ts-bridge-requires-takeover".into());
                d.insert("owner".into(), serde_json::json!({ "ownerKind": "ts-bridge", "pid": marker.pid }));
            },
        )),
        "daemon" => {
            let alive = marker.pid.map(descriptor::pid_live).unwrap_or(false);
            // The flock, not the pid, is the liveness authority: a marker
            // whose inode is NOT flocked is a tombstone (e.g. the
            // TICKET-0124 takeover fence left by an exited CLI) even when
            // its recorded pid happens to be recycled/alive.
            let flocked =
                omt_storage::home_lock::inode_is_flocked_public(home).unwrap_or(true);
            if alive && flocked {
                return Err(daemon_owns_problem(home, marker.pid));
            }
            let path = home.join(omt_storage::home_lock::LOCK_FILE_NAME);
            let _ = std::fs::remove_file(&path);
            crate::logging::log(
                "info",
                "OWNERSHIP_RECOVERY",
                &format!(
                    "removed dead-daemon owner marker (pid {:?}) from {}",
                    marker.pid,
                    home.display()
                ),
            );
            Ok(())
        }
        _ => Ok(()), // unknown kinds: the storage-layer matrix fails closed
    }
}

fn daemon_owns_problem(home: &std::path::Path, pid: Option<i64>) -> Problem {
    Problem::with_details(
        error::DAEMON_OWNS_HOME,
        format!(
            "home {} is owned by a live omt-daemon (pid {:?}); stop it first (`omt daemon-stop`)",
            home.display(),
            pid
        ),
        |d| {
            d.insert(
                "owner".into(),
                serde_json::json!({ "ownerKind": "daemon", "pid": pid }),
            );
        },
    )
}

/// Offline gate: refuse when a LIVE daemon serves this runtime dir
/// (descriptor published + pid alive + endpoint connectable).
pub fn refuse_if_served(runtime_dir: &std::path::Path) -> Result<()> {
    let Some(d) = descriptor::read(runtime_dir) else {
        return Ok(());
    };
    if d.schema_version != descriptor::DESCRIPTOR_SCHEMA_VERSION || !descriptor::pid_live(d.pid) {
        return Ok(());
    }
    let served = crate::ipc::probe(&d.endpoint, std::time::Duration::from_millis(250));
    if served {
        return Err(Problem::with_details(
            error::HOME_LOCKED,
            "a live omt-daemon is serving this runtime dir; offline maintenance refuses to race it",
            |d| {
                d.insert("reason".into(), "daemon-serving-home".into());
                d.insert(
                    "runtimeDir".into(),
                    runtime_dir.display().to_string().into(),
                );
                d.insert(
                    "guidance".into(),
                    "stop the daemon (`omt daemon-stop`) or use the online admin path".into(),
                );
            },
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home_with_marker(body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("home.lock"), body).expect("marker");
        dir
    }

    #[test]
    fn ts_bridge_markers_refuse_takeover_guidance_even_stale() {
        let dir = temp_home_with_marker(
            r#"{"schemaVersion":1,"ownerKind":"ts-bridge","pid":1,"acquiredAt":"x","heartbeatAt":"y","token":"t"}"#,
        );
        let err = recover_own_dead_daemon_marker(dir.path()).unwrap_err();
        assert_eq!(err.code, "HOME_LOCKED");
        assert_eq!(
            err.details.as_ref().unwrap()["reason"],
            "ts-bridge-requires-takeover"
        );
    }

    #[test]
    fn live_daemon_pid_refuses_second_writer() {
        let dir = temp_home_with_marker(
            r#"{"schemaVersion":1,"ownerKind":"daemon","pid":1,"acquiredAt":"x","heartbeatAt":"y","token":"t"}"#,
        );
        // A LIVE lease = the recorded pid is alive AND the kernel flock is
        // held. Simulate by actually acquiring in the same home: the handle
        // flocks the inode, so recovery must refuse.
        let clock = std::sync::Arc::new(omt_storage::clock::SystemClock);
        let _handle = omt_storage::home_lock::acquire(
            dir.path(),
            &omt_storage::home_lock::LockConfig {
                owner_kind: omt_storage::home_lock::OwnerKind::Daemon,
                hostname: "ownership-test".to_string(),
                ..Default::default()
            },
            clock,
        )
        .expect("live holder acquires");
        let err = recover_own_dead_daemon_marker(dir.path()).unwrap_err();
        assert_eq!(err.code, "DAEMON_OWNS_HOME");
    }

    #[test]
    fn live_pid_without_flock_is_a_tombstone() {
        // TICKET-0124 fence semantics: an ALIVE pid behind an UN-FLOCKED
        // inode is a takeover tombstone — the flock is the liveness
        // authority, so boot recovery clears it instead of refusing.
        let mine = std::process::id() as i64;
        let dir = temp_home_with_marker(&format!(
            r#"{{"schemaVersion":1,"ownerKind":"daemon","pid":{mine},"acquiredAt":"x","heartbeatAt":"y","token":"t"}}"#
        ));
        recover_own_dead_daemon_marker(dir.path()).expect("tombstone recovered");
        assert!(!dir.path().join("home.lock").exists(), "marker removed");
    }

    #[test]
    fn dead_daemon_marker_is_auto_recovered() {
        // pid 2^28 is essentially never a live process; assert via pid_live.
        let dead = loop {
            let candidate: i64 = 268_435_456;
            if !descriptor::pid_live(candidate) {
                break candidate;
            }
        };
        let dir = temp_home_with_marker(&format!(
            r#"{{"schemaVersion":1,"ownerKind":"daemon","pid":{dead},"acquiredAt":"x","heartbeatAt":"y","token":"t"}}"#
        ));
        recover_own_dead_daemon_marker(dir.path()).expect("auto-recovery");
        assert!(!dir.path().join("home.lock").exists(), "marker removed");
    }
}
