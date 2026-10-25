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
//!
//! U2-1（CF5/CF6，§55/§57/§59）：会话锁结构性私有化。内部 mutex 不再是
//! 公开字段（旧 `pub struct SharedSession(pub Arc<Mutex<…>>)` 允许任何
//! 代码 `.0.lock()`）；对外入口只剩两类：
//! - **async**：[`SharedSession::with_session_async`] /
//!   [`SharedSession::in_blocking_scope`]，阻塞部分（锁 + daemon socket
//!   读写最长 30s、子进程启动）经 `tauri::async_runtime::spawn_blocking`
//!   跑在阻塞线程池（CF5），不占 tokio worker；
//! - **阻塞令牌** [`BlockingScope`]：只在上述入口的闭包参数里出现，
//!   模块外不可构造——同步代码（含同步 tauri 命令，以及 U2 起新文件里
//!   的命令）在类型上就拿不到会话锁（CF6），取代 rpc_bridge.rs 的源码
//!   扫描守卫（`session_lock_shapes_live_only_in_async_commands` 等）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use omt_client::Enrollment;
use omt_exec::executor::RpcFn;

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

/// Process-wide session holder (tauri state). CF6：内部 mutex 是私有字段
/// ——模块外（含本 crate 其它文件）都不能 `.0.lock()`，会话锁只能经
/// [`SharedSession`] 的 async 入口或 [`BlockingScope`] 令牌取得。
#[derive(Clone)]
pub struct SharedSession(Arc<Mutex<Option<DaemonSession>>>);

impl Default for SharedSession {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedSession {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    /// 锁守卫（模块内共享内芯；中毒时报 `session lock poisoned`，与旧
    /// rpc_bridge::with_session 的语义一致）。
    fn guard(&self) -> Result<MutexGuard<'_, Option<DaemonSession>>, String> {
        self.0
            .lock()
            .map_err(|_| "session lock poisoned".to_string())
    }

    /// CF5：async 会话入口——establish-or-reuse + `f`，整段（锁 + 可能的
    /// 惰性重建：握手读超时 30s / sidecar 拉起 10s + daemon socket 读写）
    /// 经 `spawn_blocking` 跑在阻塞线程池，不占 tokio worker。异步 tauri
    /// 命令（omt_call / daemon_homes / daemon_ensure）走这里。
    pub async fn with_session_async<R>(
        &self,
        f: impl FnOnce(&mut DaemonSession) -> Result<R, String> + Send + 'static,
    ) -> Result<R, String>
    where
        R: Send + 'static,
    {
        self.in_blocking_scope(move |scope| scope.with_session(f))
            .await
    }

    /// CF5/CF6：阻塞作用域入口——`f` 在阻塞线程池上执行，经参数拿到
    /// [`BlockingScope`] 令牌（会话访问 / 长持有 RpcFn）。需要在同一份
    /// 阻塞预算里做更多阻塞工作的命令（如 exec_start 的整个
    /// `launch::start`：多次 RPC + 进程启动）走这里。令牌在模块外不可
    /// 构造，同步代码无法进入本入口的闭包——结构性保证由此成立。
    pub async fn in_blocking_scope<R>(
        &self,
        f: impl FnOnce(&BlockingScope) -> Result<R, String> + Send + 'static,
    ) -> Result<R, String>
    where
        R: Send + 'static,
    {
        let shared = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let scope = BlockingScope { shared };
            f(&scope)
        })
        .await
        .map_err(|err| format!("session blocking task failed: {err}"))?
    }

    /// daemon_reconnect 用：丢弃缓存 enrollment（强制下次调用重新握手，
    /// requiresRehandshake 语义）。同样经阻塞作用域取锁——本类型没有任何
    /// 同步可调的锁入口。
    pub async fn reset(&self) -> Result<(), String> {
        self.in_blocking_scope(|scope| scope.drop_session()).await
    }
}

/// CF6 阻塞令牌：会话锁的同步访问只发生在持有本令牌的上下文里。令牌
/// 只由 [`SharedSession::in_blocking_scope`] / [`with_session_async`]
/// 在 `spawn_blocking` 闭包内构造（字段私有、不可 Clone），模块外——
/// 包括同步 tauri 命令——构造不出它，也就拿不到会话锁。
pub struct BlockingScope {
    shared: SharedSession,
}

impl BlockingScope {
    /// 旧 `rpc_bridge::with_session` 的内芯：锁 + 惰性建立（含首个会话
    /// 建立的一次性后台对账，U10）+ `f`。只在阻塞上下文（`spawn_blocking`
    /// 闭包 / 后台线程）里出现。
    pub fn with_session<R>(
        &self,
        f: impl FnOnce(&mut DaemonSession) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut guard = self.shared.guard()?;
        if guard.is_none() {
            let session = DaemonSession::establish()?;
            *guard = Some(session);
            spawn_reconcile_once(&self.shared);
        }
        f(guard.as_mut().expect("session just established"))
    }

    /// 旧 `executor::rpc_of` 的内芯：可长期持有的 RpcFn（exec/launch 链
    /// 在跨进程锁内逐条调 daemon，等待线程据此释放租约）。会话未建立时
    /// 报错（**不**惰性建立）——与旧实现语义一致；锁的中毒处理沿用旧
    /// rpc_of 的宽容策略（into_inner，锁还在就继续用）。
    pub fn rpc(&self) -> RpcFn {
        let shared = self.shared.clone();
        Arc::new(move |method: &str, params: serde_json::Value| {
            let mut guard = shared
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let session = guard.as_mut().ok_or("daemon session not established")?;
            session.call(method, params)
        })
    }

    /// 丢弃缓存会话（daemon_reconnect；旧实现的 `*guard = None`）。
    pub fn drop_session(&self) -> Result<(), String> {
        let mut guard = self.shared.guard()?;
        *guard = None;
        Ok(())
    }
}

