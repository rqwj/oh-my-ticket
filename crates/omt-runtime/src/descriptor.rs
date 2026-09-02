//! Atomic generation descriptor (U5a locked contract): the discovery record
//! clients read to find a live daemon. Published tmp+rename; staleness is
//! decided by PID liveness plus a connect probe (see ipc::probe).

use crate::paths;
use omt_storage::clock::{iso_from_ms, MillisClock};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DESCRIPTOR_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Descriptor {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    pub endpoint: String,
    pub generation: i64,
    pub pid: i64,
    #[serde(rename = "bootToken")]
    pub boot_token: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
}

pub fn path(runtime_dir: &Path) -> std::path::PathBuf {
    runtime_dir.join(paths::DESCRIPTOR_FILE)
}

/// Read + parse the current descriptor, if any.
pub fn read(runtime_dir: &Path) -> Option<Descriptor> {
    let raw = std::fs::read_to_string(path(runtime_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// True when the descriptor's pid refers to a live process. A dead pid
/// makes the whole descriptor stale regardless of its other fields.
pub fn pid_live(pid: i64) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0): ESRCH → dead; 0/EPERM → alive.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        let _ = pid;
        false
    }
}

/// Publish the next generation atomically (tmp + rename). The new
/// generation is `max(existing, 0) + 1` — replacements always increment,
/// including after stale takeovers.
pub fn publish(
    runtime_dir: &Path,
    endpoint: &str,
    boot_token: &str,
    clock: &dyn MillisClock,
) -> std::io::Result<Descriptor> {
    std::fs::create_dir_all(runtime_dir)?;
    let generation = read(runtime_dir).map(|d| d.generation).unwrap_or(0) + 1;
    let descriptor = Descriptor {
        schema_version: DESCRIPTOR_SCHEMA_VERSION,
        endpoint: endpoint.to_string(),
        generation,
        pid: std::process::id() as i64,
        boot_token: boot_token.to_string(),
        started_at: iso_from_ms(clock.now_ms()),
    };
    write_atomic(runtime_dir, &descriptor)?;
    Ok(descriptor)
}

/// Remove the descriptor ONLY when it still carries our boot token — a
/// successor's descriptor is never touched.
pub fn remove_if_ours(runtime_dir: &Path, boot_token: &str) {
    match read(runtime_dir) {
        Some(current) if current.boot_token == boot_token => {
            let _ = std::fs::remove_file(path(runtime_dir));
        }
        _ => {}
    }
}

fn write_atomic(runtime_dir: &Path, descriptor: &Descriptor) -> std::io::Result<()> {
    let target = path(runtime_dir);
    let tmp = runtime_dir.join(format!(
        "{}.tmp.{}",
        paths::DESCRIPTOR_FILE,
        std::process::id()
    ));
    let body = serde_json::to_string_pretty(descriptor).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, body.as_bytes())?;
    // fsync the temp file so the rename lands durable content.
    if let Ok(file) = std::fs::File::open(&tmp) {
        let _ = file.sync_all();
    }
    std::fs::rename(&tmp, &target)?;
    // Best-effort directory fsync (POSIX).
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(runtime_dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}
