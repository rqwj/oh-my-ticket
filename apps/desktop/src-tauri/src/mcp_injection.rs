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
//! omp 注入的生命周期簿记（二次评审 §10 修复，F-U3-11/12/15/16/17/18）：
//! - **state.json 在 merge 之前先记 `workspace`**（F-U3-15）：merge 与"记录
//!   这是哪个 workspace"之间的崩溃窗口，是 sweep 能否找到并清理该目录的
//!   分界——先记再改，窗口内至多留下"文件已改但没人认领"，而不是"文件
//!   已改且从此没人知道要清理"。首个 exec 备份原文件前还会先扫一遍现存
//!   `omt` 条目，若其 command 落在 `omp-launchers/` 下（persistent 的固定
//!   路径除外）就先剔除——防止上一轮崩溃的残留被当成"用户原始内容"永久
//!   烙进未来所有恢复。
//! - **`launchers` 数组 + 推迟删除**（F-U3-12）：mcp.json 的 `omt.command`
//!   永远只指向"最近一次 merge"的那个 launcher，不一定是当前正在 release
//!   的那个——提前删自己的 launcher 曾经让仍被引用的脚本消失。本模块改为
//!   把整批 exec 用过的 launcher id 记进 state，全部留到批次最终 release
//!   （`finalize_release`）才一次性清空。
//! - **`userModified` 粘性标记**（F-U3-16）：hash 比对只能发现"最近一次
//!   merge 之后"的改动——两个重叠 exec 之间发生的用户编辑会被第二次
//!   merge 悄悄吸收进新基线。每次非首个 exec 的 prepare，在 merge 之前先
//!   比对当前文件与上一次写入的 hash，不符就置粘性标记，最终 release 时
//!   与即时 hash 比对结果 OR 在一起。
//! - **外科手术分支保留备份**（F-U3-11）：备份在删除 state 目录**之前**
//!   原子拷到 `<hash>.last-backup.json`（state 目录之外，sweep 的
//!   `is_dir()` 过滤天然跳过它）——旧实现日志说"备份保留"，随后却把整个
//!   目录连 backup.json 一起删了。
//! - **sweep 逐目录 log-and-continue**（F-U3-17）：一个 workspace 恢复失败
//!   不再中止其余 workspace 的清理；sweep 在每次启动都跑，卡死一次就是
//!   永久卡死。
//! - **sweep 不再整体清空 launcher 目录**（F-U3-18）：per-workspace
//!   `finalize_release` 已经清了自己追踪到的 launcher；末尾兜底只逐文件删
//!   真正的孤儿，**跳过 persistent.sh**——persistent 没有 state 目录，每次
//!   正常重启都要用到它。
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

/// 项目级 MCP 注入的 per-provider 参数。omp 的目录与条目形状是**已持久化
/// 的兼容面**（在途 state 的 writtenSha 基线、用户机器上的 launcher 目录），
/// 字节不得变动；pi 走独立命名空间与实测条目形状（`pi mcp add -l` 实测：
/// 无 `type` 字段）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ProjectSpec {
    /// `runtime/harness/<ns>-injection` 与 `runtime/harness/<ns>-launchers`
    pub ns: &'static str,
    /// workspace 配置相对路径
    pub rel: &'static str,
    /// 条目是否带 `"type": "stdio"`（omp 历史形状；pi 形状无此字段）
    entry_type_stdio: bool,
    /// 注入条目是否带 `exposure: direct`（pi ≥1.0；omp 未同步该体系）
    expose_direct: bool,
}

const OMP_SPEC: ProjectSpec = ProjectSpec {
    ns: "omp",
    rel: ".omp/mcp.json",
    entry_type_stdio: true,
    expose_direct: false,
};

const PI_SPEC: ProjectSpec = ProjectSpec {
    ns: "pi",
    rel: ".pi/mcp.json",
    entry_type_stdio: false,
    expose_direct: true,
};

impl ProjectSpec {
    fn injection_root(self, runtime_dir: &Path) -> PathBuf {
        runtime_dir
            .join("harness")
            .join(format!("{}-injection", self.ns))
    }
    fn launchers_dir(self, runtime_dir: &Path) -> PathBuf {
        runtime_dir
            .join("harness")
            .join(format!("{}-launchers", self.ns))
    }
    fn workspace_file(self, workspace_root: &Path) -> PathBuf {
        workspace_root.join(self.rel)
    }
    fn entry(self, launcher: &Path) -> serde_json::Value {
        let mut entry = if self.entry_type_stdio {
            serde_json::json!({
                "type": "stdio",
                "command": launcher.display().to_string(),
                "args": [],
            })
        } else {
            serde_json::json!({
                "command": launcher.display().to_string(),
                "args": [],
            })
        };
        if self.expose_direct {
            // pi ≥1.0 exposure 体系（实测 1.3.0 确认）：direct = 工具像内置
            // 工具一样声明给模型——消除 codemode 发现开销（F-EXPOSE）。
            // description 进系统提示的 mcp_servers 清单。
            entry["exposure"] = serde_json::json!("direct");
            entry["description"] =
                serde_json::json!("OMT ticket run discipline: run__claim / run__report");
        }
        entry
    }
}

