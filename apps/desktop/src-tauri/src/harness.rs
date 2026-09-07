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
pub(crate) fn which_binary(name: &str) -> Option<PathBuf> {
    which_binary_in(&augmented_path(), name)
}

/// `<binary> <probe_args>` 首行输出，带超时（F1：原 `.output()` 会永久挂起）。
/// 超时后 kill 子进程；探针是 best-effort，失败只表现为无版本号。
pub(crate) fn probe_version(binary: &Path, args: &[&str], timeout_secs: u64) -> Option<String> {
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
}

pub const REGISTRY: &[HarnessDescriptor] = &[
    HarnessDescriptor {
        id: "opencode",
        label: "OpenCode",
        binary_name: "opencode",
        probe_args: &["--version"],
        unattended_flag: &["--auto"],
    },
    HarnessDescriptor {
        id: "omp",
        label: "OMP (oh-my-pi)",
        binary_name: "omp",
        probe_args: &["--version"],
        unattended_flag: &["--yolo"],
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
    })
}

/// SpawnRequest doc 注释的 args 契约（顺序由本函数统一保证，调用方不再各自
/// 拼装）：`[extra_args..., (unattended=true 时 unattended_flag), prompt]`。
/// desktop 注入的 flag 在用户 args 之后——同名 flag 以 desktop 的为准。
pub fn compose_launch_args(plan: &LaunchPlan, unattended: bool, prompt: &str) -> Vec<String> {
    let mut args = plan.extra_args.clone();
    if unattended {
        args.extend(plan.unattended_flag.iter().cloned());
    }
    args.push(prompt.to_string());
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
// tauri commands（设置页数据面；探测可能各至 2s，必须 async）
// ---------------------------------------------------------------------------

fn probe_descriptor(detected: Option<&Path>, d: &HarnessDescriptor) -> Option<String> {
    detected.and_then(|path| {
        // 多线程进程里 fork→exec 偶发子进程卡死（exec 前的库锁），全量测试
        // 并行时会放大 2s 窗口误判——超时重试一次兜底。
        probe_version(path, d.probe_args, 2).or_else(|| probe_version(path, d.probe_args, 2))
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
            probe_version(&good, &["--version"], 5).as_deref(),
            Some("v1.2.3")
        );

        let hanging = dir.path().join("hanging");
        std::fs::write(&hanging, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&hanging, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(probe_version(&hanging, &["--version"], 1), None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
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
            unattended_flag: vec!["--yolo".to_string()],
            env: BTreeMap::new(),
        };
        // SpawnRequest doc 契约：用户 extra_args 在前、desktop 注入 flag 在后
        //（覆盖用户同名 flag）、prompt 收尾
        let args = compose_launch_args(&plan, true, "PROMPT");
        assert_eq!(
            args,
            vec!["--model", "k3", "--yolo", "PROMPT"],
            "unattended=true 时 args 必须含 unattended_flag 且顺序正确"
        );
        assert_eq!(
            compose_launch_args(&plan, false, "PROMPT"),
            vec!["--model", "k3", "PROMPT"],
            "unattended=false 不得追加放行 flag"
        );
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
            assert_eq!(plan.unattended_flag, vec!["--yolo".to_string()]);
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
            unattended_flag: vec!["--yolo".to_string()],
            env,
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
        assert_eq!(value["unattendedFlag"], serde_json::json!(["--yolo"]));
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
