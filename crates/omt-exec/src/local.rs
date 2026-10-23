//! 子项目 2 本地工具核心（设计 §3.5 / KD-E4）：harness / exec / policy 的
//! JSON 进、JSON 出。CLI 动词与 MCP 工具（omt-runtime 的 LocalImpl）只做
//! 参数搬运，语义全部在这里——两面行为一致，测试也只需一处。

use std::path::{Path, PathBuf};
use std::time::Duration;

use omt_domain::error::Problem;
use serde_json::{json, Value};

use crate::executor::LaunchTarget;
use crate::policy::LaunchOrigin;

pub struct Ctx {
    pub runtime_dir: PathBuf,
    /// 拉起 shim 的 `omt` 二进制（即当前进程的 current_exe）。
    pub omt_bin: PathBuf,
    /// 同目录的 `omt-daemon`（缺失时只能连接已在线的 daemon）。
    pub daemon_bin: Option<PathBuf>,
}

fn invalid(field: &str, message: impl Into<String>) -> Problem {
    let field = field.to_string();
    Problem::with_details("INVALID_INPUT", message.into(), |d| {
        d.insert("field".into(), field.into());
    })
}

fn not_found(message: impl Into<String>) -> Problem {
    Problem::new("NOT_FOUND", message.into())
}

fn io(message: impl Into<String>) -> Problem {
    Problem::new("IO", message.into())
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// 本地工具路径的 harness 投影：与桌面端设置页同源，但 env 只给键名、extraArgs
/// 脱敏——工具输出会进入主控 agent 的上下文与 transcript（secrets 纪律）。
pub fn harness_list(ctx: &Ctx) -> Result<Value, Problem> {
    let mut value = omt_harness::harness::list_value(&ctx.runtime_dir).map_err(io)?;
    if let Some(entries) = value["harnesses"].as_array_mut() {
        for entry in entries {
            let keys: Vec<Value> = entry["env"]
                .as_object()
                .map(|env| env.keys().map(|k| json!(k)).collect())
                .unwrap_or_default();
            let args: Vec<String> = entry["extraArgs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(obj) = entry.as_object_mut() {
                obj.remove("env");
                obj.insert("envKeys".into(), Value::Array(keys));
                obj.insert(
                    "extraArgs".into(),
                    json!(omt_harness::sanitize::sanitize_args(&args)),
                );
            }
        }
    }
    Ok(value)
}

pub fn harness_models(ctx: &Ctx, harness: &str) -> Result<Value, Problem> {
    if omt_harness::harness::descriptor_of(harness).is_none() {
        return Err(invalid("harness", format!("未知的 harness：{harness}")));
    }
    omt_harness::harness::models_value(&ctx.runtime_dir, harness).map_err(io)
}

pub fn policy_show(ctx: &Ctx) -> Result<Value, Problem> {
    let source = if crate::layout::policy_file(&ctx.runtime_dir).is_file() {
        "file"
    } else {
        "default"
    };
    let policy = crate::policy::load(&ctx.runtime_dir)?;
    Ok(json!({
        "policy": policy,
        "source": source,
        "path": crate::layout::policy_file(&ctx.runtime_dir).display().to_string(),
    }))
}

/// Q-3：解析目标 home → (homeId, .omt 路径, kind)，一律以 daemon 的映射为准。
/// MCP 只接受已服务的 `homeId`（`home` 键被忽略，MCP 不能借它声明 home，R7）；
/// CLI 另认 `home`（路径或 homeId），给出已初始化但未被服务的路径时经
/// DaemonLink::connect 声明。
fn resolve_home(
    ctx: &Ctx,
    origin: LaunchOrigin,
    args: &Value,
) -> Result<(String, PathBuf, String), Problem> {
    let cli_home = match origin {
        LaunchOrigin::Cli => opt_str(args, "home"),
        _ => None,
    };
    let wanted = opt_str(args, "homeId")
        .or(cli_home)
        .ok_or_else(|| invalid("homeId", "需要 homeId（CLI：全局 --home）"))?;
    let served = crate::daemon_link::served_homes(&ctx.runtime_dir, ctx.daemon_bin.as_deref())
        .map_err(io)?;
    let canonical = std::fs::canonicalize(wanted).ok();
    if let Some(home) = served.iter().find(|h| {
        h.home_id == wanted
            || canonical
                .as_ref()
                .is_some_and(|c| std::fs::canonicalize(&h.path).ok().as_ref() == Some(c))
    }) {
        return Ok((home.home_id.clone(), home.path.clone(), home.kind.clone()));
    }
    if cli_home.is_some() {
        if let Some(path) = canonical.filter(|p| p.join("omt.db").is_file()) {
            let link = crate::daemon_link::DaemonLink::connect(
                &ctx.runtime_dir,
                "cli",
                ctx.daemon_bin.as_deref(),
                &path,
                None,
            )
            .map_err(io)?;
            if let Some(id) = link.home_id_for(&path) {
                let kind =
                    crate::daemon_link::served_homes(&ctx.runtime_dir, ctx.daemon_bin.as_deref())
                        .map_err(io)?
                        .into_iter()
                        .find(|h| h.home_id == id)
                        .map(|h| h.kind)
                        .unwrap_or_else(|| "workspace".into());
                return Ok((id, path, kind));
            }
        }
    }
    Err(not_found(format!(
        "home {wanted} 不在 daemon 当前服务的 home 中"
    )))
}

/// Q-13：`workspaces: homeWorkspaceOnly` 的纯判定。全局 home，或 workspace 根
/// （与执行器同一规则 `executor::workspace_root_of`：尾段是 `.omt` 才剥掉）等于
/// 用户主目录，都会让无人值守 harness 以 $HOME 为 cwd 运行。
pub fn workspace_scope_denial(
    kind: &str,
    home_path: &Path,
    user_home: Option<&Path>,
) -> Option<crate::policy::PolicyDenial> {
    let root = crate::executor::workspace_root_of(home_path);
    let at_user_home = user_home.is_some_and(|h| {
        std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone())
            == std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf())
    });
    (kind == "global" || at_user_home).then(|| crate::policy::PolicyDenial {
        field: "workspaces",
        reason: format!(
            "policy 只允许在 workspace 内的 home 上执行（{} 的工作目录会是用户主目录）",
            home_path.display()
        ),
    })
}

