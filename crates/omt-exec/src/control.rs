//! 执行面控制（设计 §3.2「控制」）：定位 exec、存活判定（pid + 内核起始
//! 标识，防复用）、停止（SIGTERM → shim）、启动对账（P-8，替代 demote）。

use std::path::Path;
use std::time::Duration;

use crate::agents::{self, AgentAttachment, AgentRecord, AgentRecordStatus, ExecRecord};
use crate::layout;

/// 还没记下 shim pid 的 exec 在注册后多久内视为「可能正在拉起」。
const LAUNCH_GRACE: Duration = Duration::from_secs(60);

pub struct ExecLocation {
    pub home_id: String,
    pub agent: AgentRecord,
    pub exec: ExecRecord,
}

/// 全部 home 中按 exec id 定位。
pub fn find_exec(runtime_dir: &Path, exec_id: &str) -> Result<Option<ExecLocation>, String> {
    for home_id in layout::home_ids(runtime_dir) {
        if let Some((agent, exec)) = agents::find_exec_in_home(runtime_dir, &home_id, exec_id)? {
            return Ok(Some(ExecLocation {
                home_id,
                agent,
                exec,
            }));
        }
    }
    Ok(None)
}

/// pid 与内核起始标识都对得上才算同一个活进程。
pub fn identity_alive(pid: Option<u32>, start_id: Option<&str>) -> bool {
    match (pid, start_id) {
        (Some(pid), Some(expected)) => crate::executor::process_start_identity(pid)
            .map(|actual| actual == expected)
            .unwrap_or(false),
        _ => false,
    }
}

pub fn shim_alive(exec: &ExecRecord) -> bool {
    identity_alive(exec.shim_pid, exec.shim_start_id.as_deref())
}

/// §45/§47 double-release-false-negative：对账「是否跳过」用的保守判定——
/// 宁可把死 shim 当活的（目标留到下一轮），也不把活 shim 当死的（双重
/// 释放 + 陈旧快照的 admin report 误伤别的 exec 刚认领的 item）。与
/// `shim_alive`（发信号前必须确知身份，未知 = 否）方向相反：
/// - 无 shim_pid → 否；
/// - 身份可读：匹配 → 是，不匹配 → 否（确已换人，pid 复用不算假阴性）；
/// - start-id 缺失或 proc_pidinfo 读不出 → pid 仍存在即按「未知 → 存活」。
pub fn shim_maybe_alive(exec: &ExecRecord) -> bool {
    let Some(pid) = exec.shim_pid else {
        return false;
    };
    let pid_alive = || omt_client::pid_live(i64::from(pid));
    match exec.shim_start_id.as_deref() {
        None => pid_alive(),
        Some(expected) => match crate::executor::process_start_identity(pid) {
            Ok(actual) => actual == expected,
            Err(_) => pid_alive(),
        },
    }
}

fn is_active(agent: &AgentRecord) -> bool {
    matches!(
        agent.status,
        AgentRecordStatus::Starting | AgentRecordStatus::Running
    )
}

fn latest_exec(agent: &AgentRecord) -> Option<&ExecRecord> {
    let last = agent.last_exec_id.as_deref()?;
    agent.execs.iter().find(|e| e.exec_id == last)
}

fn within_launch_grace(exec: &ExecRecord) -> bool {
    let now = agents::now_epoch_ms().parse::<u128>().unwrap_or(0);
    let started = exec.started_at.parse::<u128>().unwrap_or(0);
    exec.shim_pid.is_none() && now.saturating_sub(started) < LAUNCH_GRACE.as_millis()
}

/// 尾随方用：exec 是否仍可能产出事件。review F9：只要 shim 还活着就算活跃，
/// 不看 agent 状态。shim 收尾时先写终态状态，再释放租约（连接 daemon、临时
/// 授权，可能耗时数秒），最后才追加 exit 行；按状态判断会让尾随方在 exit 行
/// 写出之前就放弃。
pub fn exec_active(runtime_dir: &Path, home_id: &str, agent_id: &str, exec_id: &str) -> bool {
    let Ok(Some(agent)) = agents::get_agent(runtime_dir, home_id, agent_id) else {
        return false;
    };
    let Some(exec) = agent.execs.iter().find(|e| e.exec_id == exec_id) else {
        return false;
    };
    shim_alive(exec) || (is_active(&agent) && within_launch_grace(exec))
}

