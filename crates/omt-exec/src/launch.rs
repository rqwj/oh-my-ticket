//! launcher（设计 §3.2「启动」+ 计划 P-3 / P-5）：发起方进程内执行派发链的
//! daemon 侧步骤（①–⑥ 由 `executor::dispatch_daemon_side` 提供，含补偿
//! 表），写 spec，以 setsid 拉起 `omt exec-shim`，等 shim 报告就绪后返回。
//! 发起方此后可以退出，执行继续。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use omt_harness::atomic_json::OpenOptionsUnix;
use omt_harness::harness;

use crate::agents::{self, AgentRecordStatus};
use crate::executor::{
    compensate_new_run, dispatch_daemon_side, fail_and_compensate, DaemonRpc, ExecStartOutcome,
};
use crate::layout::{self, ExecFile};
use crate::spec::{LaunchSpec, SPEC_SCHEMA_VERSION};

/// 进程内串行化派发（预检 + 建链，闭合读侧 TOCTOU §3.3；跨进程由子项目 2
/// 处理）。
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
    let runtime_dir = req.runtime_dir;
    let home_id = req.home_id;

    // ①–⑥ daemon 侧派发（executor.rs）：预检 → ensure-run → 注册/续接 →
    // 绑定，含补偿表。失败同步返回，不留半链状态。
    let plan = dispatch_daemon_side(
        rpc,
        runtime_dir,
        home_id,
        req.node_id,
        req.harness,
        req.unattended,
        req.model,
        req.resume_session,
        req.target_agent_id,
    )?;
    // ⑤ 之后失败的补偿序列（补偿表：failure_detail 落档 + Agent 转 Failed +
    // 仅 cancel 本次新建 run）——⑦⑧ 两个失败点共用。
    let compensate = |err: String| {
        fail_and_compensate(
            rpc,
            runtime_dir,
            home_id,
            &plan.agent.id,
            &plan.exec_id,
            &plan.run_id,
            plan.run_was_newly_created,
            err,
        )
    };

    // ⑦ spec 落盘（shim 按它重算 argv/env——P-4）
    let spec = LaunchSpec {
        schema_version: SPEC_SCHEMA_VERSION,
        home_id: home_id.to_string(),
        home_path: req.home_path.to_path_buf(),
        agent_id: plan.agent.id.clone(),
        exec_id: plan.exec_id.clone(),
        harness: req.harness.to_string(),
        node_id: Some(req.node_id.to_string()),
        run_id: Some(plan.run_id.clone()),
        unattended: req.unattended,
        model: req.model.map(str::to_string),
        resume_session: req.resume_session.map(str::to_string),
        prompt: plan.prompt,
        max_duration_secs: req.max_duration.as_secs(),
        omt_path: req.omt_bin.to_path_buf(),
    };
    if let Err(err) = spec.write(&layout::exec_file(
        &plan.agent_dir,
        &plan.exec_id,
        ExecFile::Spec,
    )) {
        return Err(compensate(err));
    }

    // ⑧ 拉起 shim 并记录其身份
    let shim_log = layout::exec_file(&plan.agent_dir, &plan.exec_id, ExecFile::ShimLog);
    let shim_pid = match spawn_shim(
        req.omt_bin,
        runtime_dir,
        home_id,
        &plan.agent.id,
        &plan.exec_id,
        &shim_log,
    ) {
        Ok(pid) => pid,
        Err(err) => return Err(compensate(err)),
    };
    let shim_start_id = crate::executor::process_start_identity(shim_pid).ok();
    let _ = agents::update_exec(runtime_dir, home_id, &plan.agent.id, &plan.exec_id, |e| {
        e.shim_pid = Some(shim_pid);
        e.shim_start_id.clone_from(&shim_start_id);
    });

    // ⑨ 就绪握手（P-5）
    let mut warnings = plan.warnings;
    match wait_ready(
        runtime_dir,
        home_id,
        &plan.agent.id,
        &plan.exec_id,
        &shim_log,
    ) {
        Ready::Spawned => {}
        Ready::Failed(detail) => {
            // shim 自己已把 Agent 落成 Failed（failure_detail 在），这里只补
            // 偿 run——不重复走 fail_and_compensate。
            compensate_new_run(rpc, home_id, &plan.run_id, plan.run_was_newly_created);
            return Err(detail);
        }
        Ready::Timeout => warnings.push(format!(
            "exec-shim {}s 内未拉起 harness，执行仍在进行——稍后在 Agents 面板查看",
            READY_TIMEOUT.as_secs()
        )),
    }

    Ok(ExecStartOutcome {
        exec_id: plan.exec_id,
        agent_id: plan.agent.id,
        run_id: plan.run_id,
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
