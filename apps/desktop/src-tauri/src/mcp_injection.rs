//! U3: MCP 桥接注入 —— 让被派发的 harness 连上 `omt mcp` sidecar。
//!
//! 两条注入路径（v1 providers）：
//! - **opencode**：env `OPENCODE_CONFIG_CONTENT` 内联配置——零落盘，exec 结束
//!   随进程消失；
//! - **omp**：workspace `.omp/mcp.json` 合并写入（refcount）——首个 exec 备份
//!   原文件到 runtime dir（不在用户工作区留备份），最后一个 exec 结束恢复；
//!   exec 期间用户手改则不覆盖（hash 比对）；desktop 启动时清扫 crash 残留。
//!   `persistent` 模式（设置项，U7 透传）只做幂等合并、不备份不恢复。
//!
//! 身份纪律（§3.3）：每个 exec 的 MCP 启动参数带 `--actor exec:<execId>`，
//! 经 per-exec launcher 脚本实现（omp 的 mcp.json 是共享文件，不能内嵌
//! per-exec 参数）；opencode 的 env 天然 per-process。
//!
//! 共性：spawn 前预检 sidecar 存在（缺失→派发失败，绝不 spawn 无 MCP 的
//! harness——prompt 要求 claim 而无工具 = 必然空转）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::atomic_json::OpenOptionsUnix;
use crate::daemon;

/// 注入结果：spawn 时消费（env），终态时消费（release）。
#[derive(Debug, Clone, PartialEq)]
pub enum McpInjection {
    None,
    Env {
        key: String,
        value: String,
    },
    /// omp workspace 注入——release 时恢复原文件。
    Omp {
        workspace_root: PathBuf,
        persistent: bool,
    },
}

impl McpInjection {
    /// spawn 时需要加入子进程环境的键值。
    pub fn env_entry(&self) -> Option<(String, String)> {
        match self {
            McpInjection::Env { key, value } => Some((key.clone(), value.clone())),
            _ => None,
        }
    }
}

/// 进程内串行化 omp 的 prepare/release/refcount（single-instance + U15 后
/// 跨进程由 omt CLI 共写约定接管）。
static OMP_LOCK: Mutex<()> = Mutex::new(());

fn workspace_hash(workspace_root: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workspace_root
        .to_string_lossy()
        .to_string()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn omp_injection_root(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("harness").join("omp-injection")
}

fn omp_state_dir(runtime_dir: &Path, workspace_root: &Path) -> PathBuf {
    omp_injection_root(runtime_dir).join(workspace_hash(workspace_root))
}

fn omp_workspace_file(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".omp").join("mcp.json")
}

fn sha_hex(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn single_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// 写 per-exec launcher：`omt --actor exec:<id> mcp` 的包装脚本（omp 的
/// mcp.json 是共享文件，per-exec 参数只能经 launcher 携带）。0700。
fn write_launcher(
    runtime_dir: &Path,
    exec_id: &str,
    omt_path: &Path,
    actor: &str,
) -> Result<PathBuf, String> {
    use std::io::Write;
    let dir = runtime_dir.join("harness").join("omp-launchers");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create launcher dir: {e}"))?;
    let path = dir.join(format!("{exec_id}.sh"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode_if_unix(0o700)
        .open(&path)
        .map_err(|e| format!("open launcher: {e}"))?;
    file.write_all(
        format!(
            "#!/bin/sh\nexec {} --actor {} mcp \"$@\"\n",
            single_quote(&omt_path.to_string_lossy()),
            single_quote(actor)
        )
        .as_bytes(),
    )
    .map_err(|e| format!("write launcher: {e}"))?;
    Ok(path)
}

fn remove_launcher(runtime_dir: &Path, exec_id: &str) {
    let _ = std::fs::remove_file(
        runtime_dir
            .join("harness")
            .join("omp-launchers")
            .join(format!("{exec_id}.sh")),
    );
}

/// omp 合并写入：保留用户已有 servers，注入/更新 `omt` 条目。返回写入内容
/// 的 hash（恢复时比对，防覆盖用户 mid-exec 手改）。
fn merge_omp_mcp(workspace_root: &Path, launcher: &Path) -> Result<(bool, String), String> {
    let file = omp_workspace_file(workspace_root);
    let existed = file.is_file();
    let mut root = match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| format!("workspace .omp/mcp.json 不是合法 JSON: {e}"))?,
        Err(_) => serde_json::json!({}),
    };
    if !root.is_object() {
        return Err("workspace .omp/mcp.json 顶层不是对象，拒绝注入".to_string());
    }
    root["mcpServers"]["omt"] = serde_json::json!({
        "type": "stdio",
        "command": launcher.display().to_string(),
        "args": [],
    });
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create .omp dir: {e}"))?;
    }
    std::fs::write(&file, &text).map_err(|e| format!("write workspace mcp.json: {e}"))?;
    Ok((existed, sha_hex(&text)))
}

