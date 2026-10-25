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
use crate::state::SharedSession;

/// daemon_homes / daemon_reconnect 共用的返回载荷（§51 惰性重建缝 + §53
/// F-SP2-7 重建失败上抛），抽出以便 reconnect 复用（不再经 State 转调命令）。
async fn homes_payload(shared: SharedSession) -> Result<serde_json::Value, String> {
    // CF5：阻塞部分（锁 + 惰性重建 30s/10s + socket 读）经
    // with_session_async 内部的 spawn_blocking 跑在阻塞线程池；
    // CF6：锁经 state 模块的结构性入口取得。
    shared
        .with_session_async(|session| {
            Ok(serde_json::json!({
                // §51：经 session 的惰性重建缝（超时丢弃连接后也能恢复）；
                // §53：重建失败必须上抛（`?`），不得回落成空列表
                "homes": session.handshake_homes()?,
                "homeDir": std::env::var("HOME").unwrap_or_default(),
            }))
        })
        .await
}

/// U15：async 化——同步命令在主线程跑，慢 RPC（锁竞争/大响应）会卡 UI；
/// async 命令进 tauri 线程池。U2-1（CF5）：阻塞部分再下沉一层——
/// with_session_async 内部 spawn_blocking，不占 tokio worker。
#[tauri::command(async)]
pub async fn omt_call(
    shared: State<'_, SharedSession>,
    method: String,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let shared = shared.inner().clone();
    shared
        .with_session_async(move |session| session.call(&method, params))
        .await
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
    homes_payload(shared.inner().clone()).await
}

/// §55 F-SP2-9：改 async——`daemon_ensure` 经会话入口惰性建立会话
///（连接 + 握手最长 30s），同步命令跑在主线程；与 daemon_homes/omt_call
/// 共享同一把会话 Mutex，后者持锁重建时主线程会在锁上等最长 30s（转圈）。
/// U2-1（CF5）：会话建立与 daemon_status 的描述符读取都在阻塞作用域里。
#[tauri::command(async)]
pub async fn daemon_ensure(
    shared: State<'_, SharedSession>,
) -> Result<daemon::DaemonStatus, String> {
    let shared = shared.inner().clone();
    shared
        .in_blocking_scope(|scope| {
            scope.with_session(|_session| Ok(()))?;
            Ok(daemon::status(&daemon::runtime_dir()))
        })
        .await
}

/// Force a FRESH session (drop the cached enrollment and re-handshake) —
/// used after home/declare so the new home enters the credential's scoped
/// grant (requiresRehandshake semantics, KTD3 client half).
/// §53 F-SP2-7：随 daemon_homes 一并 async（重握手同样可能等 30s/10s）。
#[tauri::command(async)]
pub async fn daemon_reconnect(
    shared: State<'_, SharedSession>,
) -> Result<serde_json::Value, String> {
    let shared = shared.inner().clone();
    shared.reset().await?;
    homes_payload(shared).await
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

/// CF3（§59 P3）：泵健康跟踪——真实失败（无活 descriptor、events/resume
/// 出错）原本只在泵里静默无限重试，外部 CLI 改动不再刷新树而界面无提示。
/// 连续失败达到阈值后应发一次 `omt://events-status {ok:false}`（掩盖瞬时
/// 抖动，又足够快地暴露「外部改动不会自动刷新」）；恢复时发一次
/// `{ok:true}`，前端提示随之消失。目标切换（换 home）时重置——新目标
/// 是新订阅，旧 home 的失败不得立刻把新 home 标为失联。
struct PumpHealth {
    consecutive_failures: u32,
    down_reported: bool,
}

impl PumpHealth {
    /// 连续失败多少次后发 ok:false（指数退避下约 0.25+0.5+1s 内达成）。
    const FAILURE_THRESHOLD: u32 = 3;

    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            down_reported: false,
        }
    }

    /// 一次失败。跨过阈值且尚未报告过 → true（应发 ok:false）。
    fn note_failure(&mut self) -> bool {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= Self::FAILURE_THRESHOLD && !self.down_reported {
            self.down_reported = true;
            true
        } else {
            false
        }
    }

    /// 一次成功。此前已报告失败 → true（应发 ok:true 恢复）；否则 false。
    fn note_success(&mut self) -> bool {
        self.consecutive_failures = 0;
        std::mem::replace(&mut self.down_reported, false)
    }

    /// 目标切换（换 home / 游标重置）：健康态归零。
    fn reset(&mut self) {
        *self = Self::new();
    }
}