pub fn exec_start(ctx: &Ctx, origin: LaunchOrigin, args: &Value) -> Result<Value, Problem> {
    let harness = opt_str(args, "harness").ok_or_else(|| invalid("harness", "需要 harness"))?;
    let target = match (opt_str(args, "nodeId"), opt_str(args, "runId")) {
        (Some(node), None) => LaunchTarget::Node(node),
        (None, Some(run)) => LaunchTarget::Run(run),
        _ => return Err(invalid("nodeId", "nodeId 与 runId 必须且只能给一个")),
    };
    // 先判 policy（不碰 daemon）：被拒的请求不留任何痕迹
    let policy = crate::policy::load(&ctx.runtime_dir)?;
    crate::policy::check_launch(&policy, origin, harness).map_err(|d| d.into_problem())?;
    let requested = args
        .get("maxDurationSec")
        .and_then(Value::as_u64)
        .map(Duration::from_secs);
    let (home_id, home_path, home_kind) = resolve_home(ctx, origin, args)?;
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(denial) = workspace_scope_denial(&home_kind, &home_path, user_home.as_deref()) {
        return Err(denial.into_problem());
    }
    let rpc = crate::daemon_link::plain_connector(ctx.runtime_dir.clone(), ctx.daemon_bin.clone())(
        &home_path, &home_id,
    )
    .map_err(io)?;
    let request = crate::launch::LaunchRequest {
        runtime_dir: &ctx.runtime_dir,
        omt_bin: &ctx.omt_bin,
        home_id: &home_id,
        home_path: &home_path,
        target,
        origin,
        harness,
        // CLI / MCP 派发总是无人值守：附加 descriptor 的无人值守参数（pi `--auto`、
        // omp `--approval-mode=yolo`），等效桌面端派发但没有派发弹窗那一步人工确认。
        // approvalMode=inherit 只表示派发方不能改它（harness 配置里没有该开关）。
        unattended: true,
        model: opt_str(args, "model"),
        resume_session: None,
        target_agent_id: opt_str(args, "agentId"),
        parent_agent_id: opt_str(args, "parentAgentId"),
        max_duration: crate::policy::effective_max_duration(&policy, requested),
    };
    let resume_session = match request.target_agent_id {
        Some(agent_id) => {
            crate::agents::get_agent(&ctx.runtime_dir, &home_id, agent_id)
                .map_err(io)?
                .ok_or_else(|| not_found(format!("agent {agent_id} 不存在")))?
                .last_session_id
        }
        None => None,
    };
    let request = crate::launch::LaunchRequest {
        resume_session: resume_session.as_deref(),
        ..request
    };
    let outcome = crate::launch::start(&|m, p| rpc(m, p), &request).map_err(|err| {
        let (code, message, field) = crate::launch::problem_from_launch_error(&err);
        Problem::with_details(code, message, |d| {
            if let Some(field) = field {
                d.insert("field".into(), field.into());
            }
        })
    })?;
    serde_json::to_value(outcome).map_err(|e| io(e.to_string()))
}

