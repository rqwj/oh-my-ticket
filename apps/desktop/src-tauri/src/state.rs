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
//!
//! F-TERM-16/18（§69）：**本模块是会话锁的信任边界。** CF6 的结构性保
//! 证在本模块内成立（字段私有、令牌不可构造、[`BlockingScope::rpc`] 借
//! 用 scope、不能带出阻塞作用域）；但 crate 外的 compile_fail doctest
//! 看不到 `pub(crate)` 内的形态（字段改 `pub(crate)`、加 pub 构造器/同步
//! 访问器等变异 doctest 抓不住），边界最终靠本模块的评审纪律守住：**新
//! 增任何 `pub` / `pub(crate)` 的锁访问（字段、构造器、访问器）都必须保
//! 持「只经 async 入口或 BlockingScope 令牌」，不得引入同步可调的锁入
//! 口，也不得让拿锁能力以 `'static` 形态外流。**

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

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
    /// [`BlockingScope`] 令牌（会话访问 / 借用式 rpc）。需要在同一份
    /// 阻塞预算里做更多阻塞工作的命令（如 exec_start 的整个
    /// `launch::start`：多次 RPC + 进程启动）走这里。令牌在模块外不可
    /// 构造，同步代码无法进入本入口的闭包——结构性保证由此成立；
    /// F-TERM-16：`R: 'static` 保证借用 scope 的 rpc 闭包带不出作用域。
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

    /// 旧 `executor::rpc_of` 的内芯，F-TERM-16 改为**借用式**：返回的闭包
    /// 借用 `self`（scope 令牌），不能被带出阻塞作用域（
    /// `in_blocking_scope` 要求 `R: Send + 'static`，类型上拒绝——见下方
    /// compile_fail doctest）；调用方要么在作用域闭包内直接使用（
    /// `omt_exec::launch::start` 本就接受借用的 `DaemonRpc<'_>`），要么把
    /// scope 整体移进专用后台线程（`spawn_reconcile_once`）再借用。会话
    /// 未建立时报错（**不**惰性建立）——与旧实现语义一致；锁的中毒处理
    /// 沿用旧 rpc_of 的宽容策略（into_inner，锁还在就继续用）。
    pub fn rpc(
        &self,
    ) -> impl Fn(&str, serde_json::Value) -> Result<serde_json::Value, String> + '_ {
        move |method: &str, params: serde_json::Value| {
            let mut guard = self
                .shared
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let session = guard.as_mut().ok_or("daemon session not established")?;
            session.call(method, params)
        }
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
/// std::thread（阻塞上下文），在模块内直接构造令牌——模块外没有等价
/// 的同步入口。F-TERM-16：scope 整体移进线程、线程内借用式 rpc
///（reconcile_interrupted_execs_with 接受 `&dyn Fn`）——拿锁能力不再
/// 以 'static RpcFn 形态外流，也不依赖（已删除的）跨线程 RpcFn。
fn spawn_reconcile_once(shared: &SharedSession) {
    static RECONCILED_ONCE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !RECONCILED_ONCE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let scope = BlockingScope {
            shared: shared.clone(),
        };
        std::thread::spawn(move || {
            let rpc = scope.rpc();
            let handled =
                omt_exec::executor::reconcile_interrupted_execs_with(&rpc, &daemon::runtime_dir());
            if handled > 0 {
                eprintln!("U10 对账：释放了 {handled} 个中断 exec 的 daemon 租约");
            }
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
/// // E0451：字段私有——令牌构造不出来
/// let _scope = BlockingScope { shared };
/// ```
const _: () = ();

/// F-TERM-16 的编译期证明：借用式 rpc 不能带出阻塞作用域。
/// `rpc()` 返回借用 scope 的闭包，`in_blocking_scope` 要求 `R: Send +
/// 'static`——把 rpc 闭包作为结果返回时隐藏类型捕获了 `&BlockingScope`
/// 的生命周期，不满足 `'static`，必须编译失败（拿锁能力出不了作用域）。
/// 回退验证缝：`rpc()` 改回返回 `'static` 的 `RpcFn`（Arc 闭包）时本
/// doctest 变绿、compile_fail 失败，测试即红。
///
/// ```compile_fail
/// use oh_my_ticket_desktop_lib::state::SharedSession;
/// let shared = SharedSession::new();
/// // 隐藏类型捕获闭包参数的生命周期，不满足 R: 'static
/// let escaped = shared.in_blocking_scope(|scope| Ok(scope.rpc()));
/// ```
const _: () = ();

// F-TERM-17：测试线程用 block_on 驱动 async 入口是合理用途——被驱动的
// 是 `spawn_blocking` 承载的阻塞工作（不占 tokio worker，探针任务照常
// 调度），不存在饿死风险；生产代码禁止 block_on（见根 clippy.toml）。
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
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

    /// CF5（F-TERM-19）：真正测饿死。持锁方长占会话锁；在锁上排队**多于
    /// tokio worker 数**（`available_parallelism() + 1`，经
    /// `tauri::async_runtime::spawn` 发起）的 `with_session_async` 调用，
    /// 再断言一个**无关**的 `tauri::async_runtime::spawn` 任务能很快完成。
    /// 回退验证缝：`in_blocking_scope` 里的 `spawn_blocking` 若被换成
    /// `async_runtime::spawn`（阻塞工作回到 tokio worker），等锁任务占满
    /// 全部 worker，无关任务饿死 → 本测试红。
    #[test]
    fn async_entry_points_do_not_starve_while_session_lock_is_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let holder = shared_with_placeholder_session(dir.path());
        let queued = holder.clone();

        // 持锁方：锁内睡 2s（阻塞线程池上，形状同慢 daemon 调用）。
        let holder_thread = std::thread::spawn(move || {
            tauri::async_runtime::block_on(holder.with_session_async(|_session| {
                std::thread::sleep(std::time::Duration::from_millis(2000));
                Ok::<_, String>(())
            }))
        });
        // 让持锁方先拿到锁（其闭包在锁内 sleep）。
        std::thread::sleep(std::time::Duration::from_millis(120));

        // 在被占住的锁上排队多于 worker 数的 with_session_async——
        // 它们的阻塞部分各自排队等 std 锁。
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let handles: Vec<_> = (0..workers + 1)
            .map(|_| {
                let queued = queued.clone();
                tauri::async_runtime::spawn(async move {
                    queued
                        .with_session_async(|_session| Ok::<_, String>(()))
                        .await
                })
            })
            .collect();
        // 等它们全部真正排在锁上。
        std::thread::sleep(std::time::Duration::from_millis(150));

        // 无关探针：不碰会话锁的 spawn 任务必须很快完成（tokio worker
        // 未被等锁的阻塞工作占用）。锁被长占（2s）期间探针若 400ms 内
        // 完不成，就是饿死。
        let (probe_tx, probe_rx) = std::sync::mpsc::channel::<()>();
        tauri::async_runtime::spawn(async move {
            let _ = probe_tx.send(());
        });
        probe_rx
            .recv_timeout(std::time::Duration::from_millis(400))
            .expect("会话锁被占 2s 期间无关 spawn 任务应在 400ms 内完成（CF5 饿死回归）");

        // 收尾：持锁方结束、排队任务全部成功。
        holder_thread
            .join()
            .expect("holder 无 panic")
            .expect("holder 成功");
        for handle in handles {
            tauri::async_runtime::block_on(handle)
                .expect("排队任务 join 成功")
                .expect("排队任务成功");
        }
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
        let rpc = scope.rpc();
        let err = rpc("node/list", serde_json::Value::Null).expect_err("未建立的会话必须报错");
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
