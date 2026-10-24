//! U9 (R12, KTD6): tauri command bridge — the frontend's ONLY path to the
//! daemon. JSON-RPC calls forward through the Rust-held session; daemon
//! event envelopes flow back over the Tauri event channel as
//! "omt://event".
//!
//! Commands:
//! - `omt_call(method, params)` — authenticated JSON-RPC forward with the
//!   session's single-shot daemon-gone recovery.
//! - `daemon_status()` — settings-page projection (running/pid/generation/
//!   endpoint).
//! - `daemon_ensure()` — establish the session on demand (first call does
//!   it lazily; the settings page can force a reconnect after user
//!   actions).

use tauri::{AppHandle, Emitter, State};

use omt_harness::harness::{augmented_path, probe_version, which_binary};

/// 检测本机已安装的 agent harness（dsh / opencode）：PATH 查找 +
/// 版本探针。渲染进程无 fs/shell 能力，探测只能在 Rust 侧做。
/// （U1 起探测原语在 harness.rs；dsh 走通用探测，opencode/omp 走
/// `harness_list` 的 registry 投影。）
#[tauri::command]
pub fn harness_detect(harness: String) -> Result<serde_json::Value, String> {
    let binary = match harness.as_str() {
        "dsh" => "dsh",
        "opencode" => "opencode",
        other => return Err(format!("unsupported harness type: {other}")),
    };
    match which_binary(binary) {
        Some(path) => {
            let version = probe_version(&path, &["--version"], 2);
            Ok(serde_json::json!({
                "installed": true,
                "path": path.display().to_string(),
                "version": version,
            }))
        }
        None => Ok(serde_json::json!({ "installed": false })),
    }
}

/// 校验 DeepSeek Harness 源码 checkout（开发模式）：pnpm-workspace.yaml
/// + apps/ + packages/ 三个标志性入口同时存在。
fn checkout_dir_valid(path: &str) -> bool {
    let dir = std::path::PathBuf::from(path);
    dir.is_dir()
        && dir.join("pnpm-workspace.yaml").is_file()
        && dir.join("apps").is_dir()
        && dir.join("packages").is_dir()
}

/// 校验 DeepSeek Harness 源码 checkout（开发模式）：pnpm-workspace.yaml
/// + apps/ + packages/ 三个标志性入口同时存在。
#[tauri::command]
pub fn harness_validate_checkout(path: String) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({ "valid": checkout_dir_valid(&path) }))
}

/// 带超时的子进程执行：stdout/stderr 走读线程（防管道写满卡死），主线程
/// 轮询 try_wait 到截止时间，超时 kill。返回 success + 合并输出。
fn run_with_timeout(
    program: &std::path::Path,
    args: &[String],
    cwd: Option<&std::path::Path>,
    timeout_secs: u64,
) -> Result<(bool, String), String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", augmented_path());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 {}: {e}", program.display()))?;
    let mut readers = Vec::new();
    // stdout/stderr 类型不同，统一擦除成 Read trait object 再分发线程。
    let mut pipes: Vec<Box<dyn std::io::Read + Send>> = Vec::new();
    if let Some(out) = child.stdout.take() {
        pipes.push(Box::new(out));
    }
    if let Some(err) = child.stderr.take() {
        pipes.push(Box::new(err));
    }
    for mut pipe in pipes {
        readers.push(std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            buf
        }));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(120)),
            Err(e) => return Err(e.to_string()),
        }
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("安装命令超过 {timeout_secs}s 未完成，已终止"));
    }
    let mut combined = String::new();
    for reader in readers {
        if let Ok(part) = reader.join() {
            combined.push_str(part.trim());
            combined.push('\n');
        }
    }
    Ok((status.expect("checked non-none above").success(), combined))
}

