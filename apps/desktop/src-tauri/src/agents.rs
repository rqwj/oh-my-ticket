//! U4a (R-E): Agent 最小存储 —— `agents/<home-id>/agents.json`。
//!
//! 存储半（本单元）：schema（§3.2：meta.agentSeq + agents[].{status,
//! attachment, execs[]}）、进程内锁 + flock + tmp/rename 原子写（共用
//! atomic_json）、`.corrupt-<ts>` 降级 + `parseFailed` 标志（U9 冻结 GC）、
//! `agentSeq` 持久单调（「Agent N」跨重启不重号）。
//! 运行半（U10）：启动 reconcile、pid liveness、回放。
//!
//! secrets 纪律（§3.1）：`execs[]` 只记 binary 路径与非敏感 args，
//! **不记 env 值**；schema 层面就没有 env 字段。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::atomic_json::{read_json, update_json};
use crate::daemon;

// ---------------------------------------------------------------------------
// 持久化 schema（§3.2；camelCase 落盘）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsFile {
    pub meta: AgentsMeta,
    pub agents: Vec<AgentRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsMeta {
    /// display_name 的持久单调序号（`Agent N` 的 N）。
    pub agent_seq: u64,
}

/// 持久化状态（§3.2 终态分类）；失败详情在 exec 的 exit 里，不塞进枚举。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentRecordStatus {
    Starting,
    Running,
    Exited,
    Failed,
    Killed,
}

/// 可控性正交字段（§3.2）：Live=本进程 spawn 且存活；Orphan=重启后 pid 仍
/// 活（只读尾随，不可 stop）；None=已结束。
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentAttachment {
    #[default]
    None,
    Live,
    Orphan,
}

/// 一次执行（exec）的持久记录——transcript header 的镜像；二者的归组键是
/// agentId / execId（transcripts/<home-id>/<execId>.jsonl）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExecRecord {
    pub exec_id: String,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    /// epoch 毫秒（字符串化，排序用；展示格式化由前端负责）
    pub started_at: String,
    /// 正常结束 = exit code；killed = reason（user|maxduration|appexit）；
    /// 失败 = stderr 尾（已脱敏）。
    pub exit_code: Option<i32>,
    pub kill_reason: Option<String>,
    pub failure_detail: Option<String>,
    pub harness_session_id: Option<String>,
    /// 相对 transcripts/<home-id>/ 的文件名
    pub transcript_file: Option<String>,
    /// binary 路径与非敏感 args（不记 env 值）
    pub binary_path: Option<String>,
    pub binary_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentRecord {
    pub id: String,
    pub display_name: String,
    pub title: Option<String>,
    pub harness: String,
    pub status: AgentRecordStatus,
    pub attachment: AgentAttachment,
    pub last_exec_id: Option<String>,
    pub last_session_id: Option<String>,
    /// P0 仅记录首个节点/最近 run 的归属（读侧展示用）；子树绑定在
    /// bindings.json（U9）。
    pub execs: Vec<ExecRecord>,
}

impl Default for AgentRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            title: None,
            harness: String::new(),
            status: AgentRecordStatus::Starting,
            attachment: AgentAttachment::None,
            last_exec_id: None,
            last_session_id: None,
            execs: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 存储 API（进程内唯一入口；U2/U4 经此读写，不直接碰文件）
// ---------------------------------------------------------------------------

/// home-id 来自 daemon handshake（`h_`+base36），防御性拒绝路径分隔符与
/// 上级目录形态。
pub(crate) fn validate_home_id(home_id: &str) -> Result<(), String> {
    if home_id.is_empty()
        || home_id.contains('/')
        || home_id.contains('\\')
        || home_id.starts_with('.')
        || home_id.contains("..")
    {
        return Err(format!("invalid home id: {home_id:?}"));
    }
    Ok(())
}

pub fn agents_path(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    validate_home_id(home_id)?;
    Ok(runtime_dir.join("agents").join(home_id).join("agents.json"))
}

