//! launcher（设计 §3.2「启动」+ 计划 P-3 / P-5）：发起方进程内执行派发链的
//! daemon 侧步骤（①–⑥，含补偿表），写 spec，以 setsid 拉起 `omt exec-shim`，
//! 等 shim 报告就绪后返回。发起方此后可以退出，执行继续。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use omt_harness::atomic_json::OpenOptionsUnix;
use omt_harness::harness;
use omt_harness::sanitize::sanitize_args;

use crate::agents::{self, AgentAttachment, AgentRecordStatus, NewExec};
use crate::executor::{
    collect_members, compensate_new_run, DaemonRpc, ExecStartOutcome, DISPATCH_PROMPT,
};
use crate::layout::{self, ExecFile};
use crate::spec::{LaunchSpec, SPEC_SCHEMA_VERSION};

/// 进程内串行化派发（与旧 DISPATCH_LOCK 同语义；跨进程由子项目 2 处理）。
static LAUNCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const READY_TIMEOUT: Duration = Duration::from_secs(30);

pub struct LaunchRequest<'a> {
    pub runtime_dir: &'a Path,
    /// `omt` 二进制：既是 shim，也是注入给 harness 的 MCP 桥
    pub omt_bin: &'a Path,
    pub home_id: &'a str,
    /// `.omt` 目录
    pub home_path: &'a Path,
    pub node_id: &'a str,
    pub harness: &'a str,
    pub unattended: bool,
    pub model: Option<&'a str>,
    pub resume_session: Option<&'a str>,
    pub target_agent_id: Option<&'a str>,
    pub max_duration: Duration,
}

enum Ready {
    Spawned,
    Failed(String),
    Timeout,
}