/// Q-4：停止不受 allowFrom* 与 policy 文件有效性约束——拒绝启动是安全方向，拒绝
/// 停止反而危险。被派发的 agent 不能停止，由 LocalImpl 的嵌套标记检查拦截（Q-12）。
pub fn exec_stop(ctx: &Ctx, exec_id: &str) -> Result<Value, Problem> {
    crate::layout::validate_segment("exec_id", exec_id).map_err(|e| invalid("execId", e))?;
    if crate::control::find_exec(&ctx.runtime_dir, exec_id)
        .map_err(io)?
        .is_none()
    {
        return Err(not_found(format!("exec {exec_id} 不存在")));
    }
    crate::control::stop_exec(&ctx.runtime_dir, exec_id).map_err(io)?;
    Ok(json!({ "execId": exec_id, "stopped": true }))
}

pub fn exec_status(ctx: &Ctx, exec_id: &str) -> Result<Value, Problem> {
    crate::layout::validate_segment("exec_id", exec_id).map_err(|e| invalid("execId", e))?;
    let loc = crate::control::find_exec(&ctx.runtime_dir, exec_id)
        .map_err(io)?
        .ok_or_else(|| not_found(format!("exec {exec_id} 不存在")))?;
    let mut exec = serde_json::to_value(&loc.exec).map_err(|e| io(e.to_string()))?;
    exec["binaryArgs"] = json!(omt_harness::sanitize::sanitize_args(&loc.exec.binary_args));
    let active =
        crate::control::exec_active(&ctx.runtime_dir, &loc.home_id, &loc.agent.id, exec_id);
    Ok(json!({
        "homeId": loc.home_id,
        "agent": {
            "id": loc.agent.id,
            "displayName": loc.agent.display_name,
            "harness": loc.agent.harness,
            "status": loc.agent.status,
            "attachment": loc.agent.attachment,
            "parentAgentId": loc.agent.parent_agent_id,
        },
        "exec": exec,
        "active": active,
    }))
}

pub fn exec_list(ctx: &Ctx, home_id: Option<&str>) -> Result<Value, Problem> {
    let homes: Vec<String> = match home_id {
        Some(id) => vec![id.to_string()],
        None => crate::layout::home_ids(&ctx.runtime_dir),
    };
    let mut rows = Vec::new();
    for home in homes {
        let (agents, _) = crate::agents::list_agents(&ctx.runtime_dir, &home).map_err(io)?;
        for agent in agents {
            for exec in &agent.execs {
                rows.push(json!({
                    "homeId": home,
                    "agentId": agent.id,
                    "displayName": agent.display_name,
                    "harness": agent.harness,
                    "status": agent.status,
                    "execId": exec.exec_id,
                    "nodeId": exec.node_id,
                    "runId": exec.run_id,
                    "launchedBy": exec.launched_by,
                    "startedAt": exec.started_at,
                    "active": crate::control::exec_active(&ctx.runtime_dir, &home, &agent.id, &exec.exec_id),
                }));
            }
        }
    }
    rows.sort_by(|a, b| b["startedAt"].as_str().cmp(&a["startedAt"].as_str()));
    Ok(json!({ "execs": rows }))
}