pub(crate) fn now_epoch_ms() -> String {
    // 排序用时间戳：epoch 毫秒（字符串化，字典序=时间序）；transcript header
    // 的 startedAt 与 execs[].startedAt 必须同格式（§3.2 / F-U4a-6）。展示层
    // 的格式化（相对时间等）由前端负责。
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("{ms}")
}

/// F-U4a-2（P0 半）：启动 demote——扫描所有 home 的 agents.json，把
/// `attachment == Live` 的记录降级：`attachment=None`、`Starting/Running →
/// Failed`（failure_detail 注明 app 重启）。pid liveness 与 Orphan 只读
/// 尾随是 U10 的活；这里只保证 P0 期间「重启后不再把死 agent 当可控项」。
/// 返回降级条数。
pub fn demote_live_agents(runtime_dir: &Path) -> Result<usize, String> {
    let root = runtime_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(0);
    };
    let mut demoted = 0usize;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let file = entry.path().join("agents.json");
        let (_, changed) = update_json(&file, |f: &mut AgentsFile| {
            let mut touched = false;
            for agent in &mut f.agents {
                if agent.attachment == AgentAttachment::Live {
                    agent.attachment = AgentAttachment::None;
                    demoted += 1;
                    touched = true;
                }
                if matches!(
                    agent.status,
                    AgentRecordStatus::Starting | AgentRecordStatus::Running
                ) {
                    agent.status = AgentRecordStatus::Failed;
                    for exec in &mut agent.execs {
                        if exec.failure_detail.is_none() {
                            exec.failure_detail =
                                Some("app restarted before exec finished".to_string());
                        }
                    }
                    touched = true;
                }
            }
            touched
        })?;
        let _ = changed;
    }
    Ok(demoted)
}

/// 一次派发的 exec 标识（U4 链步骤 ⑤ 的入参；binary 信息进 execs[] 用于
/// 展示与审计——env 值不落盘，§3.1）。
pub struct NewExec {
    pub exec_id: String,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    pub binary_path: Option<String>,
    pub binary_args: Vec<String>,
}

/// 注册新 Agent：分配持久单调序号（display_name = `Agent N`），以
/// status=Starting 先落盘（§3.2「先于 spawn」，U4 链步骤 ⑤）。
#[allow(dead_code)] // U4 派发链接线后移除
pub fn register_agent(
    runtime_dir: &Path,
    home_id: &str,
    harness: &str,
    new_exec: NewExec,
) -> Result<AgentRecord, String> {
    let path = agents_path(runtime_dir, home_id)?;
    let mut seq = 0u64;
    let record = update_json(&path, |file: &mut AgentsFile| {
        file.meta.agent_seq += 1;
        seq = file.meta.agent_seq;
        let record = AgentRecord {
            id: format!("agent-{}", new_exec.exec_id),
            display_name: format!("Agent {seq}"),
            title: None,
            harness: harness.to_string(),
            status: AgentRecordStatus::Starting,
            attachment: AgentAttachment::Live,
            last_exec_id: Some(new_exec.exec_id.clone()),
            last_session_id: None,
            execs: vec![ExecRecord {
                exec_id: new_exec.exec_id.clone(),
                node_id: new_exec.node_id.clone(),
                run_id: new_exec.run_id.clone(),
                started_at: now_epoch_ms(),
                binary_path: new_exec.binary_path.clone(),
                binary_args: new_exec.binary_args.clone(),
                ..Default::default()
            }],
        };
        file.agents.push(record.clone());
        true
    })?
    .0;
    record
        .agents
        .last()
        .cloned()
        .ok_or_else(|| "register_agent: record vanished".to_string())
}

/// 更新 Agent 终态/状态。**仅状态变化才落盘**（§3.2 写合并）——高频派生量
/// 不触发重写。返回是否实际变化。
#[allow(dead_code)] // U2 终态迁移接线后移除
pub fn update_agent_status(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    status: AgentRecordStatus,
    attachment: AgentAttachment,
) -> Result<bool, String> {
    let path = agents_path(runtime_dir, home_id)?;
    update_json(&path, |file: &mut AgentsFile| {
        let Some(agent) = file.agents.iter_mut().find(|a| a.id == agent_id) else {
            return false;
        };
        if agent.status == status && agent.attachment == attachment {
            return false;
        }
        agent.status = status;
        agent.attachment = attachment;
        true
    })
    .map(|(_, outcome)| outcome.changed)
}