fn omp_state(state_dir: &Path) -> serde_json::Value {
    std::fs::read_to_string(state_dir.join("state.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({ "refcount": 0, "backupExisted": false }))
}

fn write_omp_state(state_dir: &Path, state: &serde_json::Value) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| format!("create state dir: {e}"))?;
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(state_dir.join("state.json"), text).map_err(|e| format!("write state: {e}"))
}

/// omp 注入核心（`omt` sidecar 路径可注入以供测试）。
fn prepare_omp_with(
    omt_path: &Path,
    runtime_dir: &Path,
    workspace_root: &Path,
    exec_id: &str,
    persistent: bool,
) -> Result<McpInjection, String> {
    let _guard = OMP_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let actor = format!("exec:{exec_id}");
    let launcher = write_launcher(runtime_dir, exec_id, omt_path, &actor)?;
    if persistent {
        // 幂等合并，不备份不恢复（设置页「持久保留」开关，U7 透传）
        let (_, _) = merge_omp_mcp(workspace_root, &launcher)?;
        return Ok(McpInjection::Omp {
            workspace_root: workspace_root.to_path_buf(),
            persistent: true,
        });
    }
    let state_dir = omp_state_dir(runtime_dir, workspace_root);
    let mut state = omp_state(&state_dir);
    let refcount = state["refcount"].as_u64().unwrap_or(0);
    if refcount == 0 {
        // 首个 exec：备份原文件（内容原样存 runtime dir，不在工作区留副本）
        let workspace_file = omp_workspace_file(workspace_root);
        let backup_existed = workspace_file.is_file();
        if backup_existed {
            let original = std::fs::read_to_string(&workspace_file)
                .map_err(|e| format!("read workspace mcp.json: {e}"))?;
            std::fs::create_dir_all(&state_dir).map_err(|e| format!("create state dir: {e}"))?;
            std::fs::write(state_dir.join("backup.json"), original)
                .map_err(|e| format!("write backup: {e}"))?;
        }
        state["backupExisted"] = serde_json::json!(backup_existed);
        state["workspace"] = serde_json::json!(workspace_root.to_string_lossy());
    }
    let (existed, written_sha) = merge_omp_mcp(workspace_root, &launcher)?;
    let _ = existed;
    state["refcount"] = serde_json::json!(refcount + 1);
    state["writtenSha"] = serde_json::json!(written_sha);
    write_omp_state(&state_dir, &state)?;
    Ok(McpInjection::Omp {
        workspace_root: workspace_root.to_path_buf(),
        persistent: false,
    })
}

/// 恢复 omp workspace 的 mcp.json：仅最后一个 exec 结束才恢复；恢复前比对
/// written hash，用户 mid-exec 手改过则跳过并报告（不覆盖用户内容）。
fn release_omp(runtime_dir: &Path, workspace_root: &Path) -> Result<(), String> {
    let _guard = OMP_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state_dir = omp_state_dir(runtime_dir, workspace_root);
    let mut state = omp_state(&state_dir);
    let refcount = state["refcount"].as_u64().unwrap_or(0);
    if refcount > 1 {
        state["refcount"] = serde_json::json!(refcount - 1);
        write_omp_state(&state_dir, &state)?;
        return Ok(());
    }
    // 最后一个引用：恢复原文件
    let workspace_file = omp_workspace_file(workspace_root);
    let written_sha = state["writtenSha"].as_str().unwrap_or_default();
    let current = std::fs::read_to_string(&workspace_file).ok();
    let user_modified = match (&current, written_sha.is_empty()) {
        (Some(text), false) => sha_hex(text) != written_sha,
        (Some(_), true) => true, // 无记录可比对，视为已改
        (None, _) => false,
    };
    if user_modified {
        eprintln!(
            "omp mcp.json 在执行期间被修改，跳过自动恢复（备份保留于 {}）",
            state_dir.display()
        );
        return Ok(());
    }
    if state["backupExisted"].as_bool().unwrap_or(false) {
        let backup = std::fs::read_to_string(state_dir.join("backup.json"))
            .map_err(|e| format!("read backup: {e}"))?;
        std::fs::write(&workspace_file, backup).map_err(|e| format!("restore: {e}"))?;
    } else {
        let _ = std::fs::remove_file(&workspace_file);
    }
    let _ = std::fs::remove_dir_all(&state_dir);
    Ok(())
}

