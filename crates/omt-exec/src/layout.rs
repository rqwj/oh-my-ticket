//! 执行面存储布局（设计 §3.3 / KD-E2）：按 agent 拆分。
//!
//! ```text
//! <runtime>/agents/<homeId>/seq.json                    agent 序号（AgentsMeta）
//! <runtime>/agents/<homeId>/bindings.json               nodeId → agentId（位置不变）
//! <runtime>/agents/<homeId>/<agentId>/agent.json        AgentRecord + execs[]
//! <runtime>/agents/<homeId>/<agentId>/<execId>.jsonl     规范化事件（transcript 权威，KD-E3）
//! <runtime>/agents/<homeId>/<agentId>/<execId>.raw.log   harness 原始 stdout/stderr
//! <runtime>/agents/<homeId>/<agentId>/<execId>.prompt.md FileAttach 渠道的 prompt
//! <runtime>/agents/<homeId>/<agentId>/<execId>.spec.json launch spec（Task 8）
//! <runtime>/agents/<homeId>/<agentId>/<execId>.shim.log  shim 自身 stdout/stderr
//! ```
//! agent.json 的跨进程互斥沿用 atomic_json 的 companion `agent.json.lock`；
//! 「同一 agent 同时只允许一个活跃 exec」在 `append_exec` 的锁内校验。

use std::path::{Path, PathBuf};

/// 路径段校验：拒绝空串、分隔符、`.` 开头与 `..`（防穿越）。
pub fn validate_segment(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || value.contains("..")
    {
        return Err(format!("invalid {what}: {value:?}"));
    }
    Ok(())
}

pub fn home_dir(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    validate_segment("home id", home_id)?;
    Ok(runtime_dir.join("agents").join(home_id))
}

pub fn agent_dir(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<PathBuf, String> {
    validate_segment("agent id", agent_id)?;
    Ok(home_dir(runtime_dir, home_id)?.join(agent_id))
}

pub fn agent_file(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<PathBuf, String> {
    Ok(agent_dir(runtime_dir, home_id, agent_id)?.join("agent.json"))
}

pub fn seq_file(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    Ok(home_dir(runtime_dir, home_id)?.join("seq.json"))
}

pub fn bindings_file(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    Ok(home_dir(runtime_dir, home_id)?.join("bindings.json"))
}

/// 每次执行的伴生文件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecFile {
    Jsonl,
    RawLog,
    Prompt,
    Spec,
    ShimLog,
}

impl ExecFile {
    fn suffix(self) -> &'static str {
        match self {
            ExecFile::Jsonl => "jsonl",
            ExecFile::RawLog => "raw.log",
            ExecFile::Prompt => "prompt.md",
            ExecFile::Spec => "spec.json",
            ExecFile::ShimLog => "shim.log",
        }
    }
}

pub fn exec_file(agent_dir: &Path, exec_id: &str, kind: ExecFile) -> PathBuf {
    agent_dir.join(format!("{exec_id}.{}", kind.suffix()))
}

/// 新 agent 的 id（与旧实现 `agent-<execId>` 同形）。
pub fn new_agent_id(exec_id: &str) -> String {
    format!("agent-{exec_id}")
}

fn subdirs(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// `agents/` 下的 home id（目录名，排序）。
pub fn home_ids(runtime_dir: &Path) -> Vec<String> {
    subdirs(&runtime_dir.join("agents"))
}

/// 某 home 下的 agent 目录名（排序；不保证每个都有可读的 agent.json）。
pub fn agent_ids(runtime_dir: &Path, home_id: &str) -> Result<Vec<String>, String> {
    Ok(subdirs(&home_dir(runtime_dir, home_id)?))
}

/// 子项目 2：派发面的机器本地状态目录（policy / 跨进程派发锁 / 临时授权登记）。
pub fn exec_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("exec")
}

/// 设计 §3.6：`<runtime>/exec/policy.json`。
pub fn policy_file(runtime_dir: &Path) -> PathBuf {
    exec_dir(runtime_dir).join("policy.json")
}

/// Q-6：跨进程派发锁的目标路径（`atomic_json::lock_exclusive` 在其 `.lock` 上加 flock）。
pub fn launch_lock_file(runtime_dir: &Path) -> PathBuf {
    exec_dir(runtime_dir).join("launch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_reject_traversal() {
        let rt = Path::new("/tmp/rt");
        assert!(home_dir(rt, "../x").is_err());
        assert!(home_dir(rt, "a/b").is_err());
        assert!(agent_dir(rt, "h_ok", ".hidden").is_err());
        assert!(agent_dir(rt, "h_ok", "agent-exec-1-0").is_ok());
    }

    #[test]
    fn exec_files_share_the_agent_dir() {
        let dir = Path::new("/tmp/rt/agents/h/agent-exec-1");
        assert_eq!(
            exec_file(dir, "exec-1", ExecFile::Jsonl),
            dir.join("exec-1.jsonl")
        );
        assert_eq!(
            exec_file(dir, "exec-1", ExecFile::RawLog),
            dir.join("exec-1.raw.log")
        );
    }

    #[test]
    fn listings_skip_hidden_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agents").join("h_a");
        std::fs::create_dir_all(home.join("agent-2")).unwrap();
        std::fs::create_dir_all(home.join("agent-1")).unwrap();
        std::fs::create_dir_all(home.join(".trash")).unwrap();
        std::fs::write(home.join("seq.json"), "{}").unwrap();
        assert_eq!(home_ids(tmp.path()), vec!["h_a".to_string()]);
        assert_eq!(
            agent_ids(tmp.path(), "h_a").unwrap(),
            vec!["agent-1".to_string(), "agent-2".to_string()]
        );
    }
}
