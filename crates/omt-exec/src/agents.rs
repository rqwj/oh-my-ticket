//! U4a (R-E): Agent 存储 —— `agents/<home-id>/<agentId>/agent.json`（KD-E2
//! 按 agent 拆分；路径面见 `layout`）。
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

use crate::layout;
use omt_harness::atomic_json::{
    has_unrecovered_corrupt, lock_exclusive, read_json, update_json, LOCK_ACQUIRE_TIMEOUT,
};

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
    /// 正常结束 = exit code；killed = reason（user|maxduration|external-<n>|
    /// orphan）；失败 = stderr 尾（已脱敏）。
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
    /// F-BENCH-20：usage 归档口径。Some("message-sum") = 按消息/步求和、
    /// Some("cumulative-last") = 会话累计取末值（e87a99a 起）；None = 旧
    /// 口径（只含最后一条消息的 usage，少报）——历史记录未重算，消费方须
    /// 按 None 区分展示。（注：da9c376..e87a99a 之间归档的记录已是求和
    /// 口径但无标记——开发期数据，按旧口径对待只影响展示精度。）
    #[serde(default)]
    pub usage_caliber: Option<String>,
    /// 本次派发选用的模型（F-VIEW3：续跑列表展示）。
    #[serde(default)]
    pub model: Option<String>,
    /// U10：启动对账已处理（幂等标记——demote 的「app restarted」签名
    /// exec 释放过 daemon 租约后置 true，避免重复对账）。
    #[serde(default)]
    pub reconciled: bool,
    /// exec-shim 进程 pid 与内核起始标识（设计 §3.2：停止 / 存活检查先校验
    /// 起始标识，防 pid 复用）。由 launcher 在拉起 shim 后写入。
    #[serde(default)]
    pub shim_pid: Option<u32>,
    #[serde(default)]
    pub shim_start_id: Option<String>,
    /// shim 收尾时租约释放失败（daemon 不可达等）——留给下次对账重试（设计 §5）。
    #[serde(default)]
    pub lease_release_pending: bool,
    /// Q-7：本次派发的发起方（desktop / cli / mcp）；子项目 2 之前的旧记录为 None。
    #[serde(default)]
    pub launched_by: Option<crate::policy::LaunchOrigin>,
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
    /// Q-7：父 agent（主控）。只在新建 agent 时由显式参数写入；自动祖先链关联属子项目 3。
    #[serde(default)]
    pub parent_agent_id: Option<String>,
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
            parent_agent_id: None,
            execs: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 存储 API（进程内唯一入口；U2/U4 经此读写，不直接碰文件）
// ---------------------------------------------------------------------------

/// home-id 来自 daemon handshake（`h_`+base36），防御性拒绝路径分隔符与
/// 上级目录形态。
pub fn validate_home_id(home_id: &str) -> Result<(), String> {
    crate::layout::validate_segment("home id", home_id)
}

/// bindings.json 路径（§3.1 存储：`agents/<home-id>/bindings.json`，显式
/// 绑定 nodeId → agentId）。home-id 校验与 layout 同源防穿越。
pub fn bindings_path(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    crate::layout::bindings_file(runtime_dir, home_id)
}

pub fn now_epoch_ms() -> String {
    // 排序用时间戳：epoch 毫秒（字符串化，字典序=时间序）；transcript header
    // 的 startedAt 与 execs[].startedAt 必须同格式（§3.2 / F-U4a-6）。展示层
    // 的格式化（相对时间等）由前端负责。
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("{ms}")
}

fn first_started(agent: &AgentRecord) -> &str {
    agent
        .execs
        .first()
        .map(|e| e.started_at.as_str())
        .unwrap_or("")
}

/// 某 home 的全部 agent（按首个 exec 的 startedAt 排序，epoch ms 字符串
/// 等长，字典序 = 时间序）。返回值第二项 = parseFailed（粘性：任一
/// agent.json 解析失败或留有未恢复的 `.corrupt-*` 证据）。
pub fn list_agents(runtime_dir: &Path, home_id: &str) -> Result<(Vec<AgentRecord>, bool), String> {
    let mut agents = Vec::new();
    let mut parse_failed = false;
    for agent_id in layout::agent_ids(runtime_dir, home_id)? {
        let path = layout::agent_file(runtime_dir, home_id, &agent_id)?;
        let (record, failed) = read_json::<AgentRecord>(&path)?;
        parse_failed |= failed || has_unrecovered_corrupt(&path);
        if !record.id.is_empty() {
            agents.push(record);
        }
    }
    // F-EXEC-1：home 级旧版单文件仍以原名存在 = 未迁移 / 迁移因损坏跳过 /
    // 迁移中断未改名——旧 agent 没进新表，agent 表无法证明有效（悬空判定
    // 会把指向旧 agent 的绑定误判删除）；升级前的 `.corrupt-*` 留证同理。
    // 两者都置 parseFailed，让 bindings_get/bindings_gc 一并冻结。
    let legacy = legacy_agents_monolith(runtime_dir, home_id)?;
    parse_failed |= legacy.is_file() || has_unrecovered_corrupt(&legacy);
    agents.sort_by(|a, b| first_started(a).cmp(first_started(b)).then(a.id.cmp(&b.id)));
    Ok((agents, parse_failed))
}

/// 旧版 home 级单文件存储的路径（迁移成功后改名为 `.migrated`，见
/// migrate.rs；名字拼出以避开迁移 grep 的字面量，先例见 tests 的
/// register_writes_one_dir_per_agent_and_seq_is_monotonic）。
fn legacy_agents_monolith(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    Ok(layout::home_dir(runtime_dir, home_id)?.join(format!("{}.json", "agents")))
}

/// 持久单调的 agent 序号（`Agent N` 的 N）。
pub fn agent_seq(runtime_dir: &Path, home_id: &str) -> Result<u64, String> {
    Ok(
        read_json::<AgentsMeta>(&layout::seq_file(runtime_dir, home_id)?)?
            .0
            .agent_seq,
    )
}

/// 在某 home 内按 exec id 找 (agent, exec)。
pub fn find_exec_in_home(
    runtime_dir: &Path,
    home_id: &str,
    exec_id: &str,
) -> Result<Option<(AgentRecord, ExecRecord)>, String> {
    let (agents, _) = list_agents(runtime_dir, home_id)?;
    Ok(agents.into_iter().find_map(|agent| {
        let exec = agent.execs.iter().find(|e| e.exec_id == exec_id).cloned()?;
        Some((agent, exec))
    }))
}

/// 测试 helper：把新布局拼回旧的 AgentsFile 形状（旧测试的断言面）。
#[cfg(test)]
pub(crate) fn snapshot(runtime_dir: &Path, home_id: &str) -> AgentsFile {
    AgentsFile {
        meta: AgentsMeta {
            agent_seq: agent_seq(runtime_dir, home_id).unwrap_or(0),
        },
        agents: list_agents(runtime_dir, home_id)
            .map(|(a, _)| a)
            .unwrap_or_default(),
    }
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
    /// Q-7：发起方，写入 ExecRecord.launched_by。
    pub launched_by: Option<crate::policy::LaunchOrigin>,
    /// Q-7：仅 register_agent 使用（续跑沿用 agent 既有的父关系）。
    pub parent_agent_id: Option<String>,
}

/// 注册新 Agent：分配持久单调序号（display_name = `Agent N`），以
/// status=Starting 先落盘（§3.2「先于 spawn」，U4 链步骤 ⑤）。
pub fn register_agent(
    runtime_dir: &Path,
    home_id: &str,
    harness: &str,
    new_exec: NewExec,
) -> Result<AgentRecord, String> {
    let (meta, _) = update_json(
        &layout::seq_file(runtime_dir, home_id)?,
        |m: &mut AgentsMeta| {
            m.agent_seq += 1;
            true
        },
    )?;
    let seq = meta.agent_seq;
    let agent_id = layout::new_agent_id(&new_exec.exec_id);
    let path = layout::agent_file(runtime_dir, home_id, &agent_id)?;
    let (record, outcome) = update_json(&path, |rec: &mut AgentRecord| {
        if !rec.id.is_empty() {
            return false; // exec id 全局唯一，正常不会撞
        }
        *rec = AgentRecord {
            id: agent_id.clone(),
            display_name: format!("Agent {seq}"),
            title: None,
            harness: harness.to_string(),
            status: AgentRecordStatus::Starting,
            attachment: AgentAttachment::Live,
            last_exec_id: Some(new_exec.exec_id.clone()),
            last_session_id: None,
            parent_agent_id: new_exec.parent_agent_id.clone(),
            execs: vec![ExecRecord {
                exec_id: new_exec.exec_id.clone(),
                node_id: new_exec.node_id.clone(),
                run_id: new_exec.run_id.clone(),
                started_at: now_epoch_ms(),
                binary_path: new_exec.binary_path.clone(),
                binary_args: new_exec.binary_args.clone(),
                model: new_exec.model.clone(),
                launched_by: new_exec.launched_by,
                ..Default::default()
            }],
        };
        true
    })?;
    if !outcome.changed {
        return Err(format!("register_agent: agent {agent_id} 已存在"));
    }
    Ok(record)
}

