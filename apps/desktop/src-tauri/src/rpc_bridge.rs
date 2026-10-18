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

use crate::harness::{augmented_path, probe_version, which_binary};

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
                    crate::executor::reconcile_interrupted_execs(
                        &reconciler,
                        &daemon::runtime_dir(),
                    );
                });
            }
        }
    }
    f(guard.as_mut().expect("session just established"))
}

#[tauri::command]
pub fn omt_call(
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
#[tauri::command]
pub fn daemon_homes(shared: State<'_, SharedSession>) -> Result<serde_json::Value, String> {
    with_session(&shared, |session| {
        Ok(serde_json::json!({
            "homes": session.enrollment.handshake["homes"].clone(),
            "homeDir": std::env::var("HOME").unwrap_or_default(),
        }))
    })
}

#[tauri::command]
pub fn daemon_ensure(shared: State<'_, SharedSession>) -> Result<daemon::DaemonStatus, String> {
    with_session(&shared, |_session| Ok(()))?;
    daemon_status()
}

/// Force a FRESH session (drop the cached enrollment and re-handshake) —
/// used after home/declare so the new home enters the credential's scoped
/// grant (requiresRehandshake semantics, KTD3 client half).
#[tauri::command]
pub fn daemon_reconnect(shared: State<'_, SharedSession>) -> Result<serde_json::Value, String> {
    {
        let mut guard = shared
            .0
            .lock()
            .map_err(|_| "session lock poisoned".to_string())?;
        *guard = None;
    }
    daemon_homes(shared)
}

/// U10 seam: start streaming daemon events for one home to the frontend
/// over the Tauri event channel (`omt://event`). Spawns a reader thread
/// paging events/resume; the session's credential authorizes the stream.
/// (Wired end-to-end in U10 when the tree subscribes; registered now so
/// the bridge surface is complete.)
#[tauri::command]
pub fn events_subscribe(
    app: AppHandle,
    shared: State<'_, SharedSession>,
    home_id: String,
    since: u64,
) -> Result<(), String> {
    let runtime_dir = with_session(&shared, |session| Ok(session.runtime_dir.clone()))?;
    std::thread::spawn(move || {
        // Fresh connection per subscription (the shared session is
        // single-flight by design; readers are independent).
        let descriptor = match daemon::live_descriptor(&runtime_dir) {
            Some(descriptor) => descriptor,
            None => return,
        };
        let options = omt_client::EnrollOptions {
            kind: "desktop".into(),
            name: Some("oh-my-ticket-desktop-events".into()),
            actor_namespace: None,
            operations: None,
            credential_path: None,
        };
        let Ok(mut enrollment) = omt_client::Client::connect_and_enroll(&descriptor, &options)
        else {
            return;
        };
        let mut cursor = since;
        loop {
            let page = match enrollment.client.call(
                "events/resume",
                serde_json::json!({ "homeId": home_id, "cursor": cursor, "limit": 500 }),
            ) {
                Ok(page) => page,
                Err(_) => return, // daemon gone or stream closed — frontend re-subscribes
            };
            let events = page["events"].as_array().cloned().unwrap_or_default();
            for envelope in &events {
                let _ = app.emit("omt://event", envelope.clone());
            }
            let next = page["cursor"].as_u64().unwrap_or(cursor).max(cursor);
            if events.len() < 500 && next == cursor {
                // Caught up: long-poll cadence — the resume endpoint holds
                // or returns immediately; back off lightly either way.
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            cursor = next;
        }
    });
    Ok(())
}
