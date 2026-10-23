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
use crate::executor::{dispatch_daemon_side, fail_and_compensate, DaemonRpc, ExecStartOutcome};
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
    /// `.omt` 目录（Q-3：CLI/MCP 发起时取自 daemon 的 homeId→path 映射）
    pub home_path: &'a Path,
    /// Q-9：派发目标（取代原 node_id）
    pub target: crate::executor::LaunchTarget<'a>,
    /// Q-2 / Q-4：发起方；非 Desktop 时跳过 node/execute 并受 policy 约束（A3）
    pub origin: crate::policy::LaunchOrigin,
    pub harness: &'a str,
    pub unattended: bool,
    pub model: Option<&'a str>,
    pub resume_session: Option<&'a str>,
    pub target_agent_id: Option<&'a str>,
    /// Q-7：父 agent（仅新建 agent 时写入）
    pub parent_agent_id: Option<&'a str>,
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
        req.target,
        req.origin,
        req.harness,
        req.unattended,
        req.model,
        req.resume_session,
        req.target_agent_id,
        req.parent_agent_id,
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
        node_id: req.target.node_id().map(str::to_string),
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
    // F-EXEC-2：身份落档改为关键步骤——读取失败（重试一次，覆盖 spawn 与
    // exec 间的瞬时窗口）或写失败都视同 ⑧ 失败：SIGTERM 刚拉起的 shim（若
    // 信号早于 shim 安装处理函数，默认终止语义同样达成；shim 已自行退出的
    // ESRCH 按已停止处理），走与 spawn 失败相同的补偿表。记录不了身份的
    // shim 无法被停止 / 存活检查安全追踪，只能不留它。shim 自己的第一动作
    // 也会写同一笔（自愈），但 launcher 无法区分「shim 已写好」与「记录
    // 损坏」，故本侧失败按失败处理。
    let shim_start_id = match crate::executor::process_start_identity(shim_pid)
        .or_else(|first| crate::executor::process_start_identity(shim_pid).map_err(|_| first))
    {
        Ok(id) => Some(id),
        Err(err) => {
            terminate_shim(shim_pid);
            return Err(compensate(format!("读取 exec-shim 起始身份失败: {err}")));
        }
    };
    if let Err(err) =
        agents::update_exec(runtime_dir, home_id, &plan.agent.id, &plan.exec_id, |e| {
            e.shim_pid = Some(shim_pid);
            e.shim_start_id.clone_from(&shim_start_id);
        })
    {
        terminate_shim(shim_pid);
        return Err(compensate(format!("记录 exec-shim 身份失败: {err}")));
    }

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
            // F-EXEC-4：Failed 有两个子来源——shim 的错误臂已把 Agent 落成
            // Failed（failure_detail 在），或 shim 未及收尾就消失（旧版 omt
            // 不认识 exec-shim verb、参数解析失败、panic——后者无人写终态）。
            // 统一走 fail_and_compensate：幂等——已 Failed 时 failure_detail
            // 不覆盖、状态无变化不落盘（与原先「只补 run」行为等价）；未
            // Failed 时补齐终态 + cancel 本次新建 run。
            return Err(compensate(detail));
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

/// F-EXEC-2(2)：就绪握手里的「shim 已死」判定——shim_pid 已记**且起始身份
/// 也在**、但身份对不上，才算死；起始身份缺失（拉起时读取失败 / 旧记录）
/// 按「未知身份」处理，不算死（继续等到超时）。否则启动竞态 B：wait_ready
/// 的首个 poll 就把 start_id None 的健康 shim 判死，取消一次实际还在跑的
/// 执行。与 control 侧的保守判定（identity_alive 对 None 恒 false——发信号
/// 前必须确知身份）方向相反是有意的：这里是「不要误杀」，那里是「不要误发
/// 信号」。
fn shim_reported_dead(exec: &crate::agents::ExecRecord) -> bool {
    exec.shim_pid.is_some() && exec.shim_start_id.is_some() && !crate::control::shim_alive(exec)
}