/// 「安装 DSH 插件」设置页流程的执行半：把 oh-my-ticket 插件装进目标 dsh
/// profile。版本由前端传当前 app 版本（版本 lockstep：桌面版 = 插件版）。
///
/// mode:
/// - `global`：PATH 上的 dsh → `dsh plugin --profile <profile> add <pkg>@<ver>`
/// - `dev`：dsh 源码 checkout → 在 checkout 根目录跑
///   `pnpm --filter @deepseek-ai/dsh exec dsh plugin --profile <profile> add <pkg>@<ver>`
///   （checkout 根的 node_modules/.bin 不链接 dsh，必须经 workspace filter 解析）
#[tauri::command(async)]
pub fn dsh_plugin_install(
    mode: String,
    checkout_dir: Option<String>,
    profile: String,
    package: String,
    version: String,
) -> Result<serde_json::Value, String> {
    let spec = format!("{package}@{version}");
    let (program, args, cwd) = match mode.as_str() {
        "global" => {
            let dsh = which_binary("dsh").ok_or("未在 PATH 上找到 dsh（含常见前缀）")?;
            (
                dsh,
                vec![
                    "plugin".to_string(),
                    "--profile".to_string(),
                    profile,
                    "add".to_string(),
                    spec,
                ],
                None,
            )
        }
        "dev" => {
            let dir = checkout_dir
                .as_deref()
                .ok_or("dev 模式需要 checkout 目录")?;
            if !checkout_dir_valid(dir) {
                return Err(
                    "所选目录不是 dsh 开发环境（缺 pnpm-workspace.yaml / apps / packages）"
                        .to_string(),
                );
            }
            (
                std::path::PathBuf::from("pnpm"),
                vec![
                    "--filter".to_string(),
                    "@deepseek-ai/dsh".to_string(),
                    "exec".to_string(),
                    "dsh".to_string(),
                    "plugin".to_string(),
                    "--profile".to_string(),
                    profile,
                    "add".to_string(),
                    spec,
                ],
                Some(std::path::PathBuf::from(dir)),
            )
        }
        other => return Err(format!("unsupported install mode: {other}")),
    };
    let (success, output) = run_with_timeout(&program, &args, cwd.as_deref(), 180)
        .map_err(|e| format!("安装命令执行失败: {e}"))?;
    Ok(serde_json::json!({ "ok": success, "output": output.trim() }))
}

use crate::daemon;
use crate::state::{DaemonSession, SharedSession};

/// Establish-or-reuse the session under the shared lock.
fn with_session<R>(
    shared: &SharedSession,
    f: impl FnOnce(&mut DaemonSession) -> Result<R, String>,
) -> Result<R, String> {
    let mut guard = shared
        .0
        .lock()
        .map_err(|_| "session lock poisoned".to_string())?;
    if guard.is_none() {
        let session = DaemonSession::establish()?;
        *guard = Some(session);
        {
            // U10：首个会话建立（所有 RPC 的咽喉点——daemon_ensure 只是其一
            /// 且未必被前端调用）→ 一次性后台对账（幂等 swap 守卫）
            static RECONCILED_ONCE: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !RECONCILED_ONCE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let reconciler = crate::state::SharedSession(shared.0.clone());
                std::thread::spawn(move || {
                    omt_exec::executor::reconcile_interrupted_execs(
                        &crate::executor::rpc_of(&reconciler),
                        &daemon::runtime_dir(),
                    );
                });
            }
        }
    }
    f(guard.as_mut().expect("session just established"))
}

/// U15：async 化——同步命令在主线程跑，慢 RPC（锁竞争/大响应）会卡 UI；
/// async 命令进 tauri 线程池。with_session 的锁不跨 await（同步作用域）。
#[tauri::command(async)]
pub async fn omt_call(
    shared: State<'_, SharedSession>,
    method: String,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    with_session(&shared, |session| session.call(&method, params))
}

#[tauri::command]
pub fn daemon_status() -> Result<daemon::DaemonStatus, String> {
    Ok(daemon::status(&daemon::runtime_dir()))
}

