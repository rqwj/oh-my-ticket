//! 子项目 2（设计 §3.5 / KD-E4）本地工具族的 lib 侧契约：工具表、MCP 输入
//! schema、CLI 参数解析与 `LocalTools` trait。**本模块被 omt-daemon 链接**——
//! 不得引用 omt_exec / omt_harness（KD-E9）；实现在 `local_impl.rs`（仅 omt
//! 二进制），经 `cli::real_run_code_with` 注入。

use omt_storage::Problem;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCaller {
    Cli,
    Mcp,
}

pub trait LocalTools: Send + Sync {
    fn call(&self, caller: LocalCaller, tool: &str, args: Value) -> Result<Value, Problem>;
}

pub struct LocalToolSpec {
    pub action: &'static str,
    /// 有副作用（parity 分类 local_exec；policy 在调用时检查，Q-5）
    pub mutating: bool,
    pub description: &'static str,
    pub input_schema: fn() -> Value,
}

fn schema_harness_list() -> Value {
    json!({"type": "object", "properties": {
        "verbose": {"type": "boolean", "description": "保留字段；当前总是返回完整投影"}
    }})
}
fn schema_harness_models() -> Value {
    json!({"type": "object", "required": ["harness"], "properties": {
        "harness": {"type": "string", "description": "harness id，如 pi / omp / opencode"}
    }})
}
fn schema_exec_start() -> Value {
    json!({"type": "object", "required": ["homeId", "harness"], "properties": {
        "homeId": {"type": "string"},
        "harness": {"type": "string", "description": "须在 exec/policy.json 的 harnesses 白名单内"},
        "nodeId": {"type": "string", "description": "与 runId 二选一：派发该节点（及其可执行子成员）"},
        "runId": {"type": "string", "description": "与 nodeId 二选一：派发到已有 run"},
        "model": {"type": "string", "description": "harness__models 返回的模型 id"},
        "agentId": {"type": "string", "description": "续跑：在该 agent 下追加 exec（沿用其会话）"},
        "parentAgentId": {"type": "string", "description": "主控自己的 agentId（关联父子）"},
        "maxDurationSec": {"type": "integer", "minimum": 60, "description": "不超过 policy.maxDurationSec"}
    }})
}
fn schema_exec_id() -> Value {
    json!({"type": "object", "required": ["execId"], "properties": {
        "execId": {"type": "string"}
    }})
}
fn schema_exec_list() -> Value {
    json!({"type": "object", "properties": {
        "homeId": {"type": "string", "description": "缺省列出全部 home"}
    }})
}
fn schema_exec_tail() -> Value {
    json!({"type": "object", "required": ["execId"], "properties": {
        "execId": {"type": "string"},
        "since": {"type": "integer", "minimum": 0, "description": "只返回 seq 大于它的事件（上一页的 nextSeq）"},
        "limit": {"type": "integer", "minimum": 1, "maximum": 1000}
    }})
}
fn schema_exec_policy() -> Value {
    json!({"type": "object", "properties": {
        "verbose": {"type": "boolean", "description": "保留字段"}
    }})
}

pub const LOCAL_TOOLS: &[LocalToolSpec] = &[
    LocalToolSpec {
        action: "harness/list",
        mutating: false,
        description: "列出 harness：是否安装、版本、是否启用、无人值守标志",
        input_schema: schema_harness_list,
    },
    LocalToolSpec {
        action: "harness/models",
        mutating: false,
        description: "列出某个 harness 可用的模型",
        input_schema: schema_harness_models,
    },
    LocalToolSpec {
        action: "exec/start",
        mutating: true,
        description: "拉起一个 harness 执行 ticket/run，立即返回 {agentId, execId, runId}",
        input_schema: schema_exec_start,
    },
    LocalToolSpec {
        action: "exec/stop",
        mutating: true,
        description: "停止一个执行（SIGTERM → 宽限后 KILL）",
        input_schema: schema_exec_id,
    },
    LocalToolSpec {
        action: "exec/status",
        mutating: false,
        description: "查询一个执行的状态",
        input_schema: schema_exec_id,
    },
    LocalToolSpec {
        action: "exec/list",
        mutating: false,
        description: "列出执行（按开始时间倒序）",
        input_schema: schema_exec_list,
    },
    LocalToolSpec {
        action: "exec/tail",
        mutating: false,
        description: "按 seq 分页读取执行的归一化事件",
        input_schema: schema_exec_tail,
    },
    LocalToolSpec {
        action: "exec/policy",
        mutating: false,
        description: "查看当前生效的派发策略（exec/policy.json）",
        input_schema: schema_exec_policy,
    },
];