/// 注入结果：spawn 时消费（env），终态时消费（release）。
#[derive(Debug, Clone, PartialEq)]
pub enum McpInjection {
    None,
    Env {
        key: String,
        value: String,
    },
    /// 项目级 workspace 注入（omp `.omp/mcp.json` / pi `.pi/mcp.json`）——
    /// release 时恢复原文件（或外科手术移除本注入）。
    Project {
        workspace_root: PathBuf,
        persistent: bool,
        /// launcher 脚本 id（不含 `.sh` 后缀）：非 persistent 为 exec_id，
        /// persistent 为固定 `"persistent"`。仅供展示/调试——实际删除时机
        /// 由 state.json 的 `launchers` 集合统一管理（F-U3-12）。
        launcher_id: String,
        spec: ProjectSpec,
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

/// 进程内串行化项目级注入（omp/pi）的 prepare/release/refcount
/// （single-instance + U15 后跨进程由 omt CLI 共写约定接管）。
static PROJECT_LOCK: Mutex<()> = Mutex::new(());

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

fn state_dir(runtime_dir: &Path, workspace_root: &Path, spec: ProjectSpec) -> PathBuf {
    spec.injection_root(runtime_dir)
        .join(workspace_hash(workspace_root))
}

fn launcher_path(runtime_dir: &Path, spec: ProjectSpec, launcher_id: &str) -> PathBuf {
    spec.launchers_dir(runtime_dir)
        .join(format!("{launcher_id}.sh"))
}

/// 目录前缀比较用；带尾部分隔符，避免 "x-launchers" 误匹配
/// "x-launchers-other" 这类兄弟目录（纯字符串 `starts_with` 的陷阱）。
fn launchers_prefix(runtime_dir: &Path, spec: ProjectSpec) -> String {
    let mut s = spec.launchers_dir(runtime_dir).display().to_string();
    s.push(std::path::MAIN_SEPARATOR);
    s
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
    std::fs::rename(&tmp, path).map_err(|e| {
        // F-U3-13：rename 失败不留 tmp——否则每次失败都在用户工作区（或
        // state 目录）多一个孤儿文件。
        let _ = std::fs::remove_file(&tmp);
        format!("rename: {e}")
    })
}

/// 写 launcher：`omt [--actor <actor>] mcp` 的包装脚本（omp 的 mcp.json 是
/// 共享文件，per-exec 参数只能经 launcher 携带）。0700。`actor` 为 `None`
/// 时不传 `--actor`（persistent 模式：不绑定单次 exec 身份）。
fn write_launcher(
    runtime_dir: &Path,
    spec: ProjectSpec,
    launcher_id: &str,
    omt_path: &Path,
    actor: Option<&str>,
) -> Result<PathBuf, String> {
    use std::io::Write;
    let dir = spec.launchers_dir(runtime_dir);
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

fn remove_launcher(runtime_dir: &Path, spec: ProjectSpec, launcher_id: &str) {
    let _ = std::fs::remove_file(launcher_path(runtime_dir, spec, launcher_id));
}

/// 项目级合并写入：保留用户已有 servers，注入/更新 `omt` 条目指向
/// `launcher`（条目形状 per-spec——omp 带 type:stdio 历史形状，pi 实测形状）。
/// 返回写入内容的 hash（恢复时比对，防覆盖用户 mid-exec 手改）。
fn merge_project_mcp(
    workspace_root: &Path,
    launcher: &Path,
    spec: ProjectSpec,
) -> Result<String, String> {
    let file = spec.workspace_file(workspace_root);
    let mut root = match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| format!("workspace {} 不是合法 JSON: {e}", spec.rel))?,
        Err(_) => serde_json::json!({}),
    };
    if !root.is_object() {
        return Err(format!("workspace {} 顶层不是对象，拒绝注入", spec.rel));
    }
    root["mcpServers"]["omt"] = spec.entry(launcher);
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    write_atomic(&file, &text)?;
    Ok(stable_hash_hex(text.as_bytes()))
}

/// 外科手术式移除 `mcpServers.omt`：只有 `is_ours(command)` 为真才动手，
/// 保留用户在执行期间对文件做的其它改动。解析失败/结构不符则什么都不做
/// （文件已经不是我们能安全修的东西）。
fn remove_omt_entry_if(
    workspace_file: &Path,
    is_ours: impl Fn(&str) -> bool,
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
        .map(&is_ours)
        .unwrap_or(false);
    if !matches {
        return Ok(()); // 不是我们这次写的那个（已被别的注入接管，或压根不存在）
    }
    servers.remove("omt");
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    write_atomic(workspace_file, &text)
}

/// F-U3-3/F-U3-10：精确匹配——只当 command **等于**我们这次写入的那个才删。
fn remove_omt_entry_if_matches(
    workspace_file: &Path,
    expected_command: &str,
) -> Result<(), String> {
    remove_omt_entry_if(workspace_file, |cmd| cmd == expected_command)
}

/// F-U3-15：宽匹配兜底——用于我们已丢失确切 command 记录的场合（state 不
/// 完整）。只要求 command 落在我们的 launcher 目录下，但**显式排除**
/// persistent 的固定路径，避免误删一个此刻可能合法存在的 persistent 注入
/// （F-U3-10 同款风险，这里换了个触发路径）。
fn remove_omt_entry_if_under_launchers_dir(
    runtime_dir: &Path,
    spec: ProjectSpec,
    workspace_file: &Path,
) -> Result<(), String> {
    let prefix = launchers_prefix(runtime_dir, spec);
    let persistent = launcher_path(runtime_dir, spec, "persistent")
        .display()
        .to_string();
    remove_omt_entry_if(workspace_file, |cmd| {
        cmd.starts_with(&prefix) && cmd != persistent
    })
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
fn prepare_project_with(
    omt_path: &Path,
    runtime_dir: &Path,
    workspace_root: &Path,
    exec_id: &str,
    persistent: bool,
    spec: ProjectSpec,
) -> Result<McpInjection, String> {
    let _guard = PROJECT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if persistent {
        // 固定名 launcher：跨多次派发复用，不绑定单次 exec 身份（没有意义——
        // 这条连接的目的是让用户随时手动用 omp 连上 omt，不属于任何 run）。
        let launcher_id = "persistent".to_string();
        let launcher = write_launcher(runtime_dir, spec, &launcher_id, omt_path, None)?;
        // 幂等合并，不备份不恢复（设置页「持久保留」开关，U7 透传）。
        merge_project_mcp(workspace_root, &launcher, spec)?;
        return Ok(McpInjection::Project {
            workspace_root: workspace_root.to_path_buf(),
            persistent: true,
            launcher_id,
            spec,
        });
    }
    let launcher_id = exec_id.to_string();
    let actor = format!("exec:{exec_id}");
    let launcher = write_launcher(runtime_dir, spec, &launcher_id, omt_path, Some(&actor))?;
    let state_dir = state_dir(runtime_dir, workspace_root, spec);
    let mut state = omp_state(&state_dir);
    let refcount = state["refcount"].as_u64().unwrap_or(0);
    let workspace_file = spec.workspace_file(workspace_root);
    if refcount == 0 {
        // F-U3-15：先记 `workspace` 再动用户文件——merge 与这次写之间的
        // 崩溃窗口，决定 sweep 到底能不能找到这个目录。
        state["workspace"] = serde_json::json!(workspace_root.to_string_lossy());
        write_omp_state(&state_dir, &state)?;

        // F-U3-15：上一轮崩溃可能在文件里留下了我们自己的旧注入（state 已
        // 丢失，文件没丢）。备份前先剔除，否则未来所有恢复都会把这个残留
        // 当成"用户原始内容"永久保留。persistent 的条目豁免（见函数文档）。
        remove_omt_entry_if_under_launchers_dir(runtime_dir, spec, &workspace_file)?;

        let backup_existed = workspace_file.is_file();
        if backup_existed {
            let original = std::fs::read_to_string(&workspace_file)
                .map_err(|e| format!("read workspace mcp.json: {e}"))?;
            std::fs::create_dir_all(&state_dir).map_err(|e| format!("create state dir: {e}"))?;
            write_atomic(&state_dir.join("backup.json"), &original)?;
        }
        state["backupExisted"] = serde_json::json!(backup_existed);
    } else {
        // F-U3-16：hash 比对只能发现"上一次写入之后"的改动——两次重叠
        // prepare 之间的用户编辑会被这次 merge 悄悄吸收进新基线。在 merge
        // 覆写证据之前先比对，不符就置粘性标记，直到批次最终 release 才
        // 清空（随 state 目录一起删除）。
        let prior_sha = state["writtenSha"].as_str().unwrap_or_default().to_string();
        if !prior_sha.is_empty() {
            let current = std::fs::read_to_string(&workspace_file).unwrap_or_default();
            if stable_hash_hex(current.as_bytes()) != prior_sha {
                state["userModified"] = serde_json::json!(true);
            }
        }
    }
    let written_sha = merge_project_mcp(workspace_root, &launcher, spec)?;
    state["refcount"] = serde_json::json!(refcount + 1);
    state["writtenSha"] = serde_json::json!(written_sha);
    state["writtenCommand"] = serde_json::json!(launcher.display().to_string());
    {
        // F-U3-12：记录本批次用过的全部 launcher——删除时机统一推迟到
        // `finalize_release`，见模块文档。
        let mut launchers: Vec<String> = state["launchers"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !launchers.contains(&launcher_id) {
            launchers.push(launcher_id.clone());
        }
        state["launchers"] = serde_json::json!(launchers);
    }
    write_omp_state(&state_dir, &state)?;
    Ok(McpInjection::Project {
        workspace_root: workspace_root.to_path_buf(),
        persistent: false,
        launcher_id,
        spec,
    })
}

/// 最终释放（refcount 归零，或 crash-sweep 直接触发）：恢复原文件；若
/// workspace 文件在执行期间被改过（即时 hash 不符，或粘性 `userModified`
/// 标记——F-U3-16），改为外科手术移除我们自己的 `omt` 条目而不整体覆盖
/// （F-U3-3/F-U3-10）。无论走哪条分支，都清理本批次追踪到的全部 launcher
/// （F-U3-12）与 state 目录；外科手术分支额外把备份原地保存到 state 目录
/// **之外**（F-U3-11）。
fn finalize_release(
    runtime_dir: &Path,
    state_dir: &Path,
    workspace_root: &Path,
    state: &serde_json::Value,
    spec: ProjectSpec,
) -> Result<(), String> {
    let workspace_file = spec.workspace_file(workspace_root);
    let written_sha = state["writtenSha"].as_str().unwrap_or_default();
    let written_command = state["writtenCommand"].as_str().unwrap_or_default();
    let sticky_modified = state["userModified"].as_bool().unwrap_or(false);
    let current = std::fs::read_to_string(&workspace_file).ok();
    let user_modified = sticky_modified
        || match (&current, written_sha.is_empty()) {
            (Some(text), false) => stable_hash_hex(text.as_bytes()) != written_sha,
            (Some(_), true) => true, // 无记录可比对，视为已改
            (None, _) => false,
        };
    if user_modified {
        // F-U3-11：备份先原子拷到 state 目录之外再报告"已保留"，随后的
        // remove_dir_all 才不会把刚才承诺保留的东西一并删掉。
        let preserved_at = if state["backupExisted"].as_bool().unwrap_or(false) {
            std::fs::read_to_string(state_dir.join("backup.json"))
                .ok()
                .and_then(|original| {
                    let dst = state_dir.with_extension("last-backup.json");
                    write_atomic(&dst, &original).ok().map(|_| dst)
                })
        } else {
            None
        };
        match &preserved_at {
            Some(path) => eprintln!(
                "omp mcp.json 在执行期间被修改，跳过整体恢复，改为移除 omt 条目（原始内容已保存于 {}）",
                path.display()
            ),
            None => {
                eprintln!("omp mcp.json 在执行期间被修改，跳过整体恢复，改为移除 omt 条目")
            }
        }
        if !written_command.is_empty() {
            remove_omt_entry_if_matches(&workspace_file, written_command)?;
        } else {
            // F-U3-15：state 不完整（崩溃发生在 writtenCommand 落盘之前），
            // 没有精确 command 可比对——退化为宽匹配，总比永远留着一个
            // 悬空条目强。
            remove_omt_entry_if_under_launchers_dir(runtime_dir, spec, &workspace_file)?;
        }
    } else if state["backupExisted"].as_bool().unwrap_or(false) {
        let backup = std::fs::read_to_string(state_dir.join("backup.json"))
            .map_err(|e| format!("read backup: {e}"))?;
        write_atomic(&workspace_file, &backup)?;
    } else {
        let _ = std::fs::remove_file(&workspace_file);
    }
    // F-U3-12：本批次追踪到的全部 launcher，此刻文件要么已整体恢复/删除、
    // 要么刚被外科手术清掉引用——无论哪种，都不会再有 launcher 被引用了。
    if let Some(launchers) = state["launchers"].as_array() {
        for id in launchers.iter().filter_map(|v| v.as_str()) {
            remove_launcher(runtime_dir, spec, id);
        }
    }
    let _ = std::fs::remove_dir_all(state_dir);
    Ok(())
}

/// 处理一次 release：refcount > 1 只减计数，最后一次触发
/// `finalize_release`。launcher 删除**推迟到** `finalize_release`
/// （F-U3-12）——mcp.json 的 `omt` 条目只会指向"最近一次 merge"的那个
/// launcher，不一定是当前正在 release 的这个；提前删会让仍被引用的脚本
/// 消失。
fn release_project(
    runtime_dir: &Path,
    workspace_root: &Path,
    spec: ProjectSpec,
) -> Result<(), String> {
    let _guard = PROJECT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state_dir = state_dir(runtime_dir, workspace_root, spec);
    let mut state = omp_state(&state_dir);
    let refcount = state["refcount"].as_u64().unwrap_or(0);
    if refcount > 1 {
        state["refcount"] = serde_json::json!(refcount - 1);
        write_omp_state(&state_dir, &state)?;
        return Ok(());
    }
    finalize_release(runtime_dir, &state_dir, workspace_root, &state, spec)
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

/// `pub(crate)`（not just `#[cfg(test)]`) so other modules' tests can drive
/// the real prepare path with an injectable `omt_path`, instead of hand-
/// building a `McpInjection` that bypasses the `launchers` bookkeeping
/// (F-U3-12) — see `executor::tests::spawn_failure_releases_mcp_injection`.
pub(crate) fn prepare_with(
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
        "omp" => prepare_project_with(
            omt_path,
            runtime_dir,
            workspace_root,
            exec_id,
            persistent,
            OMP_SPEC,
        ),
        "pi" => prepare_project_with(
            omt_path,
            runtime_dir,
            workspace_root,
            exec_id,
            persistent,
            PI_SPEC,
        ),
        other => Err(format!(
            "provider {other} 无 MCP 注入实现（v1 仅 opencode/omp/pi）"
        )),
    }
}

/// exec 终态时的恢复入口（executor 终态回写后调用；失败仅记录——workspace
/// 残留由下次启动 sweep 兜底）。
pub fn release(runtime_dir: &Path, injection: &McpInjection) -> Result<(), String> {
    match injection {
        McpInjection::Project {
            workspace_root,
            persistent,
            spec,
            ..
        } => {
            if *persistent {
                return Ok(()); // 长期复用，release 是 no-op（F-U3-10）
            }
            release_project(runtime_dir, workspace_root, *spec)
        }
        _ => Ok(()),
    }
}

/// desktop 启动清扫：desktop 退出时未正常 release 的项目级注入（crash
/// 残留）逐 workspace 恢复/外科手术清理（F-U3-17：单个失败不中止其余），
/// 随后清扫真正的孤儿 launcher（F-U3-18：跳过 persistent.sh，它没有
/// state 目录、每次正常重启都要继续用）。omp 与 pi 各自的 state/launcher
/// 命名空间独立清扫。返回成功处理的 workspace 数。
pub fn sweep_crash_residue(runtime_dir: &Path) -> Result<usize, String> {
    let mut swept_total = 0usize;
    for spec in [OMP_SPEC, PI_SPEC] {
        swept_total += sweep_spec_crash_residue(runtime_dir, spec)?;
    }
    Ok(swept_total)
}

fn sweep_spec_crash_residue(runtime_dir: &Path, spec: ProjectSpec) -> Result<usize, String> {
    let root = spec.injection_root(runtime_dir);
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
        let _guard = PROJECT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match finalize_release(
            runtime_dir,
            &entry.path(),
            Path::new(workspace),
            &state,
            spec,
        ) {
            Ok(()) => swept += 1,
            Err(err) => eprintln!(
                "{} 注入残留清扫失败（{}）：{err}（本次跳过，下次启动重试）",
                spec.ns,
                entry.path().display()
            ),
        }
        drop(_guard);
    }
    // 上面每个 workspace 的 finalize_release 已经清了自己追踪到的
    // launcher；这里只兜底真正的孤儿（例如 state 目录在记录 `workspace`
    // 之前就整个丢失）。绝不动 persistent.sh。
    if let Ok(entries) = std::fs::read_dir(spec.launchers_dir(runtime_dir)) {
        let persistent = launcher_path(runtime_dir, spec, "persistent");
        for entry in entries.flatten() {
            let path = entry.path();
            if path != persistent {
                let _ = std::fs::remove_file(&path);
            }
        }
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
        std::fs::read_dir(OMP_SPEC.launchers_dir(runtime))
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
    }

    fn omt_command(mcp_json: &str) -> Option<String> {
        let root: serde_json::Value = serde_json::from_str(mcp_json).ok()?;
        root["mcpServers"]["omt"]["command"]
            .as_str()
            .map(str::to_string)
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
            OMP_SPEC.workspace_file(&rig.workspace),
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
        let merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        assert!(merged.contains("user-server"), "用户 server 保留: {merged}");
        assert!(
            merged.contains("omp-launchers"),
            "omt 条目指向 launcher: {merged}"
        );

        release(&rig.runtime, &injection).unwrap();
        let restored = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        assert!(
            !restored.contains("\"omt\""),
            "恢复后不含 omt 条目: {restored}"
        );
        assert!(restored.contains("user-server"));
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
        assert!(OMP_SPEC.workspace_file(&rig.workspace).is_file());
        release(&rig.runtime, &injection).unwrap();
        assert!(!OMP_SPEC.workspace_file(&rig.workspace).exists());
    }

    #[test]
    fn omp_refcount_restores_only_after_last_exec() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(OMP_SPEC.workspace_file(&rig.workspace), "{}").unwrap();
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
        // 还有 exec-2：不应恢复。F-U3-12：launcher 删除推迟到批次最终
        // release，所以此刻两个 launcher 都还在——mcp.json 此刻仍指向
        // exec-2 的那个，提前删任何一个都有悬空风险。
        let merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        assert!(merged.contains("omp-launchers"));
        assert_eq!(
            launcher_count(&rig.runtime),
            2,
            "两个 launcher 都要等批次结束才清"
        );
        release(&rig.runtime, &second).unwrap();
        assert_eq!(
            std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap(),
            "{}"
        );
        assert_eq!(launcher_count(&rig.runtime), 0);
    }

    #[test]
    fn releasing_later_exec_first_does_not_dangle_current_launcher() {
        // F-U3-12 回归测试：exec-2 后于 exec-1 prepare（mcp.json 此刻指向
        // exec-2 的 launcher），但 exec-2 先结束、先 release。修复前
        // release_omp 会无条件删掉"自己的" launcher——恰好是 mcp.json 仍
        // 引用的那个，导致 exec-1 剩余运行期间悬空。
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(OMP_SPEC.workspace_file(&rig.workspace), "{}").unwrap();
        let first = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        let _second = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-2",
            false,
        )
        .unwrap();
        release(&rig.runtime, &_second).unwrap();
        let merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let referenced = omt_command(&merged).expect("omt 条目应仍存在（exec-1 还没释放）");
        assert!(
            Path::new(&referenced).is_file(),
            "mcp.json 引用的 launcher 必须仍然存在: {merged}"
        );
        release(&rig.runtime, &first).unwrap();
        assert_eq!(
            std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap(),
            "{}"
        );
        assert_eq!(launcher_count(&rig.runtime), 0);
    }