/// 注入准备：sidecar 预检 + 按 provider 分派。任何失败=派发失败（绝不
/// spawn 无 MCP 的 harness）。`persistent` 供 U7 设置项透传。
pub fn prepare(
    runtime_dir: &Path,
    workspace_root: &Path,
    provider_id: &str,
    exec_id: &str,
    persistent: bool,
) -> Result<McpInjection, String> {
    let omt_path = daemon::omt_sidecar_path()?;
    prepare_with(
        &omt_path,
        runtime_dir,
        workspace_root,
        provider_id,
        exec_id,
        persistent,
    )
}

fn prepare_with(
    omt_path: &Path,
    runtime_dir: &Path,
    workspace_root: &Path,
    provider_id: &str,
    exec_id: &str,
    persistent: bool,
) -> Result<McpInjection, String> {
    if !omt_path.is_file() {
        return Err(format!(
            "omt CLI sidecar 缺失：{}（无法建立 MCP 桥，拒绝派发）",
            omt_path.display()
        ));
    }
    match provider_id {
        "opencode" => {
            let value = serde_json::json!({
                "mcp": {
                    "omt": {
                        "type": "local",
                        "command": [
                            omt_path.display().to_string(),
                            "--actor", format!("exec:{exec_id}"),
                            "mcp",
                        ],
                        "enabled": true,
                    }
                }
            })
            .to_string();
            Ok(McpInjection::Env {
                key: "OPENCODE_CONFIG_CONTENT".to_string(),
                value,
            })
        }
        "omp" => prepare_omp_with(omt_path, runtime_dir, workspace_root, exec_id, persistent),
        other => Err(format!(
            "provider {other} 无 MCP 注入实现（v1 仅 opencode/omp）"
        )),
    }
}

/// exec 终态时的恢复入口（executor 终态回写后调用；失败仅记录——workspace
/// 残留由下次启动 sweep 兜底）。
pub fn release(runtime_dir: &Path, injection: &McpInjection) -> Result<(), String> {
    match injection {
        McpInjection::Omp {
            workspace_root,
            persistent,
        } => {
            if *persistent {
                return Ok(());
            }
            release_omp(runtime_dir, workspace_root)
        }
        _ => Ok(()),
    }
}