/// review F6：退出确认用——所有 home 中仍在运行的 exec 数（shim 存活，或仍在
/// 启动宽限期内）。
pub fn active_exec_count(runtime_dir: &Path) -> usize {
    layout::home_ids(runtime_dir)
        .iter()
        .map(|home_id| {
            agents::list_agents(runtime_dir, home_id)
                .map(|(agents, _)| {
                    agents
                        .iter()
                        .filter(|a| is_active(a))
                        .filter(|a| {
                            latest_exec(a).is_some_and(|e| shim_alive(e) || within_launch_grace(e))
                        })
                        .count()
                })
                .unwrap_or(0)
        })
        .sum()
}

/// MCP 残留清扫用：launcher id（= exec id）对应的 exec 是否仍在运行。
pub fn exec_id_live(runtime_dir: &Path, exec_id: &str) -> bool {
    matches!(find_exec(runtime_dir, exec_id), Ok(Some(loc))
        if is_active(&loc.agent) && (shim_alive(&loc.exec) || within_launch_grace(&loc.exec)))
}

/// 向 shim 发 SIGTERM（F-EXEC-3 抽出的信号原语）。ESRCH = shim 在存活检查
/// 与发信号之间死亡——目标已停止，按 stop 的幂等语义视为成功；其余错误
/// （EPERM 等）如实上报。
fn signal_shim(pid: u32) -> Result<(), String> {
    let pid_i = i32::try_from(pid).map_err(|_| "shim pid 越界".to_string())?;
    // SAFETY: 仅发送信号；pid 来自已通过内核起始标识校验的持久化记录
    let rc = unsafe { libc::kill(pid_i, libc::SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(()); // 竞态窗口：目标刚消失——已停止
        }
        return Err(format!("kill({pid_i}, SIGTERM): {err}"));
    }
    Ok(())
}

/// 停止（幂等）：未知 / 已终态 → Ok；shim 存活 → SIGTERM；shim 已死 → 对账收尾。
pub fn stop_exec(runtime_dir: &Path, exec_id: &str) -> Result<(), String> {
    stop_exec_signaled(runtime_dir, exec_id).map(|_| ())
}

/// stop_exec 的内芯：Ok(true) = 已向目标的 shim 发出停止（含 ESRCH 竞态——
/// 目标在存活检查与信号之间消失，同样算已停止）；Ok(false) = 无需发信号
/// （未知 exec / 非活跃或非最新 / shim 已死走对账收尾——没有信号发出，
/// 不计入 stop_all 的 signaled_count）。
fn stop_exec_signaled(runtime_dir: &Path, exec_id: &str) -> Result<bool, String> {
    let Some(loc) = find_exec(runtime_dir, exec_id)? else {
        return Ok(false);
    };
    if !is_active(&loc.agent) || loc.agent.last_exec_id.as_deref() != Some(exec_id) {
        return Ok(false);
    }
    if shim_alive(&loc.exec) {
        signal_shim(loc.exec.shim_pid.unwrap_or(0))?;
        return Ok(true);
    }
    // §39 grace-stop-noop：拉起宽限期内（shim 还没记下身份）没有可发信号的
    // 目标，对账也会跳过它——此前直接返回 Ok，界面显示「已停止」而实际什么
    // 也没停
    if within_launch_grace(&loc.exec) {
        return Err("执行仍在拉起中，请稍后再停止".to_string());
    }
    reconcile_agent(runtime_dir, &loc.home_id, &loc.agent);
    Ok(false)
}

/// stop_all 的结果（F-EXEC-3）：实际发出停止信号的数量 + 逐 agent 收集的
/// 错误（单个失败不中止其余）。signaled_count 只计真正发了信号的 exec——
/// ESRCH 竞态（目标刚消失）计入；shim 已死走对账收尾的不计（没有信号
/// 发出，agent 被标 Failed 而非被停止）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StopAllOutcome {
    pub signaled_count: usize,
    pub errors: Vec<String>,
}

