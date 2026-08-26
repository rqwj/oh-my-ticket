//! U9 (R12-R14, KD5): desktop daemon lifecycle — the app is a PEER member
//! of the shared multi-surface runtime, not the daemon's private guardian.
//!
//! Contract:
//! - discover-or-spawn: a live descriptor (pid alive AND endpoint
//!   reachable) is reused as-is; otherwise the sidecar daemon is spawned
//!   DETACHED (its own process group) so window close never kills it —
//!   `RunEvent::ExitRequested` semantics: exit means exit the APP.
//! - handshake kind:"desktop" with the default scoped operations (same
//!   shape every surface gets; admin capability comes only from the
//!   out-of-band grants file).
//! - admin grant self-registration is single-entry REPLACEMENT (never
//!   append): one desktop principal entry, refreshed to the current pid
//!   on every launch — pid-recycled stale entries are dropped, bounding
//!   the privilege-inheritance surface (see plan Risks).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use omt_client::{Client, Descriptor, EnrollOptions, Enrollment};

/// Daemon status projection for the settings page (`daemon status`
/// command) and lifecycle decisions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub pid: Option<i64>,
    pub generation: Option<i64>,
    pub endpoint: Option<String>,
    pub spawned_by_us: bool,
}

/// Resolve the sidecar binary: sibling of the running executable
/// (bundled .app AND dev target dir — current_exe().parent(), never
/// resource_dir, per the U8 research contract).
pub fn sidecar_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "current_exe has no parent dir".to_string())?;
    let candidate = dir.join("omt-daemon");
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(format!("no sidecar omt-daemon next to {}", exe.display()))
    }
}

/// The shared per-user runtime dir (same resolution rule as every other
/// surface — see docs/runtime/config.md).
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OMT_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".omt").join("run")
}

/// Live descriptor = pid alive AND endpoint connectable.
pub fn live_descriptor(dir: &Path) -> Option<Descriptor> {
    let descriptor = omt_client::read_descriptor(dir)?;
    if omt_client::pid_live(descriptor.pid) && omt_client::endpoint_live(&descriptor.endpoint) {
        Some(descriptor)
    } else {
        None
    }
}

/// Spawn the sidecar daemon detached (own process group) and wait for its
/// descriptor. Detachment is THE R14 guarantee: the daemon outlives the
/// app; an idle watchdog exits it when truly unused.
#[cfg(unix)]
pub fn spawn_sidecar(dir: &Path) -> Result<Descriptor, String> {
    use std::os::unix::process::CommandExt;
    let binary = sidecar_path()?;
    let mut command = Command::new(binary);
    command
        .arg("--runtime-dir")
        .arg(dir)
        .env("OMT_RUNTIME_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid in the pre-exec gap detaches the daemon from our
    // process group/session — window close can never signal it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().map_err(|err| format!("spawn sidecar: {err}"))?;
    omt_client::wait_for_descriptor(dir, Duration::from_secs(10))
        .ok_or_else(|| "sidecar produced no descriptor within 10s".to_string())
}

/// discover-or-spawn → handshake kind:"desktop". Idempotent: a live
/// descriptor is reused verbatim (second-instance semantics, R13).
pub fn discover_or_spawn_and_enroll(dir: &Path) -> Result<(Enrollment, bool), String> {
    let (descriptor, spawned) = match live_descriptor(dir) {
        Some(descriptor) => (descriptor, false),
        None => {
            let descriptor = spawn_sidecar(dir)?;
            (descriptor, true)
        }
    };
    let options = EnrollOptions {
        kind: "desktop".into(),
        name: Some("oh-my-ticket-desktop".into()),
        actor_namespace: None,
        credential_path: None,
    };
    let enrollment = Client::connect_and_enroll(&descriptor, &options)
        .map_err(|problem| format!("enroll: {} ({})", problem.message, problem.code))?;
    Ok((enrollment, spawned))
}

/// Whether an IO-shaped failure means "the daemon is gone — rediscover"
/// (legal idle exit, crash, generation rotation): connection closed /
/// refused / broken pipe all qualify; protocol problems do not.
pub fn is_daemon_gone(problem_message: &str, problem_code: &str) -> bool {
    problem_code == "IO"
        && (problem_message.contains("connection closed")
            || problem_message.contains("endpoint connect")
            || problem_message.contains("request write"))
}

/// Single-entry admin-grant registration for this desktop principal
/// (REPLACEMENT semantics — see module docs). Reads and rewrites
/// `<runtime-dir>/admin-grants.json` atomically (tmp + rename); all other
/// principal entries are preserved verbatim, only desktop-kind entries
/// are collapsed to the one current principal.
pub fn register_admin_grant(dir: &Path, principal_id: &str) -> Result<(), String> {
    let path = dir.join("admin-grants.json");
    let mut principals: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| {
            value
                .get("principalIds")
                .and_then(|ids| ids.as_array())
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect()
                })
        })
        .unwrap_or_default();
    // Drop every desktop entry (stale pids included), keep everything else.
    principals.retain(|id| !id.starts_with("desktop:"));
    principals.push(principal_id.to_string());
    let body = serde_json::json!({ "schemaVersion": 1, "principalIds": principals }).to_string();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|err| format!("write grants tmp: {err}"))?;
    std::fs::rename(&tmp, &path).map_err(|err| format!("rename grants: {err}"))?;
    Ok(())
}

