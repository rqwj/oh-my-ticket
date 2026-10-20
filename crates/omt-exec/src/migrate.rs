//! 旧存储迁移（设计 §3.3）：`agents/<home>/agents.json` + `transcripts/<home>/`
//! → 按 agent 拆分。幂等、可中断重入；全局 flock 防止两个进程同时迁移。

use std::path::Path;

use omt_harness::atomic_json::{lock_exclusive, update_json, LOCK_ACQUIRE_TIMEOUT};

use crate::agents::{AgentRecord, AgentsFile, AgentsMeta};
use crate::layout::{self, ExecFile};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub homes: usize,
    pub agents: usize,
    pub files_moved: usize,
    /// 旧 agents.json 无法解析的 home（原地保留，待人工处理）。
    pub skipped_corrupt: Vec<String>,
}

const LEGACY_FILE: &str = "agents.json";
const MIGRATED_FILE: &str = "agents.json.migrated";

/// 迁移入口：任一面启动时调用。无旧数据时只做几次 stat。
pub fn migrate_legacy(runtime_dir: &Path) -> Result<MigrationReport, String> {
    let mut report = MigrationReport::default();
    let pending: Vec<String> = layout::home_ids(runtime_dir)
        .into_iter()
        .filter(|h| {
            runtime_dir
                .join("agents")
                .join(h)
                .join(LEGACY_FILE)
                .is_file()
        })
        .collect();
    if pending.is_empty() {
        return Ok(report);
    }
    let _guard = lock_exclusive(
        &runtime_dir.join("agents").join(".migrate"),
        LOCK_ACQUIRE_TIMEOUT,
    )?;
    for home_id in pending {
        migrate_home(runtime_dir, &home_id, &mut report)?;
    }
    Ok(report)
}