/// F-EXEC-2：wait_ready 的等待上限。生产缺省 30s；环境变量
/// `OMT_LAUNCH_READY_TIMEOUT_MS` 覆盖（测试用，镜像 shim 侧
/// `OMT_SHIM_LEASE_TIMEOUT_MS` 的既有 seam 形态）。
fn ready_timeout() -> Duration {
    std::env::var("OMT_LAUNCH_READY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(READY_TIMEOUT)
}

fn wait_ready(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    shim_log: &Path,
) -> Ready {
    let deadline = Instant::now() + ready_timeout();
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
                if shim_reported_dead(exec) {
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

/// F-EXEC-2：⑧ 身份落档失败时终止刚拉起的 shim（与 spawn 失败同一语义）。
/// ESRCH = 它已自行退出（幂等，按已停止处理）；其余错误只能记日志——补偿表
/// 仍照走。
fn terminate_shim(pid: u32) {
    let Ok(pid_i) = i32::try_from(pid) else {
        return;
    };
    // SAFETY: 仅发送信号；pid 是本进程刚 spawn 的 shim 子进程
    let rc = unsafe { libc::kill(pid_i, libc::SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            eprintln!("launcher: 终止 exec-shim（pid {pid}）失败: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{self, NewExec};

    fn register(runtime: &Path, exec_id: &str) -> agents::AgentRecord {
        agents::register_agent(
            runtime,
            "h_t",
            "pi",
            NewExec {
                exec_id: exec_id.into(),
                node_id: None,
                run_id: Some("RUN-1".into()),
                binary_path: None,
                binary_args: vec![],
                model: None,
                launched_by: None,
                parent_agent_id: None,
            },
        )
        .unwrap()
    }

    /// F-EXEC-2(2)：就绪握手不得把「shim_pid 已记但起始身份缺失」判成 shim
    /// 已死（场景 B：拉起时身份读取失败——start_id None 的首个 poll 会因
    /// identity_alive 恒 false 判死，把还在启动的健康执行误取消，而 shim 本体
    /// 继续跑）。身份缺失的记录配一个真存活的 sleep 进程冒充 shim：身份缺失
    /// 应按「未知」处理——继续等直到超时（Timeout），而不是 Failed。
    /// `OMT_LAUNCH_READY_TIMEOUT_MS` 把等待压到测试量级（生产缺省 30s 不变）。
    #[test]
    fn wait_ready_treats_missing_shim_start_id_as_unknown_not_dead() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path();
        let agent = register(runtime, "exec-nostart");
        let mut shim = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        agents::update_exec(runtime, "h_t", &agent.id, "exec-nostart", |e| {
            e.shim_pid = Some(shim.id());
            e.shim_start_id = None; // 场景 B：身份缺失，但 shim 本体存活
        })
        .unwrap();
        let log = runtime.join("shim.log");
        std::env::set_var("OMT_LAUNCH_READY_TIMEOUT_MS", "300");
        let ready = wait_ready(runtime, "h_t", &agent.id, "exec-nostart", &log);
        std::env::remove_var("OMT_LAUNCH_READY_TIMEOUT_MS");
        assert!(
            matches!(ready, Ready::Timeout),
            "身份缺失应视为未知（继续等到超时），而非误报 shim 已死"
        );
        // 清理冒充 shim 的 sleep 进程
        unsafe {
            libc::kill(shim.id() as i32, libc::SIGKILL);
        }
        let _ = shim.wait();
    }

    /// F-EXEC-2(2) 的判定核心（shim_reported_dead）：起始身份在且不匹配 =
    /// 已死；身份缺失 = 未知（不算死）；身份在且匹配 = 存活；未记 pid =
    /// 尚在拉起（不算死）。
    #[test]
    fn shim_reported_dead_requires_start_identity() {
        let me = std::process::id();
        let my_start = crate::executor::process_start_identity(me).unwrap();
        let dead = agents::ExecRecord {
            shim_pid: Some(me),
            shim_start_id: Some("macos:0:0".into()), // 身份不匹配 → 已死
            ..Default::default()
        };
        assert!(shim_reported_dead(&dead));
        let unknown = agents::ExecRecord {
            shim_pid: Some(me),
            shim_start_id: None, // 身份缺失 → 未知，不算死
            ..Default::default()
        };
        assert!(!shim_reported_dead(&unknown));
        let alive = agents::ExecRecord {
            shim_pid: Some(me),
            shim_start_id: Some(my_start), // 本测试进程自身 → 存活
            ..Default::default()
        };
        assert!(!shim_reported_dead(&alive));
        let untracked = agents::ExecRecord::default(); // 尚未记录 shim pid
        assert!(!shim_reported_dead(&untracked));
    }
}
