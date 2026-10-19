//! 评测机制 §6.2（F-BENCH-2）：语料采集 example
//!（三轮评审定案：cargo example 形态，不加第二个 `[[bin]]`）。
//!
//! 用法：`cargo run -p oh-my-ticket-desktop --example harness-capture -- <provider>`
//! 产出 `fixtures/harness/<provider>/<version>/` 下的 version/help/models
//! （事件流采样需要一次真实派发，由 runner 层（§6.4）负责——本 example
//! 只做零成本面）。
//!
//! 采样即脱敏 + 归一（二轮评审 B）：路径/时间戳/sessionId/pid 替换固定占位。

fn main() {
    let provider = std::env::args()
        .nth(1)
        .expect("用法: harness-capture <opencode|omp|pi>");
    let (binary, model_args): (&str, Vec<&str>) = match provider.as_str() {
        "opencode" => ("~/.opencode/bin/opencode", vec!["models"]),
        "omp" => ("/opt/homebrew/bin/omp", vec!["models"]),
        "pi" => ("pi", vec!["--list-models"]),
        other => panic!("未知 provider: {other}"),
    };
    let bin = shellexpand(binary);
    assert!(bin.is_file(), "找不到 {} ({})", provider, binary);

    let version = capture(&bin, &["--version"]);
    let version = version.trim();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/harness")
        .join(&provider)
        .join(version);
    std::fs::create_dir_all(&dir).expect("建目录");

    std::fs::write(dir.join("version.txt"), format!("{version}\n")).unwrap();
    let help = capture(&bin, &["--help"]);
    std::fs::write(dir.join("help.txt"), normalize(&help)).unwrap();
    let models = capture(&bin, &model_args);
    std::fs::write(dir.join("models.txt"), normalize(&models)).unwrap();

    let meta = format!(
        "{{\n  \"provider\": \"{provider}\",\n  \"harnessVersion\": \"{version}\",\n  \
         \"capturedAt\": \"{}\",\n  \"source\": \"harness-capture example\"\n}}\n",
        chrono_like_now()
    );
    std::fs::write(dir.join("meta.json"), meta).unwrap();
    println!("captured {provider} {version} -> {}", dir.display());
}

fn capture(bin: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new(bin)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("执行 {:?} {:?} 失败: {e}", bin, args));
    assert!(
        out.status.success(),
        "{:?} {:?} 退出非零: {}",
        bin,
        args,
        String::from_utf8_lossy(&out.stderr)
            .chars()
            .take(300)
            .collect::<String>()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 输出归一（占位替换——二轮评审 B 的 apply 侧）：路径 / 时间戳类数字。
/// 帮助文本通常无敏感值，仅去绝对路径让 diff 稳定。
fn normalize(text: &str) -> String {
    let mut s = text.to_string();
    if let Ok(home) = std::env::var("HOME") {
        s = s.replace(&home, "__HOME__");
    }
    s
}

fn chrono_like_now() -> String {
    // 无 chrono 依赖：用系统 date（example 环境可接受）
    std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn shellexpand(p: &str) -> std::path::PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
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