/// 泵线程主体：单连接长期轮询 events/resume；代际变化 → 切 home/cursor
/// （连接复用）；daemon 失联 → 指数退避重连（目标切换立即重试）；
/// 无目标 → 轻睡眠等待。emit 经闭包注入（测试不打扰 tauri AppHandle）。
/// CF3：连接/拉取连续失败达 [`PumpHealth::FAILURE_THRESHOLD`] 时经
/// `emit_status` 上报 ok:false（带错误摘要），恢复时上报 ok:true。
fn subscription_pump(
    runtime_dir: std::path::PathBuf,
    emit: impl Fn(serde_json::Value) + Send + 'static,
    emit_status: impl Fn(&str, bool, Option<&str>) + Send + 'static,
) {
    let mut client: Option<omt_client::Client> = None;
    let mut active: Option<SubscriptionTarget> = None;
    let mut cursor = 0u64;
    let mut backoff_ms = 250u64;
    let mut health = PumpHealth::new();
    loop {
        let current = SUBSCRIPTIONS.current();
        // 代际切换：换 home / 游标重置（连接保留——credential 与 home 无关）
        if current.as_ref().map(|t| t.generation) != active.as_ref().map(|t| t.generation) {
            active = current;
            cursor = active.as_ref().map(|t| t.since).unwrap_or(0);
            backoff_ms = 250; // 新目标立即全速
            health.reset(); // 新目标＝新订阅，旧 home 的失败不带入
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
            let attempt = daemon::live_descriptor(&runtime_dir)
                .ok_or_else(|| "无活 descriptor（daemon 未运行）".to_string())
                .and_then(|descriptor| {
                    omt_client::Client::connect_and_enroll(&descriptor, &options)
                        .map_err(|err| err.to_string())
                });
            match attempt {
                Ok(enrollment) => client = Some(enrollment.client),
                Err(err) => {
                    if health.note_failure() {
                        emit_status(
                            &target.home_id,
                            false,
                            Some(&format!("daemon 连接失败：{err}")),
                        );
                    }
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
                if health.note_success() {
                    emit_status(&target.home_id, true, None);
                }
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
            Err(problem) => {
                // daemon 失联/流关闭：弃连接退避重连（前端无需重订阅）；
                // CF3：连续失败达阈值后上报（外部改动不会自动刷新）
                if health.note_failure() {
                    emit_status(&target.home_id, false, Some(&problem.message));
                }
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
        let status_app = app.clone();
        std::thread::spawn(move || {
            subscription_pump(
                runtime_dir,
                move |envelope| {
                    let _ = app.emit("omt://event", envelope);
                },
                // CF3：泵健康上报——前端据此提示「外部改动不会自动刷新」
                move |home_id, ok, error| {
                    let _ = status_app.emit(
                        "omt://events-status",
                        serde_json::json!({ "homeId": home_id, "ok": ok, "error": error }),
                    );
                },
            );
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

    /// CF3：健康跟踪语义——阈值前静默、跨阈值发一次 ok:false（不重复）、
    /// 恢复发一次 ok:true、未失败时成功不发、目标切换归零。
    #[test]
    fn pump_health_reports_down_once_at_threshold_and_up_on_recovery() {
        let mut health = PumpHealth::new();
        assert!(!health.note_failure(), "第 1 次失败不报告（掩盖瞬时抖动）");
        assert!(!health.note_failure(), "第 2 次失败不报告");
        assert!(
            health.note_failure(),
            "第 {} 次连续失败跨过阈值 → 报告 ok:false",
            PumpHealth::FAILURE_THRESHOLD
        );
        assert!(!health.note_failure(), "已报告过不重复报 down");
        assert!(!health.note_failure(), "继续失败仍不重复报 down");
        assert!(health.note_success(), "恢复（此前已报 down）→ 报告 ok:true");
        assert!(!health.note_success(), "未处于 down 态的成功不再报告");
        assert!(!health.note_failure(), "恢复后计数归零：单次失败不报告");
    }

    /// CF3：目标切换归零——旧 home 的连续失败不把新 home 立刻标为失联。
    #[test]
    fn pump_health_reset_drops_stale_failure_streak() {
        let mut health = PumpHealth::new();
        for _ in 0..(PumpHealth::FAILURE_THRESHOLD - 1) {
            health.note_failure();
        }
        health.reset();
        for _ in 0..(PumpHealth::FAILURE_THRESHOLD - 1) {
            assert!(!health.note_failure(), "重置后重新计数，未跨阈值不报告");
        }
        assert!(health.note_failure(), "重置后达到阈值照常报告");
    }

    /// CF3：泵线程是无退出条件的无限循环，没有直接的测试缝——用源码锁住
    /// 健康跟踪的接线（两个失败分支都接 note_failure、成功分支接
    /// note_success、代际切换接 reset、events_subscribe 经 emit 上报
    /// omt://events-status）。删掉任一接线，本断言红。
    #[test]
    fn subscription_pump_wires_health_reporting() {
        let full =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_bridge.rs"))
                .expect("read rpc_bridge.rs source");
        let test_module = ["#[cfg(", "test)]"].join("");
        let src = &full[..full.find(&test_module).expect("rpc_bridge.rs 应有测试模块")];
        let pump_start = src
            .find("fn subscription_pump(")
            .expect("subscription_pump 应在生产代码里");
        let pump_end = src[pump_start..]
            .find("\n#[tauri::command]")
            .expect("subscription_pump 之后是 events_subscribe 命令");
        let pump = &src[pump_start..pump_start + pump_end];
        assert_eq!(
            pump.match_indices("health.note_failure()").count(),
            2,
            "连接失败与 events/resume 失败两个分支都要接 note_failure（CF3）"
        );
        assert_eq!(
            pump.match_indices("emit_status(").count(),
            3,
            "两个失败分支 + 恢复分支共 3 处 emit_status 调用"
        );
        assert!(
            pump.contains("health.note_success()"),
            "成功分支接 note_success（恢复上报）"
        );
        assert!(
            pump.contains("health.reset()"),
            "代际切换接 health.reset（新目标不带旧失败计数）"
        );
        assert!(
            src.contains("omt://events-status"),
            "events_subscribe 经 app.emit 上报 omt://events-status"
        );
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

    /// §57 F-SP2-10 → U2-1（CF6）：`events_subscribe` 保持**同步**命令——
    /// 同步命令在主线程按到达顺序串行执行，set_target 顺序 = 到达顺序。
    /// 「不碰会话锁」不再需要子串扫描：会话锁经 state.rs 结构性私有化，
    /// 同步命令在类型上拿不到锁（state.rs 的 compile_fail doctest 证明
    /// 取代旧 command_spans / session_lock_shapes 扫描）。
    #[test]
    fn events_subscribe_stays_a_sync_command() {
        let src = production_source();
        assert!(
            src.contains("#[tauri::command]\npub fn events_subscribe"),
            "events_subscribe 必须保持同步命令（到达顺序保证，§57 F-SP2-10）"
        );
    }
}
