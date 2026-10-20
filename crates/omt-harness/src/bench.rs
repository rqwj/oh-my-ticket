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
    // F-BENCH-14：按消息求和（scope=message）/ 累计取末值（cumulative）；
    // raw 重放（当前 adapter）优先于 jsonl 记录的 usage 事件
    let mut msg_tokens: u64 = 0;
    let mut msg_cost: f64 = 0.0;
    let mut cumulative: Option<(u64, Option<f64>)> = None;
    let mut jsonl_usage: Option<(u64, Option<f64>)> = None;
    let mut errors = 0usize;
    let mut outcome = None;

    // F-BENCH-19：优先重放同目录 raw.log（harness 原始 stdout 全量）——
    // 当前 adapter 的产出才是事实源；jsonl 事件（采集时 adapter 产物，
    // 含旧口径 usage）仅作回落。
    // F-BENCH-23：重放源与 jsonl 同名成对（<stem>.raw.log，与 executor 的
    // <execId>.raw.log 命名一致）——曾有目录级 raw.log 回退，同目录多卷会
    // 串用别卷的用量（omp synthetic 卷拿到 smoke 的 42430），已删。
    let raw_log = path.with_file_name(format!("{exec_id}.raw.log"));
    let has_raw_log = raw_log.is_file();

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
                // F-BENCH-3：omp 经 write 工具写虚拟路径（input.path =
                // "xd://mcp__omt_run_claim"）调 MCP——工具名与 path 都查
                let path = ev["input"]["path"].as_str().unwrap_or_default();
                // omp 虚拟路径形如 xd://mcp__omt_run_claim（omt 与动词间是
                // 单下划线）；工具名形如 mcp__omt__run__claim（双下划线）
                // F-BENCH-16：虚拟路径以 xd://mcp__omt 前缀精确匹配——
                // contains("omt_") 会把写普通文件（docs/omt_notes.md）
                // 误判成 omt 调用
                let is_omt = name.contains("run__")
                    || name.contains("node__")
                    || name.contains("omt")
                    || path.starts_with("xd://mcp__omt");
                if is_omt {
                    seen_omt_call = true;
                } else if !seen_omt_call {
                    discovery_overhead += 1;
                }
            }
            "usage" => {
                // jsonl 记录的 usage（采集时 adapter 产物）——scopeless 视为
                // cumulative 取末值；仅当 raw 重放无产出时生效
                match ev["scope"].as_str() {
                    Some("message") => {
                        msg_tokens += ev["tokens"].as_u64().unwrap_or(0);
                        msg_cost += ev["cost"].as_f64().unwrap_or(0.0);
                    }
                    _ => {
                        jsonl_usage =
                            Some((ev["tokens"].as_u64().unwrap_or(0), ev["cost"].as_f64()))
                    }
                }
            }
            "raw" => {
                // F-BENCH-11/14：语料是采集当时 adapter 的输出——经**当前**
                // adapter 重放 raw 行得到的 usage 才是事实源（求和/末值按
                // scope），有产出则 jsonl 记录的 usage 让位。
                let body = ev["text"].as_str().unwrap_or_default();
                if body.trim_start().starts_with('{') {
                    for part in crate::adapters::parse_line(&harness, body) {
                        if part["kind"] == "usage" {
                            match part["scope"].as_str() {
                                Some("message") => {
                                    msg_tokens += part["tokens"].as_u64().unwrap_or(0);
                                    msg_cost += part["cost"].as_f64().unwrap_or(0.0);
                                }
                                _ => {
                                    cumulative = Some((
                                        part["tokens"].as_u64().unwrap_or(0),
                                        part["cost"].as_f64(),
                                    ))
                                }
                            }
                        }
                    }
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
    // F-BENCH-19：raw.log 存在时全量重放（harness 原始 stdout）——当前
    // adapter 的 usage 产出覆盖 jsonl 记录值（后者是采集时口径）
    if has_raw_log {
        if let Ok(raw_text) = std::fs::read_to_string(&raw_log) {
            msg_tokens = 0;
            msg_cost = 0.0;
            cumulative = None;
            for line in raw_text.lines() {
                for part in crate::adapters::parse_line_auto(line) {
                    if part["kind"] == "usage" {
                        match part["scope"].as_str() {
                            Some("message") => {
                                msg_tokens += part["tokens"].as_u64().unwrap_or(0);
                                msg_cost += part["cost"].as_f64().unwrap_or(0.0);
                            }
                            _ => {
                                cumulative = Some((
                                    part["tokens"].as_u64().unwrap_or(0),
                                    part["cost"].as_f64(),
                                ))
                            }
                        }
                    }
                }
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
    let (tokens, cost) = match (cumulative, msg_tokens) {
        (Some((t, c)), _) => (Some(t), c),
        (None, n) if n > 0 => (Some(n), Some(msg_cost)),
        (None, _) => jsonl_usage
            .map(|(t, c)| (Some(t), c))
            .unwrap_or((None, None)),
    };
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
        // CARGO_MANIFEST_DIR = crates/omt-harness → 上两级是仓库根
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/harness")
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
        // 3-call 收官轮（exec-50001-5）：调用数与发现开销双契约
        assert_eq!(m.tool_calls, 3, "{m:?}");
        assert_eq!(m.discovery_overhead, 0, "{m:?}");
        // F-BENCH-19：opencode 也逐步计——4 步相加 98225（免费档 cost 0）
        assert_eq!(m.tokens, Some(98225), "{m:?}");
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

    /// 归一化不变量（F-BENCH-8 扩面 + F-BENCH-24 防回退）：三家全部
    /// jsonl + raw.log + help/models——不得含本机路径 / 真实 session id /
    /// 验收环境名 / 内嵌 ISO 时间戳 / 真实 pid。
    #[test]
    fn fixtures_are_normalized() {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/harness");
        // F-BENCH-18/23：每家语料的必需文件清单——raw.log 与 jsonl 同名成对
        // （<stem>.raw.log）。raw.log 曾被 .gitignore 的 *.log 吞掉（本机绿、
        // 干净 clone 红），缺一即红，不再只靠数量下限。
        for (p, v) in [("pi", "1.3.0"), ("omp", "18.12.2"), ("opencode", "1.18.32")] {
            for f in [
                "version.txt",
                "help.txt",
                "models.txt",
                "meta.json",
                "stream-smoke-hello.jsonl",
                "stream-smoke-hello.raw.log",
            ] {
                let path = root.join(p).join(v).join(f);
                assert!(path.is_file(), "语料缺文件: {}", path.display());
            }
        }
        let iso = regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}").unwrap();
        let mcp_pid = regex::Regex::new(r"mcp:\d+").unwrap();
        let mut checked = 0;
        for entry in walk(&root) {
            let name = entry
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if !(name.ends_with(".jsonl")
                || name.ends_with(".log")
                || name == "help.txt"
                || name == "models.txt")
            {
                continue;
            }
            let text = std::fs::read_to_string(&entry).unwrap_or_default();
            assert!(
                !text.contains("/Users/"),
                "{} 残留本机路径",
                entry.display()
            );
            assert!(
                !text.contains("omt-accept"),
                "{} 残留验收环境路径",
                entry.display()
            );
            assert!(
                !text.contains("01a1"),
                "{} 残留真实 session id 前缀",
                entry.display()
            );
            // F-BENCH-24：占位符之外的 ISO 时间戳 / mcp:<pid> 必须为零
            //（曾整批漏过 170 处 ISO 与 3 个真实 pid）
            assert!(!iso.is_match(&text), "{} 残留 ISO 时间戳", entry.display());
            assert!(
                !mcp_pid.is_match(&text),
                "{} 残留 mcp:<pid>",
                entry.display()
            );
            checked += 1;
        }
        // 3 家 ×（smoke.jsonl + raw.log + help.txt + models.txt）+ omp synthetic = 13
        assert!(checked >= 13, "应覆盖三家全部语料文件，实际 {checked}");
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    /// F-BENCH-3 回归：omp 全部 3 次调用经 write 虚拟路径——开销 0。
    #[test]
    fn omp_18122_smoke_fixture_metrics() {
        let m = analyze_jsonl(&fixture("omp", "18.12.2", "stream-smoke-hello.jsonl"))
            .expect("omp fixture 应存在");
        assert_eq!(m.harness, "omp");
        assert_eq!(m.tool_calls, 3, "omp 3-call 收官契约: {m:?}");
        assert_eq!(
            m.discovery_overhead, 0,
            "write 虚拟路径须识别为 omt 调用: {m:?}"
        );
    }

    /// F-BENCH-11 回归：omp usage 从 end 族事件提取（adapter 已补）。
    #[test]
    fn omp_fixture_carries_usage() {
        let m = analyze_jsonl(&fixture("omp", "18.12.2", "stream-smoke-hello.jsonl"))
            .expect("omp fixture");
        assert!(m.tokens.is_some(), "omp usage 应非空: {m:?}");
        assert!(m.cost.is_some(), "{m:?}");
    }

    /// F-BENCH-4/15（语料库核心价值）：raw.log 全量重放进**当前** adapter，
    /// 与 jsonl 记录的结构化事件**对账**——工具调用名与数量必须一致。
    /// （jsonl 里的 raw 行只是采集时 adapter 未解析的行——真正产出事件的
    /// 原文当时已转结构化、未保存，测不出解析器漂移；raw.log 才是全量。）
    #[test]
    fn raw_log_replay_matches_recorded_events() {
        for (p, v) in [("pi", "1.3.0"), ("omp", "18.12.2"), ("opencode", "1.18.32")] {
            let raw_text = std::fs::read_to_string(fixture(p, v, "stream-smoke-hello.raw.log"))
                .unwrap_or_else(|_| panic!("{p} stream-smoke-hello.raw.log 应存在"));
            let jsonl_text =
                std::fs::read_to_string(fixture(p, v, "stream-smoke-hello.jsonl")).unwrap();

            // 重放侧的工具调用名序列
            let mut replayed: Vec<String> = Vec::new();
            for line in raw_text.lines() {
                for part in crate::adapters::parse_line(p, line) {
                    if part["kind"] == "tool_call" {
                        replayed.push(part["name"].as_str().unwrap_or_default().to_string());
                    }
                }
            }
            // 记录侧
            let recorded: Vec<String> = jsonl_text
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|ev| ev["kind"] == "tool_call")
                .map(|ev| ev["name"].as_str().unwrap_or_default().to_string())
                .collect();
            assert!(!recorded.is_empty(), "{p} jsonl 应记录工具调用");
            assert_eq!(
                replayed, recorded,
                "{p}: 当前 adapter 重放 raw.log 的工具序列与采集时不一致——解析器漂移"
            );
        }
    }

    /// F-BENCH-14 验证：usage 按消息求和后的会话总量（语料实证值）。
    #[test]
    fn usage_totals_sum_per_message() {
        // pi 4 条 message_end：11738+13134+13359+14109 = 52340 tok / $0.0615
        let pi = analyze_jsonl(&fixture("pi", "1.3.0", "stream-smoke-hello.jsonl")).unwrap();
        assert_eq!(pi.tokens, Some(52340), "pi 求和总量: {pi:?}");
        assert!((pi.cost.unwrap() - 0.0614784).abs() < 1e-9, "{pi:?}");
        // omp 4 条 message_end：合计 42430 tok / $0.0431
        let omp = analyze_jsonl(&fixture("omp", "18.12.2", "stream-smoke-hello.jsonl")).unwrap();
        assert_eq!(omp.tokens, Some(42430), "omp 求和总量: {omp:?}");
        assert!((omp.cost.unwrap() - 0.0431).abs() < 1e-3, "{omp:?}");
    }

    /// F-BENCH-4 续：models.txt 过形状校验（深度比对待 §6.3 暴露
    /// parse_models_output 后接入）。
    #[test]
    fn model_lists_present_and_shaped() {
        for (p, v) in [("pi", "1.3.0"), ("omp", "18.12.2"), ("opencode", "1.18.32")] {
            let text = std::fs::read_to_string(fixture(p, v, "models.txt")).unwrap();
            assert!(!text.trim().is_empty(), "{p} models.txt 为空");
            // opencode 是 provider/model 行式；pi 是列名表头表格；omp 是
            // 分组表格（组头 "kimi-code (7)" + 框线，无列名行）
            if p == "opencode" {
                assert!(text.contains('/'), "{p} 行式列表应含 provider/id");
            } else if p == "pi" {
                assert!(
                    text.contains("provider") && text.contains("model"),
                    "{p} 表头缺列"
                );
            } else {
                assert!(
                    text.contains('(') && text.contains('┌'),
                    "{p} 分组表格形状缺失"
                );
            }
        }
    }
}