pub fn local_tool(action: &str) -> Option<&'static LocalToolSpec> {
    LOCAL_TOOLS.iter().find(|t| t.action == action)
}

fn take_flag(rest: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(pos) = rest.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if pos + 1 >= rest.len() {
        return Err(format!("{flag} 需要一个值"));
    }
    let value = rest.remove(pos + 1);
    rest.remove(pos);
    Ok(Some(value))
}

fn take_switch(rest: &mut Vec<String>, flag: &str) -> bool {
    match rest.iter().position(|a| a == flag) {
        Some(pos) => {
            rest.remove(pos);
            true
        }
        None => false,
    }
}

fn int_flag(rest: &mut Vec<String>, flag: &str) -> Result<Option<u64>, String> {
    take_flag(rest, flag)?
        .map(|v| v.parse::<u64>().map_err(|_| format!("{flag} 需要非负整数")))
        .transpose()
}

/// `harness …` / `exec …`（已去掉全局参数）→ (action, args)。
pub fn parse_cli(argv: &[String]) -> Result<(&'static str, Value), String> {
    let mut rest: Vec<String> = argv.to_vec();
    if rest.len() < 2 {
        return Err(
            "用法：omt harness list|models <id>；omt exec start|stop|status|list|tail|policy"
                .into(),
        );
    }
    let group = rest.remove(0);
    let sub = rest.remove(0);
    let positional = |rest: &mut Vec<String>, what: &str| -> Result<String, String> {
        if rest.is_empty() || rest[0].starts_with("--") {
            return Err(format!("缺少 {what}"));
        }
        Ok(rest.remove(0))
    };
    let parsed = match (group.as_str(), sub.as_str()) {
        ("harness", "list") => ("harness/list", json!({})),
        ("harness", "models") => (
            "harness/models",
            json!({"harness": positional(&mut rest, "<harness>")?}),
        ),
        ("exec", "start") => {
            let mut args = serde_json::Map::new();
            let harness = take_flag(&mut rest, "--harness")?.ok_or("缺少 --harness")?;
            args.insert("harness".into(), json!(harness));
            let node = take_flag(&mut rest, "--node")?;
            let run = take_flag(&mut rest, "--run")?;
            match (node, run) {
                (Some(n), None) => {
                    args.insert("nodeId".into(), json!(n));
                }
                (None, Some(r)) => {
                    args.insert("runId".into(), json!(r));
                }
                _ => return Err("--node 与 --run 必须且只能给一个".into()),
            }
            for (flag, key) in [
                ("--model", "model"),
                ("--agent", "agentId"),
                ("--parent", "parentAgentId"),
            ] {
                if let Some(v) = take_flag(&mut rest, flag)? {
                    args.insert(key.into(), json!(v));
                }
            }
            if let Some(v) = int_flag(&mut rest, "--max-duration")? {
                args.insert("maxDurationSec".into(), json!(v));
            }
            ("exec/start", Value::Object(args))
        }
        ("exec", "stop") => (
            "exec/stop",
            json!({"execId": positional(&mut rest, "<execId>")?}),
        ),
        ("exec", "status") => (
            "exec/status",
            json!({"execId": positional(&mut rest, "<execId>")?}),
        ),
        ("exec", "list") => {
            if take_switch(&mut rest, "--all-homes") {
                ("exec/list", json!({"allHomes": true}))
            } else {
                ("exec/list", json!({}))
            }
        }
        ("exec", "tail") => {
            let exec_id = positional(&mut rest, "<execId>")?;
            let mut args = serde_json::Map::new();
            args.insert("execId".into(), json!(exec_id));
            if let Some(v) = int_flag(&mut rest, "--since")? {
                args.insert("since".into(), json!(v));
            }
            if let Some(v) = int_flag(&mut rest, "--limit")? {
                args.insert("limit".into(), json!(v));
            }
            ("exec/tail", Value::Object(args))
        }
        ("exec", "policy") => ("exec/policy", json!({})),
        _ => return Err(format!("未知的子命令：{group} {sub}")),
    };
    if let Some(extra) = rest.first() {
        return Err(format!("多余的参数：{extra}"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_tool_table_is_the_designed_family() {
        let names: Vec<&str> = LOCAL_TOOLS.iter().map(|t| t.action).collect();
        assert_eq!(
            names,
            vec![
                "harness/list",
                "harness/models",
                "exec/start",
                "exec/stop",
                "exec/status",
                "exec/list",
                "exec/tail",
                "exec/policy",
            ]
        );
        let mutating: Vec<&str> = LOCAL_TOOLS
            .iter()
            .filter(|t| t.mutating)
            .map(|t| t.action)
            .collect();
        assert_eq!(mutating, vec!["exec/start", "exec/stop"]);
    }

    #[test]
    fn every_local_tool_has_an_object_schema_with_properties() {
        for tool in LOCAL_TOOLS {
            let schema = (tool.input_schema)();
            assert_eq!(schema["type"], "object", "{}", tool.action);
            assert!(
                schema["properties"]
                    .as_object()
                    .is_some_and(|p| !p.is_empty()),
                "{}",
                tool.action
            );
            assert!(!schema.to_string().contains("$ref"), "{}", tool.action);
        }
    }

    #[test]
    fn cli_args_parse_into_tool_calls() {
        let call =
            |argv: &[&str]| parse_cli(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            call(&["harness", "list"]).unwrap(),
            ("harness/list", serde_json::json!({}))
        );
        assert_eq!(
            call(&["harness", "models", "pi"]).unwrap(),
            ("harness/models", serde_json::json!({"harness": "pi"}))
        );
        assert_eq!(
            call(&[
                "exec",
                "start",
                "--harness",
                "pi",
                "--node",
                "TICKET-1",
                "--model",
                "k3"
            ])
            .unwrap(),
            (
                "exec/start",
                serde_json::json!({"harness": "pi", "nodeId": "TICKET-1", "model": "k3"})
            ),
        );
        assert_eq!(
            call(&[
                "exec",
                "start",
                "--harness",
                "pi",
                "--run",
                "RUN-1",
                "--agent",
                "agent-x",
                "--parent",
                "agent-m",
                "--max-duration",
                "600"
            ])
            .unwrap(),
            (
                "exec/start",
                serde_json::json!({"harness": "pi", "runId": "RUN-1", "agentId": "agent-x", "parentAgentId": "agent-m", "maxDurationSec": 600})
            ),
        );
        assert_eq!(
            call(&["exec", "tail", "exec-1", "--since", "5", "--limit", "50"]).unwrap(),
            (
                "exec/tail",
                serde_json::json!({"execId": "exec-1", "since": 5, "limit": 50})
            )
        );
        assert_eq!(
            call(&["exec", "list", "--all-homes"]).unwrap(),
            ("exec/list", serde_json::json!({"allHomes": true}))
        );
        assert_eq!(
            call(&["exec", "policy"]).unwrap(),
            ("exec/policy", serde_json::json!({}))
        );
        assert!(call(&[
            "exec",
            "start",
            "--harness",
            "pi",
            "--node",
            "a",
            "--run",
            "b"
        ])
        .is_err());
        assert!(call(&["exec", "stop"]).is_err());
        assert!(call(&["exec", "bogus"]).is_err());
    }
}
