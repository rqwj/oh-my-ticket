//! U12 (KD-D4)：结构化 transcript adapter——把 harness 的 JSON 事件流
//! 解析成 §5.3 统一事件（assistant_text / assistant_thinking / tool_call /
//! tool_result / usage / error），手写行解析、无外部依赖。
//!
//! 契约是「降级保证可用」而非「追平每个版本」：解析失败/未知形状一律
//! 回落 raw，绝不中断流。三个 provider 两种格式（2026-10-18 本机实测采样，
//! 样本存于本文件测试 fixture 与 findings log F-U12-1）：
//!
//! - **opencode**（`run --format json`）：每行 `{type, timestamp, sessionID,
//!   part}`——`text`（finalized 文本块）/ `reasoning`（需 --thinking）/
//!   `tool_use`（part 为完整 tool part：state.status=completed|error 时携带
//!   input+output+title）/ `step_finish`（tokens/cost）/ `error`。
//!   无终止事件——进程退出即结束。
//! - **pi / omp**（`-p --mode json`，两家属同一 schema——omp 是 pi fork）：
//!   `session` / `agent_start` / `turn_start` / `message_start|end` /
//!   `message_update{assistantMessageEvent{...}, usage?}` /
//!   `tool_execution_start|update|end` / `turn_end` / `agent_end` /
//!   `agent_settled`。增量（*_delta）忽略——取 *_end 的聚合内容；
//!   系统消息的 message_start/end 含巨型 sections，必须忽略而非透传。
//!
//! 返回的每个值是**不含** execId/seq/ts 的部分 envelope（executor 侧
//! spawn_reader 统一补齐），字段扁平（与既有 raw/session_start/exit 事件
//! 的既有形状一致，§5.3 的 payload{} 折叠为平铺字段）。

/// 一行输出 → 0..n 个统一事件（0 = 已知噪声，静默忽略）。
/// 非 JSON 行回落单个 raw 事件（stream 由调用方补充前的占位 stdout——
/// spawn_reader 对回落行保留其真实 stream 标注）。
pub fn parse_line(provider: &str, line: &str) -> Vec<serde_json::Value> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return vec![raw_event(trimmed)];
    }
    let Ok(ev) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return vec![raw_event(trimmed)];
    };
    match provider {
        "opencode" => parse_opencode(&ev),
        "pi" | "omp" => parse_pi_family(&ev),
        // 未知 provider 永远走 raw 路径（descriptor 不配 json_args 即不到这）
        _ => vec![raw_event(trimmed)],
    }
}

