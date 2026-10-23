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
    /// §41：harness 拉起前 agent 已被停止（Killed）——只补偿 run，不落 Failed
    Stopped,
    Timeout,
}

const LAUNCH_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// A3：policy 拒绝 / 无效时的错误文本前缀；LocalImpl 经
/// problem_from_launch_error 还原为结构化 Problem。
fn policy_err(problem: omt_domain::error::Problem) -> String {
    format!("{}:{}", problem.code, problem.message)
}

fn denial_err(d: crate::policy::PolicyDenial) -> String {
    format!("POLICY_DENIED:{}:{}", d.field, d.reason)
}

/// `"CODE:field:message"` / `"CODE:message"` / 其他 → ("LAUNCH_FAILED", 原文, None)。
pub fn problem_from_launch_error(err: &str) -> (&'static str, String, Option<String>) {
    if let Some(rest) = err.strip_prefix("POLICY_DENIED:") {
        if let Some((field, message)) = rest.split_once(':') {
            return (
                "POLICY_DENIED",
                message.to_string(),
                Some(field.to_string()),
            );
        }
    }
    if let Some(rest) = err.strip_prefix("POLICY_INVALID:") {
        return ("POLICY_INVALID", rest.to_string(), None);
    }
    ("LAUNCH_FAILED", err.to_string(), None)
}

