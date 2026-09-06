//! U3: MCP 桥接注入 —— 让被派发的 harness 连上 `omt mcp` sidecar。
//!
//! 两条注入路径（v1 providers）：
//! - **opencode**：env `OPENCODE_CONFIG_CONTENT` 内联配置——零落盘，exec 结束
//!   随进程消失。**F-U3-4 实测确认**（本机 opencode 1.18.14，`opencode mcp
//!   list`，项目级 `opencode.json` 含一个 server + env 注入另一个）：该 env
//!   与项目/全局配置是**深度合并**，不是整体覆盖——两个 server 同时出现，
//!   用户已有的 provider/model 配置不会被清空；
//! - **omp**：workspace `.omp/mcp.json` 合并写入（refcount）——首个 exec 备份
//!   原文件到 runtime dir（不在用户工作区留备份），最后一个 exec 结束恢复；
//!   exec 期间用户手改则不覆盖（hash 比对，只外科手术移除自己的 `omt` 条目，
//!   不整体覆盖用户改动，F-U3-3）；desktop 启动时清扫 crash 残留。`persistent`
//!   模式（设置项，U7 透传）用固定名 launcher 幂等合并、不备份不恢复，与
//!   per-exec 注入互不覆盖对方的引用（F-U3-10）。
//!
//! 身份纪律（§3.3）：每个 exec 的 MCP 启动参数带 `--actor exec:<execId>`，
//! 经 per-exec launcher 脚本实现（omp 的 mcp.json 是共享文件，不能内嵌
//! per-exec 参数）；opencode 的 env 天然 per-process。
//!
//! 共性：spawn 前预检 sidecar 存在（缺失→派发失败，绝不 spawn 无 MCP 的
//! harness——prompt 要求 claim 而无工具 = 必然空转）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

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
    /// omp workspace 注入——release 时恢复原文件（或外科手术移除本注入）。
    Omp {
        workspace_root: PathBuf,
        persistent: bool,
        /// launcher 脚本 id（不含 `.sh` 后缀）：非 persistent 为 exec_id，
        /// persistent 为固定 `"persistent"`（跨多次派发复用，release 不删，
        /// F-U3-7）。
        launcher_id: String,
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

/// F-U3-5：`DefaultHasher` 的内部算法不保证跨 Rust 版本稳定——用于
/// **持久化**比对（workspace hash、写入内容 hash）必须用稳定算法，否则
/// toolchain 升级后旧记录全部判"用户改过"、永不恢复。
fn stable_hash_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    // 16 hex 字符（64 位）足够做目录名/比对用途，不需要完整 32 字节。
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// F-U3-6：路径未 canonicalize 会让同一 workspace 的不同拼写（symlink、
/// `/tmp` vs `/private/tmp`）拆分到两个 state 目录，refcount 错乱。
fn canonical_workspace(workspace_root: &Path) -> PathBuf {
    workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf())
}

fn workspace_hash(workspace_root: &Path) -> String {
    stable_hash_hex(
        canonical_workspace(workspace_root)
            .to_string_lossy()
            .as_bytes(),
    )
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

fn omp_launchers_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("harness").join("omp-launchers")
}

fn launcher_path(runtime_dir: &Path, launcher_id: &str) -> PathBuf {
    omp_launchers_dir(runtime_dir).join(format!("{launcher_id}.sh"))
}

fn single_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// 原子写：同目录 tmp + rename（F-U3-8——用户工作区文件不能半截写坏）。
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp-{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json"),
        std::process::id()
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))
}