/// desktop 启动清扫：desktop 退出时未正常 release 的 omp 注入（crash 残留）
/// 逐 workspace 恢复。返回恢复的 workspace 数。启动时无存活的 desktop 注入
/// 方，所有残留均可安全恢复。
pub fn sweep_omp_crash_residue(runtime_dir: &Path) -> Result<usize, String> {
    let root = omp_injection_root(runtime_dir);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(0);
    };
    let mut swept = 0usize;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let state = omp_state(&entry.path());
        let workspace = state["workspace"].as_str().unwrap_or_default();
        if workspace.is_empty() {
            continue;
        }
        release_omp(runtime_dir, Path::new(workspace))?;
        swept += 1;
    }
    // 残留 launcher 一并清空（crash 的 exec 不会再释放）
    let launchers = runtime_dir.join("harness").join("omp-launchers");
    if launchers.is_dir() {
        let _ = std::fs::remove_dir_all(&launchers);
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rig {
        _dir: tempfile::TempDir,
        runtime: PathBuf,
        workspace: PathBuf,
        fake_omt: PathBuf,
    }

    fn rig() -> Rig {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let fake_omt = dir.path().join("omt");
        std::fs::write(&fake_omt, "#!/bin/sh\necho fake omt\n").unwrap();
        std::fs::set_permissions(&fake_omt, std::fs::Permissions::from_mode(0o755)).unwrap();
        Rig {
            _dir: dir,
            runtime,
            workspace,
            fake_omt,
        }
    }

    #[test]
    fn sidecar_missing_is_rejected_with_path() {
        let rig = rig();
        let err = prepare_with(
            Path::new("/definitely/not/here/omt"),
            &rig.runtime,
            &rig.workspace,
            "opencode",
            "exec-1",
            false,
        )
        .unwrap_err();
        assert!(err.contains("omt CLI sidecar 缺失"), "{err}");
    }

    #[test]
    fn opencode_injection_is_env_only() {
        let rig = rig();
        let injection = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "opencode",
            "exec-1",
            false,
        )
        .unwrap();
        let McpInjection::Env { key, value } = injection else {
            panic!("应为 env 注入");
        };
        assert_eq!(key, "OPENCODE_CONFIG_CONTENT");
        assert!(value.contains("omt"), "{value}");
        assert!(value.contains("--actor"), "{value}");
        assert!(value.contains("exec:exec-1"), "{value}");
        // 零落盘：workspace 不产生 .omp
        assert!(!rig.workspace.join(".omp").exists());
    }

    #[test]
    fn omp_injection_merges_and_restores_original() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(
            omp_workspace_file(&rig.workspace),
            r#"{ "mcpServers": { "user-server": { "type": "stdio", "command": "u" } } }"#,
        )
        .unwrap();
        let injection = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        let merged = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        assert!(merged.contains("user-server"), "用户 server 保留: {merged}");
        assert!(
            merged.contains("omp-launchers"),
            "omt 条目指向 launcher: {merged}"
        );

        release(&rig.runtime, &injection).unwrap();
        let restored = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        assert!(
            !restored.contains("\"omt\""),
            "恢复后不含 omt 条目: {restored}"
        );
        assert!(restored.contains("user-server"));
    }

    #[test]
    fn omp_fresh_workspace_file_removed_on_release() {
        let rig = rig();
        let injection = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        assert!(omp_workspace_file(&rig.workspace).is_file());
        release(&rig.runtime, &injection).unwrap();
        assert!(!omp_workspace_file(&rig.workspace).exists());
    }

    #[test]
    fn omp_refcount_restores_only_after_last_exec() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(omp_workspace_file(&rig.workspace), "{}").unwrap();
        let first = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        let second = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-2",
            false,
        )
        .unwrap();
        release(&rig.runtime, &first).unwrap();
        // 还有 exec-2：不应恢复
        let merged = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        assert!(merged.contains("omp-launchers"));
        release(&rig.runtime, &second).unwrap();
        assert_eq!(
            std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap(),
            "{}"
        );
    }

    #[test]
    fn omp_user_mid_exec_edit_skips_restore() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(omp_workspace_file(&rig.workspace), "{}").unwrap();
        let injection = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        // 用户 mid-exec 手改
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(omp_workspace_file(&rig.workspace), r#"{ "mine": true }"#).unwrap();
        release(&rig.runtime, &injection).unwrap();
        // 不覆盖用户内容
        assert_eq!(
            std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap(),
            r#"{ "mine": true }"#
        );
    }

    #[test]
    fn omp_persistent_mode_merges_without_state() {
        let rig = rig();
        let injection = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            true,
        )
        .unwrap();
        assert!(matches!(
            injection,
            McpInjection::Omp {
                persistent: true,
                ..
            }
        ));
        let merged = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        assert!(merged.contains("omp-launchers"));
        // persistent 模式 release 是 no-op
        release(&rig.runtime, &injection).unwrap();
        assert!(omp_workspace_file(&rig.workspace).is_file());
    }

    #[test]
    fn sweep_restores_crash_residue() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(omp_workspace_file(&rig.workspace), "{}").unwrap();
        // prepare 后不 release（模拟 crash）
        prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        assert!(omp_workspace_file(&rig.workspace).is_file());
        let swept = sweep_omp_crash_residue(&rig.runtime).unwrap();
        assert_eq!(swept, 1);
        assert_eq!(
            std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap(),
            "{}"
        );
        // 二次清扫幂等
        assert_eq!(sweep_omp_crash_residue(&rig.runtime).unwrap(), 0);
    }

    #[test]
    fn unknown_provider_rejected() {
        let rig = rig();
        assert!(prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "dsh",
            "exec-1",
            false
        )
        .is_err());
    }
}