/// U10：首个会话建立（所有 RPC 的咽喉点）→ 一次性后台对账（幂等 swap
/// 守卫）。从 rpc_bridge::with_session 原样迁入：对账线程是专用后台
/// std::thread（阻塞上下文），在模块内直接构造令牌取得 RpcFn——模块
/// 外没有等价的同步入口。
fn spawn_reconcile_once(shared: &SharedSession) {
    static RECONCILED_ONCE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !RECONCILED_ONCE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let scope = BlockingScope {
            shared: shared.clone(),
        };
        std::thread::spawn(move || {
            omt_exec::executor::reconcile_interrupted_execs(&scope.rpc(), &daemon::runtime_dir());
        });
    }
}

/// 模块外拿不到内部锁的编译期证明（CF6）。字段一旦改回 `pub`，本
/// doctest 变绿、`compile_fail` 失败，测试即红。
///
/// ```compile_fail
/// use oh_my_ticket_desktop_lib::state::SharedSession;
/// let shared = SharedSession::new();
/// // E0616：字段私有——模块外拿不到会话锁
/// let _guard = shared.0.lock();
/// ```
const _: () = ();

/// 令牌在模块外不可构造的编译期证明（CF6）——同步代码没有构造
/// [`BlockingScope`] 的路径，只能经 async 入口的闭包参数拿到 `&` 引用。
///
/// ```compile_fail
/// use oh_my_ticket_desktop_lib::state::{BlockingScope, SharedSession};
/// let shared = SharedSession::new();
/// // E0603：字段私有——令牌构造不出来
/// let _scope = BlockingScope { shared };
/// ```
const _: () = ();

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用：把一个不会触发 establish 的占位会话塞进共享槽（模块内
    /// 私有访问；Rust 测试不碰 `daemon::runtime_dir()` 所指的真实
    /// runtime——派发环境下 OMT_RUNTIME_DIR 指向正在编排的 runtime）。
    fn shared_with_placeholder_session(dir: &std::path::Path) -> SharedSession {
        let shared = SharedSession::new();
        *shared.guard().expect("fresh lock") = Some(DaemonSession {
            enrollment: None,
            runtime_dir: dir.to_path_buf(),
        });
        shared
    }

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

    /// CF5：spawn_blocking 路径生效——`in_blocking_scope` 的闭包不在
    /// 调用线程上执行（阻塞工作落在阻塞线程池，tokio worker 不被占）。
    /// 回退验证缝：把 `in_blocking_scope` 里的 `spawn_blocking` 改成
    /// 直接 `f(&scope)`，本测试红。
    #[test]
    fn blocking_scope_runs_off_the_calling_thread() {
        let shared = SharedSession::new();
        let caller = std::thread::current().id();
        let worker = tauri::async_runtime::block_on(
            shared.in_blocking_scope(|_scope| Ok::<_, String>(std::thread::current().id())),
        )
        .expect("阻塞作用域应成功（闭包不碰会话）");
        assert_ne!(
            caller, worker,
            "阻塞闭包必须跑在阻塞线程池，不是调用/tokio worker 线程（CF5）"
        );
    }

    /// CF5：会话锁被长占用时，另一个 async 调用不被饿死——持锁 500ms
    /// 的会话调用进行期间，并发的 `in_blocking_scope`（正是 async 命令
    /// 阻塞部分的承载路径）在远小于持锁时长内完成。旧实现里这些阻塞
    /// 都在 tokio worker 上，worker 排队即停摆；现在各跑各的阻塞线程。
    #[test]
    fn async_entry_points_do_not_starve_while_session_lock_is_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let holder = shared_with_placeholder_session(dir.path());
        let probe = holder.clone();

        let holder_thread = std::thread::spawn(move || {
            tauri::async_runtime::block_on(holder.with_session_async(|_session| {
                std::thread::sleep(std::time::Duration::from_millis(500));
                Ok::<_, String>(())
            }))
        });
        // 让持锁方先拿到锁（其闭包在锁内 sleep）
        std::thread::sleep(std::time::Duration::from_millis(80));

        let started = std::time::Instant::now();
        let worker = tauri::async_runtime::block_on(
            probe.in_blocking_scope(|_scope| Ok::<_, String>(std::thread::current().id())),
        )
        .expect("锁被占用时并发 async 调用仍应完成（CF5 不饿死）");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "会话锁被占用 {}ms 期间并发调用应在 400ms 内完成，实际 {:?}（CF5）",
            500,
            started.elapsed()
        );
        assert_ne!(
            std::thread::current().id(),
            worker,
            "并发调用同样跑在阻塞线程池"
        );
        holder_thread
            .join()
            .expect("holder 无 panic")
            .expect("holder 成功");
    }

    /// CF6 行为面：`rpc()` 在会话未建立时报错且**不**惰性建立（旧
    /// rpc_of 语义）；`reset()` 丢弃缓存会话（daemon_reconnect 路径）。
    #[test]
    fn rpc_reports_missing_session_and_reset_drops_cached_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = SharedSession::new();
        let scope = BlockingScope {
            shared: empty.clone(),
        };
        let err =
            scope.rpc()("node/list", serde_json::Value::Null).expect_err("未建立的会话必须报错");
        assert!(err.contains("not established"), "实际：{err}");
        assert!(
            empty.guard().expect("锁可用").is_none(),
            "rpc 不得惰性建立会话"
        );

        let shared = shared_with_placeholder_session(dir.path());
        tauri::async_runtime::block_on(shared.reset()).expect("reset 成功");
        assert!(
            shared.guard().expect("锁可用").is_none(),
            "reset 后缓存会话应被丢弃"
        );
    }
}