/// 停止某 home 的全部活跃 exec（P-10「停止全部」）。F-EXEC-3：逐 agent 处
/// 理——单个失败（记录损坏、信号失败等）收集进 errors 继续下一个，不再
/// 首错即中止。逐 agent 现读持久化记录（get_agent，与 kill_orphan 同一的
/// 身份安全读）：list_agents 对损坏记录是降级跳过，而这里要发信号，必须
/// 显式暴露「记录损坏、无法安全定位进程」的失败。
pub fn stop_all(runtime_dir: &Path, home_id: &str) -> Result<StopAllOutcome, String> {
    stop_all_with(runtime_dir, home_id, |exec| {
        stop_exec_signaled(runtime_dir, &exec.exec_id)
    })
}

/// stop_all 的内芯（F-EXEC-3 pin 的可测缝，§41）：循环本体——错误收集/
/// 继续下一个/只计真实信号——是生产逻辑；逐 agent 的停止操作由 `stop_one`
/// 注入（生产 = stop_exec_signaled；测试注入「身份可验但不可信号」的失败
/// 形态，见 stop_all_continues_after_unsignalable_live_shim_and_signals_the_rest）。
fn stop_all_with(
    runtime_dir: &Path,
    home_id: &str,
    stop_one: impl Fn(&ExecRecord) -> Result<bool, String>,
) -> Result<StopAllOutcome, String> {
    let mut outcome = StopAllOutcome::default();
    for agent_id in layout::agent_ids(runtime_dir, home_id)? {
        let agent = match agents::get_agent(runtime_dir, home_id, &agent_id) {
            Ok(Some(agent)) => agent,
            Ok(None) => continue,
            Err(err) => {
                outcome.errors.push(format!("agent {agent_id}: {err}"));
                continue;
            }
        };
        if !is_active(&agent) {
            continue;
        }
        let Some(exec) = latest_exec(&agent) else {
            continue;
        };
        match stop_one(exec) {
            Ok(signaled) => outcome.signaled_count += usize::from(signaled),
            // P3-b（§43）+ P3-2（§45）：逐 agent 错误必须携带 agent 身份，且
            // 用**展示名**（`Agent N`，与界面行首一致；缺失时回退内部 id）+
            // exec id（精确定位）——退出/停止全部的失败明细只进 stderr/通知
            // 条，内部 id（agent-<execId>）与界面显示名对不上。
            Err(err) => {
                let name = if agent.display_name.is_empty() {
                    &agent.id
                } else {
                    &agent.display_name
                };
                outcome
                    .errors
                    .push(format!("{name} ({}): {err}", exec.exec_id))
            }
        }
    }
    Ok(outcome)
}

/// 单个活跃 agent 的对账（P-8）。返回是否改成了终态。
///
/// §39 reconcile-snapshot-writeback：两个分支都是锁内 compare-and-write——
/// `agent` 是调用方读到的快照，shim 可能在快照之后已落终态。存活分支只在
/// 记录仍活跃时把 attachment 拉回 Live、绝不改 status（此前用快照的 status
/// 回写，可能把 Exited 覆盖回 Running）；已死分支在锁内复核「仍活跃且最新
/// exec 未变」后，才在同一事务里写 failure_detail + Failed + attachment。
fn reconcile_agent(runtime_dir: &Path, home_id: &str, agent: &AgentRecord) -> bool {
    let Some(exec) = latest_exec(agent) else {
        return false;
    };
    let exec_id = exec.exec_id.clone();
    if shim_alive(exec) {
        let _ = agents::update_agent(runtime_dir, home_id, &agent.id, |current| {
            if !is_active(current) || current.attachment == AgentAttachment::Live {
                return false;
            }
            current.attachment = AgentAttachment::Live;
            true
        });
        return false;
    }
    if within_launch_grace(exec) {
        return false;
    }
    let harness_alive = identity_alive(exec.pid, exec.process_start_id.as_deref());
    let attachment = if harness_alive {
        AgentAttachment::Orphan
    } else {
        AgentAttachment::None
    };
    agents::update_agent(runtime_dir, home_id, &agent.id, |current| {
        if !is_active(current) || current.last_exec_id.as_deref() != Some(exec_id.as_str()) {
            return false;
        }
        if let Some(e) = current.execs.iter_mut().find(|e| e.exec_id == exec_id) {
            if e.failure_detail.is_none() {
                e.failure_detail = Some(agents::SHIM_LOST_MARKER.to_string());
            }
        }
        current.status = AgentRecordStatus::Failed;
        current.attachment = attachment;
        true
    })
    .unwrap_or(false)
}

