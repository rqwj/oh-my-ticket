//! 评测机制 §6.2（F-BENCH-2）：契约探针——`#[ignore]` 集成测试形态
//!（三轮评审：不加第二个 `[[bin]]`；需真实 harness，默认不进 CI，按需
//! `cargo test -p oh-my-ticket-desktop --test harness_probe -- --ignored`）。
//!
//! 检查面（设计 §3）：
//! - flag 面：可执行验证优先（`--version` / 模型列表实际执行），帮助 grep
//!   只做 yellow 级存在性提示
//! - MCP 握手：真实 harness 进程能否连上注入的 omt server 并完成
//!   initialize / tools/list（端到端，与 mcp_spec 的 server 侧单测互补）
//! - 事件词汇：按 provider 的核心集（opencode 无终止事件——设计 §3.3）
//!
//! 语料采集（harness-capture）在同目录 example：`cargo run --example
//! harness-capture -- <provider>`。

//! F-BENCH-2：flag 面（可执行优先）。
#[test]
#[ignore = "requires locally installed harnesses (design §5 CI boundary)"]
fn probe_flag_surface_executable() {
    for (provider, binary) in [
        ("opencode", "~/.opencode/bin/opencode"),
        ("omp", "/opt/homebrew/bin/omp"),
        ("pi", "pi"),
    ] {
        let path = shellexpand(binary);
        if !path.is_file() {
            eprintln!("[yellow] {provider}: binary not found at {binary} — skip");
            continue;
        }
        // --version 必须成功且非空（可执行验证 = 绿/红级）
        let out = std::process::Command::new(&path)
            .arg("--version")
            .output()
            .expect("spawn --version");
        assert!(
            out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "[red] {provider}: --version 失败或为空"
        );
    }
}

/// F-BENCH-2：模型列表（可执行验证；model_list_args 既是 UI 数据面也是
/// flag 漂移探针——F-PI-2 的教训）。
#[test]
#[ignore = "requires locally installed harnesses + network for some providers"]
fn probe_model_list_executable() {
    // opencode models / pi --list-models / omp models
    for (provider, binary, args) in [
        ("opencode", "~/.opencode/bin/opencode", vec!["models"]),
        ("omp", "/opt/homebrew/bin/omp", vec!["models"]),
        ("pi", "pi", vec!["--list-models"]),
    ] {
        let path = shellexpand(binary);
        if !path.is_file() {
            eprintln!("[yellow] {provider}: skip");
            continue;
        }
        let out = std::process::Command::new(&path)
            .args(&args)
            .output()
            .expect("spawn model list");
        assert!(
            out.status.success(),
            "[red] {provider}: model list {:?} 退出非零（flag 漂移？）stderr: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        );
    }
}

/// F-BENCH-2：事件词汇核心集（按 provider）——从语料库 fixture 回放。
/// 注意这是**纯回放**（不花 LLM），但放在本文件与真实探针同列，因为校验的
/// 是「当前 adapter 契约 vs 历史版本流」的漂移面。
#[test]
fn probe_event_vocabulary_from_fixtures() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures");
    // pi/omp 核心集（经 mcp server 侧转换后我们产出 *_end 聚合事件）
    for (provider, version, file) in [
        ("pi", "1.3.0", "stream-smoke-hello.jsonl"),
        ("omp", "18.12.2", "stream-smoke-hello.jsonl"),
    ] {
        let text =
            std::fs::read_to_string(root.join("harness").join(provider).join(version).join(file))
                .expect("fixture 应存在");
        // 统一事件面（adapters 产物）：session_start / tool_call / exit 必在
        assert!(
            text.contains("\"session_start\""),
            "{provider} 缺 session_start"
        );
        assert!(text.contains("\"tool_call\""), "{provider} 缺 tool_call");
        assert!(text.contains("\"exit\""), "{provider} 缺 exit");
    }
    // opencode：无终止事件是进程退出语义——fixture 里的 exit 是我们合成的
    // 终态行（executor 侧），原始流词汇不含 agent_end 类事件
    let oc =
        std::fs::read_to_string(root.join("harness/opencode/1.18.32/stream-smoke-hello.jsonl"))
            .unwrap();
    assert!(oc.contains("\"tool_call\""), "opencode 缺 tool_call");
}

fn shellexpand(p: &str) -> std::path::PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    // bare name → PATH 查找（复用 harness crate 前，简单 which）
    if !p.contains('/') {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let c = std::path::PathBuf::from(dir).join(p);
                if c.is_file() {
                    return c;
                }
            }
        }
    }
    std::path::PathBuf::from(p)
}
