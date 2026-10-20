//! exec_* tauri command 包装。执行面内核在 omt_exec（设计 §3.1）；这里
//! 负责把 SharedSession 适配成 RpcFn、把 Tauri Channel 适配成 EventSink。

use std::path::Path;
use std::sync::Arc;

use omt_exec::executor::{EventSink, ExecStartOutcome, RpcFn};

use crate::daemon;
use crate::state::SharedSession;

/// SharedSession → RpcFn（clone 共享同一个内部 mutex，等待线程据此释放租约）。
pub fn rpc_of(shared: &SharedSession) -> RpcFn {
    let shared = shared.clone();
    Arc::new(move |method: &str, params: serde_json::Value| {
        let mut guard = shared
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = guard.as_mut().ok_or("daemon session not established")?;
        session.call(method, params)
    })
}

/// U6 (R-B)：exec_start（§3.4 统一入口，内含 §3.3 链）。
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub fn exec_start(
    shared: tauri::State<'_, SharedSession>,
    state: tauri::State<'_, crate::state::ExecutionState>,
    home_id: String,
    home_path: String,
    node_id: String,
    harness: String,
    unattended: bool,
    model: Option<String>,
    resume_session: Option<String>,
    agent_id: Option<String>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<ExecStartOutcome, String> {
    let sink: EventSink = Arc::new(move |value| {
        let _ = channel.send(value);
    });
    let omt_path = daemon::omt_sidecar_path()?;
    omt_exec::executor::exec_start_impl(
        &rpc_of(&shared),
        &state.inner().0,
        &omt_path,
        &daemon::runtime_dir(),
        &home_id,
        Path::new(&home_path),
        &node_id,
        &harness,
        unattended,
        model.as_deref(),
        resume_session.as_deref(),
        agent_id.as_deref(),
        sink,
    )
}

/// 幂等终止（§3.4）：对已终态/不存在的 execId 直接成功。
#[tauri::command(async)]
pub fn exec_stop(
    state: tauri::State<'_, crate::state::ExecutionState>,
    exec_id: String,
) -> Result<(), String> {
    state.0.exec_stop(&exec_id)
}

/// 历史 transcript 尾部回载（F-PI-3 / F-U12-4）。
#[tauri::command(async)]
pub fn exec_transcript_tail(
    home_id: String,
    exec_id: String,
    max_lines: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    Ok(omt_exec::executor::transcript_tail_from(
        &daemon::runtime_dir(),
        &home_id,
        &exec_id,
        max_lines,
    ))
}

/// Orphan 强杀。Renderer 只能提交持久化记录身份，不能提交 pid。
#[tauri::command(async)]
pub fn exec_kill_orphan(home_id: String, agent_id: String, exec_id: String) -> Result<(), String> {
    omt_exec::executor::kill_orphan(&daemon::runtime_dir(), &home_id, &agent_id, &exec_id)
}
