//! `LocalTools` 的执行面实现——只由 `omt` 二进制（cli_main.rs）编译与构造，
//! omt-daemon 不链接（KD-E9；CI nm 门同时检查 omt_exec 与 omt_harness）。

use omt_exec::local::Ctx;
use omt_exec::policy::LaunchOrigin;
use omt_runtime::local_tools::{LocalCaller, LocalTools};
use omt_storage::Problem;
use serde_json::Value;

pub struct LocalImpl {
    ctx: Ctx,
}

impl LocalImpl {
    pub fn new(runtime_dir: std::path::PathBuf) -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("omt"));
        let daemon_bin = exe
            .parent()
            .map(|dir| dir.join("omt-daemon"))
            .filter(|path| path.is_file());
        Self {
            ctx: Ctx {
                runtime_dir,
                omt_bin: exe,
                daemon_bin,
            },
        }
    }
}

fn origin(caller: LocalCaller) -> LaunchOrigin {
    match caller {
        LocalCaller::Cli => LaunchOrigin::Cli,
        LocalCaller::Mcp => LaunchOrigin::Mcp,
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, Problem> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            let field = key.to_string();
            Problem::with_details("INVALID_INPUT", format!("需要 {key}"), |d| {
                d.insert("field".into(), field.into());
            })
        })
}

/// Q-12：本进程是被派发的 harness（或其子进程）？shim 在 spawn 时注入
/// `OMT_EXEC_ID`。纵深防御：手握 shell 的 agent 仍可清掉它（见威胁模型）。
pub fn nested_exec_id() -> Option<String> {
    std::env::var("OMT_EXEC_ID").ok().filter(|v| !v.is_empty())
}

impl LocalTools for LocalImpl {
    fn call(&self, caller: LocalCaller, tool: &str, args: Value) -> Result<Value, Problem> {
        let ctx = &self.ctx;
        if matches!(tool, "exec/start" | "exec/stop") {
            if let Some(parent) = nested_exec_id() {
                return Err(Problem::with_details(
                    "POLICY_DENIED",
                    format!("被派发的 agent（exec {parent}）不能再派发或停止执行"),
                    |d| {
                        d.insert("field".into(), "nestedDispatch".into());
                    },
                ));
            }
        }
        match tool {
            "harness/list" => omt_exec::local::harness_list(ctx),
            "harness/models" => omt_exec::local::harness_models(ctx, str_arg(&args, "harness")?),
            "exec/start" => omt_exec::local::exec_start(ctx, origin(caller), &args),
            "exec/stop" => omt_exec::local::exec_stop(ctx, str_arg(&args, "execId")?),
            "exec/status" => omt_exec::local::exec_status(ctx, str_arg(&args, "execId")?),
            "exec/list" => {
                // CLI 的 home 可能是路径：映射到 homeId；allHomes / 缺省 = 全部
                let home = args
                    .get("homeId")
                    .or_else(|| args.get("home"))
                    .and_then(Value::as_str);
                let home_id = match home {
                    Some(h)
                        if !args
                            .get("allHomes")
                            .and_then(Value::as_bool)
                            .unwrap_or(false) =>
                    {
                        Some(resolve_home_id(ctx, h)?)
                    }
                    _ => None,
                };
                omt_exec::local::exec_list(ctx, home_id.as_deref())
            }
            "exec/tail" => omt_exec::local::exec_tail(
                ctx,
                str_arg(&args, "execId")?,
                args.get("since").and_then(Value::as_u64),
                args.get("limit")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize),
            ),
            "exec/policy" => omt_exec::local::policy_show(ctx),
            other => Err(Problem::new(
                "NOT_FOUND",
                format!("unknown local tool: {other}"),
            )),
        }
    }
}

/// exec list 只读：homeId 原样；路径则按目录名之外的 agents/<homeId> 布局无法反推，
/// 所以经 daemon 映射（与 exec start 同一来源）。
fn resolve_home_id(ctx: &Ctx, home: &str) -> Result<String, Problem> {
    if !home.contains('/') {
        return Ok(home.to_string());
    }
    let served = omt_exec::daemon_link::served_homes(&ctx.runtime_dir, ctx.daemon_bin.as_deref())
        .map_err(|e| Problem::new("IO", e))?;
    let want = std::fs::canonicalize(home).ok();
    served
        .into_iter()
        .find(|h| std::fs::canonicalize(&h.path).ok() == want)
        .map(|h| h.home_id)
        .ok_or_else(|| {
            Problem::new(
                "NOT_FOUND",
                format!("home {home} 不在 daemon 当前服务的 home 中"),
            )
        })
}
