//! U1 (R-A): Provider Contract — 内置 harness 类型表 × 机器探测 × 用户配置。
//!
//! daemon 是纯协调面（KD-D1），harness 的探测、配置与启动解析全部在
//! desktop 进程内闭环。本模块是 U2/U3 的地基：
//! - `REGISTRY`/`HarnessDescriptor`：编译期内置类型表（v1 仅 opencode/omp）；
//! - `HarnessUserConfig`/`BinarySource`：机器本地用户配置（0600 落盘）；
//! - `harness_list`/`harness_config_set`：设置页数据面（async command）；
//! - `which_binary`/`probe_version`：探测原语（自 rpc_bridge 上移，探测补
//!   超时——原实现 `.output()` 直等，挂起的 binary 会永久阻塞）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::daemon;

/// GUI 启动（Finder/Dock）的 PATH 往往只有系统目录（/usr/bin:/bin:…），
/// homebrew/nvm 装的 CLI 会找不到。把常见包管理器前缀补进查找与
/// 子进程环境，避免「终端里能跑、GUI 里找不到」。
pub(crate) fn augmented_path() -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let prefixes = [
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        format!("{home}/.local/bin"),
        format!("{home}/bin"),
    ];
    let missing: Vec<String> = prefixes
        .into_iter()
        .filter(|p| !existing.split(':').any(|d| d == p.as_str()))
        .collect();
    if missing.is_empty() {
        existing
    } else {
        format!("{}:{existing}", missing.join(":"))
    }
}

/// 在给定 PATH 串上查找可执行文件（探测与测试共用；`which_binary` 用增广 PATH）。
pub(crate) fn which_binary_in(path_var: &str, name: &str) -> Option<PathBuf> {
    for dir in path_var.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(dir).join(name);
        if candidate.is_file()
            && std::fs::metadata(&candidate)
                .map(|m| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        m.permissions().mode() & 0o111 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                })
                .unwrap_or(false)
        {
            return Some(candidate);
        }
    }
    None
}

/// 在增广 PATH 上查找可执行文件。
pub fn which_binary(name: &str) -> Option<PathBuf> {
    which_binary_in(&augmented_path(), name)
}

/// `<binary> <probe_args>` 首行输出，带超时（F1：原 `.output()` 会永久挂起）。
/// 超时后 kill 子进程；探针是 best-effort，失败只表现为无版本号。
pub fn probe_version(binary: &Path, args: &[&str], timeout_secs: u64) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .env("PATH", augmented_path())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let line = loop {
        match child.try_wait() {
            // 进程已退出：stdout 落 EOF，读线程随即结束。
            Ok(Some(_)) => {
                break reader.join().ok().and_then(|text| {
                    text.lines()
                        .next()
                        .map(|line| line.trim().to_string())
                        .filter(|line| !line.is_empty())
                })
            }
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    };
    // 超时路径先 kill 再返回——读线程随管道 EOF 收尾，不悬挂。
    let _ = child.kill();
    let _ = child.wait();
    line
}

/// 超时捕获完整 stdout（probe_version 的全量变体——模型列表要整份输出）。
pub(crate) fn capture_with_timeout(
    binary: &Path,
    args: &[&str],
    timeout_secs: u64,
) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .env("PATH", augmented_path())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let output = loop {
        match child.try_wait() {
            Ok(Some(status)) => break reader.join().ok().filter(|_| status.success()),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    output.map(|text| text.trim().to_string())
}

// ---------------------------------------------------------------------------
// 静态类型表（计划 §3.1）
// ---------------------------------------------------------------------------

pub struct HarnessDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    /// PATH 探测名
    pub binary_name: &'static str,
    /// 版本探针参数
    pub probe_args: &'static [&'static str],
    /// 无人值守放行 flag（U6 权限知情确认勾选后追加；omp `--approval-mode
    /// write` 无 TTY 实测结论回填前按官方文档记 `--yolo`，显式传避免默认漂移）
    pub unattended_flag: &'static [&'static str],
    /// prompt 传递渠道（F-ACC-6：opencode 对位置参数 lstat 判文件附件，
    /// 多行长 prompt 被当路径 ENAMETOOLONG 启动即炸——改走文件附着）
    pub prompt_channel: PromptChannel,
    /// 子命令前缀（F-ACC-7：`--auto`/`-f` 均为 `opencode run` 的子命令
    /// flag，缺 `run` 时根命令不识别 → 打印帮助退出 1）。omp 无子命令。
    pub argv_prefix: &'static [&'static str],
    /// 无头/一次性模式 flag（F-ACC-12：omp 不加 `-p` 会起 TUI，向无终端
    /// 的管道发能力查询序列后挂起）。opencode 的 `run` 子命令本身即一次性。
    pub headless_flags: &'static [&'static str],
    /// 「终端登录」按钮在 Terminal 里运行的交互命令（订阅/OAuth 认证
    /// 无法在无头进程内完成）。omp 裸跑进 TUI /login；opencode 走
    /// `auth login` 子命令。
    pub login_args: &'static [&'static str],
    /// 指定模型的 flag（值接一个 model id；三家同为 `--model`）。
    pub model_args: &'static [&'static str],
    /// 模型列表命令参数（F-PI-2：opencode/omp 是 `models` 子命令，pi 是
    /// `--list-models` flag——`pi models` 会被当消息起 TUI 超时失败）。
    pub model_list_args: &'static [&'static str],
    /// 结构化输出 flag（U12：开 JSON 事件流；空 = raw 透传，未知 harness
    /// 永远 raw）。实测：opencode `--format json`、pi/omp `--mode json`。
    pub json_args: &'static [&'static str],
    /// 续跑 flag（U11，值接一个 session id；空 = 该 harness 不支持续跑，
    /// UI 隐藏「继续会话」）。实测：opencode `-s`、pi `--session`、omp `--resume`。
    pub resume_args: &'static [&'static str],
    /// MCP 工具调用方式提示（F-PROMPT：Agent 26 分析——模型曾花 6 轮探索
    /// pi 里怎么调 omt 工具、3 轮挖 homeId，158s 里 2/3 耗在发现上）。
    /// 拼进 DISPATCH_PROMPT 尾部，按各 harness 实测调用形态书写。
    pub tool_hint: &'static str,
}

/// dispatch prompt 的传递渠道（provider 适配差异，注册表静态声明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptChannel {
    /// prompt 作为最后一个位置参数（omp 等）
    Argv,
    /// prompt 写 per-exec 文件，argv 只留 `-f <path>` + 短指令（opencode）
    FileAttach,
}

