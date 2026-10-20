//! exec_* tauri command 包装。执行面内核在 omt_exec（设计 §3.1）；这里
//! 负责把 SharedSession 适配成 RpcFn、把 Tauri Channel 适配成 EventSink。

use std::path::Path;
use std::sync::Arc;

use omt_exec::executor::{EventSink, ExecStartOutcome, RpcFn};
use omt_exec::layout::{self, ExecFile};

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

/// 尾随 `<agentDir>/<execId>.jsonl` 推送前端（KD-E3：取代进程内 Channel 泵）。
fn follow(
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    after_seq: Option<u64>,
    sink: EventSink,
) -> Result<(), String> {
    let runtime_dir = daemon::runtime_dir();
    let dir = layout::agent_dir(&runtime_dir, home_id, agent_id)?;
    let path = layout::exec_file(&dir, exec_id, ExecFile::Jsonl);
    let (h, a, e) = (
        home_id.to_string(),
        agent_id.to_string(),
        exec_id.to_string(),
    );
    let active = Arc::new(move || omt_exec::control::exec_active(&runtime_dir, &h, &a, &e));
    omt_exec::follow::follow_jsonl(path, after_seq, sink, active);
    Ok(())
}

fn channel_sink(channel: tauri::ipc::Channel<serde_json::Value>) -> EventSink {
    Arc::new(move |value| {
        let _ = channel.send(value);
    })
}

/// U6 (R-B)：exec_start。派发链 ①–⑥ 在本进程，执行在分离的 exec-shim 里；
/// 前端 Channel 由 jsonl 尾随喂入（事件形状不变）。
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub fn exec_start(
    shared: tauri::State<'_, SharedSession>,
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
    let rpc = rpc_of(&shared);
    let omt_bin = daemon::omt_sidecar_path()?;
    let runtime_dir = daemon::runtime_dir();
    let outcome = omt_exec::launch::start(
        &|method: &str, params: serde_json::Value| rpc(method, params),
        &omt_exec::launch::LaunchRequest {
            runtime_dir: &runtime_dir,
            omt_bin: &omt_bin,
            home_id: &home_id,
            home_path: Path::new(&home_path),
            node_id: &node_id,
            harness: &harness,
            unattended,
            model: model.as_deref(),
            resume_session: resume_session.as_deref(),
            target_agent_id: agent_id.as_deref(),
            max_duration: omt_exec::executor::DEFAULT_MAX_DURATION,
        },
    )?;
    follow(
        &home_id,
        &outcome.agent_id,
        &outcome.exec_id,
        None,
        channel_sink(channel),
    )?;
    Ok(outcome)
}

/// 幂等终止：SIGTERM 发给该 exec 的 shim（身份校验后）。
#[tauri::command(async)]
pub fn exec_stop(exec_id: String) -> Result<(), String> {
    omt_exec::control::stop_exec(&daemon::runtime_dir(), &exec_id)
}

/// P-10：停止当前 home 的全部运行中 exec（替代旧的「桌面退出即终止」）。
#[tauri::command(async)]
pub fn exec_stop_all(home_id: String) -> Result<usize, String> {
    omt_exec::control::stop_all(&daemon::runtime_dir(), &home_id)
}

/// P-10：重新接上仍在运行的 exec 的实时流（桌面重启后 shim 仍在跑）。
/// `after_seq` = 前端已有的最后一个 seq（无则 None，从头推）。
#[tauri::command(async)]
pub fn exec_attach(
    home_id: String,
    exec_id: String,
    after_seq: Option<u64>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<(), String> {
    let (agent, _) =
        omt_exec::agents::find_exec_in_home(&daemon::runtime_dir(), &home_id, &exec_id)?
            .ok_or_else(|| format!("exec {exec_id} 不存在"))?;
    follow(
        &home_id,
        &agent.id,
        &exec_id,
        after_seq,
        channel_sink(channel),
    )
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
