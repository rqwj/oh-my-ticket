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

use crate::atomic_json::{has_unrecovered_corrupt, read_json, update_json};
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
    /// Spawn 后由 Rust 侧持久化。Orphan 强杀只从本记录解析 pid，并同时校验
    /// 内核进程起始标识，绝不信任 renderer 传入的裸 pid。
    pub pid: Option<u32>,
    pub process_start_id: Option<String>,
    /// U12 跟进（F-U12-4）：终态归档的会话累计用量（usage 事件去重后的
    /// 最终快照）——成本承接的第一层落盘。
    pub usage_tokens: Option<u64>,
    pub usage_cost: Option<f64>,
    /// 本次派发选用的模型（F-VIEW3：续跑列表展示）。
    #[serde(default)]
    pub model: Option<String>,
    /// U10：启动对账已处理（幂等标记——demote 的「app restarted」签名
    /// exec 释放过 daemon 租约后置 true，避免重复对账）。
    #[serde(default)]
    pub reconciled: bool,
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

/// bindings.json 路径（§3.1 存储：`agents/<home-id>/bindings.json`，显式
/// 绑定 nodeId → agentId）。home-id 校验与 agents_path 同源防穿越。
pub fn bindings_path(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    validate_home_id(home_id)?;
    Ok(runtime_dir
        .join("agents")
        .join(home_id)
        .join("bindings.json"))
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
    /// 本次派发选用的模型（F-VIEW3：续跑列表展示；未指定 None）
    pub model: Option<String>,
}

/// 注册新 Agent：分配持久单调序号（display_name = `Agent N`），以
/// status=Starting 先落盘（§3.2「先于 spawn」，U4 链步骤 ⑤）。
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
                model: new_exec.model.clone(),
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

/// U11：向**既有** agent 追加一次 exec（会话续接派发）。守卫：agent 必须存在、
/// harness 与 provider 一致、状态非 starting/running（弹窗只列空闲 agent，
/// 这里是服务端硬校验——renderer 不可信）。成功返回更新后的 agent 记录。
pub fn append_exec(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    harness: &str,
    new_exec: NewExec,
) -> Result<AgentRecord, String> {
    let path = agents_path(runtime_dir, home_id)?;
    let record = update_json(&path, |file: &mut AgentsFile| {
        let Some(agent) = file.agents.iter_mut().find(|a| a.id == agent_id) else {
            return false;
        };
        if agent.harness != harness {
            return false; // 调用方前置校验给出可读错误；此处兜底不写
        }
        if matches!(
            agent.status,
            AgentRecordStatus::Starting | AgentRecordStatus::Running
        ) {
            return false;
        }
        agent.execs.push(ExecRecord {
            exec_id: new_exec.exec_id.clone(),
            node_id: new_exec.node_id.clone(),
            run_id: new_exec.run_id.clone(),
            started_at: now_epoch_ms(),
            binary_path: new_exec.binary_path.clone(),
            binary_args: new_exec.binary_args.clone(),
            model: new_exec.model.clone(),
            ..Default::default()
        });
        agent.status = AgentRecordStatus::Starting;
        agent.attachment = AgentAttachment::Live;
        agent.last_exec_id = Some(new_exec.exec_id.clone());
        true
    })?
    .0;
    record
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .cloned()
        .ok_or_else(|| {
            format!("append_exec: agent {agent_id} 不存在或不可追加（harness 不一致/运行中）")
        })
}

/// U10 启动对账目标：demote 标记的中断 exec（app 重启杀死、租约未释放）。
pub struct ReconcileTarget {
    pub home_id: String,
    pub agent_id: String,
    pub exec_id: String,
    pub run_id: String,
    pub harness: String,
}

/// demote 签名（与 demote_live_agents 写入的 failure_detail 字面量一致）。
const RESTART_MARKER: &str = "app restarted before exec finished";

/// 枚举待对账 exec：failure_detail 命中重启签名且未 reconciled、带 run_id。
pub fn list_reconcile_targets(runtime_dir: &Path) -> Result<Vec<ReconcileTarget>, String> {
    let root = runtime_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut targets = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(home_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let file = entry.path().join("agents.json");
        let (agents_file, _) = match read_json::<AgentsFile>(&file) {
            Ok(v) => v,
            Err(_) => continue, // 损坏/缺失 → 该 home 跳过（回放面另行降级）
        };
        for agent in &agents_file.agents {
            for exec in &agent.execs {
                if !exec.reconciled
                    && exec.run_id.is_some()
                    && exec.failure_detail.as_deref() == Some(RESTART_MARKER)
                {
                    targets.push(ReconcileTarget {
                        home_id: home_id.clone(),
                        agent_id: agent.id.clone(),
                        exec_id: exec.exec_id.clone(),
                        run_id: exec.run_id.clone().unwrap_or_default(),
                        harness: agent.harness.clone(),
                    });
                }
            }
        }
    }
    Ok(targets)
}