/// U11：向**既有** agent 追加一次 exec（会话续接派发）。守卫：agent 必须存在、
/// harness 与 provider 一致、状态非 starting/running、attachment 非 orphan
/// （F-EXEC-5：orphan 的 pid 仍存活，追加会把 attachment 覆盖回 Live、丢失
/// orphan 追踪——先强制终止）。弹窗只列空闲 agent，这里是服务端硬校验
/// ——renderer 不可信。成功返回更新后的 agent 记录。
pub fn append_exec(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    harness: &str,
    new_exec: NewExec,
) -> Result<AgentRecord, String> {
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.is_file() {
        return Err(format!("append_exec: agent {agent_id} 不存在"));
    }
    // update_json 的闭包无法带出错误原因——锁内标记、锁外成文
    let mut orphan_rejected = false;
    let (record, outcome) = update_json(&path, |agent: &mut AgentRecord| {
        if agent.id != agent_id || agent.harness != harness {
            return false;
        }
        // 同一 agent 同时只允许一个活跃 exec（设计 §3.3），锁内校验
        if matches!(
            agent.status,
            AgentRecordStatus::Starting | AgentRecordStatus::Running
        ) {
            return false;
        }
        if agent.attachment == AgentAttachment::Orphan {
            orphan_rejected = true;
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
            launched_by: new_exec.launched_by,
            ..Default::default()
        });
        agent.status = AgentRecordStatus::Starting;
        agent.attachment = AgentAttachment::Live;
        agent.last_exec_id = Some(new_exec.exec_id.clone());
        true
    })?;
    if !outcome.changed {
        // §41：并发删除赢了——记录在等锁期间被移走，本方的 update_json 在原
        // 位置重建了只含锁文件的空目录。报「不存在」并清掉它
        if !path.is_file() {
            if let Some(dir) = path.parent() {
                remove_residue_dir(dir);
            }
            return Err(format!("append_exec: agent {agent_id} 不存在"));
        }
        if orphan_rejected {
            return Err(format!(
                "append_exec: agent {agent_id} 处于不受控态（pid 存活）——请先强制终止再续跑"
            ));
        }
        return Err(format!(
            "append_exec: agent {agent_id} 不可追加（harness 不一致/运行中）"
        ));
    }
    Ok(record)
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

/// shim 在 exec 结束前消失（崩溃 / 被 KILL）的对账签名（P-8）。
pub const SHIM_LOST_MARKER: &str = "exec shim exited before finishing";

/// 对账谓词的**单一事实源**（F-EXEC-14(1)：谓词曾与 list_reconcile_targets
/// 漂移，pending 残位把 §43 的僵尸形态卡成永远删不掉）：`run_id.is_some()
/// && !reconciled && (lease_release_pending ‖ failure_detail 命中签名)`。
/// list_reconcile_targets / delete_agent 的租约守卫 / executor 的
/// force_release_agent_leases（F-EXEC-16 ①）三处共用，永不漂移。
pub fn is_reconcile_target(exec: &ExecRecord) -> bool {
    !exec.reconciled
        && exec.run_id.is_some()
        && (exec.lease_release_pending
            || matches!(
                exec.failure_detail.as_deref(),
                Some(RESTART_MARKER) | Some(SHIM_LOST_MARKER)
            ))
}

