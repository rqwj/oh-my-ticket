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
    /// 迁移中途硬错误（IO 失败等）的 home：错误已记录、跳过，其余 home
    /// 继续迁移；旧文件未改名，下次启动自动重试（F-EXEC-8：单个坏 home
    /// 不得永久阻断全部迁移）。
    pub failed: Vec<String>,
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
    // F-EXEC-8：单 home 的硬错误（IO 失败等）记进报告后继续下一个 home
    // ——`?` 穿透会让一个坏 home 拖停所有 home 且每次启动重复失败。只有
    // 全局锁获取失败才返回 Err（上面已 `?`）。
    for home_id in pending {
        if let Err(err) = migrate_home(runtime_dir, &home_id, &mut report) {
            eprintln!("迁移 home {home_id} 失败：{err}（跳过，下次启动重试）");
            report.failed.push(home_id);
        }
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
    // 不经 read_json：损坏时不改名留证，原地保留给人工。F-EXEC-8：按字节
    // 读 + from_slice——非 UTF-8 也归入「解析失败 → skipped_corrupt」，
    // 不再是硬错（read_to_string 遇非 UTF-8 直接 Err，会把该 home 以及
    // 后续所有 home 的迁移永久卡死）。
    let bytes = std::fs::read(&legacy).map_err(|e| format!("read legacy: {e}"))?;
    let Ok(file) = serde_json::from_slice::<AgentsFile>(&bytes) else {
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

    fn legacy_home(runtime: &std::path::Path, home_id: &str) {
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
        let home = runtime.join("agents").join(home_id);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("agents.json"),
            serde_json::to_string(&file).unwrap(),
        )
        .unwrap();
        let tr = runtime.join("transcripts").join(home_id);
        std::fs::create_dir_all(&tr).unwrap();
        std::fs::write(tr.join("exec-1.jsonl"), "{\"kind\":\"session_header\"}\n").unwrap();
        std::fs::write(tr.join("exec-1.raw.log"), "[stdout] hi\n").unwrap();
    }

    #[test]
    fn migrates_records_and_transcripts_then_marks_legacy_file() {
        let tmp = tempfile::tempdir().unwrap();
        legacy_home(tmp.path(), "h_a");
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
        legacy_home(tmp.path(), "h_a");
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
        assert!(report.failed.is_empty());
        assert!(
            home.join("agents.json").is_file(),
            "损坏文件留原地等人工处理"
        );
    }

    /// F-EXEC-8(a)：非 UTF-8 的旧文件必须归入 skipped_corrupt（与损坏同
    /// 语义，原地保留待人工），且**不得中止其余 home 的迁移**——旧的
    /// `read_to_string` 在此处直接报错，整个 migrate_legacy 返回 Err。
    #[test]
    fn non_utf8_legacy_is_skipped_corrupt_and_other_homes_still_migrate() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("agents").join("h_a");
        std::fs::create_dir_all(&bad).unwrap();
        // 合法 JSON 骨架里掺非 UTF-8 字节：read_to_string / from_str 必败，
        // from_slice 也无法解析——按损坏跳过而非硬错。
        std::fs::write(bad.join("agents.json"), [0x7b, 0x22, 0xFF, 0xFE, 0x7d]).unwrap();
        legacy_home(tmp.path(), "h_b");

        let report = migrate_legacy(tmp.path()).unwrap();

        assert_eq!(report.skipped_corrupt, vec!["h_a".to_string()]);
        assert!(report.failed.is_empty(), "非 UTF-8 是损坏跳过，不是硬错误");
        assert_eq!(report.homes, 1, "h_b 必须照常迁移，不被 h_a 拖停");
        assert!(
            bad.join("agents.json").is_file(),
            "非 UTF-8 文件留原地待人工"
        );
        let migrated = crate::layout::agent_dir(tmp.path(), "h_b", "agent-exec-1").unwrap();
        assert!(migrated.join("agent.json").is_file());
        assert!(
            !tmp.path()
                .join("agents")
                .join("h_b")
                .join("agents.json")
                .exists(),
            "h_b 的旧文件必须已改名 .migrated"
        );
    }

    /// F-EXEC-8(b)：某 home 迁移中途的硬错误（IO 失败）记进报告后**继续
    /// 迁移其余 home**——旧的 `migrate_home(...)?` 遇错即返回，h_a 永远
    /// 迁不过去且每次启动都拖停后面的 home。
    ///
    /// 确定性 IO 错误模拟：把 h_a 的 `seq.json` 路径占用为**目录**——
    /// migrate_home 里对 seq.json 的 `update_json` 在写回时执行
    /// `rename(tmp 文件, seq.json)`，对目录路径的 rename 确定性报
    /// EISDIR/ENOTDIR。旧文件未改名，下次启动自动重试（幂等语义不变）。
    #[test]
    fn hard_error_in_one_home_is_recorded_and_migration_continues() {
        let tmp = tempfile::tempdir().unwrap();
        legacy_home(tmp.path(), "h_a");
        let h_a = tmp.path().join("agents").join("h_a");
        std::fs::create_dir_all(h_a.join("seq.json")).unwrap(); // 占位目录
        legacy_home(tmp.path(), "h_b");

        let report = migrate_legacy(tmp.path()).unwrap();

        assert_eq!(
            report.failed,
            vec!["h_a".to_string()],
            "硬错误进报告而非中断"
        );
        assert!(report.skipped_corrupt.is_empty());
        assert_eq!(report.homes, 1, "h_b 必须照常迁移");
        assert!(
            h_a.join("agents.json").is_file(),
            "硬错误的 home 旧文件保留，下次启动重试"
        );
        let migrated = crate::layout::agent_dir(tmp.path(), "h_b", "agent-exec-1").unwrap();
        assert!(
            migrated.join("agent.json").is_file(),
            "h_b 的 agent 记录必须已落盘"
        );
        assert!(migrated.join("exec-1.jsonl").is_file());
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