/// U10 home listing: the handshake projection of currently-open homes
/// (pull-based freshness — the store re-invokes on window focus; v1 has
/// no cross-surface push signal and homes-changed deliberately never
/// enters the per-home outbox).
/// §53 F-SP2-7：改 async 命令——惰性重建（连接 + 握手最长 30s，或 sidecar
/// 拉起等待 10s）跑在主线程会卡 UI；且重建失败经 `?` 上抛（此前吞成
/// `homes: []`，home 切换器清空、前端 `error: null` 抹掉超时提示）。
#[tauri::command(async)]
pub async fn daemon_homes(shared: State<'_, SharedSession>) -> Result<serde_json::Value, String> {
    with_session(&shared, |session| {
        Ok(serde_json::json!({
            // §51：经 session 的惰性重建缝（超时丢弃连接后也能恢复）；
            // §53：重建失败必须上抛（`?`），不得回落成空列表
            "homes": session.handshake_homes()?,
            "homeDir": std::env::var("HOME").unwrap_or_default(),
        }))
    })
}

/// §55 F-SP2-9：改 async——`daemon_ensure` 经 with_session 惰性建立会话
///（连接 + 握手最长 30s），同步命令跑在主线程；与 daemon_homes/omt_call
/// 共享同一把会话 Mutex，后者持锁重建时主线程会在锁上等最长 30s（转圈）。
#[tauri::command(async)]
pub async fn daemon_ensure(
    shared: State<'_, SharedSession>,
) -> Result<daemon::DaemonStatus, String> {
    with_session(&shared, |_session| Ok(()))?;
    daemon_status()
}

/// Force a FRESH session (drop the cached enrollment and re-handshake) —
/// used after home/declare so the new home enters the credential's scoped
/// grant (requiresRehandshake semantics, KTD3 client half).
/// §53 F-SP2-7：随 daemon_homes 一并 async（重握手同样可能等 30s/10s）。
#[tauri::command(async)]
pub async fn daemon_reconnect(
    shared: State<'_, SharedSession>,
) -> Result<serde_json::Value, String> {
    {
        let mut guard = shared
            .0
            .lock()
            .map_err(|_| "session lock poisoned".to_string())?;
        *guard = None;
    }
    daemon_homes(shared).await
}

// ---------------------------------------------------------------------------
// U15：daemon 事件订阅管理器——单线程 + 订阅注册表 + 代际切换。
// 旧实现每次 events_subscribe 起一个永不退出的线程与独立 daemon 连接，
// 切 home N 次泄漏 N 份；现在全 app 一条泵线程，命令只更新注册表
// （代际递增），泵线程察觉代际变化即切换 home/cursor，连接复用。
// ---------------------------------------------------------------------------

/// 订阅目标（最新者胜）。generation 单调递增——泵线程靠它察觉切换。
#[derive(Debug, Clone)]
pub(crate) struct SubscriptionTarget {
    pub home_id: String,
    pub since: u64,
    pub generation: u64,
}

/// 注册表：命令侧写、泵线程读。`None` = 暂无订阅（泵线程空转等待）。
pub(crate) struct SubscriptionManager {
    target: std::sync::Mutex<Option<SubscriptionTarget>>,
    generation: std::sync::atomic::AtomicU64,
    /// 泵线程是否已启动（全 app 一次）
    started: std::sync::atomic::AtomicBool,
}