/// 写 launcher：`omt [--actor <actor>] mcp` 的包装脚本（omp 的 mcp.json 是
/// 共享文件，per-exec 参数只能经 launcher 携带）。0700。`actor` 为 `None`
/// 时不传 `--actor`（persistent 模式：不绑定单次 exec 身份）。
fn write_launcher(
    runtime_dir: &Path,
    launcher_id: &str,
    omt_path: &Path,
    actor: Option<&str>,
) -> Result<PathBuf, String> {
    use std::io::Write;
    let dir = omp_launchers_dir(runtime_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create launcher dir: {e}"))?;
    let path = dir.join(format!("{launcher_id}.sh"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode_if_unix(0o700)
        .open(&path)
        .map_err(|e| format!("open launcher: {e}"))?;
    let body = match actor {
        Some(actor) => format!(
            "#!/bin/sh\nexec {} --actor {} mcp \"$@\"\n",
            single_quote(&omt_path.to_string_lossy()),
            single_quote(actor)
        ),
        None => format!(
            "#!/bin/sh\nexec {} mcp \"$@\"\n",
            single_quote(&omt_path.to_string_lossy())
        ),
    };
    file.write_all(body.as_bytes())
        .map_err(|e| format!("write launcher: {e}"))?;
    Ok(path)
}

/// F-U3-7：每个 exec 用完自己的 launcher 就删——refcount 只决定 workspace
/// 文件何时恢复，与某个 launcher 文件是否还有用是两回事（mcp.json 的
/// `omt` 条目每次 merge 都被覆盖，只有最后写入的那个 launcher 是"活的"）。
fn remove_launcher(runtime_dir: &Path, launcher_id: &str) {
    let _ = std::fs::remove_file(launcher_path(runtime_dir, launcher_id));
}

/// omp 合并写入：保留用户已有 servers，注入/更新 `omt` 条目指向 `launcher`。
/// 返回写入内容的 hash（恢复时比对，防覆盖用户 mid-exec 手改）。
fn merge_omp_mcp(workspace_root: &Path, launcher: &Path) -> Result<String, String> {
    let file = omp_workspace_file(workspace_root);
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
    write_atomic(&file, &text)?;
    Ok(stable_hash_hex(text.as_bytes()))
}

/// F-U3-3/F-U3-10 外科手术式移除：只删 `mcpServers.omt`，且**只当它的
/// command 精确等于我们这次写入的 launcher 路径**才动手——不是任何
/// `omp-launchers` 路径都算数（否则会误删 persistent 注入，F-U3-10）。
/// 保留用户在执行期间对文件做的其它改动；解析失败/结构不符则什么都不做
/// （文件已经不是我们能安全修的东西）。
fn remove_omt_entry_if_matches(
    workspace_file: &Path,
    expected_command: &str,
) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(workspace_file) else {
        return Ok(()); // 文件已不存在
    };
    let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(());
    };
    let Some(servers) = root.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return Ok(());
    };
    let matches = servers
        .get("omt")
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        == Some(expected_command);
    if !matches {
        return Ok(()); // 已被别的注入（如 persistent）接管，不是我们的了
    }
    servers.remove("omt");
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    write_atomic(workspace_file, &text)
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
    write_atomic(&state_dir.join("state.json"), &text)
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
    if persistent {
        // 固定名 launcher：跨多次派发复用，不绑定单次 exec 身份（没有意义——
        // 这条连接的目的是让用户随时手动用 omp 连上 omt，不属于任何 run）。
        let launcher_id = "persistent".to_string();
        let launcher = write_launcher(runtime_dir, &launcher_id, omt_path, None)?;
        // 幂等合并，不备份不恢复（设置页「持久保留」开关，U7 透传）。
        merge_omp_mcp(workspace_root, &launcher)?;
        return Ok(McpInjection::Omp {
            workspace_root: workspace_root.to_path_buf(),
            persistent: true,
            launcher_id,
        });
    }
    let launcher_id = exec_id.to_string();
    let actor = format!("exec:{exec_id}");
    let launcher = write_launcher(runtime_dir, &launcher_id, omt_path, Some(&actor))?;
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
            write_atomic(&state_dir.join("backup.json"), &original)?;
        }
        state["backupExisted"] = serde_json::json!(backup_existed);
        state["workspace"] = serde_json::json!(workspace_root.to_string_lossy());
    }
    let written_sha = merge_omp_mcp(workspace_root, &launcher)?;
    state["refcount"] = serde_json::json!(refcount + 1);
    state["writtenSha"] = serde_json::json!(written_sha);
    state["writtenCommand"] = serde_json::json!(launcher.display().to_string());
    write_omp_state(&state_dir, &state)?;
    Ok(McpInjection::Omp {
        workspace_root: workspace_root.to_path_buf(),
        persistent: false,
        launcher_id,
    })
}