pub const REGISTRY: &[HarnessDescriptor] = &[
    HarnessDescriptor {
        id: "opencode",
        label: "OpenCode",
        binary_name: "opencode",
        probe_args: &["--version"],
        unattended_flag: &["--auto"],
        prompt_channel: PromptChannel::FileAttach,
        argv_prefix: &["run"],
        headless_flags: &[],
        login_args: &["auth", "login"],
        model_args: &["--model"],
        model_list_args: &["models"],
        json_args: &["--format", "json"],
        resume_args: &["-s"],
        tool_hint: "MCP tools appear as `omt_run__claim`-style tools in your toolset — call them directly as tools.",
    },
    HarnessDescriptor {
        id: "omp",
        label: "OMP (oh-my-pi)",
        binary_name: "omp",
        probe_args: &["--version"],
        // F-ACC-12（F-U1-4 实测回填）：裸 `--yolo` 在 omp v18.0.9 不存在，
        // 真实放行 flag 是 `--approval-mode=yolo`（always-ask|write|yolo）
        unattended_flag: &["--approval-mode=yolo"],
        prompt_channel: PromptChannel::Argv,
        argv_prefix: &[],
        headless_flags: &["-p"],
        login_args: &[],
        model_args: &["--model"],
        model_list_args: &["models"],
        json_args: &["--mode", "json"],
        resume_args: &["--resume"],
        tool_hint: "MCP tools are callable via the codemode tool, e.g. `await tools.mcp__omt__run__claim({homeId, runId})`; use run__list / run__get to inspect, mcp__omt__ prefixed names for all omt tools.",
    },
    HarnessDescriptor {
        id: "pi",
        label: "Pi",
        binary_name: "pi",
        probe_args: &["--version"],
        // pi 1.0.0 实测：基础无 omp 式审批层，-p 非交互模式工具自动执行
        unattended_flag: &[],
        prompt_channel: PromptChannel::Argv,
        argv_prefix: &[],
        // --approve（信任项目文件）是 MCP 注入的硬前提——pi mcp add 实测
        // 提示「项目未受信则 .pi/mcp.json 被忽略」，漏掉即重演 F-ACC-9
        // 式静默无工具。位置参数消息无 lstat 行为（长 prompt 可直传）。
        headless_flags: &["-p", "--approve"],
        login_args: &[],
        model_args: &["--model"],
        model_list_args: &["--list-models"],
        json_args: &["--mode", "json"],
        resume_args: &["--session"],
        // F-EXPOSE：注入的 omt server 带 exposure:direct（pi ≥1.0）——工具
        // 已像内置工具一样声明给模型，直接调用即可
        tool_hint: "The omt tools (run__claim, run__report, ...) are declared directly in your toolset (named mcp__omt__run__claim etc.) — call them directly as tools.",
    },
];

pub fn descriptor_of(id: &str) -> Option<&'static HarnessDescriptor> {
    REGISTRY.iter().find(|d| d.id == id)
}

// ---------------------------------------------------------------------------
// 用户配置（机器本地：<runtime-dir>/harness/config.json）
// ---------------------------------------------------------------------------