/// 按持久化身份读取单个 agent。Orphan 管理命令用此函数在 Rust 侧完成
/// home/agent/exec 归属校验，避免 renderer 直接提供可被滥用的 pid。
/// F-DEL：删除一个 agent（列表项右上角删除入口）。守卫：starting/running
/// 拒绝（先停止再删）；attachment=orphan 拒绝（pid 仍存活，先强制终止）。
pub fn delete_agent(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<(), String> {
    let path = agents_path(runtime_dir, home_id)?;
    let (_, changed) = update_json(&path, |file: &mut AgentsFile| {
        let Some(agent) = file.agents.iter().find(|a| a.id == agent_id) else {
            return false;
        };
        if matches!(
            agent.status,
            AgentRecordStatus::Starting | AgentRecordStatus::Running
        ) {
            return false;
        }
        if agent.attachment == AgentAttachment::Orphan {
            return false;
        }
        file.agents.retain(|a| a.id != agent_id);
        true
    })
    .map_err(|e| format!("删除失败：{e}"))?;
    if !changed.changed {
        let (file, _) = read_json::<AgentsFile>(&path)?;
        match file.agents.iter().find(|a| a.id == agent_id) {
            None => Err(format!("agent {agent_id} 不存在")),
            Some(agent)
                if matches!(
                    agent.status,
                    AgentRecordStatus::Starting | AgentRecordStatus::Running
                ) =>
            {
                Err(format!("agent {agent_id} 仍在运行——请先停止再删除"))
            }
            Some(agent) if agent.attachment == AgentAttachment::Orphan => Err(format!(
                "agent {agent_id} 处于不受控态（pid 存活）——请先强制终止再删除"
            )),
            Some(_) => Err("删除未生效（未知原因）".to_string()),
        }
    } else {
        Ok(())
    }
}

#[tauri::command(async)]
pub fn agents_delete(home_id: String, agent_id: String) -> Result<serde_json::Value, String> {
    delete_agent(&daemon::runtime_dir(), &home_id, &agent_id)?;
    Ok(serde_json::json!({ "deleted": agent_id }))
}

pub fn get_agent(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
) -> Result<Option<AgentRecord>, String> {
    let path = agents_path(runtime_dir, home_id)?;
    let (file, parse_failed) = read_json::<AgentsFile>(&path)?;
    if parse_failed || has_unrecovered_corrupt(&path) {
        return Err("agents.json 已损坏，拒绝解析 orphan 进程身份".to_string());
    }
    Ok(file.agents.into_iter().find(|agent| agent.id == agent_id))
}

/// 更新 Agent 终态/状态。**仅状态变化才落盘**（§3.2 写合并）——高频派生量
/// 不触发重写。返回是否实际变化。
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
    agents_list_in(&daemon::runtime_dir(), &home_id)
}

fn agents_list_in(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let path = agents_path(runtime_dir, home_id)?;
    let (file, parse_failed) = read_json::<AgentsFile>(&path)?;
    // F-U4a-1 修法 (b)：粘性 parseFailed——损坏文件被 rename 后的后续读取
    // 仍视为降级（同目录存在 .corrupt-* 留证），U9 的 GC 冻结据此持续生效。
    let parse_failed = parse_failed || has_unrecovered_corrupt(&path);
    Ok(serde_json::json!({
        "agents": file.agents,
        "agentSeq": file.meta.agent_seq,
        "parseFailed": parse_failed,
    }))
}

/// U4 步骤 ⑥：写入 bindings.json（显式绑定 nodeId → agentId）。
/// F-U15-1 第三条写路径：走 atomic_json 的 read-merge-write（进程内锁 +
/// companion flock + tmp/rename + corrupt 降级留证）——不再依赖调用方的
/// DISPATCH_LOCK 做写互斥。写失败由调用方（U4 链）转为警告不阻断。
pub fn write_binding(
    runtime_dir: &Path,
    home_id: &str,
    node_id: &str,
    agent_id: &str,
) -> Result<(), String> {
    let path = bindings_path(runtime_dir, home_id)?;
    update_json(&path, |bindings: &mut BTreeMap<String, String>| {
        bindings.insert(node_id.to_string(), agent_id.to_string());
        true
    })
    .map(|_| ())
}