/// U11：从一行事件流提取 harness session id（首跑捕获 → exec 记录）。
/// opencode：每个事件顶层 `sessionID`；pi/omp：`session` 事件的 `id`。
/// 返回 None = 该行无 id 信息。
pub fn extract_session_id(provider: &str, line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let ev = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    match provider {
        "opencode" => ev["sessionID"].as_str().map(str::to_string),
        "pi" | "omp" => {
            if ev["type"] == "session" {
                ev["id"].as_str().map(str::to_string)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn raw_event(text: &str) -> serde_json::Value {
    serde_json::json!({ "kind": "raw", "stream": "stdout", "text": text })
}

/// 无 provider 上下文时的自动嗅探（历史回载用）：顶层带 `part` → opencode
/// （其事件形状 part.type 携带真实类型）；带 pi 族专有 type → pi/omp；
/// 其余 JSON 落 raw。嗅探只认专有键，跨家同名键不误判。
pub fn parse_line_auto(line: &str) -> Vec<serde_json::Value> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return vec![raw_event(trimmed)];
    }
    let Ok(ev) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return vec![raw_event(trimmed)];
    };
    if ev.get("part").is_some() {
        return parse_opencode(&ev);
    }
    let t = ev["type"].as_str().unwrap_or_default();
    if matches!(
        t,
        "session"
            | "agent_start"
            | "agent_end"
            | "agent_settled"
            | "turn_start"
            | "turn_end"
            | "message_start"
            | "message_update"
            | "message_end"
            | "tool_execution_start"
            | "tool_execution_update"
            | "tool_execution_end"
    ) {
        return parse_pi_family(&ev);
    }
    vec![raw_event(trimmed)]
}

// ---------------------------------------------------------------------------
// opencode
// ---------------------------------------------------------------------------

fn parse_opencode(ev: &serde_json::Value) -> Vec<serde_json::Value> {
    let part = &ev["part"];
    let part_type = part["type"].as_str().unwrap_or_default();
    match part_type {
        "text" => {
            let text = part["text"].as_str().unwrap_or_default();
            if text.is_empty() {
                return Vec::new();
            }
            vec![serde_json::json!({ "kind": "assistant_text", "text": text })]
        }
        "reasoning" => {
            let text = part["text"].as_str().unwrap_or_default();
            if text.is_empty() {
                return Vec::new();
            }
            vec![serde_json::json!({ "kind": "assistant_thinking", "text": text })]
        }
        // 完整 tool part：state.status=completed|error 时才发（设计核实）——
        // 一次拆成 tool_call + tool_result 两个统一事件
        "tool" => {
            let state = &part["state"];
            let status = state["status"].as_str().unwrap_or_default();
            if !matches!(status, "completed" | "error") {
                return Vec::new();
            }
            let name = part["tool"].as_str().unwrap_or("tool");
            let input = state["input"].clone();
            let summary = state["title"]
                .as_str()
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    state["output"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect()
                });
            vec![
                serde_json::json!({ "kind": "tool_call", "name": name, "input": input }),
                serde_json::json!({
                    "kind": "tool_result",
                    "summary": summary,
                    "isError": status == "error",
                }),
            ]
        }
        "step-finish" => {
            let tokens = part["tokens"]["total"].as_u64();
            let cost = part["cost"].as_f64();
            if tokens.is_none() && cost.is_none() {
                return Vec::new();
            }
            vec![serde_json::json!({
                // F-BENCH-19：opencode 的 step_finish 也是**逐步**计（语料
                // 分解实证：每步 total = 该步 input+output+reasoning+cache，
                // 4 步相加 98225 而非末值 25460；免费档 cost 恒 0 曾掩盖）
                "kind": "usage", "scope": "message",
                "tokens": tokens, "cost": cost,
            })]
        }
        // step-start 与未知 part 类型：忽略
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// pi / omp（同 schema）
// ---------------------------------------------------------------------------

fn parse_pi_family(ev: &serde_json::Value) -> Vec<serde_json::Value> {
    let ev_type = ev["type"].as_str().unwrap_or_default();
    match ev_type {
        // assistantMessageEvent 增量流：只取聚合终点
        "message_update" => {
            // F-BENCH-14：本类型的 usage 是**消息内流式快照**（F-U12-3 刷屏
            // 的来源）——定值在 message_end（pi 1.3.0 / omp 18.12.2 实测均如此），
            // 此处不再提取，避免与 message_end 双计。
            let mut out = Vec::new();
            let ame = &ev["assistantMessageEvent"];
            match ame["type"].as_str().unwrap_or_default() {
                "thinking_end" => {
                    let text = ame["content"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        out.push(serde_json::json!({ "kind": "assistant_thinking", "text": text }));
                    }
                }
                "text_end" => {
                    let text = ame["content"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        out.push(serde_json::json!({ "kind": "assistant_text", "text": text }));
                    }
                }
                "toolcall_end" => {
                    let call = &ame["toolCall"];
                    out.push(serde_json::json!({
                        "kind": "tool_call",
                        "name": call["name"].as_str().unwrap_or("tool"),
                        "input": call["arguments"].clone(),
                    }));
                }
                _ => {}
            }
            out
        }
        "tool_execution_end" => {
            let result = &ev["result"];
            // content 数组的 text 块拼接；结构化兜底（bash 的 structuredContent）
            let mut summary = result["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if summary.is_empty() {
                if let Some(output) = result["structuredContent"]["output"].as_str() {
                    summary = output.to_string();
                }
            }
            if summary.is_empty() {
                summary = "(no output)".to_string();
            }
            let summary: String = summary.chars().take(400).collect();
            vec![serde_json::json!({
                "kind": "tool_result",
                "summary": summary,
                "isError": ev["isError"].as_bool().unwrap_or(false),
            })]
        }
        // F-BENCH-14：usage 是**每条 assistant 消息各自的值**（非会话累计，
        // 语料实证：cost 序列 0.0349/0.0095/0.0078/0.0092 不递增）。定值挂
        // message_end 的 message.usage；turn_end / agent_end 携带同值（重复，
        // 不提取以免双计）。scope=message 告知 reader/analyzer 按消息求和。
        "message_end" => {
            let Some(usage) = ev
                .pointer("/message/usage")
                .filter(|u| u.is_object() && u.get("totalTokens").is_some())
            else {
                return Vec::new();
            };
            match (
                usage["totalTokens"].as_u64(),
                usage["cost"]["total"].as_f64(),
            ) {
                (Some(tokens), cost) => vec![serde_json::json!({
                    "kind": "usage", "scope": "message", "tokens": tokens, "cost": cost,
                })],
                (None, _) => Vec::new(),
            }
        }
        "turn_end" | "agent_end" => Vec::new(),
        "error" => {
            let message = ev["error"]
                .as_str()
                .or_else(|| ev["message"].as_str())
                .unwrap_or("unknown error");
            vec![serde_json::json!({ "kind": "error", "message": message })]
        }
        // 已知噪声（session/agent_*/turn_*/message_start|end 的整段系统
        // 消息）与未知类型：忽略——绝不把 system prompt 透传进 transcript
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// opencode 真实样本（2026-10-18 本机 `--format json` 采样，裁剪至
    /// 关键字段——完整样本见 findings log F-U12-1）。
    #[test]
    fn opencode_fixture_stream_maps_to_unified_events() {
        let text_line = r#"{"type":"text","timestamp":1792326871320,"sessionID":"ses_x","part":{"type":"text","text":"OC-OK"}}"#;
        assert_eq!(
            parse_line("opencode", text_line),
            vec![serde_json::json!({ "kind": "assistant_text", "text": "OC-OK" })]
        );

        let tool_line = r#"{"type":"tool_use","part":{"type":"tool","tool":"bash","callID":"c1","state":{"status":"completed","input":{"command":"echo TOOLPROBE"},"output":"TOOLPROBE\n","metadata":{"exit":0},"title":"echo TOOLPROBE"}}}"#;
        assert_eq!(
            parse_line("opencode", tool_line),
            vec![
                serde_json::json!({ "kind": "tool_call", "name": "bash", "input": {"command": "echo TOOLPROBE"} }),
                serde_json::json!({ "kind": "tool_result", "summary": "echo TOOLPROBE", "isError": false }),
            ]
        );

        let step = r#"{"type":"step_finish","part":{"type":"step-finish","tokens":{"total":17137,"input":17101,"output":5},"cost":0}}"#;
        assert_eq!(
            parse_line("opencode", step),
            vec![
                serde_json::json!({ "kind": "usage", "scope": "message", "tokens": 17137u64, "cost": 0.0 })
            ]
        );

        // step-start 忽略；非 JSON 回落 raw
        assert!(parse_line(
            "opencode",
            r#"{"type":"step_start","part":{"type":"step-start"}}"#
        )
        .is_empty());
        assert_eq!(
            parse_line("opencode", "plain text line"),
            vec![
                serde_json::json!({ "kind": "raw", "stream": "stdout", "text": "plain text line" })
            ]
        );
        // 畸形 JSON（{ 开头但解析失败）→ raw 不中断
        assert_eq!(
            parse_line("opencode", "{broken json"),
            vec![serde_json::json!({ "kind": "raw", "stream": "stdout", "text": "{broken json" })]
        );
    }

    /// pi/omp 真实样本（同 schema；裁剪系统消息载荷）。
    #[test]
    fn pi_family_fixture_stream_maps_to_unified_events() {
        let thinking_end = r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_end","contentIndex":0,"content":"Let me run the probe."}}"#;
        assert_eq!(
            parse_line("pi", thinking_end),
            vec![
                serde_json::json!({ "kind": "assistant_thinking", "text": "Let me run the probe." })
            ]
        );

        let toolcall_end = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_end","contentIndex":1,"toolCall":{"type":"toolCall","id":"tool_1","name":"bash","arguments":{"command":"echo TOOLPROBE"}}}}"#;
        assert_eq!(
            parse_line("omp", toolcall_end),
            vec![
                serde_json::json!({ "kind": "tool_call", "name": "bash", "input": {"command": "echo TOOLPROBE"} })
            ]
        );

        let exec_end = r#"{"type":"tool_execution_end","toolCallId":"tool_1","toolName":"bash","result":{"content":[{"type":"text","text":"TOOLPROBE\n"}],"structuredContent":{"output":"TOOLPROBE\n","exit_code":0}},"isError":false}"#;
        assert_eq!(
            parse_line("pi", exec_end),
            vec![
                serde_json::json!({ "kind": "tool_result", "summary": "TOOLPROBE\n", "isError": false })
            ]
        );

        // F-BENCH-14：message_update 的 usage（消息内快照）不再提取；
        // 定值在 message_end（scope=message）
        let text_end = r#"{"type":"message_update","usage":{"input":6979,"totalTokens":6979,"cost":{"total":0.020937}},"assistantMessageEvent":{"type":"text_end","contentIndex":0,"content":"DONE"}}"#;
        assert_eq!(
            parse_line("pi", text_end),
            vec![serde_json::json!({ "kind": "assistant_text", "text": "DONE" })]
        );

        // 系统消息巨型载荷必须被忽略（不透传进 transcript）
        assert!(parse_line(
            "pi",
            r#"{"type":"message_start","message":{"role":"system","content":"","sections":{"preamble":"You are...","tools":"<tools>..."}}}"#
        )
        .is_empty());
        // delta/agent_*/turn_*/session 忽略
        for noise in [
            r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"The"}}"#,
            r#"{"type":"agent_start"}"#,
            r#"{"type":"turn_end","message":{"role":"assistant"}}"#,
            r#"{"type":"session","version":3,"id":"x"}"#,
        ] {
            assert!(parse_line("pi", noise).is_empty(), "应忽略: {noise}");
        }
        // 未知 provider → raw
        assert_eq!(
            parse_line("dsh", r#"{"type":"anything"}"#),
            vec![
                serde_json::json!({ "kind": "raw", "stream": "stdout", "text": r#"{"type":"anything"}"# })
            ]
        );
    }

    /// U11：session id 提取（opencode 任意事件 / pi·omp session 事件）
    #[test]
    fn extract_session_id_per_provider() {
        assert_eq!(
            extract_session_id(
                "opencode",
                r#"{"type":"text","sessionID":"ses_abc","part":{"type":"text","text":"hi"}}"#
            )
            .as_deref(),
            Some("ses_abc")
        );
        assert_eq!(
            extract_session_id(
                "pi",
                r#"{"type":"session","version":3,"id":"01a14f01-d40b"}"#
            )
            .as_deref(),
            Some("01a14f01-d40b")
        );
        // pi 非 session 事件无 id；未知 provider / 非 JSON → None
        assert_eq!(extract_session_id("pi", r#"{"type":"turn_start"}"#), None);
        assert_eq!(extract_session_id("dsh", r#"{"sessionID":"x"}"#), None);
        assert_eq!(extract_session_id("opencode", "plain"), None);
    }

    /// 历史回载嗅探：opencode（part 键）/ pi 族（专有 type）/ 其它 raw
    #[test]
    fn parse_line_auto_sniffs_provider() {
        assert_eq!(
            parse_line_auto(r#"{"type":"text","part":{"type":"text","text":"OC-OK"}}"#),
            vec![serde_json::json!({ "kind": "assistant_text", "text": "OC-OK" })]
        );
        assert_eq!(
            parse_line_auto(
                r#"{"type":"message_update","assistantMessageEvent":{"type":"text_end","content":"DONE"}}"#
            ),
            vec![serde_json::json!({ "kind": "assistant_text", "text": "DONE" })]
        );
        assert_eq!(
            parse_line_auto(r#"{"type":"unrelated"}"#),
            vec![
                serde_json::json!({ "kind": "raw", "stream": "stdout", "text": r#"{"type":"unrelated"}"# })
            ]
        );
        assert_eq!(
            parse_line_auto("plain"),
            vec![serde_json::json!({ "kind": "raw", "stream": "stdout", "text": "plain" })]
        );
    }

    /// F-BENCH-14/17/19：usage 语义——三家全是**每步/每条消息各自计**
    /// （scope=message，exec 总量求和）；turn_end/agent_end 同值重复与
    /// message_update 流式快照均不提取。
    #[test]
    fn usage_scopes_message_per_step() {
        let msg_end = r#"{"type":"message_end","message":{"role":"assistant","usage":{"totalTokens":13134,"cost":{"total":0.009486}}}}"#;
        assert_eq!(
            parse_line("pi", msg_end),
            vec![serde_json::json!({
                "kind": "usage", "scope": "message", "tokens": 13134u64, "cost": 0.009486
            })]
        );
        assert_eq!(parse_line("omp", msg_end).len(), 1, "omp 同 schema");
        // turn_end / agent_end：同值重复——零产出
        let turn_end = r#"{"type":"turn_end","message":{"role":"assistant","usage":{"totalTokens":13134,"cost":{"total":0.009486}}}}"#;
        assert!(parse_line("pi", turn_end).is_empty());
        let agent_end = r#"{"type":"agent_end","messages":[]}"#;
        assert!(parse_line("omp", agent_end).is_empty());
        // 非 assistant / 无 usage 的 message_end：零产出
        let plain = r#"{"type":"message_end","message":{"role":"system"}}"#;
        assert!(parse_line("pi", plain).is_empty());
        // opencode step_finish：逐步计（F-BENCH-19——分解实证后同 pi 求和）
        let step = r#"{"type":"step_finish","part":{"type":"step-finish","tokens":{"total":17137},"cost":0}}"#;
        assert_eq!(
            parse_line("opencode", step),
            vec![serde_json::json!({
                "kind": "usage", "scope": "message", "tokens": 17137u64, "cost": 0.0
            })]
        );
    }

    /// 错误事件（pi 族顶层 error）
    #[test]
    fn pi_family_error_event_maps() {
        let line = r#"{"type":"error","error":"boom"}"#;
        assert_eq!(
            parse_line("omp", line),
            vec![serde_json::json!({ "kind": "error", "message": "boom" })]
        );
    }
}
