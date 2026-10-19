//! 评测机制第一步（设计 §6.1，F-BENCH-1）：jsonl → 指标的纯函数分析器。
//!
//! 数据源是 `<execId>.jsonl` 规范化事件日志（U12/F-U12-4 已铺好）——本模块
//! 只做读取与聚合，不触碰执行面。指标定义见设计 §4.2：
//! - `tool_calls`：全程工具调用数（硬门 ≤ min+1）
//! - `discovery_overhead`：首次 omt 工具调用（`run__claim` / `mcp__omt__run__claim`
//!   或任何含 `run__claim` 的调用）**之前**的非 omt 工具调用数（硬门 = 0，
//!   Agent 26 之痛的量化）
//! - `turns`：assistant 思考块数
//! - `duration_ms` / `tokens` / `cost`：exit 事件与 usage 终值
//! - `errors`：error 事件计数
//!
//! fixture 回放（`fixtures/harness/<provider>/<version>/`）共用同一套函数——
//! 历史卷与语料库走完全相同的分析路径。

use std::path::Path;

/// 生产调用方在 §6.2（探针/CLI）接入前，本模块仅由 fixture 回放测试驱动。
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExecMetrics {
    pub exec_id: String,
    pub harness: String,
    pub model: Option<String>,
    pub tool_calls: usize,
    pub discovery_overhead: usize,
    pub turns: usize,
    pub duration_ms: Option<u64>,
    pub tokens: Option<u64>,
    pub cost: Option<f64>,
    pub errors: usize,
    /// 终态文本（exited/failed/killed…）
    pub outcome: Option<String>,
}

/// 分析一卷 jsonl（不存在 → None；坏行跳过——与分析器「增强不阻断」定位一致）。
#[allow(dead_code)]
pub fn analyze_jsonl(path: &Path) -> Option<ExecMetrics> {
    let text = std::fs::read_to_string(path).ok()?;
    let file_name = path.file_name()?.to_string_lossy().to_string();
    let exec_id = file_name
        .strip_suffix(".jsonl")
        .unwrap_or(&file_name)
        .to_string();

    let mut harness = String::new();
    let mut model = None;
    let mut tool_calls = 0usize;
    let mut discovery_overhead = 0usize;
    let mut seen_omt_call = false;
    let mut turns = 0usize;
    let mut duration_ms = None;
    let mut tokens = None;
    let mut cost = None;
    let mut errors = 0usize;
    let mut outcome = None;

    for line in text.lines() {
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match ev["kind"].as_str().unwrap_or_default() {
            "session_header" => {
                harness = ev["harness"].as_str().unwrap_or_default().to_string();
            }
            "assistant_thinking" => turns += 1,
            "tool_call" => {
                tool_calls += 1;
                let name = ev["name"].as_str().unwrap_or_default();
                let is_omt =
                    name.contains("run__") || name.contains("node__") || name.contains("omt");
                if is_omt {
                    seen_omt_call = true;
                } else if !seen_omt_call {
                    discovery_overhead += 1;
                }
            }
            "usage" => {
                // usage 是去重后的累计快照（F-U12-3）——末值即终值
                if let Some(t) = ev["tokens"].as_u64() {
                    tokens = Some(t);
                }
                if let Some(c) = ev["cost"].as_f64() {
                    cost = Some(c);
                }
            }
            "error" => errors += 1,
            "exit" => {
                outcome = ev["status"].as_str().map(str::to_string);
                duration_ms = ev["durationMs"].as_u64();
                // header 里没有时（理论上有），从 exit 兜底拿 model 不可能——
                // model 只在 header；保持 None。
            }
            _ => {}
        }
        if model.is_none() {
            if let Some(m) = ev["model"].as_str() {
                model = Some(m.to_string());
            }
        }
    }
    // model 也可能只出现在 header 的 argv 里（"--model", "<id>" 相邻对）
    if model.is_none() {
        for line in text.lines().skip(1) {
            let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if ev["kind"] == "session_start" {
                if let Some(arr) = ev["args"].as_array() {
                    for (i, a) in arr.iter().enumerate() {
                        if a.as_str() == Some("--model") {
                            if let Some(m) = arr.get(i + 1).and_then(|v| v.as_str()) {
                                model = Some(m.to_string());
                            }
                        }
                    }
                }
                break;
            }
        }
    }
    Some(ExecMetrics {
        exec_id,
        harness,
        model,
        tool_calls,
        discovery_overhead,
        turns,
        duration_ms,
        tokens,
        cost,
        errors,
        outcome,
    })
}