/// 启动对账：替代 demote。返回改为终态的 agent 数。
pub fn reconcile_on_startup(runtime_dir: &Path) -> usize {
    let mut changed = 0;
    for home_id in layout::home_ids(runtime_dir) {
        let Ok((agents, _)) = agents::list_agents(runtime_dir, &home_id) else {
            continue;
        };
        for agent in agents.iter().filter(|a| is_active(a)) {
            if reconcile_agent(runtime_dir, &home_id, agent) {
                changed += 1;
            }
        }
    }
    changed
}

/// F-EXEC-17：尾随方发现 exec 不再活跃、却始终没读到 exit 行（shim 崩溃 /
/// 被 kill -9）时就地收尾——与启动对账同一判定（`reconcile_agent`）把 agent
/// 落成终态（harness 仍活则 Orphan），并合成一条 exit 事件交给前端收尾。
/// 该 exec 已不是 agent 的最新 exec、或 shim 仍活 / 仍在拉起宽限期内时返回
/// None（不越权改写）。租约不在此释放：调用方走 shim 路径的
/// `executor::reconcile_with_connector`（与启动对账同一条）。
pub fn settle_lost_exec(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    next_seq: u64,
) -> Option<serde_json::Value> {
    let agent = agents::get_agent(runtime_dir, home_id, agent_id).ok()??;
    if latest_exec(&agent)?.exec_id != exec_id {
        return None;
    }
    if is_active(&agent) && !reconcile_agent(runtime_dir, home_id, &agent) {
        return None;
    }
    let agent = agents::get_agent(runtime_dir, home_id, agent_id).ok()??;
    let exec = agent.execs.iter().find(|e| e.exec_id == exec_id)?;
    let status = match agent.status {
        AgentRecordStatus::Exited => "exited",
        AgentRecordStatus::Failed => "failed",
        AgentRecordStatus::Killed => "killed",
        _ => return None,
    };
    let now = agents::now_epoch_ms();
    let duration_ms = now
        .parse::<u128>()
        .unwrap_or(0)
        .saturating_sub(exec.started_at.parse::<u128>().unwrap_or(0));
    Some(serde_json::json!({
        "execId": exec_id,
        "seq": next_seq,
        "ts": now,
        "kind": "exit",
        "status": status,
        "code": exec.exit_code,
        "killReason": exec.kill_reason,
        "durationMs": duration_ms as u64,
        "failureDetail": exec.failure_detail,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::agents::{register_agent, update_exec, AgentRecordStatus, NewExec};

    fn reg(runtime: &Path, exec_id: &str) -> String {
        register_agent(
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
        .id
    }

    fn backdate(runtime: &Path, agent_id: &str, exec_id: &str) {
        update_exec(runtime, "h_t", agent_id, exec_id, |e| {
            e.started_at = "1000000000000".into();
        })
        .unwrap();
    }

    /// F-EXEC-17：shim 已死、无 exit 行的 exec 被就地落成 Failed（SHIM_LOST），
    /// 并合成 exit 事件（seq 续接、status=failed）。
    #[test]
    fn settle_lost_exec_marks_failed_and_synthesizes_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        let dead = reg(rt, "exec-lost");
        update_exec(rt, "h_t", &dead, "exec-lost", |e| {
            e.shim_pid = Some(std::process::id());
            e.shim_start_id = Some("macos:0:0".into());
        })
        .unwrap();
        backdate(rt, &dead, "exec-lost");

        let exit = settle_lost_exec(rt, "h_t", &dead, "exec-lost", 7).expect("应合成 exit");
        assert_eq!(exit["kind"], "exit");
        assert_eq!(exit["status"], "failed");
        assert_eq!(exit["seq"], 7);
        assert_eq!(exit["execId"], "exec-lost");
        assert!(exit["durationMs"].is_u64());
        let agent = crate::agents::get_agent(rt, "h_t", &dead).unwrap().unwrap();
        assert_eq!(agent.status, AgentRecordStatus::Failed);
        assert_eq!(
            agent.execs[0].failure_detail.as_deref(),
            Some(crate::agents::SHIM_LOST_MARKER)
        );
        assert!(
            crate::agents::is_reconcile_target(&agent.execs[0]),
            "落成终态后仍是对账目标——租约由 shim 路径对账释放"
        );
    }

    /// 反例：shim 仍活（本进程冒充）→ 不改写、不合成；非最新 exec 同样不动。
    #[test]
    fn settle_lost_exec_leaves_live_or_stale_exec_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        let live = reg(rt, "exec-alive");
        let me = std::process::id();
        let my_start = crate::executor::process_start_identity(me).unwrap();
        update_exec(rt, "h_t", &live, "exec-alive", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some(my_start.clone());
        })
        .unwrap();
        backdate(rt, &live, "exec-alive");
        assert!(settle_lost_exec(rt, "h_t", &live, "exec-alive", 1).is_none());
        assert_eq!(
            crate::agents::get_agent(rt, "h_t", &live)
                .unwrap()
                .unwrap()
                .status,
            AgentRecordStatus::Starting
        );
        assert!(settle_lost_exec(rt, "h_t", &live, "exec-other", 1).is_none());
    }

    #[test]
    fn startup_reconcile_marks_dead_shim_failed_and_keeps_live_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        // 存活 shim：用本测试进程自己的 pid 与起始标识冒充
        let live = reg(rt, "exec-live");
        let me = std::process::id();
        let my_start = crate::executor::process_start_identity(me).unwrap();
        update_exec(rt, "h_t", &live, "exec-live", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some(my_start.clone());
        })
        .unwrap();
        // 已死 shim：起始标识对不上
        let dead = reg(rt, "exec-dead");
        update_exec(rt, "h_t", &dead, "exec-dead", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some("macos:0:0".into());
        })
        .unwrap();
        backdate(rt, &dead, "exec-dead");

        assert_eq!(reconcile_on_startup(rt), 1);
        let a = crate::agents::get_agent(rt, "h_t", &live).unwrap().unwrap();
        assert_eq!(a.status, AgentRecordStatus::Starting);
        assert_eq!(a.attachment, crate::agents::AgentAttachment::Live);
        let d = crate::agents::get_agent(rt, "h_t", &dead).unwrap().unwrap();
        assert_eq!(d.status, AgentRecordStatus::Failed);
        assert_eq!(
            d.execs[0].failure_detail.as_deref(),
            Some(crate::agents::SHIM_LOST_MARKER)
        );
        // 进入 U10 对账目标（释放租约）
        let targets = crate::agents::list_reconcile_targets(rt).unwrap();
        assert_eq!(
            targets
                .iter()
                .map(|t| t.exec_id.as_str())
                .collect::<Vec<_>>(),
            vec!["exec-dead"]
        );
    }

    #[test]
    fn startup_reconcile_spares_fresh_launch_without_shim_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = reg(tmp.path(), "exec-fresh");
        assert_eq!(reconcile_on_startup(tmp.path()), 0);
        let a = crate::agents::get_agent(tmp.path(), "h_t", &fresh)
            .unwrap()
            .unwrap();
        assert_eq!(
            a.status,
            AgentRecordStatus::Starting,
            "可能有别的进程正在拉起，跳过"
        );
    }

    #[test]
    fn stop_exec_is_idempotent_for_unknown_and_finished() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(stop_exec(tmp.path(), "exec-nope").is_ok());
        let id = reg(tmp.path(), "exec-done");
        crate::agents::update_agent_status(
            tmp.path(),
            "h_t",
            &id,
            AgentRecordStatus::Exited,
            crate::agents::AgentAttachment::None,
        )
        .unwrap();
        assert!(stop_exec(tmp.path(), "exec-done").is_ok());
    }

    /// review F9：shim 还活着时，即使状态已是终态，也算活跃（exit 行可能还没写出）。
    #[test]
    fn exec_active_follows_shim_liveness_not_status() {
        let tmp = tempfile::tempdir().unwrap();
        let id = reg(tmp.path(), "exec-tail");
        let me = std::process::id();
        let start = crate::executor::process_start_identity(me).unwrap();
        update_exec(tmp.path(), "h_t", &id, "exec-tail", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some(start.clone());
        })
        .unwrap();
        crate::agents::update_agent_status(
            tmp.path(),
            "h_t",
            &id,
            AgentRecordStatus::Exited,
            crate::agents::AgentAttachment::None,
        )
        .unwrap();
        assert!(exec_active(tmp.path(), "h_t", &id, "exec-tail"));
        assert_eq!(
            active_exec_count(tmp.path()),
            0,
            "终态 agent 不计入退出确认"
        );
        update_exec(tmp.path(), "h_t", &id, "exec-tail", |e| {
            e.shim_start_id = Some("macos:0:0".into());
        })
        .unwrap();
        assert!(
            !exec_active(tmp.path(), "h_t", &id, "exec-tail"),
            "shim 不在且已终态 → 不活跃"
        );
    }

    #[test]
    fn stop_exec_signals_a_live_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let id = reg(tmp.path(), "exec-sig");
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = crate::executor::process_start_identity(pid).unwrap();
        update_exec(tmp.path(), "h_t", &id, "exec-sig", |e| {
            e.shim_pid = Some(pid);
            e.shim_start_id = Some(start.clone());
        })
        .unwrap();
        stop_exec(tmp.path(), "exec-sig").unwrap();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    /// F-EXEC-3：shim 在存活检查与发信号之间死亡 → kill 得 ESRCH → 视为已
    /// 停止（幂等）。spawn+kill+wait 收割后的 pid 不复存在，kill 必得 ESRCH。
    #[test]
    fn signal_shim_maps_esrch_to_success() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        let _ = child.wait(); // 收割——僵尸不复存在，后续 kill 得 ESRCH
        signal_shim(pid).expect("ESRCH 应映射为成功（目标已停止）");
    }

    /// F-EXEC-3：单个 agent 的记录损坏（get_agent 拒绝解析进程身份——发信号
    /// 前必须能安全定位进程）不得中止其余 agent 的停止；计数只含真正发了
    /// 信号的 exec。
    #[test]
    fn stop_all_continues_past_errors_and_counts_only_signaled() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        // agent-a：活跃 + 存活 shim（真 sleep 进程）——应被发信号并计数
        let good = reg(rt, "exec-good");
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let start = crate::executor::process_start_identity(child.id()).unwrap();
        update_exec(rt, "h_t", &good, "exec-good", |e| {
            e.shim_pid = Some(child.id());
            e.shim_start_id = Some(start);
        })
        .unwrap();
        // agent-b：同样活跃，但 agent.json 损坏 → 停止路径报错（不安全，不
        // 得静默跳过——shim 可能在跑）
        let bad = reg(rt, "exec-bad");
        std::fs::write(
            crate::layout::agent_file(rt, "h_t", &bad).unwrap(),
            "{ broken",
        )
        .unwrap();

        let outcome = stop_all(rt, "h_t").unwrap();
        assert_eq!(
            outcome.signaled_count, 1,
            "健康的 agent 仍被停止: {outcome:?}"
        );
        assert_eq!(outcome.errors.len(), 1, "损坏记录报错但不中止: {outcome:?}");
        assert!(outcome.errors[0].contains(&bad), "{}", outcome.errors[0]);

        // 发出的信号确实送达（SIGTERM）
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    /// F-EXEC-3 计数语义：shim 已死的活跃 agent 走对账收尾（标 Failed），
    /// 没有信号发出——不计入 signaled_count。
    #[test]
    fn stop_all_does_not_count_reconciled_dead_shims() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        let dead = reg(rt, "exec-deadshim");
        update_exec(rt, "h_t", &dead, "exec-deadshim", |e| {
            e.shim_pid = Some(u32::MAX);
            e.shim_start_id = Some("macos:0:0".into()); // 身份不匹配 → shim 已死
        })
        .unwrap();
        backdate(rt, &dead, "exec-deadshim");

        let outcome = stop_all(rt, "h_t").unwrap();
        assert_eq!(outcome.signaled_count, 0, "{outcome:?}");
        assert!(outcome.errors.is_empty(), "{outcome:?}");
        let a = crate::agents::get_agent(rt, "h_t", &dead).unwrap().unwrap();
        assert_eq!(a.status, AgentRecordStatus::Failed, "对账收尾仍生效");
    }

    /// F-EXEC-3 的**原始** bug 钉子（§41）：既有 continuation 测试的错误源是
    /// 「记录损坏」这一新分支——旧 `?` 代码用 list_agents（损坏被静默跳过），
    /// 该测试在旧代码同样通过，钉不住「首错即中止」。本测把错误源换成「活的
    /// 已记录 shim 无法发信号」——signal_shim 的 kill 失败（EPERM 等）在旧代
    /// 码同样如实 Err（`stop_exec(..)?` 处中止），只有新代码会继续停止其余。
    ///
    /// 真·「身份可验但不可信号」的进程（pid 1 launchd）在本机不可构造：普通
    /// 用户的 `proc_pidinfo(1, PROC_PIDTBSDINFO)` 得 EPERM（本机运行时验证），
    /// 身份校验先行失败、永远到不了 `kill(1, SIGTERM)` 的 EPERM；root 属主
    /// 进程同样不可读。按 §41 的诚实兜底：逐 agent 停止抽成可测缝
    /// （stop_all_with）——首个 agent 注入与 signal_shim 同形的 kill 失败，
    /// 第二个 agent 走**真实** stop_exec_signaled（真 sleep 进程 + 真身份），
    /// 断言其仍收到 SIGTERM。旧 `?` 形态下首 agent 的 Err 会中止循环、第二
    /// 个 agent 永不被停止——本测即红。
    #[test]
    fn stop_all_continues_after_unsignalable_live_shim_and_signals_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        // agent-a（目录序在前，先被迭代）：活跃 + 存活 shim 记录（pid 1
        // launchd 的形态）。真实世界里 identity_alive 校验通过、kill 得
        // EPERM；本机不可达（见上），经缝注入同形失败。
        let unsignalable = reg(rt, "exec-a-ep");
        update_exec(rt, "h_t", &unsignalable, "exec-a-ep", |e| {
            e.shim_pid = Some(1);
            // 占位身份：可读机器上由 process_start_identity(1) 给出；本测
            // 不经真实身份校验（失败由缝注入），重点在失败后的继续语义。
            e.shim_start_id = Some("macos:1:1".into());
        })
        .unwrap();
        // agent-b：活跃 + 真 sleep 进程的真实身份——必须仍被停止
        let real = reg(rt, "exec-b-sleep");
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let start = crate::executor::process_start_identity(child.id()).unwrap();
        update_exec(rt, "h_t", &real, "exec-b-sleep", |e| {
            e.shim_pid = Some(child.id());
            e.shim_start_id = Some(start);
        })
        .unwrap();

        let outcome = stop_all_with(rt, "h_t", |exec| {
            if exec.exec_id == "exec-a-ep" {
                // 与 signal_shim 的错误同形（kill EPERM 如实上报）——旧代码
                // 在 stop_exec 的 `?` 处即中止
                return Err(format!(
                    "kill(1, SIGTERM): {}",
                    std::io::Error::from_raw_os_error(libc::EPERM)
                ));
            }
            stop_exec_signaled(rt, &exec.exec_id)
        })
        .unwrap();

        assert_eq!(
            outcome.signaled_count, 1,
            "健康 agent 仍被停止: {outcome:?}"
        );
        assert_eq!(
            outcome.errors.len(),
            1,
            "不可信号的失败如实收集: {outcome:?}"
        );
        assert!(
            outcome.errors[0].contains("exec-a-ep"),
            "{}",
            outcome.errors[0]
        );
        // P3-b（§43）+ P3-2（§45）：失败明细必须携带 agent 身份，且用展示名
        // （`Agent N`，register_agent 分配）——界面显示的是展示名，内部 id
        // （agent-<execId>）与界面对不上；exec id 保留作精确定位。
        assert!(
            outcome.errors[0].contains("Agent 1"),
            "失败明细必须含展示名（P3-2）: {}",
            outcome.errors[0]
        );
        assert!(
            outcome.errors[0].contains("exec-a-ep"),
            "失败明细必须保留 exec id（P3-b）: {}",
            outcome.errors[0]
        );
        // 第二个 agent 确实收到 SIGTERM——旧代码（首错即中止）下它永不会被
        // 停止，wait 将一直等不到退出
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(libc::SIGTERM),
            "首个 agent 不可信号后，其余 agent 仍必须被停止"
        );
    }

    /// §39 grace-stop-noop：拉起宽限期内（shim 还没记下 pid）没有可发信号
    /// 的目标——必须如实报错，而不是返回 Ok 让界面显示「已停止」。
    #[test]
    fn stop_during_launch_grace_reports_not_yet_stoppable() {
        let tmp = tempfile::tempdir().unwrap();
        let id = reg(tmp.path(), "exec-fresh");
        let err = stop_exec(tmp.path(), "exec-fresh").expect_err("宽限期内不得假装停止成功");
        assert!(err.contains("仍在拉起中"), "{err}");
        let a = crate::agents::get_agent(tmp.path(), "h_t", &id)
            .unwrap()
            .unwrap();
        assert_eq!(a.status, AgentRecordStatus::Starting, "记录不被改写");
        let outcome = stop_all(tmp.path(), "h_t").unwrap();
        assert_eq!(outcome.signaled_count, 0);
        assert_eq!(outcome.errors.len(), 1, "{outcome:?}");
        assert!(outcome.errors[0].contains("exec-fresh"), "{outcome:?}");
    }

    /// §39 reconcile-snapshot-writeback：shim 存活分支不得用过时快照回写
    /// 状态——快照是 Running、磁盘上 shim 已落 Exited 时，对账后仍是 Exited。
    #[test]
    fn reconcile_alive_branch_does_not_write_back_a_stale_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        let id = reg(rt, "exec-snap");
        let me = std::process::id();
        let start = crate::executor::process_start_identity(me).unwrap();
        update_exec(rt, "h_t", &id, "exec-snap", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some(start.clone());
        })
        .unwrap();
        crate::agents::update_agent_status(
            rt,
            "h_t",
            &id,
            AgentRecordStatus::Running,
            crate::agents::AgentAttachment::Orphan,
        )
        .unwrap();
        let snapshot = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        // 快照之后 shim 收尾写了终态
        crate::agents::update_agent_status(
            rt,
            "h_t",
            &id,
            AgentRecordStatus::Exited,
            crate::agents::AgentAttachment::None,
        )
        .unwrap();
        assert!(!reconcile_agent(rt, "h_t", &snapshot));
        let now = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        assert_eq!(
            now.status,
            AgentRecordStatus::Exited,
            "不得被快照覆盖回 Running"
        );
        assert_eq!(now.attachment, crate::agents::AgentAttachment::None);
    }

    /// 同一问题的 shim 已死分支：快照活跃、磁盘已是终态（shim 写完终态后才
    /// 退出）——锁内复核后不得改成 Failed + SHIM_LOST。
    #[test]
    fn reconcile_dead_branch_rechecks_status_under_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        let id = reg(rt, "exec-done-late");
        update_exec(rt, "h_t", &id, "exec-done-late", |e| {
            e.shim_pid = Some(u32::MAX);
            e.shim_start_id = Some("macos:0:0".into());
        })
        .unwrap();
        backdate(rt, &id, "exec-done-late");
        let snapshot = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        crate::agents::update_agent_status(
            rt,
            "h_t",
            &id,
            AgentRecordStatus::Exited,
            crate::agents::AgentAttachment::None,
        )
        .unwrap();
        assert!(!reconcile_agent(rt, "h_t", &snapshot));
        let now = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        assert_eq!(now.status, AgentRecordStatus::Exited);
        assert!(now.execs[0].failure_detail.is_none());
    }

    /// shim_maybe_alive 判定表：无 pid → 否；身份匹配 → 是；身份不匹配 →
    /// 否；身份缺失但 pid 存在 → 是（未知按存活）；pid 已不存在 → 否。
    #[test]
    fn shim_maybe_alive_treats_unknown_identity_as_alive() {
        let me = std::process::id();
        let start = crate::executor::process_start_identity(me).unwrap();
        let rec = |pid: Option<u32>, start: Option<&str>| ExecRecord {
            shim_pid: pid,
            shim_start_id: start.map(str::to_string),
            ..Default::default()
        };
        assert!(!shim_maybe_alive(&rec(None, None)));
        assert!(shim_maybe_alive(&rec(Some(me), Some(&start))));
        assert!(!shim_maybe_alive(&rec(Some(me), Some("macos:0:0"))));
        assert!(shim_maybe_alive(&rec(Some(me), None)));
        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        assert!(!shim_maybe_alive(&rec(Some(gone_pid), None)));
    }
}