/// Current status projection (settings page).
pub fn status(dir: &Path) -> DaemonStatus {
    match omt_client::read_descriptor(dir) {
        None => DaemonStatus {
            running: false,
            pid: None,
            generation: None,
            endpoint: None,
            spawned_by_us: false,
        },
        Some(descriptor) => {
            let running = omt_client::pid_live(descriptor.pid)
                && omt_client::endpoint_live(&descriptor.endpoint);
            DaemonStatus {
                running,
                pid: Some(descriptor.pid),
                generation: Some(descriptor.generation),
                endpoint: Some(descriptor.endpoint),
                spawned_by_us: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_grant_registration_replaces_desktop_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("admin-grants.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schemaVersion": 1,
                "principalIds": ["desktop:111", "desktop:222", "dsh:333", "cli:444"]
            })
            .to_string(),
        )
        .expect("seed grants");

        register_admin_grant(dir.path(), "desktop:999").expect("register");

        let raw = std::fs::read_to_string(&path).expect("read grants");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse grants");
        let ids: Vec<&str> = value["principalIds"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|id| id.as_str())
            .collect();
        let desktop_entries: Vec<&&str> = ids.iter().filter(|id| id.starts_with("desktop:")).collect();
        assert_eq!(desktop_entries.len(), 1, "exactly one desktop entry: {ids:?}");
        assert_eq!(*desktop_entries[0], "desktop:999", "entry is the current principal");
        assert!(ids.contains(&"dsh:333") && ids.contains(&"cli:444"), "other principals preserved");

        // Second launch replaces again — never accumulates.
        register_admin_grant(dir.path(), "desktop:1000").expect("re-register");
        let raw = std::fs::read_to_string(&path).expect("read grants 2");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse grants 2");
        let ids: Vec<&str> = value["principalIds"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|id| id.as_str())
            .collect();
        let desktop_entries: Vec<&&str> = ids.iter().filter(|id| id.starts_with("desktop:")).collect();
        assert_eq!(desktop_entries.len(), 1, "still one entry after second launch: {ids:?}");
        assert_eq!(*desktop_entries[0], "desktop:1000");
    }

    #[test]
    fn daemon_gone_classification_covers_shutdown_shapes_only() {
        assert!(is_daemon_gone("connection closed: eof", "IO"));
        assert!(is_daemon_gone("endpoint connect: refused", "IO"));
        assert!(is_daemon_gone("request write: broken pipe", "IO"));
        assert!(!is_daemon_gone("home not in credential scope", "FORBIDDEN"));
        assert!(!is_daemon_gone("unrelated io error", "IO"));
    }

    #[test]
    fn runtime_dir_prefers_env_then_home_default() {
        let dir = runtime_dir();
        assert!(dir.ends_with(".omt/run") || std::env::var("OMT_RUNTIME_DIR").is_ok());
    }
}