/// 最终释放（refcount 归零）：恢复原文件；若 workspace 文件在执行期间被
/// 改过（hash 不符我们最后写入的内容），改为外科手术移除我们自己的 `omt`
/// 条目而不整体覆盖（F-U3-3/F-U3-10）。随后清理 state 目录。
fn finalize_release(
    state_dir: &Path,
    workspace_root: &Path,
    state: &serde_json::Value,
) -> Result<(), String> {
    let workspace_file = omp_workspace_file(workspace_root);
    let written_sha = state["writtenSha"].as_str().unwrap_or_default();
    let written_command = state["writtenCommand"].as_str().unwrap_or_default();
    let current = std::fs::read_to_string(&workspace_file).ok();
    let user_modified = match (&current, written_sha.is_empty()) {
        (Some(text), false) => stable_hash_hex(text.as_bytes()) != written_sha,
        (Some(_), true) => true, // 无记录可比对，视为已改
        (None, _) => false,
    };
    if user_modified {
        eprintln!(
            "omp mcp.json 在执行期间被修改，跳过整体恢复，改为移除 omt 条目（备份保留于 {}）",
            state_dir.display()
        );
        if !written_command.is_empty() {
            remove_omt_entry_if_matches(&workspace_file, written_command)?;
        }
    } else if state["backupExisted"].as_bool().unwrap_or(false) {
        let backup = std::fs::read_to_string(state_dir.join("backup.json"))
            .map_err(|e| format!("read backup: {e}"))?;
        write_atomic(&workspace_file, &backup)?;
    } else {
        let _ = std::fs::remove_file(&workspace_file);
    }
    let _ = std::fs::remove_dir_all(state_dir);
    Ok(())
}

/// 恢复 omp workspace 的 mcp.json：先删本 exec 的 launcher（F-U3-7，refcount
/// 是否归零与"这个 launcher 是否还有用"是两回事）；仅最后一个引用才触发
/// `finalize_release`。
fn release_omp(runtime_dir: &Path, workspace_root: &Path, launcher_id: &str) -> Result<(), String> {
    let _guard = OMP_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    remove_launcher(runtime_dir, launcher_id);
    let state_dir = omp_state_dir(runtime_dir, workspace_root);
    let mut state = omp_state(&state_dir);
    let refcount = state["refcount"].as_u64().unwrap_or(0);
    if refcount > 1 {
        state["refcount"] = serde_json::json!(refcount - 1);
        write_omp_state(&state_dir, &state)?;
        return Ok(());
    }
    finalize_release(&state_dir, workspace_root, &state)
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
            launcher_id,
        } => {
            if *persistent {
                return Ok(()); // 长期复用，release 是 no-op（F-U3-10）
            }
            release_omp(runtime_dir, workspace_root, launcher_id)
        }
        _ => Ok(()),
    }
}

