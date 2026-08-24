//! Per-user runtime directory resolution (U5a locked contract).
//!
//! Choice (documented in README.md): the per-user runtime directory is
//! `$OMT_RUNTIME_DIR` when set (tests + sandboxes), otherwise
//! `~/.omt/run/`. The existing TypeScript host has NO runtime-directory
//! convention (`src/host` resolves only ticket homes: `.omt/` workspaces
//! and the global home), so we pin the daemon-owned location beside the
//! global home instead of a platform temp dir, keeping every daemon file
//! under one user-visible root.
//!
//! Layout inside `<runtime-dir>`:
//! - `descriptor.json`  — atomic generation descriptor
//! - `bootstrap.lock`   — single-daemon election lock
//! - `admin-grants.json`— out-of-band administrator principal list
//! - `omt/daemon.sock`  — unix endpoint (windows: named pipe, see ipc.rs)

use std::path::PathBuf;

pub const DESCRIPTOR_FILE: &str = "descriptor.json";
pub const BOOTSTRAP_LOCK_FILE: &str = "bootstrap.lock";
pub const ADMIN_GRANTS_FILE: &str = "admin-grants.json";
/// Socket lives one level below the runtime dir (packet-pinned layout).
pub const SOCKET_REL: &str = "omt/daemon.sock";

/// Resolve the per-user runtime dir. Precedence:
/// `--runtime-dir` argument > `OMT_RUNTIME_DIR` env > `~/.omt/run`.
pub fn resolve(explicit: Option<&str>) -> PathBuf {
    if let Some(dir) = explicit {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("OMT_RUNTIME_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".omt").join("run")
}

/// Absolute path of the IPC endpoint inside the runtime dir.
#[cfg(unix)]
pub fn endpoint_path(runtime_dir: &std::path::Path) -> PathBuf {
    runtime_dir.join(SOCKET_REL)
}

#[cfg(windows)]
pub fn pipe_name(runtime_dir: &std::path::Path) -> String {
    // Named pipes live in one flat namespace; disambiguate per-user runtime
    // dirs with an FNV-1a hash of the resolved path.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in runtime_dir.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!(r"\\.\pipe\omt\{hash:x}\omt-daemon.pipe")
}

#[cfg(windows)]
pub fn endpoint_path(runtime_dir: &std::path::Path) -> PathBuf {
    // Windows has no filesystem socket path; the pipe name travels in the
    // descriptor instead.
    PathBuf::from(pipe_name(runtime_dir))
}
