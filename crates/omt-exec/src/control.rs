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

/// 停止（幂等）：未知 / 已终态 → Ok；shim 存活 → SIGTERM；shim 已死 → 对账收尾。
pub fn stop_exec(runtime_dir: &Path, exec_id: &str) -> Result<(), String> {
    let Some(loc) = find_exec(runtime_dir, exec_id)? else {
        return Ok(());
    };
    if !is_active(&loc.agent) || loc.agent.last_exec_id.as_deref() != Some(exec_id) {
        return Ok(());
    }
    if shim_alive(&loc.exec) {
        let pid = i32::try_from(loc.exec.shim_pid.unwrap_or(0))
            .map_err(|_| "shim pid 越界".to_string())?;
        // SAFETY: 起始标识刚校验过，pid 指向的仍是我们的 shim
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        if rc != 0 {
            return Err(format!(
                "kill({pid}, SIGTERM): {}",
                std::io::Error::last_os_error()
            ));
        }
        return Ok(());
    }
    reconcile_agent(runtime_dir, &loc.home_id, &loc.agent);
    Ok(())
}

/// 停止某 home 的全部活跃 exec，返回发出停止的数量（P-10「停止全部」）。
pub fn stop_all(runtime_dir: &Path, home_id: &str) -> Result<usize, String> {
    let (agents, _) = agents::list_agents(runtime_dir, home_id)?;
    let mut count = 0;
    for agent in agents.iter().filter(|a| is_active(a)) {
        if let Some(exec) = latest_exec(agent) {
            stop_exec(runtime_dir, &exec.exec_id)?;
            count += 1;
        }
    }
    Ok(count)
}

/// 单个活跃 agent 的对账（P-8）。返回是否改成了终态。
fn reconcile_agent(runtime_dir: &Path, home_id: &str, agent: &AgentRecord) -> bool {
    let Some(exec) = latest_exec(agent) else {
        return false;
    };
    if shim_alive(exec) {
        let _ = agents::update_agent_status(
            runtime_dir,
            home_id,
            &agent.id,
            agent.status,
            AgentAttachment::Live,
        );
        return false;
    }
    if within_launch_grace(exec) {
        return false;
    }
    let harness_alive = identity_alive(exec.pid, exec.process_start_id.as_deref());
    let _ = agents::update_exec(runtime_dir, home_id, &agent.id, &exec.exec_id, |e| {
        if e.failure_detail.is_none() {
            e.failure_detail = Some(agents::SHIM_LOST_MARKER.to_string());
        }
    });
    let attachment = if harness_alive {
        AgentAttachment::Orphan
    } else {
        AgentAttachment::None
    };
    let _ = agents::update_agent_status(
        runtime_dir,
        home_id,
        &agent.id,
        AgentRecordStatus::Failed,
        attachment,
    );
    true
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
}