/// binary 来源：探测到 → `Detected`（路径每次 exec_start 重探，不落死路径，
/// brew 升级换 Cellar 不失效）；检测不到 → 用户手动指定 `Manual(path)`。
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BinarySource {
    #[default]
    Detected,
    Manual {
        path: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarnessUserConfig {
    pub enabled: bool,
    pub binary: BinarySource,
    pub extra_args: Vec<String>,
    /// 启动环境变量；v1 值为字面量（${VAR} 展开入 backlog）。保留键见
    /// `validate_env`。
    pub env: BTreeMap<String, String>,
}

/// 保留键：用户设了会破坏探测/MCP 连接（§3.1 spawn env 合并规则——进程 env →
/// augmented PATH → 用户 env → 强制 OMT_RUNTIME_DIR，用户值绝不覆盖这三类）。
const RESERVED_ENV_KEYS: &[&str] = &["OMT_RUNTIME_DIR", "OMT_HOME", "PATH"];

pub fn validate_env(env: &BTreeMap<String, String>) -> Result<(), String> {
    for key in env.keys() {
        if key.is_empty() || key.contains('=') {
            return Err(format!("非法环境变量名：{key:?}（空键或含 '='）"));
        }
        if RESERVED_ENV_KEYS.iter().any(|r| *r == key) {
            return Err(format!(
                "环境变量 {key} 为保留键，由 desktop 注入，拒绝保存"
            ));
        }
    }
    Ok(())
}

/// 保存前的整份配置校验（F-U1-6：Manual 空路径在这里拒绝，不要求存在——
/// 允许「待验证」态）。
pub fn validate_config(config: &HarnessUserConfig) -> Result<(), String> {
    if let BinarySource::Manual { path } = &config.binary {
        if path.trim().is_empty() {
            return Err("手动指定的 binary 路径不能为空".to_string());
        }
    }
    validate_env(&config.env)
}

fn harness_config_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("harness").join("config.json")
}

/// 读取全部 provider 配置。文件损坏 → 改名 `.corrupt-<ts>` 留证后从空开始
/// （§3.6.6 写纪律，实现在 atomic_json）；目录不存在等其它 IO 错误同样按
/// 空配置处理（首次启动）。
pub fn load_configs(runtime_dir: &Path) -> BTreeMap<String, HarnessUserConfig> {
    let path = harness_config_path(runtime_dir);
    // 损坏降级（rename 留证 + 返回默认）由 atomic_json 统一负责。
    let (configs, _parse_failed) =
        crate::atomic_json::read_json::<BTreeMap<String, HarnessUserConfig>>(&path)
            .unwrap_or_default();
    configs
}

/// 锁内 read-merge-write（F-U1-2）：进程内锁 + companion flock + 原子替换
/// 全部由共用 helper 提供（F-U15-1/F-U1-8/F-U1-9 的关闭实现）。
pub fn upsert_config(
    runtime_dir: &Path,
    id: &str,
    config: HarnessUserConfig,
) -> Result<(), String> {
    let path = harness_config_path(runtime_dir);
    crate::atomic_json::update_json(
        &path,
        |configs: &mut BTreeMap<String, HarnessUserConfig>| {
            configs.insert(id.to_string(), config);
            true
        },
    )
    .map(|_| ())
}

/// exec_start 时的 binary 解析：`Detected` 现场重探（不落死路径），
/// `Manual` 校验文件存在且可执行；错误信息带解析后的完整路径（§3.1）。
pub fn resolve_program(cfg: &HarnessUserConfig, binary_name: &str) -> Result<PathBuf, String> {
    match &cfg.binary {
        BinarySource::Detected => which_binary(binary_name).ok_or_else(|| {
            format!("未在 PATH 检测到 {binary_name}（含常见前缀），可在设置中手动指定")
        }),
        BinarySource::Manual { path } => {
            let candidate = PathBuf::from(path);
            if !candidate.is_file() {
                return Err(format!(
                    "指定的 harness 路径不存在：{}",
                    candidate.display()
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let executable = std::fs::metadata(&candidate)
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false);
                if !executable {
                    return Err(format!(
                        "指定的 harness 路径不可执行：{}",
                        candidate.display()
                    ));
                }
            }
            Ok(candidate)
        }
    }
}

// ---------------------------------------------------------------------------
// U6 (R-B)：exec_start 的 launch 解析（真实用户配置）与预览投影
// ---------------------------------------------------------------------------

/// 一次派发的启动解析结果（§3.1：binary 现场解析、args/env 来自用户配置）。
#[derive(Debug)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub extra_args: Vec<String>,
    /// 无人值守放行 flag（descriptor 静态值；unattended=true 时由
    /// `compose_launch_args` 追加）
    pub unattended_flag: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// prompt 传递渠道（descriptor 静态值）
    pub prompt_channel: PromptChannel,
    /// 子命令前缀（descriptor 静态值，如 opencode 的 `run`）
    pub argv_prefix: Vec<String>,
    /// 无头模式 flag（descriptor 静态值，如 omp 的 `-p`）
    pub headless_flags: Vec<String>,
    /// 指定模型的 flag（descriptor 静态值，两家同为 --model）
    pub model_args: Vec<String>,
    /// 结构化输出 flag（descriptor 静态值；空 = raw）
    pub json_args: Vec<String>,
    /// 续跑 flag（descriptor 静态值；空 = 不支持）
    pub resume_args: Vec<String>,
}

impl LaunchPlan {
    /// spawn env_additions：保留键防御性再滤一次（保存时 validate_env 已拒，
    /// 手工改写的 config.json 不得经此路径破坏 desktop 注入——§3.1 spawn env
    /// 合并规则，`manager.spawn` 侧还有第三道硬校验）。
    pub fn sanitized_env(&self) -> BTreeMap<String, String> {
        self.env
            .iter()
            .filter(|(k, _)| !RESERVED_ENV_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// exec_start 的 launch 解析：读真实用户配置（无配置/未启用 → Err，含可读
/// 文案——R-B 的预览与放行 flag 因此才是实话），binary 按 BinarySource 现场解析。
pub fn resolve_launch(runtime_dir: &Path, provider_id: &str) -> Result<LaunchPlan, String> {
    let descriptor = descriptor_of(provider_id)
        .ok_or_else(|| format!("unsupported harness type: {provider_id}"))?;
    let configs = load_configs(runtime_dir);
    let cfg = configs.get(provider_id).ok_or_else(|| {
        format!("harness {provider_id} 尚未配置：请到 设置 → Harnesses 完成配置并启用")
    })?;
    if !cfg.enabled {
        return Err(format!(
            "harness {provider_id} 未启用：请到 设置 → Harnesses 启用后再派发"
        ));
    }
    let program = resolve_program(cfg, descriptor.binary_name)?;
    Ok(LaunchPlan {
        program,
        extra_args: cfg.extra_args.clone(),
        unattended_flag: descriptor
            .unattended_flag
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        env: cfg.env.clone(),
        prompt_channel: descriptor.prompt_channel,
        argv_prefix: descriptor
            .argv_prefix
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        headless_flags: descriptor
            .headless_flags
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        model_args: descriptor
            .model_args
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        json_args: descriptor
            .json_args
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        resume_args: descriptor
            .resume_args
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    })
}

/// FileAttach 渠道的 per-exec prompt 文件路径（纯函数，无 IO）：
/// `<runtime>/transcripts/<home>/<exec>.prompt.md`。非该渠道返回 None。
/// 路径确定性——调用方先定路径（compose/Agent 记录用），写盘时机自定。
pub fn prompt_file_path(
    runtime_dir: &Path,
    home_id: &str,
    exec_id: &str,
    channel: PromptChannel,
) -> Option<PathBuf> {
    if channel == PromptChannel::FileAttach {
        Some(
            runtime_dir
                .join("transcripts")
                .join(home_id)
                .join(format!("{exec_id}.prompt.md")),
        )
    } else {
        None
    }
}

/// FileAttach 渠道的短指令：完整 DISPATCH_PROMPT 在附着文件里，argv 保持
/// 单行短串（ENAMETOOLONG 的另一道保险）。
const ATTACH_MESSAGE: &str = "Execute the task in the attached file. Follow its rules exactly.";

/// SpawnRequest args 契约（顺序由本函数统一保证，调用方不再各自拼装）：
/// `[argv_prefix..., headless_flags..., extra_args..., (unattended=true 时
/// unattended_flag), (model), message, -f <prompt_file>]`（FileAttach）/
/// `[..., prompt]`（Argv）。model 经 validate_model_id 后方可入参。
/// 子命令前缀在最前（F-ACC-7：opencode 的 flag 属 `run` 子命令）；
/// desktop 注入的 flag 在用户 args 之后——同名 flag 以 desktop 的为准。
/// FileAttach 渠道（F-ACC-6）prompt 落 per-exec 文件，argv 只留短指令；
/// **message 必须在 `-f` 之前**（F-ACC-8：yargs 数组 flag 贪心吞噬后续
/// 位置参数，`-f <file> <msg>` 的 msg 会被当成第二个文件 → "File not
/// found"，已本机复现）。prompt_file 为 None 时防御性回落 argv prompt。
pub fn compose_launch_args(
    plan: &LaunchPlan,
    unattended: bool,
    prompt: &str,
    prompt_file: Option<&Path>,
    model: Option<&str>,
    resume_session: Option<&str>,
) -> Vec<String> {
    let mut args = plan.argv_prefix.clone();
    args.extend(plan.headless_flags.iter().cloned());
    // U12：结构化流 flag 紧随模式 flag（opencode `run --format json` /
    // pi·omp `-p --mode json`）
    args.extend(plan.json_args.iter().cloned());
    args.extend(plan.extra_args.iter().cloned());
    if unattended {
        args.extend(plan.unattended_flag.iter().cloned());
    }
    if let Some(model) = model {
        args.extend(plan.model_args.iter().map(|s| (*s).to_string()));
        args.push(model.to_string());
    }
    // U11：续跑 flag + session id（opencode `-s <id>` / pi `--session <id>` /
    // omp `--resume <id>`）——id 经 validate_model_id 同款字符集校验
    if let Some(session) = resume_session {
        args.extend(plan.resume_args.iter().map(|s| (*s).to_string()));
        args.push(session.to_string());
    }
    match (plan.prompt_channel, prompt_file) {
        (PromptChannel::FileAttach, Some(file)) => {
            args.push(ATTACH_MESSAGE.to_string());
            args.push("-f".to_string());
            args.push(file.display().to_string());
        }
        _ => args.push(prompt.to_string()),
    }
    args
}

/// 弹窗「详情」预览投影：program/args（用户 extra_args）/env 只回键名/
/// unattendedFlag（§3.1 secrets 纪律——env 值绝不离开 Rust 侧）。
fn launch_preview_value(plan: &LaunchPlan) -> serde_json::Value {
    serde_json::json!({
        "program": plan.program.display().to_string(),
        "args": plan.extra_args,
        "envKeys": plan.env.keys().collect::<Vec<&String>>(),
        "unattendedFlag": plan.unattended_flag,
    })
}

/// 执行弹窗「详情」的只读预览数据面（`harness_launch_preview`）：入参
/// providerId，返回 {program, args, envKeys, unattendedFlag}。
#[tauri::command(async)]
pub fn harness_launch_preview(harness: String) -> Result<serde_json::Value, String> {
    let runtime_dir = daemon::runtime_dir();
    let plan = resolve_launch(&runtime_dir, &harness)?;
    Ok(launch_preview_value(&plan))
}

// ---------------------------------------------------------------------------
// 终端登录（F-ACC-13）：订阅/OAuth 认证无法在无头派发进程内完成——
// 设置页「终端登录」按钮在本机 Terminal 里启动 harness 的交互命令。
// ---------------------------------------------------------------------------

/// 登录用的 binary 解析：与派发不同，**不要求 enabled**（用户通常先登录
/// 再启用）。Manual 路径须存在；Detected 走增广 PATH 现场重探。
fn resolve_login_program(runtime_dir: &Path, provider_id: &str) -> Result<PathBuf, String> {
    let descriptor = descriptor_of(provider_id)
        .ok_or_else(|| format!("unsupported harness type: {provider_id}"))?;
    if let Some(cfg) = load_configs(runtime_dir).get(provider_id) {
        if let BinarySource::Manual { path } = &cfg.binary {
            let p = PathBuf::from(path);
            if p.is_file() {
                return Ok(p);
            }
            return Err(format!("Manual binary 不存在：{path}"));
        }
    }
    which_binary(descriptor.binary_name).ok_or_else(|| {
        format!(
            "PATH 上未找到 {}，请先安装或在设置中手动指定",
            descriptor.binary_name
        )
    })
}

/// POSIX 单引号 shell 转义：`'` → `'\''`（路径含空格/引号时保持单参数）。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// AppleScript 字符串字面量转义：`\` 与 `"`（osascript -e 的 do script 参数）。
fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 组装 `osascript -e` 的源码：activate 前置（Terminal 抢到前台），
/// do script 在新 Terminal 窗口运行登录命令。纯函数供测试。
fn terminal_login_script(shell_command: &str) -> String {
    format!(
        "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
        escape_applescript_string(shell_command)
    )
}

// ---------------------------------------------------------------------------
// 模型列表（执行弹窗下拉）：`<binary> models` 零成本本地命令，输出格式
// per-provider 解析。解析器是纯函数（真实输出 fixture 钉在测试里）。
// ---------------------------------------------------------------------------

/// 一个可选模型（group 供下拉分组显示；omp 的 provider 组头 / opencode
/// 的 id 前缀）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub group: Option<String>,
}

/// 派发入参的 model id 校验：非空、限 `[A-Za-z0-9._:/-]`、≤128——防
/// argv 注入（换行/空白/新 flag）。拒绝时返回可读文案。
pub fn validate_model_id(model: &str) -> Result<(), String> {
    let ok_len = !model.is_empty() && model.len() <= 128;
    // 不得以 '-' 开头——`--model --flag` 会被 CLI 当新 flag 解析（注入面）
    let ok_prefix = !model.starts_with('-');
    let ok_chars = model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'));
    if ok_len && ok_prefix && ok_chars {
        Ok(())
    } else {
        Err(format!(
            "非法 model id「{model}」：仅允许字母/数字/._:/-（长度 ≤128）"
        ))
    }
}

/// opencode `models`：每行 `provider/model`（纯行输出，无表头）。
fn parse_opencode_models(output: &str) -> Vec<ModelEntry> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            // 严格形状：恰一个 '/'，两侧非空——排除任何提示/警告行
            let parts: Vec<&str> = line.split('/').collect();
            parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty()
        })
        .map(|line| ModelEntry {
            id: line.to_string(),
            group: Some(line.split('/').next().unwrap_or(line).to_string()),
        })
        .collect()
}