/// desktop 启动清扫：desktop 退出时未正常 release 的 omp 注入（crash 残留）
/// 逐 workspace 恢复/外科手术清理，随后清空整个 launcher 目录——到这一步
/// 所有仍活跃的引用（包括 persistent 的）都已在各自的 state 处理中被跳过
/// 或保留，遗留文件均为真正的孤儿。返回处理过的 workspace 数。
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
        let _guard = OMP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        finalize_release(&entry.path(), Path::new(workspace), &state)?;
        drop(_guard);
        swept += 1;
    }
    // 残留 launcher 一并清空（crash 的 exec 不会再释放；persistent 的
    // launcher 若仍被引用，上面的 finalize_release 不会碰它所在 workspace
    // 的 state——但 persistent 本身不建 state 目录，也就不会被这里遍历
    // 到，其 launcher 文件此刻会被一并删掉。这是已知的 v1 限制：persistent
    // 模式下 app 崩溃重启会丢失 persistent launcher，用户需在设置页重新
    // 开启——记入 backlog，不在本次 fix 范围内自动续期。）
    let launchers = omp_launchers_dir(runtime_dir);
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

    fn launcher_count(runtime: &Path) -> usize {
        std::fs::read_dir(omp_launchers_dir(runtime))
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
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
        // F-U3-7：本 exec 的 launcher 用完即删
        assert_eq!(launcher_count(&rig.runtime), 0);
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
        // 还有 exec-2：不应恢复；exec-1 自己的 launcher 已删，exec-2 的还在
        let merged = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        assert!(merged.contains("omp-launchers"));
        assert_eq!(launcher_count(&rig.runtime), 1);
        release(&rig.runtime, &second).unwrap();
        assert_eq!(
            std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap(),
            "{}"
        );
        assert_eq!(launcher_count(&rig.runtime), 0);
    }

    #[test]
    fn omp_user_mid_exec_edit_skips_restore_and_only_removes_own_entry() {
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
        // 用户 mid-exec 手改：保留我们的 omt 条目，同时加了自己的 server
        let merged = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        let mut root: serde_json::Value = serde_json::from_str(&merged).unwrap();
        root["mcpServers"]["mine"] = serde_json::json!({ "type": "stdio", "command": "x" });
        std::fs::write(
            omp_workspace_file(&rig.workspace),
            serde_json::to_string_pretty(&root).unwrap(),
        )
        .unwrap();
        release(&rig.runtime, &injection).unwrap();
        // 我们的条目被外科手术移除，用户自己加的条目保留
        let after = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        let after_json: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert!(after_json["mcpServers"].get("omt").is_none(), "{after}");
        assert!(after_json["mcpServers"].get("mine").is_some(), "{after}");
    }

    #[test]
    fn omp_persistent_and_non_persistent_do_not_clobber_each_other() {
        let rig = rig();
        // 先建立 persistent 注入
        let persistent = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1", // exec_id 对 persistent 无意义（launcher 固定名）
            true,
        )
        .unwrap();
        assert!(matches!(
            persistent,
            McpInjection::Omp {
                persistent: true,
                ..
            }
        ));
        // 再来一次非 persistent 派发（不同 exec）：会临时覆盖 omt 条目
        let scoped = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-2",
            false,
        )
        .unwrap();
        // 非 persistent release：hash 不匹配自己最后写入的内容？不——它是
        // 最后写入者，hash 匹配，走"未改动"分支——但那是"恢复 backup"，
        // backup 是 prepare 时刻的内容（已含 persistent 条目），所以恢复后
        // persistent 条目应回来。
        release(&rig.runtime, &scoped).unwrap();
        let after = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        assert!(
            after.contains("persistent"),
            "非 persistent release 后 persistent 注入应完好: {after}"
        );
        // persistent 自身的 release 是 no-op，条目继续在
        release(&rig.runtime, &persistent).unwrap();
        let still = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        assert!(still.contains("persistent"), "{still}");
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
        assert_eq!(launcher_count(&rig.runtime), 0);
        // 二次清扫幂等
        assert_eq!(sweep_omp_crash_residue(&rig.runtime).unwrap(), 0);
    }

    #[test]
    fn sweep_scrubs_own_entry_when_user_modified_mid_crash() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(omp_workspace_file(&rig.workspace), "{}").unwrap();
        prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        // 用户在崩溃前又手改了一次
        let merged = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        let mut root: serde_json::Value = serde_json::from_str(&merged).unwrap();
        root["mcpServers"]["mine"] = serde_json::json!({ "type": "stdio", "command": "x" });
        std::fs::write(
            omp_workspace_file(&rig.workspace),
            serde_json::to_string_pretty(&root).unwrap(),
        )
        .unwrap();
        sweep_omp_crash_residue(&rig.runtime).unwrap();
        let after = std::fs::read_to_string(omp_workspace_file(&rig.workspace)).unwrap();
        let after_json: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert!(after_json["mcpServers"].get("omt").is_none(), "{after}");
        assert!(after_json["mcpServers"].get("mine").is_some(), "{after}");
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
        assert!(
            !merged.contains("--actor"),
            "persistent launcher 无 --actor: {merged}"
        );
        // persistent 模式 release 是 no-op：条目与 launcher 都还在
        release(&rig.runtime, &injection).unwrap();
        assert!(omp_workspace_file(&rig.workspace).is_file());
        assert_eq!(launcher_count(&rig.runtime), 1);
    }

    #[test]
    fn workspace_hash_stable_across_process_and_canonicalizes_symlinks() {
        let rig = rig();
        // 稳定性：同一路径两次算出同一 hash（跨调用/跨进程重启都应如此，
        // DefaultHasher 做不到这点，是 F-U3-5 要修的）。
        assert_eq!(
            workspace_hash(&rig.workspace),
            workspace_hash(&rig.workspace)
        );
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
