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

/// 泵向前端派发事件用的 Tauri 事件名（envelope 流）。
const EVENT_CHANNEL: &str = "omt://event";
/// CF3：泵健康上报的 Tauri 事件名（ok:false 失败 / ok:true 恢复）。
const EVENTS_STATUS_CHANNEL: &str = "omt://events-status";

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

/// 泵的效果面注入类型（F-TERM-15；集中定义避免签名过长）。
type TargetSource = Box<dyn Fn() -> Option<SubscriptionTarget> + Send>;
type Connect = Box<dyn FnMut() -> Result<Box<dyn ResumeConn>, String> + Send>;
type Emit = Box<dyn FnMut(&str, serde_json::Value) + Send>;
type Sleep = Box<dyn Fn(u64) + Send>;
type StopCheck = Box<dyn Fn() -> bool + Send>;

/// 一条泵连接上的 events/resume 调用（F-TERM-15 的注入缝：生产 =
/// `omt_client::Client`；测试 = 脚本化的假连接，直接驱动泵的行为）。
trait ResumeConn: Send {
    fn resume(&mut self, home_id: &str, cursor: u64) -> Result<serde_json::Value, String>;
}

/// 生产连接：desktop 会话 socket 上的 events/resume 长轮询。
struct ClientConn(omt_client::Client);

impl ResumeConn for ClientConn {
    fn resume(&mut self, home_id: &str, cursor: u64) -> Result<serde_json::Value, String> {
        self.0
            .call(
                "events/resume",
                serde_json::json!({ "homeId": home_id, "cursor": cursor, "limit": 500 }),
            )
            .map_err(|problem| format!("{} ({})", problem.message, problem.code))
    }
}

/// CF3：健康上报载荷的组装（事件名 + homeId + ok + error 一处定义——
/// 行为测试据此锁定事件名与恢复事件的 homeId）。
fn events_status_payload(
    home_id: &str,
    ok: bool,
    error: Option<&str>,
) -> (&'static str, serde_json::Value) {
    (
        EVENTS_STATUS_CHANNEL,
        serde_json::json!({ "homeId": home_id, "ok": ok, "error": error }),
    )
}

/// 泵线程主体：单连接长期轮询 events/resume；代际变化 → 切 home/cursor
/// （连接复用）；daemon 失联 → 指数退避重连（目标切换立即重试）；
/// 无目标 → 轻睡眠等待。效果面全部注入（F-TERM-15：连接 / 目标源 /
/// 睡眠 / 停止判定可注入，测试直接驱动行为，取代只统计子串的源码扫描）：
/// - `current_target`：当前订阅目标（生产 = 注册表 `SUBSCRIPTIONS`）；
/// - `connect`：建立一条新连接（生产 = live descriptor 探测 + enroll）；
/// - `emit`：向前端派发（事件名，载荷）——生产 = `app.emit`；
/// - `sleep`：退避/空转睡眠（生产 = std::thread::sleep；测试 = no-op）；
/// - `should_stop`：每次迭代开头的停止判定（生产 = 恒 false——泵是无限
///   循环；测试 = 迭代计数上限，保证有限收敛）。
///
/// CF3：连接/拉取连续失败达 [`PumpHealth::FAILURE_THRESHOLD`] 时经 `emit`
/// 上报 ok:false（带错误摘要），恢复时上报 ok:true；计成功的口径 = 一次
/// 成功的 events/resume 调用（空页也算——「只把非空页计成功」的变异会让
/// 「拉到空页但连接已恢复」永远不报警）。
fn run_subscription_pump(
    current_target: TargetSource,
    mut connect: Connect,
    mut emit: Emit,
    sleep: Sleep,
    should_stop: StopCheck,
) {
    let mut conn: Option<Box<dyn ResumeConn>> = None;
    let mut active: Option<SubscriptionTarget> = None;
    let mut cursor = 0u64;
    let mut backoff_ms = 250u64;
    let mut health = PumpHealth::new();
    loop {
        if should_stop() {
            return;
        }
        let current = current_target();
        // 代际切换：换 home / 游标重置（连接保留——credential 与 home 无关）
        if current.as_ref().map(|t| t.generation) != active.as_ref().map(|t| t.generation) {
            active = current;
            cursor = active.as_ref().map(|t| t.since).unwrap_or(0);
            backoff_ms = 250; // 新目标立即全速
            health.reset(); // 新目标＝新订阅，旧 home 的失败不带入
        }
        let Some(target) = active.clone() else {
            sleep(200);
            continue;
        };
        if conn.is_none() {
            match connect() {
                Ok(established) => conn = Some(established),
                Err(err) => {
                    if health.note_failure() {
                        let (name, payload) = events_status_payload(
                            &target.home_id,
                            false,
                            Some(&format!("daemon 连接失败：{err}")),
                        );
                        emit(name, payload);
                    }
                    sleep(backoff_ms);
                    backoff_ms = (backoff_ms * 2).min(5_000);
                    continue;
                }
            }
        }
        let page = conn
            .as_mut()
            .expect("conn just established")
            .resume(&target.home_id, cursor);
        match page {
            Ok(page) => {
                backoff_ms = 250;
                if health.note_success() {
                    let (name, payload) = events_status_payload(&target.home_id, true, None);
                    emit(name, payload);
                }
                let events = page["events"].as_array().cloned().unwrap_or_default();
                for envelope in &events {
                    emit(EVENT_CHANNEL, envelope.clone());
                }
                let next = page["cursor"].as_u64().unwrap_or(cursor).max(cursor);
                if events.len() < 500 && next == cursor {
                    sleep(250);
                }
                cursor = next;
            }
            Err(message) => {
                // daemon 失联/流关闭：弃连接退避重连（前端无需重订阅）；
                // CF3：连续失败达阈值后上报（外部改动不会自动刷新）
                if health.note_failure() {
                    let (name, payload) =
                        events_status_payload(&target.home_id, false, Some(&message));
                    emit(name, payload);
                }
                conn = None;
                sleep(backoff_ms);
                backoff_ms = (backoff_ms * 2).min(5_000);
            }
        }
    }
}