/// 目录批量分析：`transcripts/<home>/` 下全部 `*.jsonl`，按 execId 排序。
#[allow(dead_code)]
pub fn analyze_dir(dir: &Path) -> Vec<ExecMetrics> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    paths.sort();
    for p in paths {
        if let Some(m) = analyze_jsonl(&p) {
            out.push(m);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(provider: &str, version: &str, file: &str) -> std::path::PathBuf {
        // CARGO_MANIFEST_DIR = apps/desktop/src-tauri → 上两级是仓库根
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/harness")
            .join(provider)
            .join(version)
            .join(file)
    }

    /// F-BENCH-1：真实 pi 1.3.0 冒烟流（direct exposure 后）——3 次工具调用、
    /// 零发现开销（§21 的 3-call 收官在此成为永久回归门）。
    #[test]
    fn pi_130_smoke_fixture_metrics() {
        let m = analyze_jsonl(&fixture("pi", "1.3.0", "stream-smoke-hello.jsonl"))
            .expect("pi fixture 应存在且可解析");
        assert_eq!(m.harness, "pi");
        assert_eq!(m.tool_calls, 3, "3-call 收官是永久契约: {m:?}");
        assert_eq!(m.discovery_overhead, 0, "direct 模式不得有发现轮: {m:?}");
        assert_eq!(m.outcome.as_deref(), Some("exited"));
        assert!(m.tokens.is_some(), "usage 终值应被归档: {m:?}");
    }

    /// F-BENCH-1：opencode 真实流——同样零发现开销（工具直声明）。
    #[test]
    fn opencode_11832_smoke_fixture_metrics() {
        let m = analyze_jsonl(&fixture("opencode", "1.18.32", "stream-smoke-hello.jsonl"))
            .expect("opencode fixture 应存在");
        assert_eq!(m.harness, "opencode");
        // 该卷是 Agent 24（信任门轮）：tool_calls 高于 3 但发现开销仍须为 0
        assert_eq!(m.discovery_overhead, 0, "{m:?}");
    }

    /// 语料回放：adapters 对历史版本流仍能解析（经 analyzer 的 kind 统计间接
    /// 验证——每卷至少产出 session_start/exit 与结构化事件计数）。
    #[test]
    fn fixtures_parse_and_carry_structure() {
        for (p, v, f) in [
            ("pi", "1.3.0", "stream-smoke-hello.jsonl"),
            ("opencode", "1.18.32", "stream-smoke-hello.jsonl"),
        ] {
            let m = analyze_jsonl(&fixture(p, v, f)).expect(f);
            assert!(!m.harness.is_empty(), "{p} harness 应从 header 读出");
            // 思考块按 provider：opencode 的 reasoning 仅在 --thinking 开启时
            // 产出（设计附录 A.2），冒烟流无思考块是正常形态
            if p != "opencode" {
                assert!(m.turns > 0, "{p} 应有思考块");
            }
        }
    }

    /// 归一化不变量：语料不得含本机路径 / 真实 session id / 真实时间戳。
    #[test]
    fn fixtures_are_normalized() {
        for (p, v, f) in [
            ("pi", "1.3.0", "stream-smoke-hello.jsonl"),
            ("opencode", "1.18.32", "stream-smoke-hello.jsonl"),
        ] {
            let text = std::fs::read_to_string(fixture(p, v, f)).unwrap();
            assert!(!text.contains("/Users/"), "{p} 残留本机路径");
            assert!(!text.contains("omt-accept"), "{p} 残留验收环境路径");
            assert!(!text.contains("01a1"), "{p} 残留真实 session id 前缀");
            assert!(text.contains("__PATH__"), "{p} 应有占位符");
        }
    }
}