/// bindings 表的进程内读取（同 write_binding 的锁语义；不存在 → 空表）。
fn read_bindings(runtime_dir: &Path, home_id: &str) -> Result<BTreeMap<String, String>, String> {
    let path = bindings_path(runtime_dir, home_id)?;
    Ok(read_json::<BTreeMap<String, String>>(&path)?.0)
}

/// U9（§3.4）：显式绑定表投影。`parseFailed` 粘性语义同 agents_list
/// （F-U4a-1），且**叠加 agents.json 的降级态**——悬空判定（agentId 是否
/// 存在）依赖健康的 agents 数据，任一文件降级时前端都不得做悬空判定。
#[tauri::command(async)]
pub fn bindings_get(home_id: String) -> Result<serde_json::Value, String> {
    bindings_get_in(&daemon::runtime_dir(), &home_id)
}

fn bindings_get_in(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let bindings = read_bindings(runtime_dir, home_id)?;
    let b_path = bindings_path(runtime_dir, home_id)?;
    let agents_path = agents_path(runtime_dir, home_id)?;
    // 读一次 agents.json：让「此刻损坏」也进粘性留证（rename 发生在本读），
    // 与 agents_list 的判定完全同源。
    let (_, agents_parse_failed) = read_json::<AgentsFile>(&agents_path)?;
    let parse_failed = has_unrecovered_corrupt(&b_path)
        || has_unrecovered_corrupt(&agents_path)
        || agents_parse_failed;
    Ok(serde_json::json!({
        "bindings": bindings,
        "parseFailed": parse_failed,
    }))
}

/// U9（§3.4）：用户触发的一键清理——移除 agentId 已不存在于 agents.json 的
/// 绑定项，返回移除数与清理后的表。§3.6.6 硬规则：**bindings 非空且
/// agents.json 无法给出有效 agent 表时冻结**（解析失败 / 粘性 corrupt 残留
/// F-U4a-1 / 文件缺失任一情形）——「desktop 是唯一写者」不覆盖手工删除、
/// 同步工具误删、部分恢复等异常路径，无法证明 agent 表有效时绝不能把绑定
/// 按空表全清。bindings 为空时 GC 本来就是 no-op，正常返回 0。
#[tauri::command(async)]
pub fn bindings_gc(home_id: String) -> Result<serde_json::Value, String> {
    bindings_gc_in(&daemon::runtime_dir(), &home_id)
}

