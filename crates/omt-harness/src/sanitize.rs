//! §3.1 secrets 纪律：argv / stderr 进事件、日志、持久化之前的行级脱敏。
//! 执行器（omt-exec）与 launcher 共用。

/// Failed 诊断 stderr 尾部的最大字符数（截断在 UTF-8 边界回退）。
const STDERR_TAIL_MAX_CHARS: usize = 2000;

/// §3.1 secrets 纪律：无 regex 依赖的行级启发式脱敏——宁枉勿纵，只用于
/// Failed 的 stderr 尾诊断。
///
/// §51 修订：
/// - 关键字与 `sk-` 只在**词首**匹配——`--ask-for-approval`、`--task-file`、
///   `/w/task-list.md`、`https://desk-api…`、`--monkey=banana` 这类中间撞上
///   `sk-`/`key` 的正常参数不再被截断（此前 `--ask- ***`）。
/// - `bearer` 必须是独立词（其后不得紧跟字母数字或 `-`）——`--no-bearer-check`
///   不再被截断；`Bearer <值>` 照旧脱敏。
/// - `Authorization:` 后按 scheme 整体脱敏（任意 scheme）：`Basic …`、
///   `token ghp_…` 此前完全漏网；scheme 词保留（诊断有用），其后的值整体
///   替换。
pub fn sanitize_line(line: &str) -> String {
    const KEYWORDS: &[&str] = &["key", "token", "secret", "password", "bearer"];
    // F-U2-8(a)：ascii 小写不改字节长度——lower 的偏移对原串安全（多字节
    // 非 ASCII 字符不会被 to_ascii_lowercase 改写）。
    let lower = line.to_ascii_lowercase();
    let mut cut: Option<usize> = None;
    let mut note = |at: usize| {
        cut = Some(cut.map_or(at, |c| c.min(at)));
    };
    // 词首判定：前一个字节不是 ascii 字母数字（行首也算词首；多字节 UTF-8
    // 字节的 is_ascii_alphanumeric 为 false，中文后接英文关键词也算词首——
    // 宁枉勿纵）。
    let is_word_start =
        |pos: usize| -> bool { pos == 0 || !lower.as_bytes()[pos - 1].is_ascii_alphanumeric() };
    for kw in KEYWORDS {
        // 词首的全部命中里取第一个（首个命中若非词首——如 `monkey` 里的
        // `key`——不得挡住同串后面的真键值形态 `… key=x`）
        for (pos, _) in lower.match_indices(kw) {
            if !is_word_start(pos) {
                continue;
            }
            let after = &lower[pos + kw.len()..];
            // 关键字后紧跟分隔符（= 或 :）即视为键值形态；bearer 单独算
            let is_kv = after.starts_with('=') || after.starts_with(':');
            let is_bearer = *kw == "bearer";
            if is_kv {
                // 值起点：跳过连续的 =/:/空白（皆为 ascii，字节偏移即字符边界）
                let rest = after;
                let value_start = rest
                    .find(|c: char| c != '=' && c != ':' && c != ' ')
                    .map(|off| pos + kw.len() + off)
                    .unwrap_or(line.len());
                note(value_start);
            } else if is_bearer
                // 独立词：其后不得紧跟字母数字或 `-`（kebab-case 的
                // `--no-bearer-check` 不是 Bearer scheme）
                && !after
                    .as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-')
            {
                // 保留 Bearer，值部分替换
                note(pos + kw.len());
            }
            break;
        }
    }
    if let Some(pos) = lower.find("sk-") {
        // sk- 前缀密钥形态（词首才算）：保留 sk-，值部分替换
        if is_word_start(pos) {
            note(pos + 3);
        }
    }
    // §51：Authorization: 后按 scheme 脱敏（任意 scheme）——scheme 词保留，
    // 其后的值整体替换；`Authorization: <单段值>`（无 scheme 形态）从冒号后
    // 直接整体替换。
    if let Some(pos) = lower.find("authorization") {
        if is_word_start(pos) {
            let after_kw = &lower[pos + "authorization".len()..];
            if let Some(colon) = after_kw.find(':') {
                let credentials = &after_kw[colon + 1..];
                if let Some(mut value_start) = credentials.find(|c: char| !c.is_whitespace()) {
                    // 跳过一个 scheme 词（Basic / Bearer / token …）再取值；
                    // scheme 词之后没有更多内容（单段值形态）则从值起点整体替换
                    let scheme_and_rest = &credentials[value_start..];
                    let scheme_len = scheme_and_rest
                        .find(|c: char| c.is_whitespace())
                        .unwrap_or(scheme_and_rest.len());
                    let after_scheme = &scheme_and_rest[scheme_len..];
                    if let Some(follow) = after_scheme.find(|c: char| !c.is_whitespace()) {
                        value_start += scheme_len + follow;
                    }
                    note(pos + "authorization".len() + colon + 1 + value_start);
                }
            }
        }
    }
    match cut {
        Some(idx) if idx <= line.len() && line.is_char_boundary(idx) => {
            format!("{} ***", line[..idx].trim_end())
        }
        _ => line.to_string(),
    }
}