/// 把 exec 的终态/会话 id 写回对应 ExecRecord（U2/U11 接线）。
#[allow(dead_code)]
pub fn update_exec(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    apply: impl FnOnce(&mut ExecRecord),
) -> Result<bool, String> {
    let path = agents_path(runtime_dir, home_id)?;
    update_json(&path, |file: &mut AgentsFile| {
        let Some(agent) = file.agents.iter_mut().find(|a| a.id == agent_id) else {
            return false;
        };
        let Some(exec) = agent.execs.iter_mut().find(|e| e.exec_id == exec_id) else {
            return false;
        };
        let before = exec.clone();
        apply(exec);
        if let Some(session) = &exec.harness_session_id {
            agent.last_session_id = Some(session.clone());
        }
        *exec != before
    })
    .map(|(_, outcome)| outcome.changed)
}

/// Agents 面板数据面（U5 消费）：记录列表 + `parseFailed` 标志（前端提示
/// 数据已降级；U9 据此冻结 GC）。
#[tauri::command(async)]
pub fn agents_list(home_id: String) -> Result<serde_json::Value, String> {
    let runtime_dir = daemon::runtime_dir();
    let path = agents_path(&runtime_dir, &home_id)?;
    let (file, parse_failed) = read_json::<AgentsFile>(&path)?;
    Ok(serde_json::json!({
        "agents": file.agents,
        "agentSeq": file.meta.agent_seq,
        "parseFailed": parse_failed,
    }))
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;

    fn temp_runtime() -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().to_path_buf();
        (dir, runtime, "h_1a2b3c4d5e6f".to_string())
    }

    fn new_exec(exec_id: &str) -> NewExec {
        NewExec {
            exec_id: exec_id.to_string(),
            node_id: None,
            run_id: None,
            binary_path: None,
            binary_args: vec![],
        }
    }

    #[test]
    fn register_assigns_persistent_monotonic_seq() {
        let (_dir, runtime, home) = temp_runtime();
        let first = register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        assert_eq!(first.display_name, "Agent 1");
        assert_eq!(first.status, AgentRecordStatus::Starting);
        assert_eq!(first.attachment, AgentAttachment::Live);

        let second = register_agent(&runtime, &home, "omp", new_exec("exec-2")).unwrap();
        assert_eq!(second.display_name, "Agent 2");

        // 跨「会话」（新调用、同文件）序号继续单调——agentSeq 已持久
        let (file, parse_failed) =
            read_json::<AgentsFile>(&agents_path(&runtime, &home).unwrap()).unwrap();
        assert!(!parse_failed);
        assert_eq!(file.meta.agent_seq, 2);
        assert_eq!(file.agents.len(), 2);
    }

    #[test]
    fn same_status_update_does_not_rewrite_file() {
        let (_dir, runtime, home) = temp_runtime();
        register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        let path = agents_path(&runtime, &home).unwrap();
        let before = std::fs::read(&path).unwrap();

        // 相同状态 → 不落盘（写合并）
        assert!(!update_agent_status(
            &runtime,
            &home,
            "agent-exec-1",
            AgentRecordStatus::Starting,
            AgentAttachment::Live
        )
        .unwrap());
        assert_eq!(before, std::fs::read(&path).unwrap());

        // 状态变化 → 落盘
        assert!(update_agent_status(
            &runtime,
            &home,
            "agent-exec-1",
            AgentRecordStatus::Running,
            AgentAttachment::Live
        )
        .unwrap());
        let (file, _) = read_json::<AgentsFile>(&path).unwrap();
        assert_eq!(file.agents[0].status, AgentRecordStatus::Running);
    }

    #[test]
    fn concurrent_register_does_not_lose_agents() {
        let (_dir, runtime, home) = temp_runtime();
        std::thread::scope(|scope| {
            for i in 0..6u32 {
                let runtime = runtime.clone();
                let home = home.clone();
                scope.spawn(move || {
                    register_agent(&runtime, &home, "omp", new_exec(&format!("exec-{i}"))).unwrap();
                });
            }
        });
        let (file, _) = read_json::<AgentsFile>(&agents_path(&runtime, &home).unwrap()).unwrap();
        assert_eq!(file.agents.len(), 6, "并发注册不得丢 agent");
        assert_eq!(file.meta.agent_seq, 6, "序号必须与 agent 数一致");
        let names: Vec<&str> = file
            .agents
            .iter()
            .map(|a| a.display_name.as_str())
            .collect();
        for n in 1..=6u64 {
            assert!(names.contains(&&format!("Agent {n}")[..]), "{names:?}");
        }
    }

    #[test]
    fn corrupted_agents_file_downgrades_with_parse_failed_flag() {
        let (_dir, runtime, home) = temp_runtime();
        let path = agents_path(&runtime, &home).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ broken").unwrap();
        let (_, parse_failed) = read_json::<AgentsFile>(&path).unwrap();
        assert!(
            parse_failed,
            "损坏必须暴露 parseFailed（U9 冻结 GC 的依据）"
        );
        let restored: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            restored
                .iter()
                .any(|n| n.starts_with("agents.json.corrupt-")),
            "{restored:?}"
        );
    }

    #[test]
    fn home_id_rejects_path_traversal() {
        assert!(agents_path(Path::new("/tmp"), "../escape").is_err());
        assert!(agents_path(Path::new("/tmp"), "a/b").is_err());
        assert!(agents_path(Path::new("/tmp"), "h_ok").is_ok());
    }

    #[test]
    fn exec_record_schema_has_no_env_field() {
        // secrets 纪律的 schema 级保证：ExecRecord 序列化不含 env
        let exec = ExecRecord {
            exec_id: "exec-1".into(),
            binary_path: Some("/usr/local/bin/omp".into()),
            binary_args: vec!["--approval-mode".into(), "write".into()],
            ..Default::default()
        };
        let text = serde_json::to_string(&exec).unwrap();
        assert!(!text.contains("\"env\""), "{text}");
    }

    #[test]
    fn demote_live_agents_after_restart() {
        // F-U4a-2：写入 Live 后模拟重启 → 列表不再 Live
        let (_dir, runtime, home) = temp_runtime();
        register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        assert_eq!(demote_live_agents(&runtime).unwrap(), 1);
        let (file, _) = read_json::<AgentsFile>(&agents_path(&runtime, &home).unwrap()).unwrap();
        assert_eq!(file.agents[0].attachment, AgentAttachment::None);
        assert_eq!(file.agents[0].status, AgentRecordStatus::Failed);
        assert!(
            file.agents[0].execs[0]
                .failure_detail
                .as_deref()
                .unwrap()
                .contains("app restarted"),
            "failure_detail 应注明重启"
        );
        // 幂等：再次 demote 无新增
        assert_eq!(demote_live_agents(&runtime).unwrap(), 0);
    }
}

/// U4 步骤 ⑥：写入 bindings.json（显式绑定 nodeId → agentId）。
/// 原子写（tmp+rename，先例 atomic_json）；写失败由调用方转为警告。
#[allow(dead_code)]
pub fn write_binding(
    runtime_dir: &Path,
    home_id: &str,
    node_id: &str,
    agent_id: &str,
) -> Result<(), String> {
    let dir = runtime_dir.join("agents").join(home_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create agents dir: {e}"))?;
    let path = dir.join("bindings.json");
    // read-merge-write（进程内锁由调用方 DISPATCH_LOCK 保证；文件级 tmp+rename）
    let mut bindings: BTreeMap<String, String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    bindings.insert(node_id.to_string(), agent_id.to_string());
    let text = serde_json::to_string_pretty(&bindings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &text).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
}