/// omp `models`：provider 组头 `name (N)` + 表格行 `│ model │ … │`。
fn parse_omp_models(output: &str) -> Vec<ModelEntry> {
    let mut models = Vec::new();
    let mut group: Option<String> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix('│') {
            // 表格数据行：取第一格（│ 后到下一个 │）
            if let Some(end) = stripped.find('│') {
                let id = stripped[..end].trim();
                if id != "model"
                    && !id.is_empty()
                    && id.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-')
                    })
                {
                    models.push(ModelEntry {
                        id: id.to_string(),
                        group: group.clone(),
                    });
                }
            }
            continue;
        }
        // 组头：`kimi-code (7)` 形状（数字 + 括号收尾）
        if let Some(paren) = trimmed.rfind(" (") {
            let (name, tail) = trimmed.split_at(paren);
            let digits: String = tail
                .trim_start_matches(" (")
                .trim_end_matches(')')
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            if !name.is_empty() && !digits.is_empty() {
                group = Some(name.to_string());
            }
        }
    }
    models
}

/// pi `--list-models`：空白对齐表格，表头 `provider model context …`，
/// 取前两列组成 `provider/model` id（pi `--model` 支持 provider/id 形式）。
fn parse_pi_models(output: &str) -> Vec<ModelEntry> {
    let mut models = Vec::new();
    for line in output.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 2 || cols[0] == "provider" {
            continue;
        }
        let (provider, model) = (cols[0], cols[1]);
        let id = format!("{provider}/{model}");
        if validate_model_id(&id).is_ok() {
            models.push(ModelEntry {
                id,
                group: Some(provider.to_string()),
            });
        }
    }
    models
}

fn parse_models_output(provider_id: &str, output: &str) -> Vec<ModelEntry> {
    match provider_id {
        "opencode" => parse_opencode_models(output),
        "omp" => parse_omp_models(output),
        "pi" => parse_pi_models(output),
        _ => Vec::new(),
    }
}

/// 执行弹窗「模型」下拉数据面（`harness_list_models`）：运行
/// `<binary> models`（≤8s，失败/超时 → 可读 Err，弹窗降级隐藏下拉）。
#[tauri::command(async)]
pub fn harness_list_models(harness: String) -> Result<serde_json::Value, String> {
    let runtime_dir = daemon::runtime_dir();
    let program = resolve_login_program(&runtime_dir, &harness)?;
    let descriptor =
        descriptor_of(&harness).ok_or_else(|| format!("unsupported harness type: {harness}"))?;
    let output =
        capture_with_timeout(&program, descriptor.model_list_args, 8).ok_or_else(|| {
            format!(
                "运行 {} {} 失败（未安装/超时）——模型下拉不可用",
                program.display(),
                descriptor.model_list_args.join(" ")
            )
        })?;
    let models = parse_models_output(&harness, &output);
    Ok(serde_json::json!({ "models": models }))
}

/// 设置页「终端登录」（`harness_open_login`）：在 Terminal 里运行
/// `<binary> <login_args>`（omp 裸跑 TUI；opencode 走 `auth login`）。
/// 仅 macOS（osascript / Terminal.app）；命令在用户 Terminal 中交互，
/// 凭据落在 harness 自己的全局存储（~/.omp 等），OMT 不经手。
#[tauri::command(async)]
pub fn harness_open_login(harness: String) -> Result<serde_json::Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = &harness;
        return Err("终端登录目前仅支持 macOS（Terminal.app + osascript）".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        let runtime_dir = daemon::runtime_dir();
        let program = resolve_login_program(&runtime_dir, &harness)?;
        let descriptor = descriptor_of(&harness).expect("checked in resolve_login_program");
        let mut shell_command = shell_quote(&program.display().to_string());
        for arg in descriptor.login_args {
            shell_command.push(' ');
            shell_command.push_str(&shell_quote(arg));
        }
        let script = terminal_login_script(&shell_command);
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .spawn()
            .map_err(|e| format!("打开 Terminal 失败：{e}"))?;
        Ok(serde_json::json!({ "opened": true, "program": program.display().to_string() }))
    }
}