fn bindings_gc_in(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let bindings_file = bindings_path(runtime_dir, home_id)?;
    // 空 bindings → 无害 no-op，先于 agents 有效性检查返回（缺失的
    // agents.json 不必挡住 0 移除路径）。
    let existing = read_bindings(runtime_dir, home_id)?;
    if existing.is_empty() {
        return Ok(serde_json::json!({ "removed": 0, "bindings": existing }));
    }
    let agents_path = agents_path(runtime_dir, home_id)?;
    // 缺失判定必须在 read_json 之前采集：损坏文件会在读时被 rename 成
    // .corrupt-<ts>，之后 exists() 就永远是 false 了。
    let agents_missing = !agents_path.exists();
    let (agents, agents_parse_failed) = read_json::<AgentsFile>(&agents_path)?;
    if agents_missing || agents_parse_failed || has_unrecovered_corrupt(&agents_path) {
        return Err(
            "agents.json 无法给出有效 agent 表（缺失 / 解析失败 / .corrupt-* 留证未恢复）——绑定清理已冻结"
                .to_string(),
        );
    }
    let live_ids: std::collections::HashSet<String> =
        agents.agents.iter().map(|a| a.id.clone()).collect();
    let mut removed = 0usize;
    // bindings.json 自身损坏降级为空表时无项可移（removed=0、不落盘），对
    // 留证无损；agentId 判定以 agents.json 的 agents[].id 为准。
    let (table, _) = update_json(&bindings_file, |bindings: &mut BTreeMap<String, String>| {
        let before = bindings.len();
        bindings.retain(|_, agent_id| live_ids.contains(agent_id));
        removed = before - bindings.len();
        removed > 0
    })?;
    Ok(serde_json::json!({
        "removed": removed,
        "bindings": table,
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

    #[test]
    fn delete_agent_guards_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let agent = register_agent(&runtime, "h_test", "pi", new_exec("exec-del")).unwrap();
        // 运行中 → 拒绝（可读理由）
        update_agent_status(
            &runtime,
            "h_test",
            &agent.id,
            AgentRecordStatus::Running,
            AgentAttachment::Live,
        )
        .unwrap();
        let err = delete_agent(&runtime, "h_test", &agent.id).unwrap_err();
        assert!(err.contains("仍在运行"), "{err}");
        // 终态 → 删除成功；再删 → 不存在
        update_agent_status(
            &runtime,
            "h_test",
            &agent.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        delete_agent(&runtime, "h_test", &agent.id).unwrap();
        let err2 = delete_agent(&runtime, "h_test", &agent.id).unwrap_err();
        assert!(err2.contains("不存在"), "{err2}");
        let (file, _) = read_json::<AgentsFile>(&agents_path(&runtime, "h_test").unwrap()).unwrap();
        assert!(file.agents.is_empty());
    }

    fn new_exec(exec_id: &str) -> NewExec {
        NewExec {
            exec_id: exec_id.to_string(),
            node_id: None,
            run_id: None,
            binary_path: None,
            binary_args: vec![],
            model: None,
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

    #[test]
    fn parse_failed_is_sticky_across_reads() {
        // F-U4a-1：损坏 → 第一次读（rename 后返回默认 + parseFailed=true）→
        // 第二次读 → parseFailed 仍必须为 true（粘性），否则 U9 的 GC 冻结
        // 在第二次读后失效，会把全部绑定当悬空清掉（§3.6.6 要防的事故）。
        let (_dir, runtime, home) = temp_runtime();
        let path = agents_path(&runtime, &home).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ broken").unwrap();
        let first = agents_list_in(&runtime, &home).unwrap();
        assert_eq!(first["parseFailed"], true);
        let second = agents_list_in(&runtime, &home).unwrap();
        assert_eq!(
            second["parseFailed"], true,
            "第二次读取 parseFailed 不得翻回 false（粘性判定）"
        );
    }

    #[test]
    fn write_binding_on_corrupt_file_downgrades_with_evidence() {
        // F-U15-1 第三条写路径：bindings.json 必须走 atomic_json——损坏文件
        // 要改名 .corrupt-<ts> 留证后从空开始，而不是被静默覆盖（丢留证）。
        let (_dir, runtime, home) = temp_runtime();
        let path = bindings_path(&runtime, &home).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ broken").unwrap();
        write_binding(&runtime, &home, "T-1", "agent-exec-1").unwrap();
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("bindings.json.corrupt-")),
            "损坏文件必须留证 .corrupt-*: {names:?}"
        );
        let bindings: BTreeMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            bindings.get("T-1").map(String::as_str),
            Some("agent-exec-1")
        );
    }

    #[test]
    fn concurrent_write_binding_does_not_lose_bindings() {
        // atomic_json 迁移后的锁语义：并发写不得丢更新（先例
        // concurrent_register_does_not_lose_agents）
        let (_dir, runtime, home) = temp_runtime();
        std::thread::scope(|scope| {
            for i in 0..6u32 {
                let runtime = runtime.clone();
                let home = home.clone();
                scope.spawn(move || {
                    write_binding(
                        &runtime,
                        &home,
                        &format!("T-{i}"),
                        &format!("agent-exec-{i}"),
                    )
                    .unwrap();
                });
            }
        });
        let bindings = read_bindings(&runtime, &home).unwrap();
        assert_eq!(bindings.len(), 6, "{bindings:?}");
        let leftovers: Vec<String> =
            std::fs::read_dir(bindings_path(&runtime, &home).unwrap().parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.contains(".json.tmp-"))
                .collect();
        assert!(leftovers.is_empty(), "rename 后不应残留 tmp: {leftovers:?}");
    }

    #[test]
    fn bindings_get_projects_table_and_sticky_parse_failed() {
        let (_dir, runtime, home) = temp_runtime();
        write_binding(&runtime, &home, "T-1", "agent-exec-1").unwrap();
        write_binding(&runtime, &home, "T-2", "agent-exec-1").unwrap();
        let result = bindings_get_in(&runtime, &home).unwrap();
        assert_eq!(result["bindings"]["T-1"], "agent-exec-1");
        assert_eq!(result["bindings"]["T-2"], "agent-exec-1");
        assert_eq!(result["parseFailed"], false);

        // agents.json 损坏 → bindings_get 的 parseFailed 粘性置位（悬空判定
        // 依赖 agents 数据，降级期间前端不得判悬空）
        let agents_file = agents_path(&runtime, &home).unwrap();
        std::fs::write(&agents_file, "{ broken").unwrap();
        let _ = read_json::<AgentsFile>(&agents_file).unwrap();
        let degraded = bindings_get_in(&runtime, &home).unwrap();
        assert_eq!(
            degraded["bindings"]["T-1"], "agent-exec-1",
            "绑定表不因 agents 降级而丢"
        );
        assert_eq!(degraded["parseFailed"], true);
    }

    fn seed_agents(runtime: &Path, home: &str, exec_suffixes: &[&str]) {
        // register_agent 的 id = `agent-<execId>`——后缀即得 agent-a1 形态的 id
        for s in exec_suffixes {
            register_agent(runtime, home, "omp", new_exec(s)).unwrap();
        }
    }

    #[test]
    fn bindings_gc_removes_only_dangling_and_persists() {
        let (_dir, runtime, home) = temp_runtime();
        seed_agents(&runtime, &home, &["a1", "a2"]);
        write_binding(&runtime, &home, "T-keep-1", "agent-a1").unwrap();
        write_binding(&runtime, &home, "T-keep-2", "agent-a2").unwrap();
        write_binding(&runtime, &home, "T-dangling", "agent-gone").unwrap();

        let result = bindings_gc_in(&runtime, &home).unwrap();
        assert_eq!(result["removed"], 1);
        assert_eq!(result["bindings"]["T-keep-1"], "agent-a1");
        assert_eq!(result["bindings"]["T-keep-2"], "agent-a2");
        assert!(result["bindings"].get("T-dangling").is_none());

        // 清理后文件内容断言：落盘形态与返回表一致
        let persisted = read_bindings(&runtime, &home).unwrap();
        assert_eq!(persisted.len(), 2);
        assert!(!persisted.contains_key("T-dangling"));
    }

    #[test]
    fn bindings_gc_frozen_while_agents_corrupt() {
        // §3.6.6 硬规则：解析失败期间冻结 GC——绝不能把全部绑定当悬空清掉
        let (_dir, runtime, home) = temp_runtime();
        write_binding(&runtime, &home, "T-1", "agent-x").unwrap();
        write_binding(&runtime, &home, "T-2", "agent-y").unwrap();
        let agents_file = agents_path(&runtime, &home).unwrap();
        std::fs::write(&agents_file, "{ broken").unwrap();
        let _ = read_json::<AgentsFile>(&agents_file).unwrap();

        let err = bindings_gc_in(&runtime, &home).unwrap_err();
        assert!(err.contains("冻结"), "{err}");
        // 文件内容断言：绑定原样保留
        let persisted = read_bindings(&runtime, &home).unwrap();
        assert_eq!(persisted.len(), 2, "{persisted:?}");
        assert_eq!(persisted.get("T-1").map(String::as_str), Some("agent-x"));
    }

    #[test]
    fn bindings_gc_frozen_when_agents_file_missing() {
        // §3.6.6 收紧：agents.json 缺失（手工删除/同步工具误删/部分恢复）
        // 同样无法证明 agent 表有效——非空绑定必须冻结，不得按空表全清。
        let (_dir, runtime, home) = temp_runtime();
        write_binding(&runtime, &home, "T-1", "agent-x").unwrap();
        write_binding(&runtime, &home, "T-2", "agent-y").unwrap();
        assert!(!agents_path(&runtime, &home).unwrap().exists());

        let err = bindings_gc_in(&runtime, &home).unwrap_err();
        assert!(err.contains("冻结"), "{err}");
        let persisted = read_bindings(&runtime, &home).unwrap();
        assert_eq!(persisted.len(), 2, "缺失路径下绑定原样保留: {persisted:?}");
    }

    #[test]
    fn bindings_gc_empty_bindings_is_noop_zero_even_when_agents_missing() {
        // 空 bindings 本来就是 no-op：先于 agents 有效性检查返回 0——
        // agents.json 缺失也不冻结无害路径。
        let (_dir, runtime, home) = temp_runtime();
        assert!(!agents_path(&runtime, &home).unwrap().exists());
        let result = bindings_gc_in(&runtime, &home).unwrap();
        assert_eq!(result["removed"], 0);
        assert_eq!(result["bindings"], serde_json::json!({}));
    }
}
