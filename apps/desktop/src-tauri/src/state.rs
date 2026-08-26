//! U9 (R12-R14): the desktop's daemon session state. One connection owned
//! Rust-side (KTD6); the frontend never holds the socket — it forwards
//! JSON-RPC through tauri commands and receives daemon events over the
//! Tauri event channel (rpc_bridge.rs).
//!
//! Recovery rule: an IO-shaped failure classified as daemon-gone (legal
//! idle exit, generation rotation) re-runs discover-or-spawn and retries
//! the call ONCE; protocol problems propagate untouched. Reconnection
//! mints a FRESH credential — the events subscription is re-established
//! by the bridge afterwards (it owns the reader thread).

use std::path::PathBuf;
use std::sync::Mutex;

use omt_client::Enrollment;

use crate::daemon;

/// Live daemon session: enrollment + the runtime dir it serves.
pub struct DaemonSession {
    pub enrollment: Enrollment,
    pub runtime_dir: PathBuf,
}

impl DaemonSession {
    /// Establish (or re-establish) the session: discover-or-spawn → enroll
    /// kind:"desktop" → single-entry admin-grant refresh. The principal id
    /// comes from the handshake credential ("desktop:<pid>").
    pub fn establish() -> Result<Self, String> {
        let runtime_dir = daemon::runtime_dir();
        let (enrollment, _spawned) = daemon::discover_or_spawn_and_enroll(&runtime_dir)?;
        let principal = enrollment.handshake["credential"]["principalId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !principal.is_empty() {
            // Best-effort: a grants failure must not block app start (the
            // desktop is fully functional as a non-admin surface).
            let _ = daemon::register_admin_grant(&runtime_dir, &principal);
        }
        Ok(Self { enrollment, runtime_dir })
    }

    /// One authenticated call with single-shot daemon-gone recovery.
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        match self.enrollment.client.call(method, params.clone()) {
            Ok(value) => Ok(value),
            Err(problem) if daemon::is_daemon_gone(&problem.message, problem.code) => {
                // The daemon exited legally (idle watchdog) or the endpoint
                // died: rediscover-or-respawn, re-enroll, retry ONCE.
                let (enrollment, _spawned) = daemon::discover_or_spawn_and_enroll(&self.runtime_dir)
                    .map_err(|err| format!("rediscover after daemon loss: {err}"))?;
                self.enrollment = enrollment;
                self.enrollment
                    .client
                    .call(method, params)
                    .map_err(|retry| format!("{} ({})", retry.message, retry.code))
            }
            Err(problem) => Err(format!("{} ({})", problem.message, problem.code)),
        }
    }
}

/// Process-wide session holder (tauri state).
pub struct SharedSession(pub Mutex<Option<DaemonSession>>);

impl SharedSession {
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }
}
