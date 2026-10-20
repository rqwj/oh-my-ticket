//! 评测机制 §6.2（F-BENCH-2/5/10/13）：契约探针——`#[ignore]` 集成测试形态
//!（三轮评审：不加第二个 `[[bin]]`；需真实 harness，默认不进 CI，按需
//! `cargo test -p oh-my-ticket-desktop --test harness_probe -- --ignored`）。
//!
//! **registry 驱动**（F-BENCH-5）：binary 解析走 `which_binary`（REGISTRY 的
//! binary_name），model_list_args 从 descriptor 读——探针不再自带 flag 副本，
//! registry 写错（F-PI-2 类）探针必红。**找不到 binary = fail**（不再静默
//! 跳过假绿）。**超时**（F-BENCH-10）：每命令 30s 兜底，防交互式 TUI 挂起。
//!
//! 语料采集（harness-capture）在同目录 example。

use oh_my_ticket_desktop_lib::{descriptor_of, which_binary};
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const PROVIDERS: &[&str] = &["opencode", "omp", "pi"];
const CMD_TIMEOUT: Duration = Duration::from_secs(30);

/// F-BENCH-10：带超时兜底的同步执行——超时 kill 并返回失败
///（flag 写错时 harness 可能起交互界面，裸 .output() 会永久挂起）。
fn run_with_timeout(bin: &PathBuf, args: &[&str]) -> (bool, String, String) {
    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {} {:?}: {e}", bin.display(), args));
    let deadline = Instant::now() + CMD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => {
                let _ = child.kill();
                break None;
            }
        }
    };
    let mut out_s = String::new();
    let mut err_s = String::new();
    if let Some(mut p) = child.stdout.take() {
        let _ = p.read_to_string(&mut out_s);
    }
    if let Some(mut p) = child.stderr.take() {
        let _ = p.read_to_string(&mut err_s);
    }
    let timed_out = status.is_none();
    (
        status.map(|s| s.success()).unwrap_or(false) && !timed_out,
        out_s,
        err_s,
    )
}

fn resolve_binary(provider: &str) -> PathBuf {
    let d = descriptor_of(provider).unwrap_or_else(|| panic!("REGISTRY 缺 {provider}"));
    which_binary(d.binary_name).unwrap_or_else(|| {
        panic!(
            "[red] {provider}: PATH 上找不到 {}（F-BENCH-5：缺失即红，不静默跳过）",
            d.binary_name
        )
    })
}

/// F-BENCH-5/10：flag 面——`--version` 对 REGISTRY 全部 provider 可执行。
#[test]
#[ignore = "requires locally installed harnesses (design §5 CI boundary)"]
fn probe_flag_surface_executable() {
    for provider in PROVIDERS {
        let bin = resolve_binary(provider);
        let (ok, out, err) = run_with_timeout(&bin, &["--version"]);
        assert!(
            ok && !out.trim().is_empty(),
            "[red] {provider}: --version 失败或空（flag 漂移 / 超时 30s？）stderr={}",
            err.chars().take(200).collect::<String>()
        );
    }
}

/// F-BENCH-5/10：模型列表——model_list_args 从 descriptor 读（与 UI 数据面
/// 同源；registry 写错必红——F-PI-2 形态的自动拦截）。
#[test]
#[ignore = "requires locally installed harnesses"]
fn probe_model_list_executable() {
    for provider in PROVIDERS {
        let d = descriptor_of(provider).expect("provider");
        let bin = resolve_binary(provider);
        let args: Vec<&str> = d.model_list_args.to_vec();
        let (ok, out, err) = run_with_timeout(&bin, &args);
        assert!(
            ok && !out.trim().is_empty(),
            "[red] {provider}: model_list_args {args:?} 失败或空输出 stderr={}",
            err.chars().take(200).collect::<String>()
        );
    }
}

/// 事件词汇（按 provider）——从语料 fixture 回放，纯回放不花 LLM。
/// （fixture 由 capture/采样脚本产出；此处断言统一事件面要素。）
#[test]
fn probe_event_vocabulary_from_fixtures() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures");
    for (provider, version) in [("pi", "1.3.0"), ("omp", "18.12.2"), ("opencode", "1.18.32")] {
        let text = std::fs::read_to_string(
            root.join("harness")
                .join(provider)
                .join(version)
                .join("stream-smoke-hello.jsonl"),
        )
        .unwrap_or_else(|e| panic!("{provider} fixture 缺失: {e}"));
        assert!(
            text.contains("\"session_start\""),
            "{provider} 缺 session_start"
        );
        assert!(text.contains("\"tool_call\""), "{provider} 缺 tool_call");
        // opencode 无终止事件（进程退出语义）——exit 是 executor 合成行
        assert!(text.contains("\"exit\""), "{provider} 缺 exit");
    }
}
