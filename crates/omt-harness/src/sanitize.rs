//! §3.1 secrets 纪律：argv / stderr 进事件、日志、持久化之前的行级脱敏。
//! 执行器（omt-exec）与 launcher 共用。

/// Failed 诊断 stderr 尾部的最大字符数（截断在 UTF-8 边界回退）。
const STDERR_TAIL_MAX_CHARS: usize = 2000;

/// §3.1 secrets 纪律：无 regex 依赖的行级启发式脱敏——宁枉勿纵，只用于
/// Failed 的 stderr 尾诊断。
pub fn sanitize_line(line: &str) -> String {
    const KEYWORDS: &[&str] = &["key", "token", "secret", "password", "bearer"];
    // F-U2-8(a)：ascii 小写不改字节长度——lower 的偏移对原串安全（多字节
    // 非 ASCII 字符不会被 to_ascii_lowercase 改写）。
    let lower = line.to_ascii_lowercase();
    let mut cut: Option<usize> = None;
    for kw in KEYWORDS {
        if let Some(pos) = lower.find(kw) {
            let after = lower[pos + kw.len()..].trim_start();
            // 关键字后紧跟分隔符（= 或 :）即视为键值形态；bearer 单独算
            let is_kv = after.starts_with('=') || after.starts_with(':');
            let is_bearer = *kw == "bearer";
            if is_kv || is_bearer {
                let value_start = if is_kv {
                    let rest = &lower[pos + kw.len()..];
                    rest.find(|c: char| c != '=' && c != ':' && c != ' ')
                        .map(|off| {
                            rest.char_indices()
                                .nth(off)
                                .map(|(i, _)| pos + kw.len() + i)
                                .unwrap_or(line.len())
                        })
                        .unwrap_or(line.len())
                } else {
                    pos + kw.len()
                };
                cut = Some(cut.map_or(value_start, |c| c.min(value_start)));
            }
        }
    }
    if let Some(pos) = lower.find("sk-") {
        // sk- 前缀密钥形态：保留 sk-，值部分替换
        cut = Some(cut.map_or(pos + 3, |c| c.min(pos + 3)));
    }
    match cut {
        Some(idx) if idx <= line.len() && line.is_char_boundary(idx) => {
            format!("{} ***", line[..idx].trim_end())
        }
        _ => line.to_string(),
    }
}

pub fn sanitize_stderr_tail(tail: &str) -> String {
    let mut joined = tail
        .lines()
        .map(sanitize_line)
        .collect::<Vec<_>>()
        .join("\n");
    if joined.len() > STDERR_TAIL_MAX_CHARS {
        let mut end = STDERR_TAIL_MAX_CHARS;
        while !joined.is_char_boundary(end) {
            end -= 1;
        }
        joined.truncate(end);
    }
    joined
}

/// F-U2-8(b)：argv 进事件/日志前的脱敏。`--api-key=xxx` 同词内替换值；
/// `--api-key xxx` 值在下一个 arg，置 redact_next。宁枉勿纵：以 `-` 开头的
/// 词以关键字结尾即视为敏感标志位。
pub fn sanitize_args(args: &[String]) -> Vec<String> {
    const KEYWORDS: &[&str] = &["key", "token", "secret", "password"];
    let mut out = Vec::with_capacity(args.len());
    let mut redact_next = false;
    for arg in args {
        if redact_next {
            out.push("***".to_string());
            redact_next = false;
            continue;
        }
        let lower = arg.to_ascii_lowercase();
        let bare = lower.trim_start_matches('-');
        if let Some(eq) = bare.find('=') {
            if KEYWORDS.iter().any(|kw| bare[..eq].ends_with(kw)) {
                out.push(format!(
                    "{}=***",
                    &arg[..arg.find('=').unwrap_or(arg.len())]
                ));
                continue;
            }
        }
        if KEYWORDS.iter().any(|kw| bare.ends_with(kw)) {
            out.push(arg.clone());
            redact_next = true;
            continue;
        }
        out.push(arg.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_stderr_tail_redacts_secret_lines() {
        let tail = "starting up\nAPI_KEY=sk-abcdef123\nAuthorization: Bearer abc.def\ndone\n";
        let sanitized = sanitize_stderr_tail(tail);
        assert!(!sanitized.contains("sk-abcdef123"), "{sanitized}");
        assert!(!sanitized.contains("abc.def"), "{sanitized}");
        assert!(sanitized.contains("API_KEY= ***"), "{sanitized}");
        assert!(
            sanitized.contains("Authorization: Bearer ***"),
            "{sanitized}"
        );
    }

    #[test]
    fn sanitize_stderr_tail_truncates_on_utf8_boundary() {
        let tail = "界".repeat(STDERR_TAIL_MAX_CHARS);
        let sanitized = sanitize_stderr_tail(&tail);
        assert!(sanitized.len() <= STDERR_TAIL_MAX_CHARS);
        assert!(sanitized.is_char_boundary(sanitized.len()));
        assert!(!sanitized.is_empty());
    }

    #[test]
    fn sanitize_args_redacts_key_value_flags() {
        let args = vec![
            "--model".to_string(),
            "k3".to_string(),
            "--api-key".to_string(),
            "sk-live123".to_string(),
            "--token=abcdef".to_string(),
            "--verbose".to_string(),
        ];
        let sanitized = sanitize_args(&args);
        assert_eq!(sanitized[0], "--model");
        assert_eq!(sanitized[1], "k3");
        assert_eq!(sanitized[2], "--api-key");
        assert_eq!(sanitized[3], "***");
        assert_eq!(sanitized[4], "--token=***");
        assert_eq!(sanitized[5], "--verbose");
    }

    #[test]
    fn sanitize_line_redacts_secret_shapes() {
        assert_eq!(sanitize_line("API_KEY=sk-abcdef123"), "API_KEY= ***");
        assert!(sanitize_line("Authorization: Bearer abc.def").starts_with("Authorization:"));
        assert!(sanitize_line("Authorization: Bearer abc.def").ends_with("***"));
        assert!(sanitize_line("password: hunter2").ends_with("***"));
        assert!(!sanitize_line("plain log line").contains("***"));
    }
}