/// F-BENCH-8（评测设计 B）：语料采集的整段文本脱敏——逐行过 sanitize_line，
/// 保留原有换行结构（含末尾换行）。help / models 输出可能回显 env 里的 key
/// （如 `--api-key <KEY>  (current: sk-…)`），harness-capture 先调用本函数，
/// 再做 HOME / 时间戳 / pid 归一。
pub fn sanitize_text(text: &str) -> String {
    let mut out = text
        .lines()
        .map(sanitize_line)
        .collect::<Vec<_>>()
        .join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
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
/// §51：`--auth` / `-H` 的值是凭证（header 串 / token）——按敏感标志位处理
///（精确匹配，不做后缀匹配：`--path` 这类以 h 结尾的词不受影响）。
pub fn sanitize_args(args: &[String]) -> Vec<String> {
    const KEYWORDS: &[&str] = &["key", "token", "secret", "password"];
    const VALUE_FLAGS: &[&str] = &["auth", "h"];
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
            let name = &bare[..eq];
            if KEYWORDS.iter().any(|kw| name.ends_with(kw)) || VALUE_FLAGS.contains(&name) {
                out.push(format!(
                    "{}=***",
                    &arg[..arg.find('=').unwrap_or(arg.len())]
                ));
                continue;
            }
        }
        if KEYWORDS.iter().any(|kw| bare.ends_with(kw)) || VALUE_FLAGS.contains(&bare) {
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

    /// §51「sanitize_line 误伤正常参数且仍有漏网」：`sk-`/关键字只在词首
    /// 匹配、bearer 必须独立词——§51 列的误伤样本必须原样保留。
    #[test]
    fn sanitize_line_keeps_args_that_only_collide_mid_word() {
        for intact in [
            "--ask-for-approval",
            "--task-file /w/task-list.md",
            "/w/task-list.md",
            "https://desk-api.example.com/v1",
            "--no-bearer-check",
            "--monkey=banana",
            "plain log line",
        ] {
            assert_eq!(
                sanitize_line(intact),
                intact,
                "§51 误伤样本必须原样保留：{intact}"
            );
        }
    }

    /// §51 泄网样本：`Authorization:` 后按 scheme 脱敏（任意 scheme——
    /// Basic / token 此前完全漏网；scheme 词保留、值整体替换）。
    #[test]
    fn sanitize_line_redacts_the_value_after_authorization_any_scheme() {
        assert_eq!(
            sanitize_line("Authorization: Basic dXNlcjpwYXNz"),
            "Authorization: Basic ***"
        );
        assert_eq!(
            sanitize_line("Authorization: token ghp_abc123def"),
            "Authorization: token ***"
        );
        assert_eq!(
            sanitize_line("Authorization: Bearer abc.def"),
            "Authorization: Bearer ***"
        );
        // 单段值形态（无 scheme 词）：冒号后整体替换
        assert_eq!(sanitize_line("Authorization: abc123"), "Authorization: ***");
    }

    /// §51：sanitize_args 把 `--auth` / `-H` 的值视为敏感（header 串 / token）；
    /// 精确匹配——`--path` 这类不以敏感词命名的词不受影响。
    #[test]
    fn sanitize_args_redacts_auth_and_header_flag_values() {
        let args = vec![
            "--verbose".to_string(),
            "--auth".to_string(),
            "ghp_abc123".to_string(),
            "-H".to_string(),
            "Authorization: Bearer abc.def".to_string(),
            "--auth=sk-live-1".to_string(),
            "--path".to_string(),
            "/tmp/out.json".to_string(),
        ];
        let sanitized = sanitize_args(&args);
        assert_eq!(sanitized[0], "--verbose");
        assert_eq!(sanitized[1], "--auth");
        assert_eq!(sanitized[2], "***", "--auth 的值是凭证，必须替换");
        assert_eq!(sanitized[3], "-H");
        assert_eq!(sanitized[4], "***", "-H 的值是 header 串，必须替换");
        assert_eq!(sanitized[5], "--auth=***");
        assert_eq!(sanitized[6], "--path", "精确匹配：--path 不是敏感 flag");
        assert_eq!(sanitized[7], "/tmp/out.json");
    }

    /// F-BENCH-8：整段脱敏——含密钥的行被截断，普通行与空行原样，行数与
    /// 末尾换行保持不变。
    #[test]
    fn sanitize_text_masks_secret_lines_and_keeps_layout() {
        let input =
            "Usage: x [options]\n  --api-key <KEY>  (current: sk-live-abc123)\nAUTH_TOKEN=abc\n\nplain\n";
        let out = sanitize_text(input);
        assert!(!out.contains("abc123"), "{out}");
        assert!(!out.contains("AUTH_TOKEN=abc"), "{out}");
        assert!(out.starts_with("Usage: x [options]\n"), "{out}");
        assert!(out.ends_with("plain\n"), "{out}");
        assert_eq!(out.lines().count(), input.lines().count());
    }
}