// ---------------------------------------------------------------------------
// tauri commands（设置页数据面；探测可能各至 2s，必须 async）
// ---------------------------------------------------------------------------

fn probe_descriptor(detected: Option<&Path>, d: &HarnessDescriptor) -> Option<String> {
    detected.and_then(|path| {
        // F-BENCH-22（真因回填）：首次 exec「刚落盘的可执行文件」在 macOS
        // 有系统级评估停顿（实测退化态 p50 6.3s / worst 9.3s；重复 exec 或
        // sh 以数据方式读同文件仅 ~15-40ms；spawn 本身不报错——ETXTBSY
        // 推断已被插桩证伪）。梯子：首试 2s 快速否决 TUI 挂起（F1 语义），
        // 后续 6s×2 覆盖评估停顿窗口——旧 2s×3 梯子在停顿 >6s 时全灭。
        probe_version(path, d.probe_args, 2)
            .or_else(|| probe_version(path, d.probe_args, 6))
            .or_else(|| probe_version(path, d.probe_args, 6))
    })
}

fn harness_entry(d: &HarnessDescriptor, cfg: Option<&HarnessUserConfig>) -> serde_json::Value {
    let cfg = cfg.cloned().unwrap_or_default();
    let (detected_path, detected_version) = match &cfg.binary {
        BinarySource::Detected => {
            let path = which_binary(d.binary_name);
            let version = probe_descriptor(path.as_deref(), d);
            (path, version)
        }
        BinarySource::Manual { path } => {
            // 手动路径同样探针回显；探针失败 = 「待验证」（§3.1）
            let path = PathBuf::from(path);
            let version = probe_descriptor(Some(path.as_path()), d);
            (Some(path), version)
        }
    };
    let state = match &cfg.binary {
        BinarySource::Manual { .. } => {
            if detected_version.is_some() {
                "ready"
            } else {
                "pendingVerification"
            }
        }
        BinarySource::Detected => {
            if detected_path.is_some() {
                "ready"
            } else {
                "notDetected"
            }
        }
    };
    let manual_path = match &cfg.binary {
        BinarySource::Manual { path } => Some(path.clone()),
        BinarySource::Detected => None,
    };
    // F-U1-5：installed 表达「binary 文件真实存在」——Manual 路径缺失时
    // 不得与 pendingVerification 并存出 true。
    let installed = match &cfg.binary {
        BinarySource::Detected => detected_path.is_some(),
        BinarySource::Manual { path } => Path::new(path).is_file(),
    };
    serde_json::json!({
        "id": d.id,
        "label": d.label,
        "installed": installed,
        "detectedPath": detected_path.map(|p| p.display().to_string()),
        "detectedVersion": detected_version,
        "enabled": cfg.enabled,
        "binarySource": match cfg.binary {
            BinarySource::Detected => "detected",
            BinarySource::Manual { .. } => "manual",
        },
        "manualPath": manual_path,
        "state": state,
        "unattendedFlag": d.unattended_flag,
        // U7 (R-A)：设置页编辑需要回显已存配置（v1 值为字面量）——否则任何
        // 保存（含 Enabled 切换）都会以未知空值全量替换既有 env/args。只读
        // 投影，零语义变化；§3.1 secrets 纪律约束的是 transcript/execs/预览，
        // 设置页回显的是用户刚写入的自身配置（config.json 0600 机器本地）。
        "extraArgs": cfg.extra_args,
        "env": cfg.env,
    })
}

/// 设置页 Harnesses tab 的数据面：类型表 × 探测 × 用户配置投影。
#[tauri::command(async)]
pub fn harness_list() -> Result<serde_json::Value, String> {
    let runtime_dir = daemon::runtime_dir();
    let configs = load_configs(&runtime_dir);
    let harnesses: Vec<serde_json::Value> = REGISTRY
        .iter()
        .map(|d| harness_entry(d, configs.get(d.id)))
        .collect();
    Ok(serde_json::json!({ "harnesses": harnesses }))
}