pub fn exec_tail(
    ctx: &Ctx,
    exec_id: &str,
    since: Option<u64>,
    limit: Option<usize>,
) -> Result<Value, Problem> {
    crate::layout::validate_segment("exec_id", exec_id).map_err(|e| invalid("execId", e))?;
    let loc = crate::control::find_exec(&ctx.runtime_dir, exec_id)
        .map_err(io)?
        .ok_or_else(|| not_found(format!("exec {exec_id} 不存在")))?;
    crate::executor::transcript_page(
        &ctx.runtime_dir,
        &loc.home_id,
        exec_id,
        since,
        limit.unwrap_or(crate::executor::TAIL_PAGE_DEFAULT),
    )
    .map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{register_agent, NewExec};

    fn ctx(rt: &Path) -> Ctx {
        Ctx {
            runtime_dir: rt.to_path_buf(),
            omt_bin: rt.join("omt"),
            daemon_bin: None,
        }
    }

    fn seed_exec(rt: &Path, exec_id: &str, lines: &[&str]) -> String {
        let agent = register_agent(
            rt,
            "h_t",
            "pi",
            NewExec {
                exec_id: exec_id.into(),
                node_id: Some("T-1".into()),
                run_id: Some("RUN-1".into()),
                binary_path: None,
                binary_args: vec!["--api-key".into(), "sk-secret".into()],
                model: None,
                launched_by: Some(LaunchOrigin::Cli),
                parent_agent_id: None,
            },
        )
        .unwrap();
        let dir = crate::layout::agent_dir(rt, "h_t", &agent.id).unwrap();
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(
            crate::layout::exec_file(&dir, exec_id, crate::layout::ExecFile::Jsonl),
            body,
        )
        .unwrap();
        agent.id
    }

    #[test]
    fn exec_list_and_status_project_records() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = seed_exec(tmp.path(), "exec-a", &[]);
        let list = exec_list(&ctx(tmp.path()), None).unwrap();
        assert_eq!(list["execs"][0]["execId"], "exec-a");
        assert_eq!(list["execs"][0]["launchedBy"], "cli");
        assert_eq!(list["execs"][0]["agentId"], agent_id.as_str());
        let status = exec_status(&ctx(tmp.path()), "exec-a").unwrap();
        assert_eq!(status["homeId"], "h_t");
        assert_eq!(status["agent"]["harness"], "pi");
        let args = status["exec"]["binaryArgs"].to_string();
        assert!(!args.contains("sk-secret"), "argv 输出前必须脱敏：{args}");
    }

    #[test]
    fn exec_status_unknown_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            exec_status(&ctx(tmp.path()), "exec-nope").unwrap_err().code,
            "NOT_FOUND"
        );
    }

    #[test]
    fn exec_tail_pages_by_seq_and_reports_done() {
        let tmp = tempfile::tempdir().unwrap();
        seed_exec(
            tmp.path(),
            "exec-t",
            &[
                r#"{"kind":"session_header","seq":0}"#,
                r#"{"kind":"session_start","execId":"exec-t","seq":1}"#,
                r#"{"kind":"raw","execId":"exec-t","seq":2,"text":"a"}"#,
                r#"{"kind":"raw","execId":"exec-t","seq":3,"text":"b"}"#,
                r#"{"kind":"exit","execId":"exec-t","seq":4,"status":"exited"}"#,
            ],
        );
        let first = exec_tail(&ctx(tmp.path()), "exec-t", None, Some(2)).unwrap();
        let seqs: Vec<u64> = first["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(first["nextSeq"], 2);
        assert_eq!(first["done"], false);
        let rest = exec_tail(&ctx(tmp.path()), "exec-t", Some(2), Some(10)).unwrap();
        let seqs: Vec<u64> = rest["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![3, 4]);
        assert_eq!(rest["done"], true);
    }

    #[test]
    fn exec_start_is_policy_gated_before_daemon_contact() {
        let tmp = tempfile::tempdir().unwrap();
        let err = exec_start(
            &ctx(tmp.path()),
            LaunchOrigin::Mcp,
            &serde_json::json!({
                "homeId": "h_t", "harness": "pi", "nodeId": "T-1"
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, "POLICY_DENIED");
        assert_eq!(err.details.as_ref().unwrap()["field"], "allowFromMcp");
    }

    /// Q-4：停止不受 allowFrom* 与 policy 文件有效性约束（止损方向不 fail closed）。
    #[test]
    fn exec_stop_ignores_policy_even_when_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::layout::exec_dir(tmp.path())).unwrap();
        std::fs::write(crate::layout::policy_file(tmp.path()), "{broken").unwrap();
        // 不存在的 exec → NOT_FOUND，而不是 POLICY_INVALID / POLICY_DENIED
        assert_eq!(
            exec_stop(&ctx(tmp.path()), "exec-x").unwrap_err().code,
            "NOT_FOUND"
        );
    }

    /// 本地工具路径不泄露 harness env 的值（secrets 纪律）。
    #[test]
    fn harness_list_redacts_env_values_and_extra_args() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("API_KEY".to_string(), "sk-secret-123".to_string());
        omt_harness::harness::upsert_config(
            tmp.path(),
            "pi",
            omt_harness::harness::HarnessUserConfig {
                enabled: true,
                extra_args: vec!["--api-key".into(), "sk-secret-456".into()],
                env,
                ..Default::default()
            },
        )
        .unwrap();
        let out = harness_list(&ctx(tmp.path())).unwrap().to_string();
        assert!(!out.contains("sk-secret"), "{out}");
        assert!(out.contains("API_KEY"), "键名保留：{out}");
        assert!(!out.contains("\"env\""), "env 字段整体换成 envKeys：{out}");
    }

    /// Q-13：全局 home、或 workspace 根等于用户主目录 → 拒绝；workspace home 放行。
    #[test]
    fn workspace_scope_rejects_global_and_user_home_roots() {
        let user_home = Path::new("/Users/someone");
        assert_eq!(
            workspace_scope_denial("global", Path::new("/Users/someone/.omt"), Some(user_home))
                .unwrap()
                .field,
            "workspaces"
        );
        assert!(
            workspace_scope_denial(
                "workspace",
                Path::new("/Users/someone/.omt"),
                Some(user_home)
            )
            .is_some(),
            "kind 报 workspace 但根目录就是 $HOME 也拒绝"
        );
        assert!(workspace_scope_denial(
            "workspace",
            Path::new("/Users/someone/dev/proj/.omt"),
            Some(user_home)
        )
        .is_none());
        // 没有 .omt 尾段的 home（测试与 --home 直开的形态）：根就是它自己
        assert!(
            workspace_scope_denial("workspace", Path::new("/tmp/ws-home"), Some(user_home))
                .is_none()
        );
    }

    #[test]
    fn exec_start_validates_target_shape() {
        let tmp = tempfile::tempdir().unwrap();
        for args in [
            serde_json::json!({"homeId": "h_t", "harness": "pi"}),
            serde_json::json!({"homeId": "h_t", "harness": "pi", "nodeId": "a", "runId": "b"}),
            serde_json::json!({"homeId": "h_t", "nodeId": "a"}),
        ] {
            assert_eq!(
                exec_start(&ctx(tmp.path()), LaunchOrigin::Cli, &args)
                    .unwrap_err()
                    .code,
                "INVALID_INPUT",
                "{args}"
            );
        }
    }

    #[test]
    fn policy_show_reports_effective_policy_and_source() {
        let tmp = tempfile::tempdir().unwrap();
        let shown = policy_show(&ctx(tmp.path())).unwrap();
        assert_eq!(shown["policy"]["allowFromMcp"], false);
        assert_eq!(shown["source"], "default");
    }
}