pub fn start(rpc: DaemonRpc<'_>, req: &LaunchRequest<'_>) -> Result<ExecStartOutcome, String> {
    if let Some(model) = req.model {
        harness::validate_model_id(model)?;
    }
    if let Some(session) = req.resume_session {
        harness::validate_model_id(session)?;
    }
    let runtime_dir = req.runtime_dir;
    let home_id = req.home_id;
    // Q-4：policy 只约束 CLI/MCP；先于任何 daemon 调用判定（被拒不留痕）
    let policy = match req.origin {
        crate::policy::LaunchOrigin::Desktop => None,
        _ => {
            let policy = crate::policy::load(runtime_dir).map_err(policy_err)?;
            crate::policy::check_launch(&policy, req.origin, req.harness).map_err(denial_err)?;
            Some(policy)
        }
    };
    let max_duration = match &policy {
        Some(p) => crate::policy::effective_max_duration(p, Some(req.max_duration)),
        None => req.max_duration,
    };
    // Q-6：跨进程派发锁——并发计数与登记、spawn_shim、身份记录同在锁内；
    // wait_ready 之前释放（§39：锁不再覆盖 30s 就绪等待）
    let launch_lock = omt_harness::atomic_json::lock_exclusive(
        &layout::launch_lock_file(runtime_dir),
        LAUNCH_LOCK_TIMEOUT,
    )?;
    if let Some(p) = &policy {
        crate::policy::check_capacity(p, crate::control::active_exec_count(runtime_dir))
            .map_err(denial_err)?;
    }

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
        max_duration_secs: max_duration.as_secs(),
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
    if let Err(err) = record_shim_identity(
        runtime_dir,
        home_id,
        &plan.agent.id,
        &plan.exec_id,
        shim_pid,
        &crate::executor::process_start_identity,
    ) {
        return Err(compensate(err));
    }

    // Q-6：⑧ 已完成（身份落档成功）——锁在此释放，wait_ready 的 30s 就绪
    // 等待不再占用派发锁（§39）。
    drop(launch_lock);

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
        Ready::Stopped => {
            compensate_new_run(rpc, home_id, &plan.run_id, plan.run_was_newly_created);
            return Err("执行在 harness 拉起前已被停止".into());
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

/// ⑧ 后半（F-EXEC-2 / §41）：记录 shim 身份。读取起始身份失败（重试一次，
/// 覆盖 spawn 与 exec 之间的瞬时窗口）或写不进记录，都 SIGTERM 刚拉起的
/// shim 并返回 Err，由调用方走补偿表。`update_exec` 得 Ok(false) 有两种
/// 含义：shim 已自写了同一身份（成功），或记录不存在（失败）——落档后复核
/// 区分。`read_identity` 是测试缝（生产 = `executor::process_start_identity`）。
fn record_shim_identity(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    shim_pid: u32,
    read_identity: &dyn Fn(u32) -> Result<String, String>,
) -> Result<(), String> {
    let start_id =
        match read_identity(shim_pid).or_else(|first| read_identity(shim_pid).map_err(|_| first)) {
            Ok(id) => id,
            Err(err) => {
                terminate_shim(shim_pid);
                return Err(format!("读取 exec-shim 起始身份失败: {err}"));
            }
        };
    let written = agents::update_exec(runtime_dir, home_id, agent_id, exec_id, |e| {
        e.shim_pid = Some(shim_pid);
        e.shim_start_id = Some(start_id.clone());
    });
    let recorded = match written {
        Ok(true) => true,
        Ok(false) => agents::get_agent(runtime_dir, home_id, agent_id)
            .ok()
            .flatten()
            .and_then(|agent| agent.execs.into_iter().find(|e| e.exec_id == exec_id))
            .is_some_and(|e| {
                e.shim_pid == Some(shim_pid)
                    && e.shim_start_id.as_deref() == Some(start_id.as_str())
            }),
        Err(_) => false,
    };
    if !recorded {
        terminate_shim(shim_pid);
        let detail = match written {
            Err(err) => err,
            _ => "exec 记录不存在".to_string(),
        };
        return Err(format!("记录 exec-shim 身份失败: {detail}"));
    }
    Ok(())
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
                // §41：拉起前被停止（shim 落了 Killed、没有 pid）
                if agent.status == AgentRecordStatus::Killed && exec.pid.is_none() {
                    return Ready::Stopped;
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

    fn sleeper() -> std::process::Child {
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap()
    }

    fn terminated_by_sigterm(child: &mut std::process::Child) -> bool {
        use std::os::unix::process::ExitStatusExt;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status.signal() == Some(libc::SIGTERM);
            }
            if Instant::now() >= deadline {
                unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
                let _ = child.wait();
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// §41（F-EXEC-2 的补偿分支）：读 shim 起始身份失败（含重试一次）→
    /// SIGTERM 刚拉起的 shim 并返回 Err（调用方随即走 fail_and_compensate）。
    #[test]
    fn identity_read_failure_terminates_the_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let agent = register(tmp.path(), "exec-idfail");
        let mut shim = sleeper();
        let failing = |_: u32| -> Result<String, String> { Err("proc_pidinfo: EPERM".into()) };
        let err = record_shim_identity(
            tmp.path(),
            "h_t",
            &agent.id,
            "exec-idfail",
            shim.id(),
            &failing,
        )
        .expect_err("身份读不出必须失败");
        assert!(err.contains("读取 exec-shim 起始身份失败"), "{err}");
        assert!(terminated_by_sigterm(&mut shim), "shim 必须被 SIGTERM");
    }

    /// §41：身份写不进记录（记录已不存在——update_exec 得 Ok(false)）同样
    /// SIGTERM shim 并返回 Err；shim 已自写了同一身份（Ok(false) 但记录一致）
    /// 则视为成功、不发信号。
    #[test]
    fn identity_write_failure_terminates_the_shim_but_self_written_identity_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let mut shim = sleeper();
        let real = |pid: u32| crate::executor::process_start_identity(pid);
        let err = record_shim_identity(
            tmp.path(),
            "h_t",
            "agent-gone",
            "exec-gone",
            shim.id(),
            &real,
        )
        .expect_err("记录不存在必须失败");
        assert!(err.contains("记录 exec-shim 身份失败"), "{err}");
        assert!(terminated_by_sigterm(&mut shim));

        let agent = register(tmp.path(), "exec-self");
        let mut shim = sleeper();
        let start = crate::executor::process_start_identity(shim.id()).unwrap();
        agents::update_exec(tmp.path(), "h_t", &agent.id, "exec-self", |e| {
            e.shim_pid = Some(shim.id());
            e.shim_start_id = Some(start.clone());
        })
        .unwrap();
        record_shim_identity(tmp.path(), "h_t", &agent.id, "exec-self", shim.id(), &real)
            .expect("shim 已自写同一身份 = 成功");
        assert!(shim.try_wait().unwrap().is_none(), "成功路径不得发信号");
        unsafe { libc::kill(shim.id() as i32, libc::SIGKILL) };
        let _ = shim.wait();
    }
}

#[cfg(test)]
mod policy_gate_tests {
    use super::*;
    use crate::policy::{ExecPolicy, LaunchOrigin};

    fn req<'a>(
        rt: &'a Path,
        home: &'a Path,
        bin: &'a Path,
        origin: LaunchOrigin,
    ) -> LaunchRequest<'a> {
        LaunchRequest {
            runtime_dir: rt,
            omt_bin: bin,
            home_id: "h_test",
            home_path: home,
            target: crate::executor::LaunchTarget::Node("t1"),
            origin,
            harness: "pi",
            unattended: true,
            model: None,
            resume_session: None,
            target_agent_id: None,
            parent_agent_id: None,
            max_duration: crate::executor::DEFAULT_MAX_DURATION,
        }
    }

    /// MCP 默认被拒，且被拒时不发出任何 daemon 调用。
    #[test]
    fn mcp_origin_denied_by_default_before_any_rpc() {
        let tmp = tempfile::tempdir().unwrap();
        let calls = std::sync::Mutex::new(Vec::<String>::new());
        let rpc = |m: &str, _p: serde_json::Value| {
            calls.lock().unwrap().push(m.to_string());
            Err::<serde_json::Value, String>("unreachable".into())
        };
        let bin = tmp.path().join("omt");
        let err = start(&rpc, &req(tmp.path(), tmp.path(), &bin, LaunchOrigin::Mcp)).unwrap_err();
        assert!(err.starts_with("POLICY_DENIED:"), "{err}");
        assert!(err.contains("allowFromMcp"), "{err}");
        assert!(calls.lock().unwrap().is_empty());
    }

    /// 无效 policy fail closed。
    #[test]
    fn invalid_policy_fails_closed_for_cli() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::layout::exec_dir(tmp.path())).unwrap();
        std::fs::write(crate::layout::policy_file(tmp.path()), "{bad").unwrap();
        let rpc = |_m: &str, _p: serde_json::Value| Ok(serde_json::json!({}));
        let bin = tmp.path().join("omt");
        let err = start(&rpc, &req(tmp.path(), tmp.path(), &bin, LaunchOrigin::Cli)).unwrap_err();
        assert!(err.starts_with("POLICY_INVALID:"), "{err}");
    }

    /// 桌面端豁免：policy 拒绝 CLI/MCP 时桌面端仍走原链（这里以 rpc 第一次被调用为证）。
    #[test]
    fn desktop_origin_ignores_policy() {
        let tmp = tempfile::tempdir().unwrap();
        crate::policy::save(
            tmp.path(),
            &ExecPolicy {
                allow_from_cli: false,
                ..ExecPolicy::default()
            },
        )
        .unwrap();
        let first = std::sync::Mutex::new(None::<String>);
        let rpc = |m: &str, _p: serde_json::Value| {
            first.lock().unwrap().get_or_insert(m.to_string());
            Err::<serde_json::Value, String>("stop here".into())
        };
        let bin = tmp.path().join("omt");
        let err = start(
            &rpc,
            &req(tmp.path(), tmp.path(), &bin, LaunchOrigin::Desktop),
        )
        .unwrap_err();
        assert_eq!(err, "stop here");
        assert_eq!(first.lock().unwrap().as_deref(), Some("node/get"));
    }

    #[test]
    fn launch_error_round_trips_to_problem_parts() {
        let (code, message, field) =
            problem_from_launch_error("POLICY_DENIED:maxConcurrent:已有 4 个运行中的执行");
        assert_eq!(code, "POLICY_DENIED");
        assert_eq!(field.as_deref(), Some("maxConcurrent"));
        assert_eq!(message, "已有 4 个运行中的执行");
        let (code, _, field) = problem_from_launch_error("无未完成可执行成员");
        assert_eq!(code, "LAUNCH_FAILED");
        assert!(field.is_none());
    }
}
