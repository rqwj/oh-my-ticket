//! 评测机制 §6.2（F-BENCH-2/5/6/7/10/13）：语料采集 example
//!（三轮评审定案：cargo example 形态，不加第二个 `[[bin]]`）。
//!
//! 用法：`cargo run -p omt-harness --example harness-capture -- <provider>`
//! 产出 `fixtures/harness/<provider>/<version>/` 下的 version/help/models
//! （事件流采样需要一次真实派发，由 runner 层（§6.4）负责——本 example
//! 只做零成本面）。
//!
//! - **registry 驱动**（F-BENCH-5）：binary 与 model_list_args 从 REGISTRY 读
//! - **版本串归一**（F-BENCH-6）：omp 报 `omp/18.12.2`——取末段建目录
//! - **meta 合并**（F-BENCH-7）：已有字段保留只补缺——重跑不冲掉出处信息
//! - **超时**（F-BENCH-10）：每命令 30s 兜底
//! - 采样即脱敏 + 归一（二轮评审 B / F-BENCH-8）：先 `sanitize_text`，再 HOME → `__HOME__`

use omt_harness::{descriptor_of, which_binary};
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CMD_TIMEOUT: Duration = Duration::from_secs(30);

fn main() {
    let provider = std::env::args()
        .nth(1)
        .expect("用法: harness-capture <opencode|omp|pi>");
    let d = descriptor_of(&provider).unwrap_or_else(|| panic!("未知 provider: {provider}"));
    let bin = which_binary(d.binary_name).unwrap_or_else(|| {
        panic!(
            "PATH 上找不到 {}（REGISTRY 驱动，缺失即失败）",
            d.binary_name
        )
    });

    let version_raw = capture(&bin, &["--version"]);
    // F-BENCH-6：omp 报 `omp/18.12.2`——取最后一个路径段
    let version = version_raw
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or(version_raw.trim())
        .to_string();
    assert!(
        !version.is_empty() && version != provider,
        "版本串异常: {version_raw:?}"
    );

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/harness")
        .join(&provider)
        .join(&version);
    std::fs::create_dir_all(&dir).expect("建目录");

    std::fs::write(dir.join("version.txt"), &version_raw).unwrap();
    let help = capture(&bin, &["--help"]);
    std::fs::write(dir.join("help.txt"), normalize(&help)).unwrap();
    let models = capture(&bin, d.model_list_args);
    std::fs::write(dir.join("models.txt"), normalize(&models)).unwrap();

    // F-BENCH-7：meta 合并——保留已有字段（出处/归一化说明），只补缺
    let meta_path = dir.join("meta.json");
    let mut meta: serde_json::Value = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let obj = meta.as_object_mut().expect("meta 应为对象");
    obj.entry("provider").or_insert(serde_json::json!(provider));
    obj.entry("harnessVersion")
        .or_insert(serde_json::json!(version));
    obj.entry("capturedAt")
        .or_insert(serde_json::json!(today()));
    obj.entry("source").or_insert(serde_json::json!(
        "harness-capture example (registry-driven)"
    ));
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

    println!("captured {provider} {version} -> {}", dir.display());
}

fn capture(bin: &std::path::Path, args: &[&str]) -> String {
    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("执行 {} {:?} 失败: {e}", bin.display(), args));
    // F-BENCH-21：读线程必须在等待退出**之前**启动——两路管道各 ~64KB，
    // 退出后才读，大输出（omp --help 已 16KB，模型列表增长后更大）会让
    // 子进程卡在 write，30s 后被误判超时（与探针 run_with_timeout 同款）
    let out_handle = child.stdout.take().map(|mut p| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = p.read_to_string(&mut s);
            s
        })
    });
    let err_handle = child.stderr.take().map(|mut p| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = p.read_to_string(&mut s);
            s
        })
    });
    let deadline = Instant::now() + CMD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "执行 {:?} {:?} 超时（{}s）",
                    bin,
                    args,
                    CMD_TIMEOUT.as_secs()
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("wait 失败: {e}"),
        }
    };
    let out = out_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let err = err_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    assert!(
        status.map(|s| s.success()).unwrap_or(false),
        "{:?} {:?} 退出非零",
        bin,
        args
    );
    if out.trim().is_empty() {
        err
    } else {
        out
    }
}

/// 输出归一（二轮评审 B / F-BENCH-8 / F-BENCH-24）：先逐行脱敏（F-BENCH-8，
/// 设计 B——help / models 可能回显 env 里的 key），再 HOME → __HOME__、
/// ISO 时间戳 → __TS__、mcp:<pid> → mcp:__PID__——跨机器 diff 稳定，且不泄
/// 真实时间 / pid / 密钥（占位符与语料既有约定一致；到分钟精度即覆盖全部
/// 已知形态）。
fn normalize(text: &str) -> String {
    let mut s = omt_harness::sanitize::sanitize_text(text);
    if let Ok(home) = std::env::var("HOME") {
        s = s.replace(&home, "__HOME__");
    }
    let iso = regex::Regex::new(
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?",
    )
    .unwrap();
    let mcp_pid = regex::Regex::new(r"mcp:\d+").unwrap();
    mcp_pid
        .replace_all(&iso.replace_all(&s, "__TS__"), "mcp:__PID__")
        .into_owned()
}

fn today() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}