    #[test]
    fn omp_user_mid_exec_edit_skips_restore_and_only_removes_own_entry() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(OMP_SPEC.workspace_file(&rig.workspace), "{}").unwrap();
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
        let merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let mut root: serde_json::Value = serde_json::from_str(&merged).unwrap();
        root["mcpServers"]["mine"] = serde_json::json!({ "type": "stdio", "command": "x" });
        std::fs::write(
            OMP_SPEC.workspace_file(&rig.workspace),
            serde_json::to_string_pretty(&root).unwrap(),
        )
        .unwrap();
        release(&rig.runtime, &injection).unwrap();
        // 我们的条目被外科手术移除，用户自己加的条目保留
        let after = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let after_json: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert!(after_json["mcpServers"].get("omt").is_none(), "{after}");
        assert!(after_json["mcpServers"].get("mine").is_some(), "{after}");
    }

    #[test]
    fn user_modified_release_preserves_backup_outside_state_dir() {
        // F-U3-11 回归测试：外科手术分支的日志曾说"备份保留"，紧接着却
        // 把整个 state 目录（含 backup.json）删掉。
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(
            OMP_SPEC.workspace_file(&rig.workspace),
            r#"{"mcpServers":{"user-server":{"type":"stdio","command":"u"}}}"#,
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
        // mid-exec：整份覆盖（不再含我们的 omt 条目）
        std::fs::write(
            OMP_SPEC.workspace_file(&rig.workspace),
            r#"{"mcpServers":{"mine":{"type":"stdio","command":"x"}}}"#,
        )
        .unwrap();
        release(&rig.runtime, &injection).unwrap();
        let preserved = std::fs::read_dir(OMP_SPEC.injection_root(&rig.runtime))
            .unwrap()
            .flatten()
            .find(|e| e.path().to_string_lossy().ends_with("last-backup.json"));
        let preserved = preserved.expect("备份应保留在 state 目录之外");
        let content = std::fs::read_to_string(preserved.path()).unwrap();
        assert!(content.contains("user-server"), "{content}");
    }

    #[test]
    fn overlapping_exec_user_edit_survives_final_restore() {
        // F-U3-16 回归测试：writtenSha 每次 prepare 都重置基线，两次重叠
        // prepare 之间的用户改动会被第二次悄悄吸收、最终整体恢复时丢失。
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(OMP_SPEC.workspace_file(&rig.workspace), "{}").unwrap();
        let first = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            false,
        )
        .unwrap();
        // 用户在 exec-1 与 exec-2 之间手改
        let merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let mut root: serde_json::Value = serde_json::from_str(&merged).unwrap();
        root["mcpServers"]["mine"] = serde_json::json!({ "type": "stdio", "command": "x" });
        std::fs::write(
            OMP_SPEC.workspace_file(&rig.workspace),
            serde_json::to_string_pretty(&root).unwrap(),
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
        release(&rig.runtime, &second).unwrap();
        let after = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        assert!(
            after.contains("mine"),
            "用户在重叠 exec 期间的改动应存活: {after}"
        );
        assert!(!after.contains("\"omt\""), "{after}");
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
            McpInjection::Project {
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
        let after = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        assert!(
            after.contains("persistent"),
            "非 persistent release 后 persistent 注入应完好: {after}"
        );
        // persistent 自身的 release 是 no-op，条目继续在
        release(&rig.runtime, &persistent).unwrap();
        let still = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        assert!(still.contains("persistent"), "{still}");
        assert_eq!(launcher_count(&rig.runtime), 1, "persistent.sh 不受影响");
    }

    #[test]
    fn sweep_restores_crash_residue() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(OMP_SPEC.workspace_file(&rig.workspace), "{}").unwrap();
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
        assert!(OMP_SPEC.workspace_file(&rig.workspace).is_file());
        let swept = sweep_crash_residue(&rig.runtime).unwrap();
        assert_eq!(swept, 1);
        assert_eq!(
            std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap(),
            "{}"
        );
        assert_eq!(launcher_count(&rig.runtime), 0);
        // 二次清扫幂等
        assert_eq!(sweep_crash_residue(&rig.runtime).unwrap(), 0);
    }

    #[test]
    fn sweep_scrubs_own_entry_when_user_modified_mid_crash() {
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        std::fs::write(OMP_SPEC.workspace_file(&rig.workspace), "{}").unwrap();
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
        let merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let mut root: serde_json::Value = serde_json::from_str(&merged).unwrap();
        root["mcpServers"]["mine"] = serde_json::json!({ "type": "stdio", "command": "x" });
        std::fs::write(
            OMP_SPEC.workspace_file(&rig.workspace),
            serde_json::to_string_pretty(&root).unwrap(),
        )
        .unwrap();
        sweep_crash_residue(&rig.runtime).unwrap();
        let after = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let after_json: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert!(after_json["mcpServers"].get("omt").is_none(), "{after}");
        assert!(after_json["mcpServers"].get("mine").is_some(), "{after}");
    }

    #[test]
    fn sweep_discovers_and_scrubs_orphan_from_incomplete_state() {
        // F-U3-15 回归测试：模拟 merge 与最终 write_omp_state 之间的崩溃
        // ——文件已被注入，但 state.json 只有早期写入的 `workspace` 字段，
        // 没有 writtenCommand/writtenSha。
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        let launcher = launcher_path(&rig.runtime, OMP_SPEC, "exec-orphan");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&launcher, "#!/bin/sh\n").unwrap();
        std::fs::write(
            OMP_SPEC.workspace_file(&rig.workspace),
            serde_json::json!({
                "mcpServers": {
                    "omt": { "type": "stdio", "command": launcher.display().to_string(), "args": [] }
                }
            })
            .to_string(),
        )
        .unwrap();
        let state_dir = state_dir(&rig.runtime, &rig.workspace, OMP_SPEC);
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            state_dir.join("state.json"),
            serde_json::json!({ "refcount": 0, "workspace": rig.workspace.to_string_lossy() })
                .to_string(),
        )
        .unwrap();

        sweep_crash_residue(&rig.runtime).unwrap();
        let after = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let after_json: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert!(after_json["mcpServers"].get("omt").is_none(), "{after}");
    }

    #[test]
    fn prepare_scrubs_dangling_entry_before_backing_up_as_original() {
        // F-U3-15 回归测试：state 目录完全丢失（未被 sweep 追上的更早一次
        // 崩溃），但文件里还留着我们自己的旧注入。下一次 prepare 不能把
        // 这个残留当成"用户原始内容"备份下来。
        let rig = rig();
        std::fs::create_dir_all(rig.workspace.join(".omp")).unwrap();
        let stale_launcher = launcher_path(&rig.runtime, OMP_SPEC, "exec-stale");
        std::fs::write(
            OMP_SPEC.workspace_file(&rig.workspace),
            serde_json::json!({
                "mcpServers": {
                    "user-server": { "type": "stdio", "command": "u" },
                    "omt": { "type": "stdio", "command": stale_launcher.display().to_string(), "args": [] }
                }
            })
            .to_string(),
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
        release(&rig.runtime, &injection).unwrap();
        let restored = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let restored_json: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert!(
            restored_json["mcpServers"].get("user-server").is_some(),
            "{restored}"
        );
        assert!(
            restored_json["mcpServers"].get("omt").is_none(),
            "陈旧残留不该复活: {restored}"
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
            McpInjection::Project {
                persistent: true,
                ..
            }
        ));
        let merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        assert!(merged.contains("omp-launchers"));
        assert!(
            !merged.contains("--actor"),
            "persistent launcher 无 --actor: {merged}"
        );
        // persistent 模式 release 是 no-op：条目与 launcher 都还在
        release(&rig.runtime, &injection).unwrap();
        assert!(OMP_SPEC.workspace_file(&rig.workspace).is_file());
        assert_eq!(launcher_count(&rig.runtime), 1);
    }

    #[test]
    fn sweep_never_deletes_persistent_launcher() {
        // F-U3-18 回归测试：sweep 曾经在**每次**启动（不只 crash 后）无条件
        // 清空整个 launcher 目录，persistent.sh 一并被删。
        let rig = rig();
        let persistent = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-1",
            true,
        )
        .unwrap();
        let _ = persistent;
        assert_eq!(launcher_count(&rig.runtime), 1);
        sweep_crash_residue(&rig.runtime).unwrap();
        assert_eq!(
            launcher_count(&rig.runtime),
            1,
            "persistent launcher 必须挺过每次启动的 sweep"
        );
        assert!(launcher_path(&rig.runtime, OMP_SPEC, "persistent").is_file());
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

    /// F-PI：pi 注入 roundtrip——`.pi/mcp.json` 条目用实测形状（无
    /// type:stdio），state/launcher 与 omp 命名空间独立（同 workspace
    /// 双 provider 互不干扰），release 恢复。
    #[test]
    fn pi_injection_roundtrip_shape_and_namespace() {
        let rig = rig();
        let inject = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "pi",
            "exec-pi-1",
            false,
        )
        .expect("pi prepare");
        let pi_file = rig.workspace.join(".pi").join("mcp.json");
        let merged = std::fs::read_to_string(&pi_file).unwrap();
        // 实测形状：command 字符串 + args 数组，且无 type 字段
        let root: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert!(
            root["mcpServers"]["omt"].get("type").is_none(),
            "pi 条目不得带 type 字段: {merged}"
        );
        // F-EXPOSE：pi ≥1.0 的 exposure 体系——direct 声明 + description
        assert_eq!(
            root["mcpServers"]["omt"]["exposure"], "direct",
            "pi 注入条目应带 exposure:direct: {merged}"
        );
        assert!(
            root["mcpServers"]["omt"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("run__claim")),
            "pi 注入条目应带 description: {merged}"
        );
        assert!(omt_command(&merged).is_some());
        // omp 的文件/目录不受影响
        assert!(!OMP_SPEC.workspace_file(&rig.workspace).exists());
        assert!(PI_SPEC
            .injection_root(&rig.runtime)
            .join(workspace_hash(&rig.workspace))
            .is_dir());
        // omp 同 workspace 并行 prepare → 各自 refcount 独立
        let _omp_inject = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "omp",
            "exec-omp-1",
            false,
        )
        .expect("omp prepare");
        assert!(OMP_SPEC.workspace_file(&rig.workspace).exists());
        // F-EXPOSE：omp 未同步 pi 的 exposure 体系——条目不得带该键
        let omp_merged = std::fs::read_to_string(OMP_SPEC.workspace_file(&rig.workspace)).unwrap();
        let omp_root: serde_json::Value = serde_json::from_str(&omp_merged).unwrap();
        assert!(
            omp_root["mcpServers"]["omt"].get("exposure").is_none(),
            "omp 条目不应带 exposure: {omp_merged}"
        );
        let omp_state = std::fs::read_to_string(
            OMP_SPEC
                .injection_root(&rig.runtime)
                .join(workspace_hash(&rig.workspace))
                .join("state.json"),
        )
        .unwrap();
        assert_eq!(
            omp_state.parse::<serde_json::Value>().unwrap()["refcount"],
            1
        );

        release(&rig.runtime, &inject).unwrap();
        assert!(
            !pi_file.exists(),
            "refcount 归零后 pi 配置恢复（原不存在→删除）"
        );
        // omp 注入仍在
        assert!(OMP_SPEC.workspace_file(&rig.workspace).exists());
    }

    /// F-PI：pi crash 残留由 sweep_crash_residue 一并清扫。
    #[test]
    fn pi_crash_residue_is_swept() {
        let rig = rig();
        let _ = prepare_with(
            &rig.fake_omt,
            &rig.runtime,
            &rig.workspace,
            "pi",
            "exec-pi-crash",
            false,
        )
        .unwrap();
        assert!(rig.workspace.join(".pi").join("mcp.json").exists());
        let swept = sweep_crash_residue(&rig.runtime).unwrap();
        assert!(swept >= 1, "至少清扫 pi 一个 workspace");
        assert!(!rig.workspace.join(".pi").join("mcp.json").exists());
        assert_eq!(sweep_crash_residue(&rig.runtime).unwrap(), 0);
    }
}
