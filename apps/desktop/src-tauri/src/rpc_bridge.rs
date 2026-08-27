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

/// 在 PATH 上查找可执行文件（harness 安装探测）。
fn which_binary(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = std::path::PathBuf::from(dir).join(name);
        if candidate.is_file()
            && std::fs::metadata(&candidate)
                .map(|m| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        m.permissions().mode() & 0o111 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                })
                .unwrap_or(false)
        {
            return Some(candidate);
        }
    }
    None
}

/// `<binary> --version` 首行输出（2s 上限；失败不阻断探测）。
fn probe_version(binary: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().next().map(|line| line.trim().to_string()).filter(|line| !line.is_empty())
}

/// 检测本机已安装的 agent harness（dsh / opencode）：PATH 查找 +
/// 版本探针。渲染进程无 fs/shell 能力，探测只能在 Rust 侧做。
#[tauri::command]
pub fn harness_detect(harness: String) -> Result<serde_json::Value, String> {
    let binary = match harness.as_str() {
        "dsh" => "dsh",
        "opencode" => "opencode",
        other => return Err(format!("unsupported harness type: {other}")),
    };
    match which_binary(binary) {
        Some(path) => {
            let version = probe_version(&path);
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
#[tauri::command]
pub fn harness_validate_checkout(path: String) -> Result<serde_json::Value, String> {
    let dir = std::path::PathBuf::from(&path);
    let valid = dir.is_dir()
        && dir.join("pnpm-workspace.yaml").is_file()
        && dir.join("apps").is_dir()
        && dir.join("packages").is_dir();
    Ok(serde_json::json!({ "valid": valid }))
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
        *guard = Some(DaemonSession::establish()?);
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