fn migrate_home(
    runtime_dir: &Path,
    home_id: &str,
    report: &mut MigrationReport,
) -> Result<(), String> {
    let home = layout::home_dir(runtime_dir, home_id)?;
    let legacy = home.join(LEGACY_FILE);
    if !legacy.is_file() {
        return Ok(()); // 另一个进程刚迁完
    }
    // 不经 read_json：损坏时不改名留证，原地保留给人工
    let text = std::fs::read_to_string(&legacy).map_err(|e| format!("read legacy: {e}"))?;
    let Ok(file) = serde_json::from_str::<AgentsFile>(&text) else {
        report.skipped_corrupt.push(home_id.to_string());
        return Ok(());
    };
    update_json(
        &layout::seq_file(runtime_dir, home_id)?,
        |m: &mut AgentsMeta| {
            if m.agent_seq >= file.meta.agent_seq {
                return false;
            }
            m.agent_seq = file.meta.agent_seq;
            true
        },
    )?;
    let transcripts = runtime_dir.join("transcripts").join(home_id);
    for agent in &file.agents {
        let dir = layout::agent_dir(runtime_dir, home_id, &agent.id)?;
        update_json(&dir.join("agent.json"), |rec: &mut AgentRecord| {
            if !rec.id.is_empty() {
                return false; // 已迁过（断点续做），不覆盖
            }
            *rec = agent.clone();
            true
        })?;
        for exec in &agent.execs {
            for (legacy_suffix, kind) in [
                ("jsonl", ExecFile::Jsonl),
                ("raw.log", ExecFile::RawLog),
                ("prompt.md", ExecFile::Prompt),
            ] {
                let src = transcripts.join(format!("{}.{legacy_suffix}", exec.exec_id));
                let dst = layout::exec_file(&dir, &exec.exec_id, kind);
                if src.is_file() && !dst.exists() {
                    std::fs::rename(&src, &dst)
                        .map_err(|e| format!("move {}: {e}", src.display()))?;
                    report.files_moved += 1;
                }
            }
        }
        report.agents += 1;
    }
    std::fs::rename(&legacy, home.join(MIGRATED_FILE))
        .map_err(|e| format!("mark migrated: {e}"))?;
    let _ = std::fs::remove_file(home.join("agents.json.lock"));
    report.homes += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentRecord, AgentRecordStatus, AgentsFile, AgentsMeta, ExecRecord};

    fn legacy_home(runtime: &std::path::Path) {
        let file = AgentsFile {
            meta: AgentsMeta { agent_seq: 7 },
            agents: vec![AgentRecord {
                id: "agent-exec-1".into(),
                display_name: "Agent 7".into(),
                harness: "pi".into(),
                status: AgentRecordStatus::Exited,
                last_exec_id: Some("exec-1".into()),
                execs: vec![ExecRecord {
                    exec_id: "exec-1".into(),
                    started_at: "1760000000000".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let home = runtime.join("agents").join("h_a");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("agents.json"),
            serde_json::to_string(&file).unwrap(),
        )
        .unwrap();
        let tr = runtime.join("transcripts").join("h_a");
        std::fs::create_dir_all(&tr).unwrap();
        std::fs::write(tr.join("exec-1.jsonl"), "{\"kind\":\"session_header\"}\n").unwrap();
        std::fs::write(tr.join("exec-1.raw.log"), "[stdout] hi\n").unwrap();
    }

    #[test]
    fn migrates_records_and_transcripts_then_marks_legacy_file() {
        let tmp = tempfile::tempdir().unwrap();
        legacy_home(tmp.path());
        let report = migrate_legacy(tmp.path()).unwrap();
        assert_eq!((report.homes, report.agents, report.files_moved), (1, 1, 2));
        let dir = crate::layout::agent_dir(tmp.path(), "h_a", "agent-exec-1").unwrap();
        assert!(dir.join("agent.json").is_file());
        assert!(dir.join("exec-1.jsonl").is_file());
        assert!(dir.join("exec-1.raw.log").is_file());
        assert_eq!(crate::agents::agent_seq(tmp.path(), "h_a").unwrap(), 7);
        let home = tmp.path().join("agents").join("h_a");
        assert!(!home.join("agents.json").exists());
        assert!(home.join("agents.json.migrated").is_file());
        let (agents, failed) = crate::agents::list_agents(tmp.path(), "h_a").unwrap();
        assert!(!failed);
        assert_eq!(agents[0].display_name, "Agent 7");
    }

    #[test]
    fn rerun_is_a_noop_and_interrupted_run_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        legacy_home(tmp.path());
        // 模拟中断：agent.json 已写、transcript 尚未移动、旧文件仍在
        let dir = crate::layout::agent_dir(tmp.path(), "h_a", "agent-exec-1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.json"),
            "{\"id\":\"agent-exec-1\",\"harness\":\"pi\"}",
        )
        .unwrap();
        let first = migrate_legacy(tmp.path()).unwrap();
        assert_eq!(first.files_moved, 2, "断点续做应补完移动");
        // 已写的 agent.json 不被覆盖
        let (rec, _) =
            omt_harness::atomic_json::read_json::<AgentRecord>(&dir.join("agent.json")).unwrap();
        assert_eq!(rec.display_name, "");
        let second = migrate_legacy(tmp.path()).unwrap();
        assert_eq!((second.homes, second.files_moved), (0, 0));
    }

    #[test]
    fn corrupt_legacy_file_is_left_alone_and_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agents").join("h_bad");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("agents.json"), "{oops").unwrap();
        let report = migrate_legacy(tmp.path()).unwrap();
        assert_eq!(report.skipped_corrupt, vec!["h_bad".to_string()]);
        assert!(
            home.join("agents.json").is_file(),
            "损坏文件留原地等人工处理"
        );
    }

    #[test]
    fn no_legacy_data_is_cheap_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            migrate_legacy(tmp.path()).unwrap(),
            MigrationReport::default()
        );
    }
}