impl SubscriptionManager {
    const fn new() -> Self {
        Self {
            target: std::sync::Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
            started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 命令入口：登记/切换订阅目标（不 spawn——线程预算与调用次数无关）。
    fn set_target(&self, home_id: String, since: u64) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        *self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(SubscriptionTarget {
            home_id,
            since,
            generation,
        });
        generation
    }

    fn current(&self) -> Option<SubscriptionTarget> {
        self.target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

static SUBSCRIPTIONS: SubscriptionManager = SubscriptionManager::new();

/// 泵线程主体：单连接长期轮询 events/resume；代际变化 → 切 home/cursor
/// （连接复用）；daemon 失联 → 指数退避重连（目标切换立即重试）；
/// 无目标 → 轻睡眠等待。emit 经闭包注入（测试不打扰 tauri AppHandle）。
fn subscription_pump(
    runtime_dir: std::path::PathBuf,
    emit: impl Fn(serde_json::Value) + Send + 'static,
) {
    let mut client: Option<omt_client::Client> = None;
    let mut active: Option<SubscriptionTarget> = None;
    let mut cursor = 0u64;
    let mut backoff_ms = 250u64;
    loop {
        let current = SUBSCRIPTIONS.current();
        // 代际切换：换 home / 游标重置（连接保留——credential 与 home 无关）
        if current.as_ref().map(|t| t.generation) != active.as_ref().map(|t| t.generation) {
            active = current;
            cursor = active.as_ref().map(|t| t.since).unwrap_or(0);
            backoff_ms = 250; // 新目标立即全速
        }
        let Some(target) = active.clone() else {
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        };
        if client.is_none() {
            let options = omt_client::EnrollOptions {
                kind: "desktop".into(),
                name: Some("oh-my-ticket-desktop-events".into()),
                actor_namespace: None,
                operations: None,
                credential_path: None,
            };
            match daemon::live_descriptor(&runtime_dir)
                .and_then(|d| omt_client::Client::connect_and_enroll(&d, &options).ok())
            {
                Some(enrollment) => client = Some(enrollment.client),
                None => {
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                    backoff_ms = (backoff_ms * 2).min(5_000);
                    continue;
                }
            }
        }
        let page = client.as_mut().expect("client just established").call(
            "events/resume",
            serde_json::json!({ "homeId": target.home_id, "cursor": cursor, "limit": 500 }),
        );
        match page {
            Ok(page) => {
                backoff_ms = 250;
                let events = page["events"].as_array().cloned().unwrap_or_default();
                for envelope in &events {
                    emit(envelope.clone());
                }
                let next = page["cursor"].as_u64().unwrap_or(cursor).max(cursor);
                if events.len() < 500 && next == cursor {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                cursor = next;
            }
            Err(_) => {
                // daemon 失联/流关闭：弃连接退避重连（前端无需重订阅）
                client = None;
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms * 2).min(5_000);
            }
        }
    }
}

/// U10 seam（U15 重构）：登记/切换某 home 的事件订阅。幂等线程预算——
/// 全 app 仅一条泵线程（首次调用时启动），此后调用只更新注册表。
/// §55 F-SP2-9 → §57 F-SP2-10：§55 为避开会话锁改 async，§57 复审取
/// 更优解——**完全去掉 with_session**：runtime 目录直接取
/// `daemon::runtime_dir()`（与 state.rs 建会话同源），泵线程自己连接/
/// 退避重试，命令无需会话。由此（a）快速切 home A→B 时 set_target 的
/// 到达顺序得到保住（改回**同步**命令——主线程按到达顺序串行执行；
/// §56 的 async 形态下各调用是独立 tokio 任务，先在非 FIFO 的会话
/// Mutex 上排队，A 可能后拿锁 → 泵停在 A，B 的监听器按 homeId 过滤
/// 永不触发）；（b）消除「订阅失败时 with_session 先返回、不
/// set_target，泵继续轮询旧 home 且无重试」的既有问题。
#[tauri::command]
pub fn events_subscribe(app: AppHandle, home_id: String, since: u64) -> Result<(), String> {
    let runtime_dir = daemon::runtime_dir();
    SUBSCRIPTIONS.set_target(home_id, since);
    if !SUBSCRIPTIONS
        .started
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        std::thread::spawn(move || {
            subscription_pump(runtime_dir, move |envelope| {
                let _ = app.emit("omt://event", envelope);
            });
        });
    }
    Ok(())
}

#[cfg(test)]
mod subscription_tests {
    use super::*;

    /// U15：注册表语义——代际单调递增、最新者胜、读侧随时取到当前目标。
    /// （线程预算由构造保证：set_target 不 spawn，泵线程 started 一次性。）
    #[test]
    fn subscription_registry_latest_wins_and_generation_monotonic() {
        let mgr = SubscriptionManager::new();
        assert!(mgr.current().is_none());
        let g1 = mgr.set_target("h1".into(), 0);
        let g2 = mgr.set_target("h2".into(), 100);
        assert!(g2 > g1, "代际单调递增");
        let cur = mgr.current().expect("最新目标可见");
        assert_eq!(
            (cur.home_id.as_str(), cur.since, cur.generation),
            ("h2", 100, g2)
        );
    }

    /// U15：线程预算——N 次切换后 started 只翻一次、不新增线程位
    ///（set_target 为纯注册表写；这里以 started 位 + 代际验证调用无副作用扩散）。
    #[test]
    fn subscription_set_target_is_spawn_free() {
        let mgr = SubscriptionManager::new();
        for i in 0..10 {
            mgr.set_target(format!("h{i}"), i as u64);
        }
        assert!(
            !mgr.started.load(std::sync::atomic::Ordering::SeqCst),
            "set_target 不启动泵线程"
        );
        assert_eq!(mgr.current().unwrap().home_id, "h9", "十次切换后最新者胜");
    }
}

#[cfg(test)]
mod other_tests_placeholder {}

#[cfg(test)]
mod daemon_homes_tests {
    /// §57 P3：只扫生产代码（测试模块的注释/needle 会混进扫描造出假命中）。
    fn production_source() -> String {
        let full =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_bridge.rs"))
                .expect("read rpc_bridge.rs source");
        let test_module = ["#[cfg(", "test)]"].join("");
        full[..full.find(&test_module).expect("rpc_bridge.rs 应有测试模块")].to_string()
    }

    /// §57 P3：按 `#[tauri::command…]` 属性切命令 span（attr 起点 → 下一个
    /// attr 起点），返回 (span 去空白文本, 是否 async 命令)。以属性定边界
    /// 而非 `rfind("pub ")`——私有 / pub(crate) 同步 fn 不会再被算进前面
    /// 的 pub async fn（§57 点名的假通过形态）；去空白让 needle 命中
    /// rustfmt 拆行的形态（`.\n0\n.\nlock(`）。
    fn command_spans(src: &str) -> Vec<(String, bool)> {
        let attr = ["#[tauri::", "command"].join("");
        let starts: Vec<usize> = src.match_indices(&attr).map(|(i, _)| i).collect();
        starts
            .iter()
            .enumerate()
            .map(|(n, &start)| {
                let end = starts.get(n + 1).copied().unwrap_or(src.len());
                let span = &src[start..end];
                let attr_args_end = span.find(']').expect("attribute 应闭合");
                let is_async = span[..attr_args_end].contains("(async)");
                let collapsed: String = span.chars().filter(|c| !c.is_whitespace()).collect();
                (collapsed, is_async)
            })
            .collect()
    }

    /// §53 F-SP2-7：`daemon_homes` 必须是 `#[tauri::command(async)]`——
    /// 惰性重建（连接 + 握手最长 30s，或 sidecar 拉起等待 10s）跑在主线程
    /// 会卡 UI（同步命令在主线程执行）。tauri 胶水无运行时缝，按源文本
    /// 锁定属性；`?` 透传由 state.rs 的缝级测试覆盖。
    #[test]
    fn daemon_homes_command_is_async_attributed() {
        let src = production_source();
        assert!(
            src.contains("#[tauri::command(async)]\npub async fn daemon_homes"),
            "daemon_homes 必须保持 #[tauri::command(async)]（重建不得跑主线程）"
        );
        assert!(
            src.contains("session.handshake_homes()?"),
            "daemon_homes 必须透传 handshake_homes 的错误（§53 F-SP2-7）"
        );
    }

    /// §57 F-SP2-10：`events_subscribe` 不得经 with_session 碰会话锁——
    /// §56 改 async 后各调用是独立 tokio 任务，先在会话 Mutex（std 锁非
    /// FIFO）上排队，拿到锁才 set_target：快速切 home A→B 时 A 可能后拿
    /// 锁 → 代次按拿锁顺序分配，泵停在 A，B 的监听器按 homeId 过滤永不
    /// 触发（外部 CLI 改动不再刷新树）。修复取 §57 处方：runtime 目录
    /// 直接取 `daemon::runtime_dir()`（与 state.rs 建会话同源），泵自己
    /// 连接/退避——命令不碰会话，且改回**同步**命令以保住到达顺序
    /// （同步命令在主线程按到达顺序串行执行；同时消除「订阅失败时
    /// with_session 先返回、不 set_target，泵停在旧 home 无重试」）。
    #[test]
    fn events_subscribe_is_sync_and_session_lock_free() {
        let src = production_source();
        let needle = ["fnevents_", "subscribe"].join("");
        let spans = command_spans(&src);
        let subscribe_spans: Vec<&(String, bool)> = spans
            .iter()
            .filter(|(span, _)| span.contains(&needle))
            .collect();
        assert_eq!(
            subscribe_spans.len(),
            1,
            "应恰好有一个 events_subscribe 命令 span，实际 {} 个",
            subscribe_spans.len()
        );
        let (span, is_async) = subscribe_spans[0];
        assert!(
            !is_async,
            "events_subscribe 必须是同步命令（§57 F-SP2-10：不持锁后命令极轻，\
             同步保住 set_target 的到达顺序；async 形态下会话锁排队非 FIFO）"
        );
        let with_session = ["with_", "session"].join("");
        let direct_lock = [".0", ".lock("].join("");
        let rpc_fn = ["rpc", "_of("].join("");
        for shape in [&with_session, &direct_lock, &rpc_fn] {
            assert!(
                !span.contains(shape),
                "events_subscribe 不得触碰会话锁（形态 {shape:?}）：{span:?}"
            );
        }
    }

    /// §55 F-SP2-9 → §57 P3 修订：会话锁（with_session / 直接 `.0.lock()` /
    /// `rpc_of`）只允许出现在 **async 命令**里——同步命令在主线程执行，
    /// daemon_homes/omt_call 惰性重建（握手读超时 30s / sidecar 拉起 10s）
    /// 整段持锁时会在主线程锁上等最长 30s（转圈）。§57 点名旧断言两处假
    /// 通过：`rfind("pub ")` 定边界漏私有/pub(crate) 同步 fn；无最少命中数
    /// （with_session 改名 = 零命中 = 空过）。本测按 `#[tauri::command…]`
    /// 属性定边界 + 断言 ≥3 处调用点（omt_call / daemon_homes / daemon_ensure；
    /// §57 起 events_subscribe 不再持锁）。
    #[test]
    fn session_lock_shapes_live_only_in_async_commands() {
        let src = production_source();
        let with_session_call = ["with_session", "(&shared"].join("");
        let direct_lock = [".0", ".lock("].join("");
        let rpc_fn = ["rpc", "_of("].join("");
        let mut calls = 0usize;
        for (span, is_async) in command_spans(&src) {
            calls += span.match_indices(&with_session_call).count();
            if is_async {
                continue;
            }
            for shape in [&with_session_call, &direct_lock, &rpc_fn] {
                assert!(
                    !span.contains(shape),
                    "同步命令不得触碰会话锁（形态 {shape:?}，主线程会在锁上等 30s）：{span:?}"
                );
            }
        }
        assert!(
            calls >= 3,
            "with_session 调用点应 ≥3（omt_call / daemon_homes / daemon_ensure）——\
             改名后零命中空过是 §57 点名的假通过，实际 {calls} 处"
        );
    }
}