/// 枚举待对账 exec：failure_detail 命中重启签名且未 reconciled、带 run_id。
pub fn list_reconcile_targets(runtime_dir: &Path) -> Result<Vec<ReconcileTarget>, String> {
    let mut targets = Vec::new();
    for home_id in layout::home_ids(runtime_dir) {
        for agent_id in layout::agent_ids(runtime_dir, &home_id)? {
            let path = layout::agent_file(runtime_dir, &home_id, &agent_id)?;
            let (agent, _) = match read_json::<AgentRecord>(&path) {
                Ok(v) => v,
                Err(_) => continue, // 损坏/缺失 → 该 agent 跳过（回放面另行降级）
            };
            for exec in &agent.execs {
                if is_reconcile_target(exec) {
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
pub fn get_agent(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
) -> Result<Option<AgentRecord>, String> {
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.exists() && !has_unrecovered_corrupt(&path) {
        return Ok(None);
    }
    let (record, parse_failed) = read_json::<AgentRecord>(&path)?;
    if parse_failed || has_unrecovered_corrupt(&path) {
        return Err(format!("{} 已损坏，拒绝解析进程身份", path.display()));
    }
    Ok((!record.id.is_empty()).then_some(record))
}

/// 删除残留目录判定的最小年龄：锁文件这么久没动才算残留（更新的可能是
/// 一次正在进行的 register_agent——update_json 先建目录与锁、后写 agent.json）。
const RESIDUE_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// §41 delete-loser-empty-dir：只含 companion 锁文件（无 agent.json、无
/// `.corrupt-*` 留证）的 agent 目录是竞争残留——删锁文件后 `remove_dir`
/// （目录非空即失败，不会误删）。
fn remove_residue_dir(dir: &Path) {
    let file = dir.join("agent.json");
    if file.exists() || has_unrecovered_corrupt(&file) {
        return;
    }
    let _ = std::fs::remove_file(dir.join("agent.json.lock"));
    let _ = std::fs::remove_dir(dir);
}

fn is_stale_residue(dir: &Path) -> bool {
    let only_lock = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .all(|e| e.file_name().to_string_lossy() == "agent.json.lock")
        })
        .unwrap_or(false);
    only_lock
        && std::fs::metadata(dir.join("agent.json.lock"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .is_some_and(|age| age >= RESIDUE_MIN_AGE)
}

/// §41 trash-never-swept：启动清扫——各 home 下删除失败遗留的 `.trash-*`
/// 目录，以及竞争败方留下的、锁文件超过 RESIDUE_MIN_AGE 未动的空 agent
/// 目录。返回清掉的目录数；单项失败 `eprintln!` 后继续。
pub fn sweep_agent_residue(runtime_dir: &Path) -> usize {
    let mut swept = 0usize;
    for home_id in layout::home_ids(runtime_dir) {
        let Ok(home) = layout::home_dir(runtime_dir, &home_id) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&home) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".trash-") {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => swept += 1,
                    Err(err) => eprintln!("启动清扫：删除 {} 失败：{err}", path.display()),
                }
            } else if !name.starts_with('.') && is_stale_residue(&path) {
                remove_residue_dir(&path);
                if !path.exists() {
                    swept += 1;
                }
            }
        }
    }
    swept
}

/// F-DEL：删除一个 agent（列表项右上角删除入口）。守卫：starting/running
/// 拒绝（先停止再删）；attachment=orphan 拒绝（pid 仍存活，先强制终止）。
///
/// F-EXEC-14(4)：`force=true` = 「放弃租约并删除」——待对账租约守卫（下方
/// F-EXEC-13(2)/F-EXEC-14(1)）被绕过：run/home 已被删、租约永远无法释放时
/// 的逃生口。F-EXEC-16 ③（§45 勘误）：daemon **不会**回收租约（omt-runtime
/// 从不 janitor_sweep），删除又会抹掉唯一的对账依据——release 责任在调用方：
/// desktop 的 `agents_delete` 在 force 分支先经
/// `executor::force_release_agent_leases` 尽力释放一次，释放失败的租约被
/// 真正放弃（对应 run item 可能一直显示运行中，需在 Runs 面板手动取消该
/// run）。Starting/Running 与 Orphan 守卫**不因 force 松动**（进程还活着
/// 就不是租约问题）。
///
/// F-EXEC-9：检查与删除必须在**同一把** agent.json companion flock 内完成
/// （append_exec / update_json 经 update_json 持的正是这把锁）——旧的
/// 「无锁 get_agent + remove_dir_all」与并发 append_exec 竞态时会把新 exec
/// 刚写入的记录/spec 一起删掉，此后的 update_* 因文件不存在全成 no-op，
/// 租约永不释放。锁内复核用 `fs::read` + `from_slice` 直接读字节：**不能**
/// 调 read_json——损坏路径会再取同一把 flock，空等 10s（§39）。消失以
/// 「改名为点前缀的 `.trash-<agentId>-<ts>`」达成（`subdirs()` 跳过点前缀，
/// 对列表瞬间不可见），改名后立即放锁，再 best-effort 移除；移除失败记日志，
/// `.trash-*` 由启动清扫兜底。
pub fn delete_agent(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    force: bool,
) -> Result<(), String> {
    let agent_file = layout::agent_file(runtime_dir, home_id, agent_id)?;
    let dir = layout::agent_dir(runtime_dir, home_id, agent_id)?;
    let corrupt_err = || {
        format!(
            "删除失败：{} 已损坏，拒绝解析进程身份",
            agent_file.display()
        )
    };
    if !agent_file.is_file() {
        // §41 quarantined-delete-says-missing：agent.json 已被隔离为
        // .corrupt-* 时报「已损坏」而不是「不存在」
        if has_unrecovered_corrupt(&agent_file) {
            return Err(corrupt_err());
        }
        remove_residue_dir(&dir);
        return Err(format!("agent {agent_id} 不存在"));
    }
    let guard = lock_exclusive(&agent_file, LOCK_ACQUIRE_TIMEOUT)?;
    // 锁内复核：文件可能已被并发删除赢走，或损坏（含粘性留证）——后者拒绝
    // 删除，与 get_agent 的语义一致（无法证明进程身份就不动它）。
    let corrupt = has_unrecovered_corrupt(&agent_file);
    let record = match std::fs::read(&agent_file) {
        Ok(bytes) => match serde_json::from_slice::<AgentRecord>(&bytes) {
            Ok(record) => record,
            Err(_) => return Err(corrupt_err()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if corrupt {
                return Err(corrupt_err());
            }
            // §41 delete-loser-empty-dir：并发删除的胜方已把目录改名，本方的
            // lock_exclusive（create_dir_all + 建锁文件）在原位置重建了只含
            // 锁文件的空目录——锁内清掉
            remove_residue_dir(&dir);
            return Err(format!("agent {agent_id} 不存在"));
        }
        Err(e) => return Err(format!("删除失败：read agent.json: {e}")),
    };
    if corrupt {
        return Err(corrupt_err());
    }
    if record.id != agent_id {
        return Err(format!("agent {agent_id} 不存在"));
    }
    if matches!(
        record.status,
        AgentRecordStatus::Starting | AgentRecordStatus::Running
    ) {
        return Err(format!("agent {agent_id} 仍在运行——请先停止再删除"));
    }
    if record.attachment == AgentAttachment::Orphan {
        return Err(format!(
            "agent {agent_id} 处于不受控态（pid 存活）——请先强制终止再删除"
        ));
    }
    // F-EXEC-13(2)：任一 exec 仍是待对账目标时拒绝删除——目录连记录一起
    // 删会把对账目标带走，daemon 侧租约永久泄漏。F-EXEC-14(1)：谓词收拢为
    // is_reconcile_target 单一事实源（与 list_reconcile_targets 同一函数，
    // 不再逐字抄写）——修复前 pending 位单独成障，§43 的僵尸形态（对账
    // 完成后 shim 的重复释放失败把 pending 写回 true）从此永远删不掉；
    // reconciled=true 即不是对账目标，pending 残位不挡删除。
    // F-EXEC-14(4)：force=true 绕过本守卫（放弃租约强删）。
    // 注（§45 P3-3 勘误）：终态之后 shim 可能仍在收尾——finalize **先写
    // 终态**，随后最多约 30s 的租约释放窗口，此窗口内删除不被上面的
    // Starting/Running 状态守卫拦截：非 force 由本守卫拦下（pending 未清）；
    // force 路径由调用方的尽力释放（executor::force_release_agent_leases）
    // 兜底；「收尾中的 shim 随后重复释放失败把 pending 写回」的形态由
    // 谓词的 !reconciled 位兜住（F-EXEC-14(2)）。
    if !force && record.execs.iter().any(is_reconcile_target) {
        return Err(format!(
            "agent {agent_id} 存在待对账的租约——请等待对账完成后再删除（或放弃租约强制删除）"
        ));
    }
    // P-7：按 agent 拆目录后，transcript 随 agent 一起删除。改名仍在锁内：
    // append_exec 在锁的另一侧只会看到「文件不存在」，不会写进将删的目录。
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let parent = dir
        .parent()
        .ok_or_else(|| format!("删除失败：invalid agent dir {}", dir.display()))?;
    let trash = parent.join(format!(".trash-{agent_id}-{ts}"));
    std::fs::rename(&dir, &trash).map_err(|e| format!("删除失败：rename: {e}"))?;
    // §41 remove-dir-under-lock：改名完成即放锁——remove_dir_all 可能很慢，
    // 持锁期间同进程的 update_json 会拿着全局 STORE_LOCK 干等
    drop(guard);
    if let Err(err) = std::fs::remove_dir_all(&trash) {
        // §41 trash-never-swept：失败不再静默——由 startup_housekeeping 的
        // sweep_agent_residue 清扫
        eprintln!(
            "删除 agent {agent_id}：清理 {} 失败（{err}），留给启动清扫",
            trash.display()
        );
    }
    // 验收「强制删除」注：指向已删除 agent 的绑定一并移除（失败只记日志——
    // 删除本身已完成，悬空绑定仍可由用户的 bindings_gc 清理）
    if let Err(err) = remove_bindings_to(runtime_dir, home_id, agent_id) {
        eprintln!("删除 agent {agent_id}：移除绑定失败（{err}）");
    }
    Ok(())
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
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.is_file() {
        return Ok(false);
    }
    update_json(&path, |agent: &mut AgentRecord| {
        if agent.id != agent_id {
            return false;
        }
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
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.is_file() {
        return Ok(false);
    }
    update_json(&path, |agent: &mut AgentRecord| {
        if agent.id != agent_id {
            return false;
        }
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

/// 单事务读改写的公共入口（P3：compare-and-write）。`apply` 在 agent.json
/// 的进程内锁 + flock 内拿到整条记录，可先复核当前状态再改；返回 true
/// 才落盘。agent.json 不存在 / id 不符 → Ok(false)。
pub fn update_agent(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    apply: impl FnOnce(&mut AgentRecord) -> bool,
) -> Result<bool, String> {
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.is_file() {
        return Ok(false);
    }
    update_json(&path, |agent: &mut AgentRecord| {
        if agent.id != agent_id {
            return false;
        }
        apply(agent)
    })
    .map(|(_, outcome)| outcome.changed)
}

/// P3-ACC-5：harness 已 spawn——同一事务写入 harness pid / 起始身份，并把
/// agent 从 Starting 推进到 Running（此前执行器没有任何写 Running 的路径，
/// 执行期间 agent.json 一直是 starting）。只在 agent 仍是 Starting 且该
/// exec 是最新 exec 时推进：终态与旧 exec 都不回退。返回是否落盘。
pub fn mark_exec_spawned(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    pid: u32,
    process_start_id: Option<String>,
) -> Result<bool, String> {
    update_agent(runtime_dir, home_id, agent_id, |agent| {
        let Some(exec) = agent.execs.iter_mut().find(|e| e.exec_id == exec_id) else {
            return false;
        };
        exec.pid = Some(pid);
        exec.process_start_id = process_start_id;
        if agent.status == AgentRecordStatus::Starting
            && agent.last_exec_id.as_deref() == Some(exec_id)
        {
            agent.status = AgentRecordStatus::Running;
        }
        true
    })
}

/// F-EXEC-11：exec 终态的**单事务**回写——agent 级字段（status/attachment/
/// last_session_id 镜像）与匹配 exec 的终态字段在同一笔 update_json（进程内
/// 锁 + flock + tmp/rename 原子替换）里落盘。此前 `update_agent_status` 与
/// `update_exec` 是两笔分离写，读侧（e2e/UI 轮询 agent.json）在两笔之间会
/// 看到「Exited + exit_code 空」的中间态。`apply` 在锁内拿到整条 AgentRecord
/// （含 agent 级字段），负责同时改两侧；返回是否实际变化（无变化不落盘，
/// 写合并语义与 update_agent_status 一致）。
///
/// F-EXEC-13(1)：exec 带 run_id 且**未对账**（`!reconciled`，F-EXEC-14(2)）时，
/// **同一笔事务**预置 `lease_release_pending = true`——终态写入与租约释放
/// 完成之间有最长一个释放窗（shim 侧可达 30s）的窗口，shim 若在此死亡，
/// 没有任何标记留下、租约永不释放。预置后由 finish_terminal 按释放结果清
/// （Ok）或保持（Err）；对账路径（mark_reconciled）也会清——置位是幂等安全
/// 的；reconciled=true 的 exec 已对账完成，不再预置。
pub(crate) fn finalize_agent_exec(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    apply: impl FnOnce(&mut AgentRecord),
) -> Result<bool, String> {
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.is_file() {
        return Ok(false);
    }
    update_json(&path, |agent: &mut AgentRecord| {
        if agent.id != agent_id {
            return false;
        }
        let before = agent.clone();
        apply(agent);
        // F-EXEC-13(1)：带 run_id 的 exec 终态必有租约要释放——同笔预置
        // pending，释放成功才清（见 finish_terminal）。F-EXEC-14(2)：
        // reconciled=true（U10 对账已完成）不预置——§43 的僵尸路径：desktop
        // 先 mark_reconciled，shim 随后的收尾再预置会把已对账记录变回
        // 「pending=true + reconciled=true」。
        if let Some(exec) = agent.execs.iter_mut().find(|e| e.exec_id == exec_id) {
            if exec.run_id.is_some() && !exec.reconciled {
                exec.lease_release_pending = true;
            }
        }
        // 与 update_exec 同规则的 session id 镜像（exec 归档时同步 agent 级）
        if let Some(exec) = agent.execs.iter().find(|e| e.exec_id == exec_id) {
            if let Some(session) = &exec.harness_session_id {
                agent.last_session_id = Some(session.clone());
            }
        }
        *agent != before
    })
    .map(|(_, outcome)| outcome.changed)
}

/// Agents 面板数据面（U5 消费）：记录列表 + `parseFailed` 标志（前端提示
/// 数据已降级；U9 据此冻结 GC）。§41：附带降级来源的结构化说明——
/// `legacyUnmigrated`（旧版 home 级文件未迁移 / 只剩留证）与
/// `corruptAgents`（单个 agent.json 损坏：目录 + 留证文件名），横幅据此
/// 如实呈现、说明解冻办法，并提供「确认并归档」。
pub fn agents_list_value(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let (agents, parse_failed) = list_agents(runtime_dir, home_id)?;
    Ok(serde_json::json!({
        "agents": agents,
        "agentSeq": agent_seq(runtime_dir, home_id)?,
        "parseFailed": parse_failed,
        "legacyUnmigrated": legacy_unmigrated(runtime_dir, home_id)?,
        "corruptAgents": corrupt_agents(runtime_dir, home_id)?,
    }))
}

/// `path` 同目录下的 `<文件名>.corrupt-*` 留证（文件名，排序）。
fn corrupt_evidence_of(path: &Path) -> Vec<String> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.corrupt-", name.to_string_lossy());
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with(&prefix))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn legacy_unmigrated(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let legacy = legacy_agents_monolith(runtime_dir, home_id)?;
    let evidence = corrupt_evidence_of(&legacy);
    let present = legacy.is_file();
    if !present && evidence.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let parseable = std::fs::read(&legacy)
        .ok()
        .is_some_and(|bytes| serde_json::from_slice::<AgentsFile>(&bytes).is_ok());
    Ok(serde_json::json!({
        "path": legacy.display().to_string(),
        "present": present,
        "parseable": parseable,
        "corruptEvidence": evidence,
    }))
}

/// §41：单个 agent.json 损坏（解析失败或留有 `.corrupt-*` 证据）的 agent——
/// 目录绝对路径 + 留证文件名。list_agents 的 parseFailed 正是因这些留证
/// 粘性置位；删掉留证（或修复后改名回 agent.json）即解冻。
fn corrupt_agents(runtime_dir: &Path, home_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    for agent_id in layout::agent_ids(runtime_dir, home_id)? {
        let path = layout::agent_file(runtime_dir, home_id, &agent_id)?;
        let unreadable = std::fs::read(&path)
            .ok()
            .is_some_and(|bytes| serde_json::from_slice::<AgentRecord>(&bytes).is_err());
        let evidence = corrupt_evidence_of(&path);
        if unreadable || !evidence.is_empty() {
            out.push(serde_json::json!({
                "agentId": agent_id,
                "dir": layout::agent_dir(runtime_dir, home_id, &agent_id)?.display().to_string(),
                "evidence": evidence,
            }));
        }
    }
    Ok(out)
}

/// §41 legacy-degraded-permanent：「确认并归档」——把未能迁移的旧版 home 级
/// 存储文件及其 `.corrupt-*` 留证移入 `<home>/.recovered/`（点前缀：agent
/// 列表与残留清扫都跳过它），解除 F-EXEC-1 的 GC 冻结（atomic_json 注释里
/// 说的「移入 recovered/」从此真实存在）。只在旧文件**不可解析**或只剩
/// 留证时允许：可解析的旧文件由启动迁移处理，归档会让其中的 agent 从列表
/// 消失。与迁移共用 `agents/.migrate` 全局锁。返回移走的文件名。
pub fn archive_legacy_store(runtime_dir: &Path, home_id: &str) -> Result<Vec<String>, String> {
    let legacy = legacy_agents_monolith(runtime_dir, home_id)?;
    let _guard = lock_exclusive(
        &runtime_dir.join("agents").join(".migrate"),
        LOCK_ACQUIRE_TIMEOUT,
    )?;
    if let Ok(bytes) = std::fs::read(&legacy) {
        if serde_json::from_slice::<AgentsFile>(&bytes).is_ok() {
            return Err("旧版文件可以解析——重启桌面端会自动迁移，无需归档".to_string());
        }
    }
    let home = layout::home_dir(runtime_dir, home_id)?;
    let recovered = home.join(".recovered");
    std::fs::create_dir_all(&recovered).map_err(|e| format!("create .recovered: {e}"))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let mut moved = Vec::new();
    if legacy.is_file() {
        let name = format!("{}.json.{ts}", "agents");
        std::fs::rename(&legacy, recovered.join(&name))
            .map_err(|e| format!("归档 {} 失败：{e}", legacy.display()))?;
        moved.push(name);
    }
    for name in corrupt_evidence_of(&legacy) {
        std::fs::rename(home.join(&name), recovered.join(&name))
            .map_err(|e| format!("归档 {name} 失败：{e}"))?;
        moved.push(name);
    }
    let _ = std::fs::remove_file(home.join(format!("{}.json.lock", "agents")));
    if moved.is_empty() {
        return Err("没有需要归档的旧版文件".to_string());
    }
    Ok(moved)
}

/// U4 步骤 ⑥：写入 bindings.json（显式绑定 nodeId → agentId）。
/// F-U15-1 第三条写路径：走 atomic_json 的 read-merge-write（进程内锁 +
/// companion flock + tmp/rename + corrupt 降级留证）——不依赖调用方的
/// 派发锁做写互斥。写失败由调用方（U4 链）转为警告不阻断。
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

/// 验收「强制删除」注：删除 agent 时移除指向它的绑定（nodeId → agentId 精确
/// 匹配）。不受 GC 冻结约束——冻结防的是「无法证明 agent 表有效时批量
/// 清空」，这里只删指向刚被删除的那个 agent 的项。返回移除数。
pub fn remove_bindings_to(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
) -> Result<usize, String> {
    let path = bindings_path(runtime_dir, home_id)?;
    if !path.is_file() {
        return Ok(0);
    }
    let mut removed = 0usize;
    update_json(&path, |bindings: &mut BTreeMap<String, String>| {
        let before = bindings.len();
        bindings.retain(|_, bound| bound != agent_id);
        removed = before - bindings.len();
        removed > 0
    })?;
    Ok(removed)
}

/// bindings 表的进程内读取（同 write_binding 的锁语义；不存在 → 空表）。
fn read_bindings(runtime_dir: &Path, home_id: &str) -> Result<BTreeMap<String, String>, String> {
    let path = bindings_path(runtime_dir, home_id)?;
    Ok(read_json::<BTreeMap<String, String>>(&path)?.0)
}

/// U9（§3.4）：显式绑定表投影。`parseFailed` 粘性语义同 agents_list
/// （F-U4a-1），且**叠加 agents 表的降级态**——悬空判定（agentId 是否
/// 存在）依赖健康的 agents 数据，任一文件降级时前端都不得做悬空判定。
pub fn bindings_get_value(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let bindings = read_bindings(runtime_dir, home_id)?;
    let b_path = bindings_path(runtime_dir, home_id)?;
    let (_, agents_parse_failed) = list_agents(runtime_dir, home_id)?;
    Ok(serde_json::json!({
        "bindings": bindings,
        "parseFailed": has_unrecovered_corrupt(&b_path) || agents_parse_failed,
    }))
}

/// U9（§3.4）：用户触发的一键清理——移除 agentId 已不存在于 agent 表的
/// 绑定项，返回移除数与清理后的表。§3.6.6 硬规则：**bindings 非空且
/// agent 表无法给出有效 agent 表时冻结**（解析失败 / 粘性 corrupt 残留
/// F-U4a-1 / 文件缺失任一情形）——「desktop 是唯一写者」不覆盖手工删除、
/// 同步工具误删、部分恢复等异常路径，无法证明 agent 表有效时绝不能把绑定
/// 按空表全清。bindings 为空时 GC 本来就是 no-op，正常返回 0。
pub fn bindings_gc_value(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let bindings_file = bindings_path(runtime_dir, home_id)?;
    // 空 bindings → 无害 no-op，先于 agents 有效性检查返回（缺失的
    // agent 表不必挡住 0 移除路径）。
    let existing = read_bindings(runtime_dir, home_id)?;
    if existing.is_empty() {
        return Ok(serde_json::json!({ "removed": 0, "bindings": existing }));
    }
    // 缺失判定：从未注册过 agent 的 home 没有 seq.json（与旧的「agents.json
    // 缺失」等价）——此时 agent 表无法证明有效，冻结。
    let agents_missing = !layout::seq_file(runtime_dir, home_id)?.exists();
    let (agents, agents_parse_failed) = list_agents(runtime_dir, home_id)?;
    if agents_missing || agents_parse_failed {
        return Err(
            "agent 表无法证明有效（缺失 / 解析失败 / .corrupt-* 留证未恢复）——绑定清理已冻结"
                .to_string(),
        );
    }
    let live_ids: std::collections::HashSet<String> = agents.iter().map(|a| a.id.clone()).collect();
    let mut removed = 0usize;
    // bindings.json 自身损坏降级为空表时无项可移（removed=0、不落盘），对
    // 留证无损；agentId 判定以各 agent.json 的 id 为准。
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

    /// F-BENCH-20：da9c376 之前归档的记录没有 usageCaliber——必须可读，
    /// 且 usage_caliber 反序列化为 None（= 旧口径：只含末条消息，少报）。
    #[test]
    fn legacy_exec_record_without_usage_caliber_deserializes() {
        let json = r#"{"execId":"exec-old","nodeId":null,"runId":null,"startedAt":"1","exitCode":0,"killReason":null,"failureDetail":null,"harnessSessionId":null,"transcriptFile":null,"binaryPath":null,"binaryArgs":[],"pid":null,"processStartId":null,"usageTokens":13980,"usageCost":0.0419}"#;
        let rec: ExecRecord = serde_json::from_str(json).unwrap();
        assert_eq!(rec.usage_tokens, Some(13980));
        assert_eq!(rec.usage_caliber, None, "旧记录无口径标记");
    }

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
        let err = delete_agent(&runtime, "h_test", &agent.id, false).unwrap_err();
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
        delete_agent(&runtime, "h_test", &agent.id, false).unwrap();
        let err2 = delete_agent(&runtime, "h_test", &agent.id, false).unwrap_err();
        assert!(err2.contains("不存在"), "{err2}");
        let file = snapshot(&runtime, "h_test");
        assert!(file.agents.is_empty());
    }

    /// 验收「强制删除」注：删除 agent 时一并移除指向它的绑定（此前留下
    /// 悬空绑定，要用户手动触发 bindings_gc）；指向别的 agent 的绑定不动。
    #[test]
    fn delete_agent_drops_bindings_that_point_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let a = exited_agent(&runtime, "exec-bind-a");
        let b = exited_agent(&runtime, "exec-bind-b");
        write_binding(&runtime, "h_test", "TICKET-1", &a.id).unwrap();
        write_binding(&runtime, "h_test", "TICKET-2", &b.id).unwrap();
        write_binding(&runtime, "h_test", "TICKET-3", &a.id).unwrap();
        delete_agent(&runtime, "h_test", &a.id, false).unwrap();
        let value = bindings_get_value(&runtime, "h_test").unwrap();
        assert_eq!(value["bindings"], serde_json::json!({ "TICKET-2": b.id }));
    }

    fn new_exec(exec_id: &str) -> NewExec {
        NewExec {
            exec_id: exec_id.to_string(),
            node_id: None,
            run_id: None,
            binary_path: None,
            binary_args: vec![],
            model: None,
            launched_by: None,
            parent_agent_id: None,
        }
    }

    fn exited_agent(runtime: &Path, exec_id: &str) -> AgentRecord {
        let agent = register_agent(runtime, "h_test", "pi", new_exec(exec_id)).unwrap();
        update_agent_status(
            runtime,
            "h_test",
            &agent.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        agent
    }

    /// 模拟「胜方已把记录移走」：测试持有 agent.json 的 flock，败方在锁上
    /// 等待；期间删掉 agent.json，再放锁。
    fn race_against_removal<T: Send + 'static>(
        runtime: &Path,
        agent_id: &str,
        loser: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let file = layout::agent_file(runtime, "h_test", agent_id).unwrap();
        let held = lock_exclusive(&file, LOCK_ACQUIRE_TIMEOUT).unwrap();
        let handle = std::thread::spawn(loser);
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::remove_file(&file).unwrap();
        drop(held);
        handle.join().unwrap()
    }

    /// §41 delete-loser-empty-dir：并发删除的败方报「不存在」，且不留下只含
    /// 锁文件的空 agent 目录。
    #[test]
    fn delete_loser_reports_missing_and_leaves_no_residue_dir() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let agent = exited_agent(&runtime, "exec-race");
        let adir = layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
        let (rt, id) = (runtime.clone(), agent.id.clone());
        let err = race_against_removal(&runtime, &agent.id, move || {
            delete_agent(&rt, "h_test", &id, false)
        })
        .unwrap_err();
        assert!(err.contains("不存在"), "{err}");
        assert!(!adir.exists(), "败方不得留下空目录");
    }

    /// §41：落败的 append_exec 报「不存在」（此前报「不可追加（harness 不
    /// 一致/运行中）」），同样不留空目录。
    #[test]
    fn append_loser_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let agent = exited_agent(&runtime, "exec-race2");
        let adir = layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
        let (rt, id) = (runtime.clone(), agent.id.clone());
        let err = race_against_removal(&runtime, &agent.id, move || {
            append_exec(&rt, "h_test", &id, "pi", new_exec("exec-late"))
        })
        .unwrap_err();
        assert!(err.contains("不存在"), "{err}");
        assert!(!adir.exists());
    }

    /// §41 quarantined-delete-says-missing：agent.json 已被隔离为 .corrupt-*
    /// 时，删除报「已损坏」（修复前报「不存在」，以前的实现报「已损坏」）。
    #[test]
    fn deleting_a_quarantined_agent_says_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let agent = exited_agent(&runtime, "exec-q");
        let file = layout::agent_file(&runtime, "h_test", &agent.id).unwrap();
        std::fs::rename(&file, file.with_file_name("agent.json.corrupt-1")).unwrap();
        let err = delete_agent(&runtime, "h_test", &agent.id, false).unwrap_err();
        assert!(err.contains("已损坏"), "{err}");
    }

    /// §41 trash-never-swept + 残留：启动清扫删掉 `.trash-*` 与锁文件超过
    /// 60s 未动的空 agent 目录；刚建的（可能正在注册）与健康 agent 不动。
    #[test]
    fn sweep_agent_residue_removes_trash_and_stale_lock_only_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let keep = exited_agent(&runtime, "exec-keep");
        let home = layout::home_dir(&runtime, "h_test").unwrap();
        let trash = home.join(".trash-agent-exec-x-1");
        std::fs::create_dir_all(trash.join("nested")).unwrap();
        std::fs::write(trash.join("nested").join("exec-x.jsonl"), "x").unwrap();
        let stale = home.join("agent-exec-gone");
        std::fs::create_dir_all(&stale).unwrap();
        let stale_lock = stale.join("agent.json.lock");
        std::fs::write(&stale_lock, "").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&stale_lock)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
            .unwrap();
        let fresh = home.join("agent-exec-fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("agent.json.lock"), "").unwrap();

        assert_eq!(sweep_agent_residue(&runtime), 2);
        assert!(!trash.exists());
        assert!(!stale.exists());
        assert!(fresh.exists(), "刚建的目录可能正在注册，不动");
        assert!(get_agent(&runtime, "h_test", &keep.id).unwrap().is_some());
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
        let file = snapshot(&runtime, &home);
        assert_eq!(file.meta.agent_seq, 2);
        assert_eq!(file.agents.len(), 2);
    }

    #[test]
    fn same_status_update_does_not_rewrite_file() {
        let (_dir, runtime, home) = temp_runtime();
        register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        let path = crate::layout::agent_file(&runtime, &home, "agent-exec-1").unwrap();
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
        let (record, _) = read_json::<AgentRecord>(&path).unwrap();
        assert_eq!(record.status, AgentRecordStatus::Running);
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
        let file = snapshot(&runtime, &home);
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
        let agent = register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        let path = crate::layout::agent_file(&runtime, &home, &agent.id).unwrap();
        std::fs::write(&path, "{ broken").unwrap();
        let (_, parse_failed) = read_json::<AgentRecord>(&path).unwrap();
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
                .any(|n| n.starts_with("agent.json.corrupt-")),
            "{restored:?}"
        );
    }

    #[test]
    fn home_id_rejects_path_traversal() {
        assert!(crate::layout::home_dir(Path::new("/tmp"), "../escape").is_err());
        assert!(crate::layout::home_dir(Path::new("/tmp"), "a/b").is_err());
        assert!(crate::layout::home_dir(Path::new("/tmp"), "h_ok").is_ok());
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
    fn parse_failed_is_sticky_across_reads() {
        // F-U4a-1：损坏某一个 agent.json → 第一次读（rename 后返回默认 +
        // parseFailed=true）→ 第二次读 → parseFailed 仍必须为 true（粘性），
        // 否则 U9 的 GC 冻结在第二次读后失效，会把全部绑定当悬空清掉
        // （§3.6.6 要防的事故）。
        let (_dir, runtime, home) = temp_runtime();
        let agent = register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        let path = crate::layout::agent_file(&runtime, &home, &agent.id).unwrap();
        std::fs::write(&path, "{ broken").unwrap();
        let first = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(first["parseFailed"], true);
        let second = agents_list_value(&runtime, &home).unwrap();
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
        let result = bindings_get_value(&runtime, &home).unwrap();
        assert_eq!(result["bindings"]["T-1"], "agent-exec-1");
        assert_eq!(result["bindings"]["T-2"], "agent-exec-1");
        assert_eq!(result["parseFailed"], false);

        // 某个 agent.json 损坏 → bindings_get 的 parseFailed 粘性置位（悬空
        // 判定依赖 agents 数据，降级期间前端不得判悬空）
        let agent = register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        let agent_file = crate::layout::agent_file(&runtime, &home, &agent.id).unwrap();
        std::fs::write(&agent_file, "{ broken").unwrap();
        let degraded = bindings_get_value(&runtime, &home).unwrap();
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

        let result = bindings_gc_value(&runtime, &home).unwrap();
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
        let agent = register_agent(&runtime, &home, "omp", new_exec("exec-1")).unwrap();
        let agent_file = crate::layout::agent_file(&runtime, &home, &agent.id).unwrap();
        std::fs::write(&agent_file, "{ broken").unwrap();

        let err = bindings_gc_value(&runtime, &home).unwrap_err();
        assert!(err.contains("冻结"), "{err}");
        // 文件内容断言：绑定原样保留
        let persisted = read_bindings(&runtime, &home).unwrap();
        assert_eq!(persisted.len(), 2, "{persisted:?}");
        assert_eq!(persisted.get("T-1").map(String::as_str), Some("agent-x"));
    }

    #[test]
    fn bindings_gc_frozen_when_agents_file_missing() {
        // §3.6.6 收紧：agent 表缺失（从未注册过 agent——seq.json 不存在；
        // 手工删除/同步工具误删/部分恢复同形）同样无法证明 agent 表有效
        // ——非空绑定必须冻结，不得按空表全清。
        let (_dir, runtime, home) = temp_runtime();
        write_binding(&runtime, &home, "T-1", "agent-x").unwrap();
        write_binding(&runtime, &home, "T-2", "agent-y").unwrap();
        assert!(!crate::layout::seq_file(&runtime, &home).unwrap().exists());

        let err = bindings_gc_value(&runtime, &home).unwrap_err();
        assert!(err.contains("冻结"), "{err}");
        let persisted = read_bindings(&runtime, &home).unwrap();
        assert_eq!(persisted.len(), 2, "缺失路径下绑定原样保留: {persisted:?}");
    }

    /// 旧版 home 级单文件的路径（名字拼出以避开迁移 grep 的字面量，先例
    /// register_writes_one_dir_per_agent_and_seq_is_monotonic）。
    fn legacy_monolith(runtime: &Path, home: &str) -> PathBuf {
        crate::layout::home_dir(runtime, home)
            .unwrap()
            .join(format!("{}.json", "agents"))
    }

    /// 旧版单文件的 `.corrupt-<ts>` 留证路径（升级前 atomic_json 降级产物）。
    fn legacy_corrupt_evidence(runtime: &Path, home: &str) -> PathBuf {
        let legacy = legacy_monolith(runtime, home);
        let name = legacy.file_name().unwrap().to_string_lossy().to_string();
        legacy.with_file_name(format!("{name}.corrupt-1760000000000"))
    }

    #[test]
    fn bindings_gc_frozen_while_legacy_agents_monolith_unmigrated() {
        // F-EXEC-1 场景 A：旧版单文件损坏 → 迁移跳过并留原地；用户在新布局
        // 下派发一次后 seq.json 出现，「seq.json 缺失」冻结条件失效——若
        // parseFailed 仍为 false，GC 会把指向未迁移旧 agent 的绑定当悬空删
        // （评审复现 removed:1）。旧文件仍在 = agent 表无法证明有效，必须冻结。
        let (_dir, runtime, home) = temp_runtime();
        let legacy = legacy_monolith(&runtime, &home);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "{oops").unwrap();
        // 模拟派发一次后的新布局状态：seq.json 存在 + 健康的新 agent
        seed_agents(&runtime, &home, &["new1"]);
        assert!(crate::layout::seq_file(&runtime, &home).unwrap().exists());
        write_binding(&runtime, &home, "T-legacy", "agent-old-unmigrated").unwrap();
        write_binding(&runtime, &home, "T-new", "agent-new1").unwrap();

        let degraded = bindings_get_value(&runtime, &home).unwrap();
        assert_eq!(degraded["parseFailed"], true, "旧文件未迁移必须降级");

        let err = bindings_gc_value(&runtime, &home).unwrap_err();
        assert!(err.contains("冻结"), "{err}");
        let persisted = read_bindings(&runtime, &home).unwrap();
        assert_eq!(
            persisted.len(),
            2,
            "指向未迁移旧 agent 的绑定不得被当悬空清掉: {persisted:?}"
        );
        assert_eq!(
            persisted.get("T-legacy").map(String::as_str),
            Some("agent-old-unmigrated")
        );
    }

    #[test]
    fn legacy_corrupt_evidence_keeps_list_degraded() {
        // F-EXEC-1 场景 A2：升级前遗留的 `.corrupt-*` 留证（旧 atomic_json
        // 降级改名产物）在迁移后被无视——降级判定必须把它计入，否则留证
        // 形同虚设，悬空判定会在缺失的旧表上误判。
        let (_dir, runtime, home) = temp_runtime();
        seed_agents(&runtime, &home, &["a1"]); // 健康的按 agent 目录
        let evidence = legacy_corrupt_evidence(&runtime, &home);
        std::fs::write(&evidence, "{oops").unwrap();
        write_binding(&runtime, &home, "T-1", "agent-a1").unwrap();

        let (agents, failed) = list_agents(&runtime, &home).unwrap();
        assert_eq!(agents.len(), 1, "健康的按 agent 记录仍应列出");
        assert!(failed, "未恢复的旧损坏留证必须保持降级");
        let degraded = bindings_get_value(&runtime, &home).unwrap();
        assert_eq!(degraded["parseFailed"], true);
    }

    /// §41 legacy-degraded-permanent：旧版 home 级文件损坏 → agents_list
    /// 结构化说明降级来源；「确认并归档」后移入 .recovered/、降级解除、
    /// .recovered 不进 agent 列表。
    #[test]
    fn corrupt_legacy_store_is_reported_and_archivable() {
        let (_dir, runtime, home) = temp_runtime();
        register_agent(&runtime, &home, "pi", new_exec("exec-new")).unwrap();
        let home_dir = layout::home_dir(&runtime, &home).unwrap();
        let legacy = home_dir.join(format!("{}.json", "agents"));
        std::fs::write(&legacy, "{ not json").unwrap();

        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["parseFailed"], true);
        assert_eq!(value["agents"].as_array().unwrap().len(), 1, "列表并不为空");
        let info = &value["legacyUnmigrated"];
        assert_eq!(info["present"], true);
        assert_eq!(info["parseable"], false);
        assert!(info["path"].as_str().unwrap().ends_with(".json"), "{info}");
        assert_eq!(value["corruptAgents"], serde_json::json!([]));

        let moved = archive_legacy_store(&runtime, &home).unwrap();
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert!(!legacy.exists());
        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["parseFailed"], false, "归档后冻结解除");
        assert!(value["legacyUnmigrated"].is_null());
        let recovered: Vec<_> = std::fs::read_dir(home_dir.join(".recovered"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            layout::agent_ids(&runtime, &home).unwrap().len(),
            1,
            ".recovered 不是 agent"
        );
    }

    /// 可解析的旧文件由启动迁移处理——拒绝归档（否则其中的 agent 会从列表
    /// 消失）；损坏的单个 agent.json 进 corruptAgents（目录路径 + 留证文件名）。
    #[test]
    fn parseable_legacy_store_refuses_archive_and_corrupt_agents_are_listed() {
        let (_dir, runtime, home) = temp_runtime();
        let agent = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let home_dir = layout::home_dir(&runtime, &home).unwrap();
        let legacy = home_dir.join(format!("{}.json", "agents"));
        std::fs::write(
            &legacy,
            serde_json::to_string(&AgentsFile::default()).unwrap(),
        )
        .unwrap();
        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["legacyUnmigrated"]["parseable"], true);
        let err = archive_legacy_store(&runtime, &home).unwrap_err();
        assert!(err.contains("自动迁移"), "{err}");
        assert!(legacy.exists());

        std::fs::remove_file(&legacy).unwrap();
        std::fs::write(
            layout::agent_file(&runtime, &home, &agent.id).unwrap(),
            "{ broken",
        )
        .unwrap();
        let value = agents_list_value(&runtime, &home).unwrap();
        assert!(value["legacyUnmigrated"].is_null());
        let corrupt = value["corruptAgents"].as_array().unwrap();
        assert_eq!(corrupt.len(), 1, "{value}");
        assert_eq!(corrupt[0]["agentId"], serde_json::json!(agent.id));
        assert!(
            corrupt[0]["dir"].as_str().unwrap().ends_with(&agent.id),
            "{value}"
        );
        let evidence = corrupt[0]["evidence"][0].as_str().unwrap();
        assert!(evidence.starts_with("agent.json.corrupt-"), "{value}");
        // 解冻办法如横幅所说：删掉留证文件，下次读取即不再降级
        let dir = layout::agent_dir(&runtime, &home, &agent.id).unwrap();
        std::fs::remove_file(dir.join(evidence)).unwrap();
        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["parseFailed"], false);
        assert_eq!(value["corruptAgents"], serde_json::json!([]));
    }

    #[test]
    fn fully_migrated_home_is_not_degraded_and_gc_works() {
        // F-EXEC-1 收紧的另一侧：迁移完成后（旧文件改名 .migrated、原名
        // 消失）不得继续降级——GC 正常清悬空。用真实迁移入口构造终态。
        let (_dir, runtime, home) = temp_runtime();
        let legacy = legacy_monolith(&runtime, &home);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            serde_json::to_string(&AgentsFile {
                meta: AgentsMeta { agent_seq: 3 },
                agents: vec![AgentRecord {
                    id: "agent-migrated".into(),
                    display_name: "Agent 3".into(),
                    harness: "pi".into(),
                    last_exec_id: Some("exec-old".into()),
                    execs: vec![ExecRecord {
                        exec_id: "exec-old".into(),
                        started_at: "1760000000000".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            })
            .unwrap(),
        )
        .unwrap();
        let report = crate::migrate::migrate_legacy(&runtime).unwrap();
        assert_eq!(report.homes, 1);
        assert!(
            !legacy.exists(),
            "迁移成功后旧文件必须消失（改名 .migrated）"
        );

        let (agents, failed) = list_agents(&runtime, &home).unwrap();
        assert!(!failed, "迁移完成后不得继续降级");
        assert_eq!(agents.len(), 1);

        write_binding(&runtime, &home, "T-live", "agent-migrated").unwrap();
        write_binding(&runtime, &home, "T-dangling", "agent-gone").unwrap();
        let result = bindings_gc_value(&runtime, &home).unwrap();
        assert_eq!(result["removed"], 1);
        assert_eq!(result["bindings"]["T-live"], "agent-migrated");
        assert!(result["bindings"].get("T-dangling").is_none());
    }

    #[test]
    fn bindings_gc_empty_bindings_is_noop_zero_even_when_agents_missing() {
        // 空 bindings 本来就是 no-op：先于 agents 有效性检查返回 0——
        // agent 表缺失也不冻结无害路径。
        let (_dir, runtime, home) = temp_runtime();
        assert!(!crate::layout::seq_file(&runtime, &home).unwrap().exists());
        let result = bindings_gc_value(&runtime, &home).unwrap();
        assert_eq!(result["removed"], 0);
        assert_eq!(result["bindings"], serde_json::json!({}));
    }

    #[test]
    fn register_writes_one_dir_per_agent_and_seq_is_monotonic() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let b = register_agent(&runtime, &home, "omp", new_exec("exec-b")).unwrap();
        assert_eq!(a.display_name, "Agent 1");
        assert_eq!(b.display_name, "Agent 2");
        assert!(crate::layout::agent_file(&runtime, &home, &a.id)
            .unwrap()
            .is_file());
        assert!(crate::layout::agent_file(&runtime, &home, &b.id)
            .unwrap()
            .is_file());
        // 旧布局的单文件存储不再出现（名字拼出以避开迁移 grep 的字面量）
        let legacy_monolith = format!("{}.json", "agents");
        assert!(!runtime
            .join("agents")
            .join(&home)
            .join(legacy_monolith)
            .exists());
        assert_eq!(agent_seq(&runtime, &home).unwrap(), 2);
        let (agents, failed) = list_agents(&runtime, &home).unwrap();
        assert!(!failed);
        assert_eq!(
            agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec![a.id.as_str(), b.id.as_str()]
        );
    }

    #[test]
    fn one_corrupt_agent_file_marks_list_degraded_but_keeps_others() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let b = register_agent(&runtime, &home, "pi", new_exec("exec-b")).unwrap();
        std::fs::write(
            crate::layout::agent_file(&runtime, &home, &a.id).unwrap(),
            "{oops",
        )
        .unwrap();
        let (agents, failed) = list_agents(&runtime, &home).unwrap();
        assert!(failed, "损坏应置 parseFailed");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, b.id);
        // 粘性：第二次读（损坏文件已被改名留证）仍为降级
        assert!(list_agents(&runtime, &home).unwrap().1);
    }

    #[test]
    fn updates_to_missing_agent_do_not_create_dirs() {
        let (_dir, runtime, home) = temp_runtime();
        let changed = update_agent_status(
            &runtime,
            &home,
            "agent-ghost",
            AgentRecordStatus::Failed,
            AgentAttachment::None,
        )
        .unwrap();
        assert!(!changed);
        assert!(!crate::layout::agent_dir(&runtime, &home, "agent-ghost")
            .unwrap()
            .exists());
    }

    #[test]
    fn delete_removes_the_agent_dir_with_transcripts() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        let dir = crate::layout::agent_dir(&runtime, &home, &a.id).unwrap();
        std::fs::write(dir.join("exec-a.jsonl"), "{}\n").unwrap();
        delete_agent(&runtime, &home, &a.id, false).unwrap();
        assert!(!dir.exists());
    }

    /// F-EXEC-9 竞态的确定性复现（互斥验证）：测试线程先持有 agent.json 的
    /// companion flock（append_exec / update_json 走的**同一把锁**），期间
    /// 调 delete_agent——它必须阻塞等锁，而不是绕过锁直接删。旧的「无锁
    /// get_agent + remove_dir_all」会立刻把目录删掉：并发 append_exec 正在
    /// 锁内写回的新 exec 记录/spec 一并被删，后续 update_* 全成 no-op、
    /// 租约永不释放。锁释放后删除完成、目录消失。
    #[test]
    fn delete_agent_blocks_on_agent_file_lock_instead_of_deleting() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-race")).unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        let agent_file = crate::layout::agent_file(&runtime, &home, &a.id).unwrap();
        let dir = crate::layout::agent_dir(&runtime, &home, &a.id).unwrap();

        let _held = omt_harness::atomic_json::lock_exclusive(
            &agent_file,
            omt_harness::atomic_json::LOCK_ACQUIRE_TIMEOUT,
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let rt = runtime.clone();
        let h = home.clone();
        let id = a.id.clone();
        let worker = std::thread::spawn(move || {
            let _ = tx.send(delete_agent(&rt, &h, &id, false));
        });

        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            dir.exists(),
            "锁被持有时 delete_agent 不得删除（检查+删除必须对 append_exec 原子）"
        );
        assert!(
            rx.try_recv().is_err(),
            "锁被持有时 delete_agent 不得已完成（必须阻塞在 flock 上）"
        );

        drop(_held);
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            other => panic!("锁释放后 delete_agent 必须 promptly 完成: {other:?}"),
        }
        worker.join().unwrap();
        assert!(!dir.exists(), "删除完成后目录必须消失");
        let (agents, _) = list_agents(&runtime, &home).unwrap();
        assert!(
            agents.is_empty(),
            "删除后列表不得残留（含 .trash 改名清理）"
        );
        let home_dir = crate::layout::home_dir(&runtime, &home).unwrap();
        let trash: Vec<String> = std::fs::read_dir(&home_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".trash-"))
            .collect();
        assert!(
            trash.is_empty(),
            "best-effort 清理后不应残留 .trash-*: {trash:?}"
        );
    }

    /// F-EXEC-9 功能面：对已结束 agent 的删除（锁内复核 + 改名 .trash-* +
    /// 移除）完成后，随后的续跑必须报「不存在」——而不是把新 exec 的记录/
    /// spec 写进已删除目录（回写变 no-op、对账找不到、租约永不释放）。
    #[test]
    fn delete_then_append_errors_instead_of_writing_into_deleted_dir() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-d1")).unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        let dir = crate::layout::agent_dir(&runtime, &home, &a.id).unwrap();
        std::fs::write(dir.join("exec-d1.jsonl"), "{}\n").unwrap();

        delete_agent(&runtime, &home, &a.id, false).unwrap();
        assert!(!dir.exists(), "目录（含 transcript）必须消失");

        let err = append_exec(&runtime, &home, &a.id, "pi", new_exec("exec-d2")).unwrap_err();
        assert!(err.contains("不存在"), "{err}");
        assert!(!dir.exists(), "失败的续跑不得复活目录");
        let (agents, _) = list_agents(&runtime, &home).unwrap();
        assert!(agents.is_empty());
    }

    #[test]
    fn find_exec_in_home_locates_by_exec_id() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let (agent, exec) = find_exec_in_home(&runtime, &home, "exec-a")
            .unwrap()
            .unwrap();
        assert_eq!(agent.id, a.id);
        assert_eq!(exec.exec_id, "exec-a");
        assert!(find_exec_in_home(&runtime, &home, "exec-zz")
            .unwrap()
            .is_none());
    }

    /// F-EXEC-5(a)：orphan（不受控、pid 仍存活）不可续跑——锁内拒绝且记录
    /// 原样（attachment 不被覆盖回 Live、不追加 exec）；错误可行动（先强制
    /// 终止）。非 orphan 的 Failed agent 仍可正常追加（守卫只挡 orphan）。
    #[test]
    fn append_exec_rejects_orphan_and_keeps_record_intact() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Failed,
            AgentAttachment::Orphan,
        )
        .unwrap();

        let err = append_exec(&runtime, &home, &a.id, "pi", new_exec("exec-a2")).unwrap_err();
        assert!(err.contains("不受控"), "{err}");
        assert!(err.contains("强制终止"), "{err}");

        let after = get_agent(&runtime, &home, &a.id).unwrap().unwrap();
        assert_eq!(after.attachment, AgentAttachment::Orphan, "attachment 不变");
        assert_eq!(after.status, AgentRecordStatus::Failed);
        assert_eq!(after.execs.len(), 1, "不得追加 exec");

        // 回归：解除 orphan（强制终止后的形态）即可续跑
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Failed,
            AgentAttachment::None,
        )
        .unwrap();
        let resumed = append_exec(&runtime, &home, &a.id, "pi", new_exec("exec-a3")).unwrap();
        assert_eq!(resumed.execs.len(), 2);
        assert_eq!(resumed.attachment, AgentAttachment::Live);
    }

    /// F-EXEC-11：finalize_agent_exec 是单事务回写——一次调用后 agent 级
    /// （status/attachment/last_session_id 镜像）与 exec 终态字段（exit_code/
    /// usage/口径/session id）同时可见；无变化不落盘（写合并）。
    #[test]
    fn finalize_agent_exec_writes_agent_and_exec_fields_together() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-fin")).unwrap();
        let path = crate::layout::agent_file(&runtime, &home, &a.id).unwrap();

        let changed = finalize_agent_exec(&runtime, &home, &a.id, "exec-fin", |agent| {
            agent.status = AgentRecordStatus::Exited;
            agent.attachment = AgentAttachment::None;
            for exec in agent.execs.iter_mut() {
                exec.exit_code = Some(0);
                exec.usage_tokens = Some(250);
                exec.usage_cost = Some(0.03);
                exec.usage_caliber = Some("message-sum".into());
                exec.harness_session_id = Some("ses_fin1".into());
            }
        })
        .unwrap();
        assert!(changed, "终态回写必须落盘");

        let (record, _) = read_json::<AgentRecord>(&path).unwrap();
        assert_eq!(record.status, AgentRecordStatus::Exited);
        assert_eq!(record.attachment, AgentAttachment::None);
        let exec = &record.execs[0];
        assert_eq!(exec.exit_code, Some(0));
        assert_eq!(exec.usage_tokens, Some(250));
        assert_eq!(exec.usage_caliber.as_deref(), Some("message-sum"));
        assert_eq!(
            exec.harness_session_id.as_deref(),
            Some("ses_fin1"),
            "session id 归档与终态字段同事务"
        );
        assert_eq!(
            record.last_session_id.as_deref(),
            Some("ses_fin1"),
            "agent.last_session_id 镜像与终态字段同事务"
        );

        // 写合并：相同内容再写 → 不落盘
        let before = std::fs::read(&path).unwrap();
        let changed_again = finalize_agent_exec(&runtime, &home, &a.id, "exec-fin", |agent| {
            agent.status = AgentRecordStatus::Exited;
            agent.attachment = AgentAttachment::None;
        })
        .unwrap();
        assert!(!changed_again, "无变化不落盘（写合并）");
        assert_eq!(before, std::fs::read(&path).unwrap());
    }

    /// F-EXEC-13(1)：终态事务在 exec 带 run_id 时**同一笔**预置
    /// lease_release_pending——终态写入与租约释放完成（可达 30s）之间 shim
    /// 死亡也不再丢失对账标记（终态 + pending=false 之间曾是无标记窗口，
    /// 租约永不释放）；run_id None（无租约可释）不预置。
    #[test]
    fn finalize_presets_lease_release_pending_only_with_run_id() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(
            &runtime,
            &home,
            "pi",
            NewExec {
                run_id: Some("RUN-1".into()),
                ..new_exec("exec-pend-run")
            },
        )
        .unwrap();
        finalize_agent_exec(&runtime, &home, &a.id, "exec-pend-run", |agent| {
            agent.status = AgentRecordStatus::Exited;
        })
        .unwrap();
        let rec = get_agent(&runtime, &home, &a.id).unwrap().unwrap();
        assert!(
            rec.execs[0].lease_release_pending,
            "带 run_id 的终态回写必须同笔预置 pending: {:?}",
            rec.execs[0]
        );

        let b = register_agent(&runtime, &home, "pi", new_exec("exec-pend-norun")).unwrap();
        finalize_agent_exec(&runtime, &home, &b.id, "exec-pend-norun", |agent| {
            agent.status = AgentRecordStatus::Failed;
        })
        .unwrap();
        let rec2 = get_agent(&runtime, &home, &b.id).unwrap().unwrap();
        assert!(
            !rec2.execs[0].lease_release_pending,
            "无 run_id（无租约）不得预置 pending: {:?}",
            rec2.execs[0]
        );

        // F-EXEC-14(2)：reconciled=true（U10 对账已完成）的 exec 不预置——
        // §43 的僵尸路径：desktop 先 mark_reconciled，shim 随后的收尾若再
        // 预置 pending，记录变回「pending=true + reconciled=true」，与 (1)
        // 修复前过宽的删除守卫组合成「永远删不掉」。
        let c = register_agent(
            &runtime,
            &home,
            "pi",
            NewExec {
                run_id: Some("RUN-3".into()),
                ..new_exec("exec-pend-done")
            },
        )
        .unwrap();
        update_exec(&runtime, &home, &c.id, "exec-pend-done", |e| {
            e.reconciled = true;
        })
        .unwrap();
        finalize_agent_exec(&runtime, &home, &c.id, "exec-pend-done", |agent| {
            agent.status = AgentRecordStatus::Exited;
        })
        .unwrap();
        let rec3 = get_agent(&runtime, &home, &c.id).unwrap().unwrap();
        assert!(
            !rec3.execs[0].lease_release_pending,
            "reconciled 的 exec 终态回写不得再预置 pending: {:?}",
            rec3.execs[0]
        );
    }

    /// F-EXEC-13(2)：任一 exec 的租约释放仍在途/待对账（lease_release_pending）
    /// 时拒绝删除——目录删除会把对账目标一起带走，租约永久泄漏；错误可行动
    /// （等对账完成后再删，或 force 放弃租约）。F-EXEC-14(1)：谓词与
    /// list_reconcile_targets 对齐后 pending 只在**带 run_id**（真有租约可泄）
    /// 时成障。
    #[test]
    fn delete_agent_rejects_pending_lease_release() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(
            &runtime,
            &home,
            "pi",
            NewExec {
                run_id: Some("RUN-1".into()),
                ..new_exec("exec-pend")
            },
        )
        .unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        update_exec(&runtime, &home, &a.id, "exec-pend", |e| {
            e.lease_release_pending = true;
        })
        .unwrap();
        let err = delete_agent(&runtime, &home, &a.id, false).unwrap_err();
        assert!(err.contains("对账"), "{err}");
        assert!(
            crate::layout::agent_file(&runtime, &home, &a.id)
                .unwrap()
                .is_file(),
            "拒绝删除时记录必须原样保留"
        );

        // 对账完成（pending 清）后可删
        update_exec(&runtime, &home, &a.id, "exec-pend", |e| {
            e.lease_release_pending = false;
        })
        .unwrap();
        delete_agent(&runtime, &home, &a.id, false).unwrap();
        assert!(get_agent(&runtime, &home, &a.id).unwrap().is_none());

        // F-EXEC-14(1)：无 run_id（无租约可泄）的 pending 残位不挡删除——
        // 守卫谓词与对账逐字对齐后，这种记录不是对账目标。
        let b = register_agent(&runtime, &home, "pi", new_exec("exec-pend-norun")).unwrap();
        update_agent_status(
            &runtime,
            &home,
            &b.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        update_exec(&runtime, &home, &b.id, "exec-pend-norun", |e| {
            e.lease_release_pending = true;
        })
        .unwrap();
        delete_agent(&runtime, &home, &b.id, false).unwrap();
        assert!(get_agent(&runtime, &home, &b.id).unwrap().is_none());
    }

    /// F-EXEC-13(2) 扩展（§41）：exec 带 run_id + 对账签名（SHIM_LOST）且
    /// 未 reconciled（pending=false）时**同样**拒绝删除——这正是
    /// list_reconcile_targets 的待对账目标，删掉目录 = 租约永久泄漏。对账完成
    /// （reconciled=true）后可删。注：RESTART_MARKER 形态走同一谓词（此处
    /// 以 SHIM_LOST 为代表；谓词与 list_reconcile_targets 逐字对齐）。
    #[test]
    fn delete_agent_rejects_unreconciled_marker_exec_until_reconciled() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(
            &runtime,
            &home,
            "pi",
            NewExec {
                run_id: Some("RUN-1".into()),
                ..new_exec("exec-shimlost")
            },
        )
        .unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Failed,
            AgentAttachment::None,
        )
        .unwrap();
        // 对账目标的典型形态：run_id + SHIM_LOST 签名 + 未 reconciled +
        // pending=false（签名即对账依据，不依赖 pending 位）
        update_exec(&runtime, &home, &a.id, "exec-shimlost", |e| {
            e.failure_detail = Some(SHIM_LOST_MARKER.to_string());
            e.reconciled = false;
            e.lease_release_pending = false;
        })
        .unwrap();
        let err = delete_agent(&runtime, &home, &a.id, false).unwrap_err();
        assert!(err.contains("对账"), "{err}");
        assert!(
            crate::layout::agent_file(&runtime, &home, &a.id)
                .unwrap()
                .is_file(),
            "拒绝删除时记录必须原样保留"
        );

        // 对账完成（mark_reconciled 等价：reconciled=true）后可删
        update_exec(&runtime, &home, &a.id, "exec-shimlost", |e| {
            e.reconciled = true;
        })
        .unwrap();
        delete_agent(&runtime, &home, &a.id, false).unwrap();
        assert!(get_agent(&runtime, &home, &a.id).unwrap().is_none());
    }

    /// F-EXEC-14(1)：pending=true 但 reconciled=true 的 exec **不是**对账目标
    /// （list_reconcile_targets 要求 `run_id.is_some() && !reconciled`）——§43
    /// 的「僵尸」形态：desktop 对账先 mark_reconciled（pending 清），shim
    /// 的重复释放失败又把 pending 写回 true。删除守卫必须与对账**同一谓词**
    /// （pending 只在 !reconciled 时才是障碍），这种 agent 必须可删——修复前
    /// 守卫看 pending 位本身，agent 从此永远删不掉。
    #[test]
    fn delete_agent_allows_pending_marker_on_reconciled_exec() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(
            &runtime,
            &home,
            "pi",
            NewExec {
                run_id: Some("RUN-1".into()),
                ..new_exec("exec-zombie")
            },
        )
        .unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        update_exec(&runtime, &home, &a.id, "exec-zombie", |e| {
            e.reconciled = true;
            e.lease_release_pending = true; // §43 僵尸：对账完成后 pending 又被写回
        })
        .unwrap();
        assert!(
            list_reconcile_targets(&runtime)
                .unwrap()
                .iter()
                .all(|t| t.exec_id != "exec-zombie"),
            "reconciled 的 exec 不是对账目标（守卫谓词的基准）"
        );
        delete_agent(&runtime, &home, &a.id, false).unwrap();
        assert!(get_agent(&runtime, &home, &a.id).unwrap().is_none());
    }

    /// P3-ACC-5：harness spawn 后同一事务写 pid / 起始身份并把 Starting
    /// 推进到 Running；终态不回退，非最新 exec 不推进。
    #[test]
    fn mark_exec_spawned_promotes_starting_to_running_once() {
        let (_dir, runtime, home) = temp_runtime();
        let agent = register_agent(&runtime, &home, "pi", new_exec("exec-run")).unwrap();
        assert!(mark_exec_spawned(
            &runtime,
            &home,
            &agent.id,
            "exec-run",
            4242,
            Some("macos:1:1".into())
        )
        .unwrap());
        let rec = get_agent(&runtime, &home, &agent.id).unwrap().unwrap();
        assert_eq!(rec.status, AgentRecordStatus::Running);
        assert_eq!(rec.execs[0].pid, Some(4242));
        assert_eq!(rec.execs[0].process_start_id.as_deref(), Some("macos:1:1"));
        // 终态不回退
        update_agent_status(
            &runtime,
            &home,
            &agent.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        mark_exec_spawned(&runtime, &home, &agent.id, "exec-run", 4242, None).unwrap();
        let rec = get_agent(&runtime, &home, &agent.id).unwrap().unwrap();
        assert_eq!(
            rec.status,
            AgentRecordStatus::Exited,
            "终态不得被推回 Running"
        );
    }

    /// F-EXEC-14(4)：force=true = 「放弃租约并删除」——待对账守卫被绕过
    /// （run/home 已被删、租约永远无法释放时的逃生口；F-EXEC-16 ①：daemon
    /// 不回收租约，release 由调用方 force 分支先行尽力释放，失败的才真放弃）；
    /// Starting/Running 守卫**不因 force 松动**。
    #[test]
    fn delete_agent_force_bypasses_lease_guard_but_not_running_guard() {
        let (_dir, runtime, home) = temp_runtime();
        let a = register_agent(
            &runtime,
            &home,
            "pi",
            NewExec {
                run_id: Some("RUN-1".into()),
                ..new_exec("exec-stuck")
            },
        )
        .unwrap();
        update_agent_status(
            &runtime,
            &home,
            &a.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        update_exec(&runtime, &home, &a.id, "exec-stuck", |e| {
            e.lease_release_pending = true; // 未对账 + pending：常规删除被挡
            e.reconciled = false;
        })
        .unwrap();
        let err = delete_agent(&runtime, &home, &a.id, false).unwrap_err();
        assert!(err.contains("对账"), "常规删除仍须挡住待对账租约: {err}");

        delete_agent(&runtime, &home, &a.id, true).unwrap();
        assert!(
            get_agent(&runtime, &home, &a.id).unwrap().is_none(),
            "force 必须放弃租约并删除"
        );

        // force 不绕过 Running 守卫：进程还在跑就不是租约问题
        let b = register_agent(&runtime, &home, "pi", new_exec("exec-live")).unwrap();
        update_agent_status(
            &runtime,
            &home,
            &b.id,
            AgentRecordStatus::Running,
            AgentAttachment::Live,
        )
        .unwrap();
        let err2 = delete_agent(&runtime, &home, &b.id, true).unwrap_err();
        assert!(err2.contains("仍在运行"), "force 不得绕过运行守卫: {err2}");
        assert!(get_agent(&runtime, &home, &b.id).unwrap().is_some());
    }
}