#[allow(clippy::too_many_lines)]
pub fn start(rpc: DaemonRpc<'_>, req: &LaunchRequest<'_>) -> Result<ExecStartOutcome, String> {
    if let Some(model) = req.model {
        harness::validate_model_id(model)?;
    }
    if let Some(session) = req.resume_session {
        harness::validate_model_id(session)?;
    }
    let _guard = LAUNCH_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (runtime_dir, home_id, node_id) = (req.runtime_dir, req.home_id, req.node_id);
    let dispatch_id = format!(
        "d-{}-{}",
        agents::now_epoch_ms(),
        omt_harness::atomic_json::unique_suffix()
    );

    // ① 预检
    let node_resp = rpc(
        "node/get",
        serde_json::json!({"homeId": home_id, "nodeId": node_id}),
    )?;
    let members = collect_members(rpc, home_id, node_id)?;
    if members.is_empty() {
        return Err("无未完成可执行成员".into());
    }
    let active_runs: Vec<String> = node_resp["runs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["runId"].as_str().map(str::to_owned))
        .collect();

    // ② node/execute（发起方是 desktop，adapter_only 可用；P-3）
    rpc(
        "node/execute",
        serde_json::json!({
            "homeId": home_id, "nodeId": node_id, "commandId": format!("{dispatch_id}:execute"),
        }),
    )?;

    // ③ ensure-run
    let (run_id, run_was_newly_created) = if let Some(existing) = active_runs.first() {
        (existing.clone(), false)
    } else {
        let resp = rpc(
            "run/create",
            serde_json::json!({
                "homeId": home_id, "title": format!("dispatch {node_id}"),
                "nodeIds": members, "commandId": format!("{dispatch_id}:create"),
            }),
        )?;
        let created = resp["run"]["runId"]
            .as_str()
            .ok_or("run/create 响应缺少 run.runId（daemon 契约漂移）")?
            .to_string();
        (created, true)
    };

    // ④ run/control start（仅 pending）
    let run_status = rpc(
        "run/get",
        serde_json::json!({"homeId": home_id, "runId": run_id}),
    )?["run"]["status"]
        .as_str()
        .unwrap_or("pending")
        .to_string();
    match run_status.as_str() {
        "pending" => {
            rpc(
                "run/control",
                serde_json::json!({
                    "homeId": home_id, "runId": run_id, "action": "start",
                    "commandId": format!("{dispatch_id}:start"),
                }),
            )?;
        }
        "running" => {}
        "paused" | "interrupted" => {
            return Err(format!(
                "run {run_id} 是 {run_status}，需在弹窗中选择 resume/retry 或取消"
            ));
        }
        other => return Err(format!("run {run_id} 状态 {other}，无法派发")),
    }

    // 启动参数（记录用；shim 会用同一输入重算，P-4）
    let plan = harness::resolve_launch(runtime_dir, req.harness)?;
    let prompt = DISPATCH_PROMPT
        .replace("{run_id}", &run_id)
        .replace("{home_id}", home_id)
        .replace(
            "{tool_hint}",
            harness::descriptor_of(req.harness)
                .map(|d| d.tool_hint)
                .unwrap_or(""),
        );
    let exec_id = format!("exec-{}", omt_harness::atomic_json::unique_suffix());
    let agent_id = match req.target_agent_id {
        Some(target) => target.to_string(),
        None => layout::new_agent_id(&exec_id),
    };
    let agent_dir = layout::agent_dir(runtime_dir, home_id, &agent_id)?;
    let prompt_file = harness::prompt_file_path(&agent_dir, &exec_id, plan.prompt_channel);
    let args = harness::compose_launch_args(
        &plan,
        req.unattended,
        &prompt,
        prompt_file.as_deref(),
        req.model,
        req.resume_session,
    );

    // ⑤ 注册 / 续接 agent
    if let Some(target) = req.target_agent_id {
        match agents::get_agent(runtime_dir, home_id, target)? {
            Some(agent) if agent.harness != req.harness => {
                return Err(format!(
                    "目标 agent 属于 {}，不能以 {} 续跑",
                    agent.harness, req.harness
                ));
            }
            Some(agent)
                if matches!(
                    agent.status,
                    AgentRecordStatus::Starting | AgentRecordStatus::Running
                ) =>
            {
                return Err(format!("目标 agent {target} 仍在运行，拒绝并发派发"));
            }
            Some(_) => {}
            None => return Err(format!("目标 agent {target} 不存在（可能已被清理）")),
        }
    }
    let new_exec = NewExec {
        exec_id: exec_id.clone(),
        node_id: Some(node_id.to_string()),
        run_id: Some(run_id.clone()),
        binary_path: Some(plan.program.display().to_string()),
        binary_args: sanitize_args(&args),
        model: req.model.map(str::to_string),
    };
    let registered = match req.target_agent_id {
        Some(target) => agents::append_exec(runtime_dir, home_id, target, req.harness, new_exec),
        None => agents::register_agent(runtime_dir, home_id, req.harness, new_exec),
    };
    let agent = match registered {
        Ok(agent) => agent,
        Err(err) => {
            compensate_new_run(rpc, home_id, &run_id, run_was_newly_created);
            return Err(err);
        }
    };

    // ⑥ 绑定（失败只警告）
    let mut warnings: Vec<String> = agents::write_binding(runtime_dir, home_id, node_id, &agent.id)
        .err()
        .map(|e| format!("绑定写入失败：{e}"))
        .into_iter()
        .collect();

    let fail_and_compensate = |err: String| -> String {
        let _ = agents::update_exec(runtime_dir, home_id, &agent.id, &exec_id, |e| {
            if e.failure_detail.is_none() {
                e.failure_detail = Some(err.clone());
            }
        });
        let _ = agents::update_agent_status(
            runtime_dir,
            home_id,
            &agent.id,
            AgentRecordStatus::Failed,
            AgentAttachment::None,
        );
        compensate_new_run(rpc, home_id, &run_id, run_was_newly_created);
        err
    };

    // ⑦ spec 落盘
    let spec = LaunchSpec {
        schema_version: SPEC_SCHEMA_VERSION,
        home_id: home_id.to_string(),
        home_path: req.home_path.to_path_buf(),
        agent_id: agent.id.clone(),
        exec_id: exec_id.clone(),
        harness: req.harness.to_string(),
        node_id: Some(node_id.to_string()),
        run_id: Some(run_id.clone()),
        unattended: req.unattended,
        model: req.model.map(str::to_string),
        resume_session: req.resume_session.map(str::to_string),
        prompt,
        max_duration_secs: req.max_duration.as_secs(),
        omt_path: req.omt_bin.to_path_buf(),
    };
    if let Err(err) = spec.write(&layout::exec_file(&agent_dir, &exec_id, ExecFile::Spec)) {
        return Err(fail_and_compensate(err));
    }

    // ⑧ 拉起 shim 并记录其身份
    let shim_log = layout::exec_file(&agent_dir, &exec_id, ExecFile::ShimLog);
    let shim_pid = match spawn_shim(
        req.omt_bin,
        runtime_dir,
        home_id,
        &agent.id,
        &exec_id,
        &shim_log,
    ) {
        Ok(pid) => pid,
        Err(err) => return Err(fail_and_compensate(err)),
    };
    let shim_start_id = crate::executor::process_start_identity(shim_pid).ok();
    let _ = agents::update_exec(runtime_dir, home_id, &agent.id, &exec_id, |e| {
        e.shim_pid = Some(shim_pid);
        e.shim_start_id.clone_from(&shim_start_id);
    });

    // ⑨ 就绪握手（P-5）
    match wait_ready(runtime_dir, home_id, &agent.id, &exec_id, &shim_log) {
        Ready::Spawned => {}
        Ready::Failed(detail) => {
            compensate_new_run(rpc, home_id, &run_id, run_was_newly_created);
            return Err(detail);
        }
        Ready::Timeout => warnings.push(format!(
            "exec-shim {}s 内未拉起 harness，执行仍在进行——稍后在 Agents 面板查看",
            READY_TIMEOUT.as_secs()
        )),
    }

    Ok(ExecStartOutcome {
        exec_id,
        agent_id: agent.id,
        run_id,
        warnings,
    })
}

fn wait_ready(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    shim_log: &Path,
) -> Ready {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Ok(Some(agent)) = agents::get_agent(runtime_dir, home_id, agent_id) {
            if let Some(exec) = agent.execs.iter().find(|e| e.exec_id == exec_id) {
                if exec.pid.is_some() {
                    return Ready::Spawned;
                }
                if agent.status == AgentRecordStatus::Failed {
                    return Ready::Failed(
                        exec.failure_detail
                            .clone()
                            .unwrap_or_else(|| "exec-shim 启动失败".into()),
                    );
                }
                if exec.shim_pid.is_some() && !crate::control::shim_alive(exec) {
                    return Ready::Failed(format!(
                        "exec-shim 意外退出，详见 {}",
                        shim_log.display()
                    ));
                }
            }
        }
        if Instant::now() >= deadline {
            return Ready::Timeout;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 以新会话（setsid）拉起 shim；stdout/stderr 写 shim.log。发起方存活时由
/// 收割线程 wait，避免僵尸；发起方先退出则由 init/launchd 收割。
pub fn spawn_shim(
    omt_bin: &Path,
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    log_path: &Path,
) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode_if_unix(0o600)
        .open(log_path)
        .map_err(|e| format!("open shim log: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("dup shim log: {e}"))?;
    let mut cmd = Command::new(omt_bin);
    cmd.arg("--runtime-dir")
        .arg(runtime_dir)
        .args(["exec-shim", home_id, agent_id, exec_id])
        .env("OMT_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // SAFETY: setsid 只在 fork 与 exec 之间调用
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn exec-shim: {e}"))?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}
