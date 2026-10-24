//! 派发策略（设计 §3.6 / KD-E5）：`<runtime>/exec/policy.json`。
//!
//! 只约束 CLI / MCP 发起的派发（Q-4）；桌面端豁免。文件缺失 = 设计默认值；
//! 文件存在但无法解析或含未知字段 = fail closed（`POLICY_INVALID`），
//! 绝不静默回落到默认值——那会把「用户关掉的派发」重新打开。

use std::path::Path;
use std::time::Duration;

use omt_domain::error::Problem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaunchOrigin {
    Desktop,
    Cli,
    Mcp,
}

impl LaunchOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchOrigin::Desktop => "desktop",
            LaunchOrigin::Cli => "cli",
            LaunchOrigin::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceScope {
    /// 只允许在 workspace 内 `.omt` 的 home 上执行（Q-13）：全局 home 或 workspace
    /// 根等于用户主目录的派发被 `local::exec_start` 拒绝（harness 的 cwd 由 home 决定）。
    #[default]
    HomeWorkspaceOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalMode {
    /// 派发方不能改变审批模式（设计 §3.6）。注意 harness 配置里没有「是否无人值守」
    /// 开关：CLI / MCP 派发总是附加 descriptor 的无人值守参数（`--auto`、
    /// `--approval-mode=yolo` 等），与桌面端派发等效但没有那一步人工确认。
    #[default]
    Inherit,
}

/// 派发请求 maxDurationSec 的下限（§49：exec_start 对 <60 与非整数报
/// INVALID_INPUT——0/30 会被 watchdog 首个 tick 以 MaxDuration 杀掉）。
pub(crate) const MIN_DURATION_SEC: u64 = 60;
const MAX_CONCURRENT_CAP: u32 = 32;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ExecPolicy {
    pub allow_from_cli: bool,
    pub allow_from_mcp: bool,
    pub harnesses: Vec<String>,
    pub workspaces: WorkspaceScope,
    pub max_concurrent: u32,
    pub max_duration_sec: u64,
    pub approval_mode: ApprovalMode,
}

impl Default for ExecPolicy {
    fn default() -> Self {
        Self {
            allow_from_cli: true,
            allow_from_mcp: false,
            harnesses: vec!["pi".into(), "omp".into(), "opencode".into()],
            workspaces: WorkspaceScope::HomeWorkspaceOnly,
            max_concurrent: 4,
            max_duration_sec: 1800,
            approval_mode: ApprovalMode::Inherit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDenial {
    pub field: &'static str,
    pub reason: String,
}

impl PolicyDenial {
    pub fn into_problem(self) -> Problem {
        let field = self.field;
        Problem::with_details("POLICY_DENIED", self.reason, |d| {
            d.insert("field".into(), field.into());
        })
    }
}

fn denial(field: &'static str, reason: impl Into<String>) -> PolicyDenial {
    PolicyDenial {
        field,
        reason: reason.into(),
    }
}

pub fn load(runtime_dir: &Path) -> Result<ExecPolicy, Problem> {
    let path = crate::layout::policy_file(runtime_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(ExecPolicy::default()),
        Err(err) => {
            return Err(Problem::new(
                "POLICY_INVALID",
                format!("读取 {}: {err}", path.display()),
            ))
        }
    };
    let policy: ExecPolicy = serde_json::from_str(&text).map_err(|err| {
        Problem::new(
            "POLICY_INVALID",
            format!("{} 无法解析：{err}", path.display()),
        )
    })?;
    validate(&policy).map_err(|d| {
        Problem::new(
            "POLICY_INVALID",
            format!("{} 的 {} 无效：{}", path.display(), d.field, d.reason),
        )
    })?;
    Ok(policy)
}

pub fn save(runtime_dir: &Path, policy: &ExecPolicy) -> Result<(), String> {
    validate(policy).map_err(|d| format!("{}：{}", d.field, d.reason))?;
    let text = serde_json::to_string_pretty(policy).map_err(|e| e.to_string())?;
    let path = crate::layout::policy_file(runtime_dir);
    let _lock = omt_harness::atomic_json::lock_exclusive(&path, Duration::from_secs(10))?;
    omt_harness::atomic_json::write_atomic(&path, &text)
}

pub fn validate(policy: &ExecPolicy) -> Result<(), PolicyDenial> {
    if policy.max_concurrent == 0 || policy.max_concurrent > MAX_CONCURRENT_CAP {
        return Err(denial(
            "maxConcurrent",
            format!("必须在 1..={MAX_CONCURRENT_CAP} 之间"),
        ));
    }
    if policy.max_duration_sec < MIN_DURATION_SEC {
        return Err(denial(
            "maxDurationSec",
            format!("不得小于 {MIN_DURATION_SEC} 秒"),
        ));
    }
    if let Some(unknown) = policy
        .harnesses
        .iter()
        .find(|id| omt_harness::harness::descriptor_of(id).is_none())
    {
        return Err(denial("harnesses", format!("未知的 harness：{unknown}")));
    }
    Ok(())
}

pub fn check_launch(
    policy: &ExecPolicy,
    origin: LaunchOrigin,
    harness: &str,
) -> Result<(), PolicyDenial> {
    match origin {
        LaunchOrigin::Desktop => return Ok(()),
        LaunchOrigin::Cli if !policy.allow_from_cli => {
            return Err(denial("allowFromCli", "policy 未允许经 CLI 派发（exec/policy.json allowFromCli）"))
        }
        LaunchOrigin::Mcp if !policy.allow_from_mcp => {
            return Err(denial(
                "allowFromMcp",
                "policy 未允许经 MCP 派发——请在桌面端 设置 → 派发策略 中开启（exec/policy.json allowFromMcp）",
            ))
        }
        _ => {}
    }
    if !policy.harnesses.iter().any(|id| id == harness) {
        return Err(denial(
            "harnesses",
            format!("harness {harness} 不在 policy 白名单内"),
        ));
    }
    Ok(())
}

pub fn check_capacity(policy: &ExecPolicy, active: usize) -> Result<(), PolicyDenial> {
    if active >= policy.max_concurrent as usize {
        return Err(denial(
            "maxConcurrent",
            format!(
                "已有 {active} 个运行中的执行，达到 policy 上限 {}",
                policy.max_concurrent
            ),
        ));
    }
    Ok(())
}

pub fn effective_max_duration(policy: &ExecPolicy, requested: Option<Duration>) -> Duration {
    let cap = Duration::from_secs(policy.max_duration_sec);
    requested.map_or(cap, |r| r.min(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_design_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let p = load(tmp.path()).unwrap();
        assert!(p.allow_from_cli);
        assert!(!p.allow_from_mcp);
        assert_eq!(p.harnesses, vec!["pi", "omp", "opencode"]);
        assert_eq!(p.workspaces, WorkspaceScope::HomeWorkspaceOnly);
        assert_eq!(p.max_concurrent, 4);
        assert_eq!(p.max_duration_sec, 1800);
        assert_eq!(p.approval_mode, ApprovalMode::Inherit);
    }

    #[test]
    fn malformed_or_unknown_field_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::layout::exec_dir(tmp.path())).unwrap();
        let file = crate::layout::policy_file(tmp.path());
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(load(tmp.path()).unwrap_err().code, "POLICY_INVALID");
        std::fs::write(&file, r#"{"allowFromMcp": true, "alowFromCli": false}"#).unwrap();
        let err = load(tmp.path()).unwrap_err();
        assert_eq!(err.code, "POLICY_INVALID");
        assert!(err.message.contains("alowFromCli"), "{}", err.message);
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_fields() {
        let tmp = tempfile::tempdir().unwrap();
        save(
            tmp.path(),
            &ExecPolicy {
                allow_from_mcp: true,
                ..ExecPolicy::default()
            },
        )
        .unwrap();
        std::fs::write(
            crate::layout::policy_file(tmp.path()),
            r#"{"allowFromMcp": true}"#,
        )
        .unwrap();
        let p = load(tmp.path()).unwrap();
        assert!(p.allow_from_mcp);
        assert_eq!(p.max_concurrent, 4);
    }

    #[test]
    fn origin_and_whitelist_gate_launches() {
        let p = ExecPolicy::default();
        assert!(
            check_launch(&p, LaunchOrigin::Desktop, "anything").is_ok(),
            "桌面端豁免（Q-4）"
        );
        assert!(check_launch(&p, LaunchOrigin::Cli, "pi").is_ok());
        let mcp = check_launch(&p, LaunchOrigin::Mcp, "pi").unwrap_err();
        assert_eq!(mcp.field, "allowFromMcp");
        let off_list = check_launch(&p, LaunchOrigin::Cli, "claude").unwrap_err();
        assert_eq!(off_list.field, "harnesses");
        let no_cli = ExecPolicy {
            allow_from_cli: false,
            ..ExecPolicy::default()
        };
        assert_eq!(
            check_launch(&no_cli, LaunchOrigin::Cli, "pi")
                .unwrap_err()
                .field,
            "allowFromCli"
        );
    }

    #[test]
    fn capacity_and_duration_never_widen_policy() {
        let p = ExecPolicy::default();
        assert!(check_capacity(&p, 3).is_ok());
        assert_eq!(check_capacity(&p, 4).unwrap_err().field, "maxConcurrent");
        assert_eq!(effective_max_duration(&p, None), Duration::from_secs(1800));
        assert_eq!(
            effective_max_duration(&p, Some(Duration::from_secs(60))),
            Duration::from_secs(60)
        );
        assert_eq!(
            effective_max_duration(&p, Some(Duration::from_secs(99_999))),
            Duration::from_secs(1800)
        );
    }

    #[test]
    fn validate_rejects_nonsense_values() {
        let zero = ExecPolicy {
            max_concurrent: 0,
            ..ExecPolicy::default()
        };
        assert_eq!(validate(&zero).unwrap_err().field, "maxConcurrent");
        let unknown = ExecPolicy {
            harnesses: vec!["nope".into()],
            ..ExecPolicy::default()
        };
        assert_eq!(validate(&unknown).unwrap_err().field, "harnesses");
        let tiny = ExecPolicy {
            max_duration_sec: 10,
            ..ExecPolicy::default()
        };
        assert_eq!(validate(&tiny).unwrap_err().field, "maxDurationSec");
    }

    #[test]
    fn denial_maps_to_policy_denied_problem() {
        let problem = PolicyDenial {
            field: "allowFromMcp",
            reason: "x".into(),
        }
        .into_problem();
        assert_eq!(problem.code, "POLICY_DENIED");
        assert_eq!(problem.details.as_ref().unwrap()["field"], "allowFromMcp");
    }
}