/// 生产组装（F-TERM-15 后的薄壳）：注册表 + live descriptor 探测 + 真实
/// 睡眠 + 永不停止；行为本体在 [`run_subscription_pump`]，测试直接驱动。
fn subscription_pump(
    runtime_dir: std::path::PathBuf,
    emit: impl Fn(&str, serde_json::Value) + Send + 'static,
) {
    run_subscription_pump(
        Box::new(|| SUBSCRIPTIONS.current()),
        {
            let runtime_dir = runtime_dir.clone();
            Box::new(move || {
                let options = omt_client::EnrollOptions {
                    kind: "desktop".into(),
                    name: Some("oh-my-ticket-desktop-events".into()),
                    actor_namespace: None,
                    operations: None,
                    credential_path: None,
                };
                daemon::live_descriptor(&runtime_dir)
                    .ok_or_else(|| "无活 descriptor（daemon 未运行）".to_string())
                    .and_then(|descriptor| {
                        omt_client::Client::connect_and_enroll(&descriptor, &options)
                            .map(|enrollment| {
                                Box::new(ClientConn(enrollment.client)) as Box<dyn ResumeConn>
                            })
                            .map_err(|err| err.to_string())
                    })
            })
        },
        Box::new(emit),
        Box::new(|ms| std::thread::sleep(std::time::Duration::from_millis(ms))),
        Box::new(|| false),
    )
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
            subscription_pump(runtime_dir, move |name, payload| {
                let _ = app.emit(name, payload);
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
}

/// F-TERM-15：泵的**行为**测试（直接驱动 [`run_subscription_pump`]，取代
/// 只统计子串的源码扫描 `subscription_pump_wires_health_reporting`）。变异
/// 覆盖：每次成功都发 ok、「事件名改错」、「恢复时 homeId 为空」、「只在
/// 非空页计成功」——任一变异都会让这里的断言红。
#[cfg(test)]
mod pump_behavior_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// 脚本化的假连接：events/resume 的结果序列在**连接之间共享**（每次
    /// 失败后泵弃连接重连，脚本跨连接继续）。每次调用记录
    /// (homeId, cursor)，供游标推进断言。
    /// 事件流里的 resume 调用标记（名字非 Tauri 事件名；顺序断言用）。
    const RESUME_MARK: &str = ":resume";

    type EventSink = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    #[derive(Clone)]
    struct SharedScript {
        outcomes: Arc<Mutex<VecDeque<Result<serde_json::Value, String>>>>,
        calls: Arc<Mutex<Vec<(String, u64)>>>,
        /// resume 调用的时序标记直接写进与 emit 相同的事件流（同一线程，
        /// 保持真实交错——顺序断言的基础）
        sink: EventSink,
    }

    impl SharedScript {
        fn new(outcomes: Vec<Result<serde_json::Value, String>>, sink: EventSink) -> Self {
            Self {
                outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
                calls: Arc::new(Mutex::new(Vec::new())),
                sink,
            }
        }
    }

    struct ScriptedConn(SharedScript);

    impl ResumeConn for ScriptedConn {
        fn resume(&mut self, home_id: &str, cursor: u64) -> Result<serde_json::Value, String> {
            let nth = {
                let mut calls = self.0.calls.lock().unwrap();
                calls.push((home_id.to_string(), cursor));
                calls.len()
            };
            let outcome = self
                .0
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(serde_json::json!({ "events": [], "cursor": cursor })));
            // 记录第几次拉取 + 结果形状（F-TERM-15「计成功的口径」的顺序断言：
            // 空页的成功也必须立刻计——恢复上报不得等下一页）
            let shape = match &outcome {
                Ok(page) => {
                    if page["events"]
                        .as_array()
                        .map(|list| list.len())
                        .unwrap_or(0)
                        > 0
                    {
                        "page-with-events"
                    } else {
                        "empty-page"
                    }
                }
                Err(_) => "err",
            };
            self.0.sink.lock().unwrap().push((
                RESUME_MARK.to_string(),
                serde_json::json!({ "nth": nth, "shape": shape }),
            ));
            outcome
        }
    }

    /// 一次泵运行的装配：注入目标源 / 脚本连接 / 记录型 emit / no-op 睡眠 /
    /// 迭代上限停止。返回 (发出的事件列表, resume 调用参数)。
    type Emitted = Vec<(String, serde_json::Value)>;
    type ResumeCalls = Vec<(String, u64)>;

    fn run_pump(
        script: Vec<Result<serde_json::Value, String>>,
        enough: usize,
    ) -> (Emitted, ResumeCalls) {
        let emitted: EventSink = Arc::new(Mutex::new(Vec::new()));
        let shared = SharedScript::new(script, emitted.clone());
        let target = Arc::new(Mutex::new(Some(SubscriptionTarget {
            home_id: "h_pump".to_string(),
            since: 7,
            generation: 1,
        })));
        let probe_target = target.clone();
        let probe_emit = emitted.clone();
        let resumed = shared.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let probe_stop = stopped.clone();
        let iterations = Arc::new(AtomicUsize::new(0));
        let probe_iterations = iterations.clone();
        let handle = std::thread::spawn(move || {
            run_subscription_pump(
                Box::new(move || probe_target.lock().unwrap().clone()),
                Box::new(
                    move || Ok(Box::new(ScriptedConn(resumed.clone())) as Box<dyn ResumeConn>),
                ),
                Box::new(move |name, payload| {
                    // 收到足够多的 events-status 后主动停——否则靠迭代上限兑底
                    if name == EVENTS_STATUS_CHANNEL {
                        let mut events = probe_emit.lock().unwrap();
                        let status_count = events
                            .iter()
                            .filter(|(n, _)| n == EVENTS_STATUS_CHANNEL)
                            .count();
                        events.push((name.to_string(), payload));
                        drop(events);
                        if status_count + 1 >= enough {
                            probe_stop.store(true, Ordering::SeqCst);
                        }
                    } else {
                        probe_emit.lock().unwrap().push((name.to_string(), payload));
                    }
                }),
                Box::new(|_ms| {}),
                Box::new(move || probe_iterations.fetch_add(1, Ordering::SeqCst) > 2_000),
            );
        });
        handle.join().expect("泵线程不 panic");
        let events = emitted.lock().unwrap().clone();
        let calls = shared.calls.lock().unwrap().clone();
        (events, calls)
    }

    fn statuses(events: &[(String, serde_json::Value)]) -> Vec<&serde_json::Value> {
        events
            .iter()
            .filter(|(name, _)| name == "omt://events-status")
            .map(|(_, payload)| payload)
            .collect()
    }

    /// 失败只发一次 + 恢复发一次 + 事件名 + 恢复事件的 homeId + 计成功的
    /// 口径（空页的成功也计恢复）。脚本：4 次失败（阈值 3 → 第 3 次发
    /// down，第 4 次不重复）、一次**空页**成功（恢复上报）、一次带事件的
    /// 成功页（envelope 走 omt://event）、再两次成功（不再上报）。
    #[test]
    fn pump_reports_down_once_recovers_once_and_forwards_envelopes() {
        let envelope = serde_json::json!({ "homeId": "h_pump", "kind": "node.created" });
        let (events, calls) = run_pump(
            vec![
                Err("events/resume: connection closed: eof (IO)".to_string()),
                Err("events/resume: connection closed: eof (IO)".to_string()),
                Err("events/resume: connection closed: eof (IO)".to_string()),
                Err("events/resume: connection closed: eof (IO)".to_string()),
                // 空页成功——计成功的口径：空页也算，恢复上报必须发生
                Ok(serde_json::json!({ "events": [], "cursor": 7 })),
                // 带 envelope 的成功页：游标推进，envelope 经 omt://event 转发
                Ok(serde_json::json!({ "events": [envelope], "cursor": 8 })),
                Ok(serde_json::json!({ "events": [], "cursor": 8 })),
            ],
            2,
        );
        let statuses = statuses(&events);
        assert_eq!(
            statuses.len(),
            2,
            "失败只发一次 ok:false、恢复只发一次 ok:true：{events:?}"
        );
        assert_eq!(statuses[0]["ok"], false, "第一份是失败上报");
        assert_eq!(
            statuses[0]["error"], "events/resume: connection closed: eof (IO)",
            "失败上报携带错误摘要"
        );
        assert_eq!(statuses[1]["ok"], true, "第二份是恢复上报");
        assert_eq!(
            statuses[1]["homeId"], "h_pump",
            "恢复事件带目标 homeId（不为空、不是别的 home）"
        );
        assert_eq!(statuses[0]["homeId"], "h_pump");
        // envelope 转发：事件名 + 原样载荷
        let forwarded: Vec<_> = events
            .iter()
            .filter(|(name, _)| name == "omt://event")
            .map(|(_, payload)| payload.clone())
            .collect();
        assert_eq!(forwarded, vec![envelope], "envelope 经 omt://event 转发");
        // resume 参数：目标 homeId + 初始游标（since）与推进后的游标
        assert_eq!(calls[0], ("h_pump".to_string(), 7), "首拉用 since 游标");
        assert!(
            calls.contains(&("h_pump".to_string(), 8)),
            "游标随页推进：{calls:?}"
        );
        // F-TERM-15「计成功的口径」的顺序面：空页的成功也要立刻计——恢复
        // 上报必须出现在第 5 次拉取（空页）与第 6 次拉取（带事件的页）之间，
        // 不得等下一页非空才算成功
        let position = |name: &str, payload: &dyn Fn(&serde_json::Value) -> bool| -> usize {
            events
                .iter()
                .position(|(n, p)| n == name && payload(p))
                .unwrap_or_else(|| panic!("事件流里找不到：{name} — {events:?}"))
        };
        let empty_resume = position(RESUME_MARK, &|p| {
            p["nth"] == 5 && p["shape"] == "empty-page"
        });
        let recovery = position(EVENTS_STATUS_CHANNEL, &|p| p["ok"] == true);
        let next_resume = position(RESUME_MARK, &|p| p["nth"] == 6);
        assert!(
            empty_resume < recovery && recovery < next_resume,
            "恢复上报必须紧跟空页成功（计成功不看页是否为空）：{events:?}"
        );
    }

    /// 连接失败同样只发一次 ok:false（错误摘要带 daemon 连接失败前缀）；
    /// 连接永不成功 → 没有恢复事件、没有 envelope。
    #[test]
    fn pump_reports_connection_failure_once() {
        let target = Arc::new(Mutex::new(Some(SubscriptionTarget {
            home_id: "h_dead".to_string(),
            since: 0,
            generation: 1,
        })));
        let probe_target = target.clone();
        let emitted: EventSink = Arc::new(Mutex::new(Vec::new()));
        let probe_emit = emitted.clone();
        let iterations = Arc::new(AtomicUsize::new(0));
        let probe_iterations = iterations.clone();
        run_subscription_pump(
            Box::new(move || probe_target.lock().unwrap().clone()),
            Box::new(|| Err("无活 descriptor（daemon 未运行）".to_string())),
            Box::new(move |name, payload| {
                probe_emit.lock().unwrap().push((name.to_string(), payload))
            }),
            Box::new(|_ms| {}),
            Box::new(move || probe_iterations.fetch_add(1, Ordering::SeqCst) > 30),
        );
        let events = emitted.lock().unwrap().clone();
        let statuses = statuses(&events);
        assert_eq!(statuses.len(), 1, "连接失败只发一次：{events:?}");
        assert_eq!(statuses[0]["ok"], false);
        assert_eq!(statuses[0]["homeId"], "h_dead");
        assert_eq!(
            statuses[0]["error"],
            "daemon 连接失败：无活 descriptor（daemon 未运行）"
        );
        assert!(
            events.iter().all(|(name, _)| name == "omt://events-status"),
            "没有 envelope 被转发（连接从未建立）"
        );
    }

    /// 代际切换重置健康态（行为面）：先在 h_old 上跨阈值报 down，切到
    /// h_new 后同样次数的失败**重新计数**——第二份 down 属于 h_new 且在
    /// 切换后的第 3 次失败才发出。
    #[test]
    fn pump_resets_health_on_target_switch() {
        let target = Arc::new(Mutex::new(SubscriptionTarget {
            home_id: "h_old".to_string(),
            since: 0,
            generation: 1,
        }));
        let emitted: EventSink = Arc::new(Mutex::new(Vec::new()));
        let script = SharedScript::new(
            vec![
                Err("boom (IO)".to_string()),
                Err("boom (IO)".to_string()),
                Err("boom (IO)".to_string()), // h_old 跨阈值 → down #1
                Err("boom (IO)".to_string()),
                Err("boom (IO)".to_string()),
                Err("boom (IO)".to_string()),
            ],
            emitted.clone(),
        );
        let iterations = Arc::new(AtomicUsize::new(0));
        let stop_counter = iterations.clone();
        let resumed = script.clone();
        let probe_emit = emitted.clone();
        let target_probe = target.clone();
        let emit_probe = emitted.clone();
        let handle = std::thread::spawn(move || {
            run_subscription_pump(
                // 目标源：h_old（generation 1）→ 第一份 down 上报落地后切
                // h_new（generation 2）——确定性切换，不靠迭代计数
                Box::new(move || {
                    let switched = probe_emit
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(name, _)| name == EVENTS_STATUS_CHANNEL);
                    let mut guard = target_probe.lock().unwrap();
                    if switched && guard.generation == 1 {
                        *guard = SubscriptionTarget {
                            home_id: "h_new".to_string(),
                            since: 0,
                            generation: 2,
                        };
                    }
                    Some(guard.clone())
                }),
                Box::new(
                    move || Ok(Box::new(ScriptedConn(resumed.clone())) as Box<dyn ResumeConn>),
                ),
                Box::new(move |name, payload| {
                    emit_probe.lock().unwrap().push((name.to_string(), payload))
                }),
                Box::new(|_ms| {}),
                Box::new(move || stop_counter.fetch_add(1, Ordering::SeqCst) > 200),
            );
        });
        handle.join().expect("泵线程不 panic");
        let events = emitted.lock().unwrap().clone();
        let statuses = statuses(&events);
        // 脚本耗尽后的缺省响应是成功空页——恢复上报可能出现，只断言 down
        // 的归属与次数（这是 reset 语义的核心）
        let downs: Vec<_> = statuses
            .iter()
            .filter(|payload| payload["ok"] == false)
            .collect();
        assert_eq!(downs.len(), 2, "两个 home 各报一次 down：{events:?}");
        assert_eq!(downs[0]["homeId"], "h_old");
        assert_eq!(
            downs[1]["homeId"], "h_new",
            "切换后重新计数：h_new 的 down 要再凑满阈值才发（无 reset 时第 4 次\
             失败不报，永远不会有 h_new 的 down）"
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