/// 保存一个 provider 的用户配置：校验 id + 整份配置（Manual 空路径、env
/// 保留键/非法键拒绝），锁内 read-merge-write 原子落盘，返回最新投影。
#[tauri::command(async)]
pub fn harness_config_set(
    harness: String,
    config: HarnessUserConfig,
) -> Result<serde_json::Value, String> {
    let d =
        descriptor_of(&harness).ok_or_else(|| format!("unsupported harness type: {harness}"))?;
    validate_config(&config)?;
    let runtime_dir = daemon::runtime_dir();
    upsert_config(&runtime_dir, &harness, config)?;
    let configs = load_configs(&runtime_dir);
    Ok(harness_entry(d, configs.get(&harness)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_validation_rejects_reserved_and_illegal_keys() {
        let mut env = BTreeMap::new();
        for reserved in ["OMT_RUNTIME_DIR", "OMT_HOME", "PATH"] {
            env.insert(reserved.to_string(), "x".to_string());
            assert!(validate_env(&env).is_err(), "{reserved} 应为保留键");
            env.clear();
        }
        env.insert(String::new(), "v".to_string());
        assert!(validate_env(&env).is_err(), "空键应拒绝");
        env.insert("A=B".to_string(), "v".to_string());
        assert!(validate_env(&env).is_err(), "含 '=' 的键应拒绝");
        env.clear();
        env.insert("MY_FLAG".to_string(), "1".to_string());
        assert!(validate_env(&env).is_ok());
    }

    #[test]
    fn config_roundtrip_preserves_binary_source() {
        let mut env = BTreeMap::new();
        env.insert("MY_FLAG".to_string(), "1".to_string());
        let mut configs = BTreeMap::new();
        configs.insert(
            "omp".to_string(),
            HarnessUserConfig {
                enabled: true,
                binary: BinarySource::Manual {
                    path: "/usr/local/bin/omp".to_string(),
                },
                extra_args: vec!["--model".to_string(), "k3".to_string()],
                env,
            },
        );
        let text = serde_json::to_string(&configs).unwrap();
        assert!(text.contains("\"kind\":\"manual\""), "{text}");
        let parsed: BTreeMap<String, HarnessUserConfig> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, configs);
        // 缺省字段回退默认（binary=Detected、env/args 空）
        let minimal: BTreeMap<String, HarnessUserConfig> =
            serde_json::from_str(r#"{ "opencode": { "enabled": true } }"#).unwrap();
        assert_eq!(minimal["opencode"].binary, BinarySource::Detected);
        assert!(minimal["opencode"].env.is_empty());
    }

    #[test]
    fn corrupt_config_downgrades_to_empty_with_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness").join("config.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let configs = load_configs(dir.path());
        assert!(configs.is_empty());
        let restored: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            restored
                .iter()
                .any(|n| n.starts_with("config.json.corrupt-")),
            "{restored:?}"
        );
    }

    #[test]
    fn validate_config_rejects_empty_manual_path() {
        let empty = HarnessUserConfig {
            binary: BinarySource::Manual {
                path: "   ".to_string(),
            },
            ..Default::default()
        };
        let err = validate_config(&empty).unwrap_err();
        assert!(err.contains("不能为空"), "{err}");
        // 存在性不在这里要求（允许「待验证」态）——合法路径即通过
        let plausible = HarnessUserConfig {
            binary: BinarySource::Manual {
                path: "/opt/harness/maybe-later".to_string(),
            },
            ..Default::default()
        };
        assert!(validate_config(&plausible).is_ok());
    }

    #[test]
    fn concurrent_upsert_does_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for i in 0..8u32 {
                let dir = dir.path().to_path_buf();
                scope.spawn(move || {
                    for _ in 0..5 {
                        upsert_config(
                            &dir,
                            &format!("provider-{i}"),
                            HarnessUserConfig {
                                enabled: true,
                                ..Default::default()
                            },
                        )
                        .unwrap();
                    }
                });
            }
        });
        let configs = load_configs(dir.path());
        assert_eq!(configs.len(), 8, "并发 upsert 不得丢 key：{configs:?}");
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_is_0600_and_parseable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        upsert_config(
            dir.path(),
            "opencode",
            HarnessUserConfig {
                enabled: true,
                ..Default::default()
            },
        )
        .unwrap();
        let path = dir.path().join("harness").join("config.json");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mut expected = BTreeMap::new();
        expected.insert(
            "opencode".to_string(),
            HarnessUserConfig {
                enabled: true,
                ..Default::default()
            },
        );
        assert_eq!(load_configs(dir.path()), expected);
        let leftovers: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "tmp 残留: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn which_binary_in_requires_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("fake-harness");
        std::fs::write(&candidate, "#!/bin/sh\necho ok\n").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(which_binary_in(dir.path().to_str().unwrap(), "fake-harness").is_none());
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            which_binary_in(dir.path().to_str().unwrap(), "fake-harness"),
            Some(candidate)
        );
    }

    #[cfg(unix)]
    #[test]
    fn probe_version_times_out_hanging_binary() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        std::fs::write(&good, "#!/bin/sh\necho v1.2.3\n").unwrap();
        std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            // F-BENCH-22：现写脚本首跑有 exec 评估停顿（退化态实测 worst
            // 9.3s）——预算须远超停顿窗口；hanging 相位单独计时
            probe_version(&good, &["--version"], 20).as_deref(),
            Some("v1.2.3")
        );

        let hanging = dir.path().join("hanging");
        std::fs::write(&hanging, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&hanging, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hang_started = std::time::Instant::now();
        assert_eq!(probe_version(&hanging, &["--version"], 1), None);
        assert!(
            hang_started.elapsed() < std::time::Duration::from_secs(10),
            "探针必须在超时后返回而不是永久挂起"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_program_validates_manual_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let missing = HarnessUserConfig {
            binary: BinarySource::Manual {
                path: dir.path().join("nope").display().to_string(),
            },
            ..Default::default()
        };
        let err = resolve_program(&missing, "omp").unwrap_err();
        assert!(err.contains("不存在"), "{err}");

        let non_exec = dir.path().join("tool");
        std::fs::write(&non_exec, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();
        let cfg = HarnessUserConfig {
            binary: BinarySource::Manual {
                path: non_exec.display().to_string(),
            },
            ..Default::default()
        };
        assert!(resolve_program(&cfg, "tool")
            .unwrap_err()
            .contains("不可执行"));
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_program(&cfg, "tool").unwrap(), non_exec);

        // Detected：binary 未在 PATH 时报错信息含 binary 名（现场重探语义由 U2 集成验证）
        let detected_missing = HarnessUserConfig::default();
        assert!(resolve_program(&detected_missing, "definitely-not-on-path-xyz").is_err());
    }

    // ── U6 (R-B)：launch 解析接真实用户配置 ────────────────────────────────

    #[test]
    fn compose_launch_args_extra_args_first_flag_then_prompt() {
        let plan = LaunchPlan {
            program: PathBuf::from("/usr/local/bin/omp"),
            extra_args: vec!["--model".to_string(), "k3".to_string()],
            unattended_flag: vec!["--approval-mode=yolo".to_string()],
            env: BTreeMap::new(),
            prompt_channel: PromptChannel::Argv,
            argv_prefix: vec![],
            headless_flags: vec!["-p".to_string()],
            model_args: vec!["--model".to_string()],
            json_args: vec![],
            resume_args: vec![],
        };
        // SpawnRequest doc 契约：用户 extra_args 在前、desktop 注入 flag 在后
        //（覆盖用户同名 flag）、prompt 收尾
        let args = compose_launch_args(&plan, true, "PROMPT", None, None, None);
        assert_eq!(
            args,
            vec!["-p", "--model", "k3", "--approval-mode=yolo", "PROMPT"],
            "unattended=true 时 args 必须含 headless 与放行 flag 且顺序正确"
        );
        assert_eq!(
            compose_launch_args(&plan, false, "PROMPT", None, None, None),
            vec!["-p", "--model", "k3", "PROMPT"],
            "unattended=false 不得追加放行 flag（headless 始终在）"
        );
    }

    /// 模型解析：两家真实输出 fixture 钉形状（opencode 纯行 / omp 分组表格，
    /// 表头行 `│ model │` 不得误收）。
    #[test]
    fn parse_models_output_handles_both_providers() {
        let oc = parse_models_output(
            "opencode",
            "opencode/glm-5.3-flash-free\nzai-coding-plan/glm-4.7\nnot-a-model-line\n",
        );
        assert_eq!(
            oc,
            vec![
                ModelEntry {
                    id: "opencode/glm-5.3-flash-free".into(),
                    group: Some("opencode".into())
                },
                ModelEntry {
                    id: "zai-coding-plan/glm-4.7".into(),
                    group: Some("zai-coding-plan".into())
                },
            ]
        );
        let omp_out = concat!(
            "kimi-code (7)\n",
            "┌───────────┬─────────┐\n",
            "│ model     │ context │\n",
            "├───────────┼─────────┤\n",
            "│ k3        │      1M │\n",
            "│ kimi-k2.5 │    262K │\n",
        );
        let omp = parse_models_output("omp", omp_out);
        assert_eq!(
            omp,
            vec![
                ModelEntry {
                    id: "k3".into(),
                    group: Some("kimi-code".into())
                },
                ModelEntry {
                    id: "kimi-k2.5".into(),
                    group: Some("kimi-code".into())
                },
            ]
        );
    }

    /// model id 校验：正常 id 放行；argv 注入形状（换行/空白/flag 前缀/
    /// 空/超长）拒绝。
    #[test]
    fn validate_model_id_rejects_injection_shapes() {
        assert!(validate_model_id("zai-coding-plan/glm-4.7").is_ok());
        assert!(validate_model_id("k3").is_ok());
        for bad in ["", "k3 -x", "k3\n--flag", "--flag", &"a".repeat(129)] {
            assert!(validate_model_id(bad).is_err(), "应拒绝: {bad:?}");
        }
    }

    /// compose：model 位于 unattended flag 之后、prompt 位之前。
    #[test]
    fn compose_launch_args_places_model_before_prompt() {
        let plan = LaunchPlan {
            program: PathBuf::from("/usr/local/bin/omp"),
            extra_args: vec![],
            unattended_flag: vec!["--approval-mode=yolo".to_string()],
            env: BTreeMap::new(),
            prompt_channel: PromptChannel::Argv,
            argv_prefix: vec![],
            headless_flags: vec!["-p".to_string()],
            model_args: vec!["--model".to_string()],
            json_args: vec![],
            resume_args: vec![],
        };
        assert_eq!(
            compose_launch_args(&plan, true, "PROMPT", None, Some("k3"), None),
            vec!["-p", "--approval-mode=yolo", "--model", "k3", "PROMPT"],
        );
    }

    /// F-PI：pi 的 argv 形态——`-p --approve`（MCP 注入需信任项目文件），
    /// 无放行 flag，prompt 位置参数直传。
    #[test]
    fn compose_launch_args_pi_headless_with_approve() {
        let plan = LaunchPlan {
            program: PathBuf::from("/usr/local/bin/pi"),
            extra_args: vec![],
            unattended_flag: vec![],
            env: BTreeMap::new(),
            prompt_channel: PromptChannel::Argv,
            argv_prefix: vec![],
            headless_flags: vec!["-p".to_string(), "--approve".to_string()],
            model_args: vec!["--model".to_string()],
            json_args: vec!["--mode".to_string(), "json".to_string()],
            resume_args: vec!["--session".to_string()],
        };
        assert_eq!(
            compose_launch_args(&plan, true, "LONG PROMPT", None, None, None),
            vec!["-p", "--approve", "--mode", "json", "LONG PROMPT"],
            "无放行 flag 时 unattended=true 不追加任何项"
        );
        // U11：续跑 session 紧随其后（payload 之前）
        assert_eq!(
            compose_launch_args(&plan, true, "P", None, None, Some("ses_pi1")),
            vec![
                "-p",
                "--approve",
                "--mode",
                "json",
                "--session",
                "ses_pi1",
                "P"
            ],
        );
        assert_eq!(
            compose_launch_args(&plan, true, "P", None, Some("kimi-coding/k3"), None),
            vec![
                "-p",
                "--approve",
                "--mode",
                "json",
                "--model",
                "kimi-coding/k3",
                "P"
            ],
        );
    }

    /// F-PI：pi --list-models 表格解析（表头跳过，provider/model 组 id）。
    #[test]
    fn parse_pi_models_table() {
        let out = concat!(
            "provider     model                      context  max-out  thinking  images\n",
            "kimi-coding  k3                         1.0M     131.1K   yes       yes\n",
            "openai       gpt-4.1                    1.0M     32.8K    no        no\n",
        );
        assert_eq!(
            parse_models_output("pi", out),
            vec![
                ModelEntry {
                    id: "kimi-coding/k3".into(),
                    group: Some("kimi-coding".into())
                },
                ModelEntry {
                    id: "openai/gpt-4.1".into(),
                    group: Some("openai".into())
                },
            ]
        );
    }

    /// F-ACC-13：终端登录脚本的转义——敌意路径（空格/单引号/双引号/
    /// 反斜杠）不得破坏 shell 引号或 AppleScript 字面量。
    #[test]
    fn terminal_login_script_escapes_hostile_paths() {
        let normal = shell_quote("/opt/homebrew/bin/omp");
        assert_eq!(normal, "'/opt/homebrew/bin/omp'");
        let hostile = shell_quote("/x/my omp/y'z\"w");
        // 单引号 → '\''，整串仍被引号包裹
        assert!(hostile.starts_with("'/x/my omp"), "{hostile}");
        assert!(hostile.contains(r"'\''"), "单引号必须转义: {hostile}");

        let script = terminal_login_script(&hostile);
        assert!(script.contains("activate"), "Terminal 抢前置前: {script}");
        // 期望行手写字面量（shell 单引号转义 + AppleScript 反斜杠/双引号
        // 转义各就各位）——精确钉住两级转义的产物
        let line = script.lines().nth(2).unwrap_or_default();
        assert_eq!(
            line, r##"do script "'/x/my omp/y'\\''z\"w'""##,
            "敌意路径的两级转义: {script}"
        );

        // registry：omp 裸跑（TUI /login），opencode 走 auth login
        assert_eq!(descriptor_of("omp").unwrap().login_args, &[] as &[&str]);
        // F-PI-2：pi 的模型列表是 flag 不是子命令
        assert_eq!(
            descriptor_of("pi").unwrap().model_list_args,
            &["--list-models"] as &[&str]
        );
        assert_eq!(
            descriptor_of("omp").unwrap().model_list_args,
            &["models"] as &[&str]
        );
        assert_eq!(
            descriptor_of("opencode").unwrap().login_args,
            &["auth", "login"] as &[&str]
        );
    }

    /// F-ACC-13：登录解析不要求 enabled（先登录后启用是常态流）。
    #[test]
    fn resolve_login_program_works_without_enabled_config() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("omp-tool");
        std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        upsert_config(
            dir.path(),
            "omp",
            HarnessUserConfig {
                enabled: false,
                binary: BinarySource::Manual {
                    path: tool.display().to_string(),
                },
                ..Default::default()
            },
        )
        .unwrap();
        let p = resolve_login_program(dir.path(), "omp").expect("未启用也应解析出登录 binary");
        assert_eq!(p, tool);
        // Manual 路径不存在 → 可读错误（不静默回落 Detected）
        upsert_config(
            dir.path(),
            "omp",
            HarnessUserConfig {
                enabled: false,
                binary: BinarySource::Manual {
                    path: dir.path().join("nope").display().to_string(),
                },
                ..Default::default()
            },
        )
        .unwrap();
        let err = resolve_login_program(dir.path(), "omp").unwrap_err();
        assert!(err.contains("不存在"), "{err}");
    }

    /// F-ACC-6 回归：opencode 对位置参数 lstat 判附件，多行长 prompt 被
    /// 当路径 ENAMETOOLONG——FileAttach 渠道 argv 只留 -f + 短指令。
    #[test]
    fn compose_launch_args_file_attach_avoids_long_positional() {
        let plan = LaunchPlan {
            program: PathBuf::from("/opencode/bin/opencode"),
            extra_args: vec![],
            unattended_flag: vec!["--auto".to_string()],
            env: BTreeMap::new(),
            prompt_channel: PromptChannel::FileAttach,
            argv_prefix: vec!["run".to_string()],
            headless_flags: vec![],
            model_args: vec!["--model".to_string()],
            json_args: vec![],
            resume_args: vec![],
        };
        let long_prompt = "You are executing tickets…\nrule 1\nrule 2";
        let args = compose_launch_args(
            &plan,
            true,
            long_prompt,
            Some(Path::new("/rt/transcripts/h/exec-1.prompt.md")),
            None,
            None,
        );
        assert_eq!(
            args,
            vec![
                "run",
                "--auto",
                ATTACH_MESSAGE,
                "-f",
                "/rt/transcripts/h/exec-1.prompt.md"
            ]
        );
        assert!(
            !args.iter().any(|a| a.contains('\n')),
            "argv 不得再含多行 prompt: {args:?}"
        );
        // 渠道为 FileAttach 但文件缺失 → 防御性回落 argv prompt
        let fallback = compose_launch_args(&plan, true, long_prompt, None, None, None);
        assert_eq!(fallback.last().map(String::as_str), Some(long_prompt));
    }

    #[test]
    fn resolve_launch_requires_enabled_user_config() {
        let dir = tempfile::tempdir().unwrap();
        // 无配置 → Err（含可读文案）
        let err = resolve_launch(dir.path(), "omp").unwrap_err();
        assert!(err.contains("omp"), "{err}");
        // 未启用 → Err
        upsert_config(
            dir.path(),
            "omp",
            HarnessUserConfig {
                enabled: false,
                ..Default::default()
            },
        )
        .unwrap();
        let err = resolve_launch(dir.path(), "omp").unwrap_err();
        assert!(err.contains("未启用"), "{err}");
        // enabled + Manual 可执行路径 → program/flags/extra_args 如实解析
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tool = dir.path().join("omp-tool");
            std::fs::write(&tool, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
            upsert_config(
                dir.path(),
                "omp",
                HarnessUserConfig {
                    enabled: true,
                    binary: BinarySource::Manual {
                        path: tool.display().to_string(),
                    },
                    extra_args: vec!["--model".to_string()],
                    ..Default::default()
                },
            )
            .unwrap();
            let plan = resolve_launch(dir.path(), "omp").unwrap();
            assert_eq!(plan.program, tool);
            assert_eq!(
                plan.unattended_flag,
                vec!["--approval-mode=yolo".to_string()]
            );
            assert_eq!(plan.headless_flags, vec!["-p".to_string()]);
            assert_eq!(plan.extra_args, vec!["--model".to_string()]);
        }
        // 未知 provider → Err
        assert!(resolve_launch(dir.path(), "definitely-not-a-provider").is_err());
    }

    #[test]
    fn sanitized_env_filters_reserved_keys_defensively() {
        // 保存时 validate_env 已拒绝保留键；这里防御手工改写的 config.json
        let mut env = BTreeMap::new();
        env.insert("MY_FLAG".to_string(), "1".to_string());
        for reserved in ["OMT_RUNTIME_DIR", "OMT_HOME", "PATH"] {
            env.insert(reserved.to_string(), "/evil".to_string());
        }
        let plan = LaunchPlan {
            program: PathBuf::from("/bin/omp"),
            extra_args: vec![],
            unattended_flag: vec![],
            env,
            prompt_channel: PromptChannel::Argv,
            argv_prefix: vec![],
            headless_flags: vec![],
            model_args: vec!["--model".to_string()],
            json_args: vec![],
            resume_args: vec![],
        };
        let filtered = plan.sanitized_env();
        assert_eq!(filtered.len(), 1, "{filtered:?}");
        assert_eq!(filtered.get("MY_FLAG").map(String::as_str), Some("1"));
    }

    #[test]
    fn launch_preview_projects_env_keys_never_values() {
        // §3.1 secrets 纪律：预览只回键名，任何位置不得出现 env 值
        let mut env = BTreeMap::new();
        env.insert("API_KEY".to_string(), "sk-secret-value".to_string());
        let plan = LaunchPlan {
            program: PathBuf::from("/bin/omp"),
            extra_args: vec!["--model".to_string()],
            unattended_flag: vec!["--approval-mode=yolo".to_string()],
            env,
            prompt_channel: PromptChannel::Argv,
            argv_prefix: vec![],
            headless_flags: vec![],
            model_args: vec!["--model".to_string()],
            json_args: vec![],
            resume_args: vec![],
        };
        let value = launch_preview_value(&plan);
        let keys: Vec<&str> = value["envKeys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert_eq!(keys, vec!["API_KEY"]);
        let text = serde_json::to_string(&value).unwrap();
        assert!(!text.contains("sk-secret-value"), "{text}");
        assert_eq!(value["program"], "/bin/omp");
        assert_eq!(value["args"], serde_json::json!(["--model"]));
        assert_eq!(
            value["unattendedFlag"],
            serde_json::json!(["--approval-mode=yolo"])
        );
    }

    #[cfg(unix)]
    #[test]
    fn harness_entry_projects_state_from_binary_source() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tool");
        std::fs::write(&tool, "#!/bin/sh\necho v9\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let d = descriptor_of("omp").unwrap();

        // Manual + 探针成功 → ready
        let ready = harness_entry(
            d,
            Some(&HarnessUserConfig {
                enabled: true,
                binary: BinarySource::Manual {
                    path: tool.display().to_string(),
                },
                extra_args: vec!["--model".to_string(), "k3".to_string()],
                env: BTreeMap::from([("MY_FLAG".to_string(), "1".to_string())]),
            }),
        );
        assert_eq!(ready["state"], "ready");
        assert_eq!(ready["installed"], true);
        assert_eq!(ready["detectedVersion"], "v9");
        assert_eq!(ready["binarySource"], "manual");
        // U7：设置页回显已存 extra_args/env（v1 值为字面量）——缺失会让任何
        // 保存以空值全量替换既有配置
        assert_eq!(ready["extraArgs"], serde_json::json!(["--model", "k3"]));
        assert_eq!(ready["env"], serde_json::json!({ "MY_FLAG": "1" }));

        // Manual + 探针失败 → pendingVerification（「待验证」）
        let pending = harness_entry(
            d,
            Some(&HarnessUserConfig {
                enabled: true,
                binary: BinarySource::Manual {
                    path: dir.path().join("missing").display().to_string(),
                },
                ..Default::default()
            }),
        );
        assert_eq!(pending["state"], "pendingVerification");
        assert_eq!(pending["installed"], false, "F-U1-5：路径缺失不得报已安装");

        // 默认配置（Detected）→ installed 跟随探测（测试环境无 omp 也可能真有——
        // 只断言 state 与 installed 自洽）
        let projected = harness_entry(d, None);
        let installed = projected["installed"].as_bool().unwrap();
        let state = projected["state"].as_str().unwrap();
        assert_eq!(
            installed,
            state == "ready",
            "Detected 投影：installed 与 state 必须自洽"
        );
        assert_eq!(projected["binarySource"], "detected");
    }
}
