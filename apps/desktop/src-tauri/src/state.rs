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
//!
//! §51 timeout-no-replay: a 30s READ TIMEOUT is NOT daemon-gone — the
//! daemon is slow but alive, so replaying would double-submit writes that
//! carry no commandId (node/create, run/create, node/update …). The
//! timeout path DROPS the connection (the enrollment slot is emptied and
//! lazily re-established on the next call) and surfaces
//! [`crate::daemon::DAEMON_TIMEOUT_MESSAGE`] without retrying.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use omt_client::Enrollment;

use crate::daemon;

/// Live daemon session: enrollment + the runtime dir it serves.
/// §51：enrollment 变为 Option——超时路径丢弃连接（置 None），下次调用经
/// [`DaemonSession::enrollment_mut`] 惰性重建。
pub struct DaemonSession {
    enrollment: Option<Enrollment>,
    runtime_dir: PathBuf,
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
        Ok(Self {
            enrollment: Some(enrollment),
            runtime_dir,
        })
    }

    /// 惰性（重）建立（§51 timeout-no-replay 丢弃连接后的下一次调用）。
    fn enrollment_mut(&mut self) -> Result<&mut Enrollment, String> {
        if self.enrollment.is_none() {
            let (enrollment, _spawned) = daemon::discover_or_spawn_and_enroll(&self.runtime_dir)?;
            self.enrollment = Some(enrollment);
        }
        Ok(self
            .enrollment
            .as_mut()
            .expect("enrollment just established"))
    }

    /// 会话服务的 runtime 目录（events_subscribe 的泵线程用）。
    pub fn runtime_dir(&self) -> &std::path::Path {
        &self.runtime_dir
    }

    /// handshake 的 homes 投影（daemon_homes 命令用；必要时先惰性重建）。
    /// §53 F-SP2-7：重建失败必须**上抛**——此前 Err 被吞成 `json!([])`，
    /// rpc_bridge 据此返回 `Ok({homes: []})`：home 切换器被清空，且前端
    /// 成功路径的 `error: null` 抹掉刚显示的「daemon 无响应（30s）」。
    pub fn handshake_homes(&mut self) -> Result<serde_json::Value, String> {
        let enrollment = self.enrollment_mut()?;
        Ok(enrollment.handshake["homes"].clone())
    }

    /// One authenticated call with single-shot daemon-gone recovery.
    /// §51 timeout-no-replay：读超时 → 丢弃连接、不重放（避免非幂等写重复
    /// 提交）；只有真正的 daemon-gone（断连 / 合法退出）才重连 + 重放一次。
    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let attempt = match self.enrollment_mut() {
            Ok(enrollment) => enrollment.client.call(method, params.clone()),
            Err(establish) => return Err(format!("reestablish session: {establish}")),
        };
        match attempt {
            Ok(value) => Ok(value),
            Err(problem) if daemon::is_read_timeout(&problem.message, problem.code) => {
                // 慢但存活的 daemon：丢弃连接（下次调用惰性重建），绝不重放
                self.enrollment = None;
                Err(daemon::DAEMON_TIMEOUT_MESSAGE.to_string())
            }
            Err(problem) if daemon::is_daemon_gone(&problem.message, problem.code) => {
                // The daemon exited legally (idle watchdog) or the endpoint
                // died: rediscover-or-respawn, re-enroll, retry ONCE.
                let (enrollment, _spawned) =
                    daemon::discover_or_spawn_and_enroll(&self.runtime_dir)
                        .map_err(|err| format!("rediscover after daemon loss: {err}"))?;
                self.enrollment = Some(enrollment);
                let retry = self
                    .enrollment_mut()
                    .expect("enrollment just established")
                    .client
                    .call(method, params);
                match retry {
                    // 重放的那次也超时：同样丢弃连接、报超时（下次不再重放）
                    Err(problem) if daemon::is_read_timeout(&problem.message, problem.code) => {
                        self.enrollment = None;
                        Err(daemon::DAEMON_TIMEOUT_MESSAGE.to_string())
                    }
                    Err(problem) => Err(format!("{} ({})", problem.message, problem.code)),
                    Ok(value) => Ok(value),
                }
            }
            Err(problem) => Err(format!("{} ({})", problem.message, problem.code)),
        }
    }
}

/// Process-wide session holder (tauri state).
#[derive(Clone)]
pub struct SharedSession(pub Arc<Mutex<Option<DaemonSession>>>);

impl SharedSession {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §53 F-SP2-7：`handshake_homes` 的惰性重建失败必须上抛——此前 Err
    /// 被吞成 `json!([])`，rpc_bridge 据此返回 `Ok({homes: []})`：home
    /// 切换器被清空，且前端成功路径的 `error: null` 抹掉刚显示的
    /// 「daemon 无响应（30s）」。缝级测试：连接已被丢弃（enrollment =
    /// None）、重建注定失败（runtime 目录不存在 → 探活失败 → 测试二进制
    /// 旁无 omt-daemon sidecar → spawn 失败）——错误必须穿透出来。
    #[test]
    fn handshake_homes_propagates_reestablishment_failure() {
        let mut session = DaemonSession {
            enrollment: None,
            runtime_dir: PathBuf::from("/nonexistent-omt-53/runtime"),
        };
        let err = session
            .handshake_homes()
            .expect_err("重建失败必须上抛（§53 F-SP2-7），不得回落空 homes");
        assert!(
            !err.is_empty(),
            "错误信息非空（前端要靠它呈现 daemon 无响应）"
        );
    }
}
