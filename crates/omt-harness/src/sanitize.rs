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
///
/// §53 F-SP2-6 修订（§51 的词首修复削弱了脱敏，三组回归——行为由表驱动
/// 测试 `sanitize_line_leak_table_every_entry_is_redacted` /
/// `sanitize_line_intact_table_is_untouched` 锁定）：
/// - **键值形态不再要求词首**：「关键字 + 可选空白 + `=`/`:`」任意位置命中
///   （`apiKey=…`、`PGPASSWORD=…`、`clientSecret: …`）——唯一例外是命中
///   所在 token 以 `-` 开头（CLI flag 名，如 `--monkey=banana`）时仍按误伤
///   处理；分隔符前的空白（`password = hunter2`、`token : abc`）重新算键值。
/// - `sk-` 用 `match_indices` 取第一个**词首**命中——词中的 `task-`/
///   `desk-` 不再挡住后面的真 `sk-` 密钥。
/// - Authorization 遍历命中（`reauthorization` 的词中命中不挡真头），且
///   冒号必须**紧跟**关键字（中间只许空白）——`… see https://…` 的 `https:`
///   冒号不再误伤。
/// - 关键字循环不再在首个词首非键值命中处 break（`--key-file /x
///   --key=SECRET` 的后者必须命中）。
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
    // §53：命中所在的 CLI token（往前扫字母数字/`-`/`_`）以 `-` 开头即为
    // flag 名——flag 名里的词中键值命中（`--monkey=banana`）按 §51 误伤
    // 处理；裸复合名（`apiKey=`、`PGPASSWORD=`、`clientSecret:`）不是 flag，
    // 键值形态照脱（宁枉勿纵）。
    let in_dash_flag = |kw_pos: usize| -> bool {
        let bytes = lower.as_bytes();
        let mut start = kw_pos;
        while start > 0
            && (bytes[start - 1].is_ascii_alphanumeric()
                || bytes[start - 1] == b'-'
                || bytes[start - 1] == b'_')
        {
            start -= 1;
        }
        bytes[start] == b'-'
    };
    for kw in KEYWORDS {
        // §53：遍历全部命中——词中命中、或词首但非键值（`--key-file`）都
        // 不得挡住同串后面的真键值形态（`--key-file /x --key=SECRET`）。
        for (pos, _) in lower.match_indices(kw) {
            let after = &lower[pos + kw.len()..];
            // §53：键值判定先跳过空白——`password = hunter2`、`token : abc`
            // 都是键值形态（空白皆为 ascii，字节偏移安全）。
            let ws = after.len() - after.trim_start_matches([' ', '\t']).len();
            let rest = &after[ws..];
            let is_kv = rest.starts_with('=') || rest.starts_with(':');
            if is_kv && (is_word_start(pos) || !in_dash_flag(pos)) {
                // 值起点：跳过连续的 =/:/空白（皆为 ascii，字节偏移即字符边界）
                let value_start = rest
                    .find(|c: char| c != '=' && c != ':' && c != ' ' && c != '\t')
                    .map(|off| pos + kw.len() + ws + off)
                    .unwrap_or(line.len());
                note(value_start);
                break;
            }
            if !is_word_start(pos) {
                // 词中且非（本规则认定的）键值形态——继续找后面的命中
                continue;
            }
            if *kw == "bearer"
                // 独立词：其后不得紧跟字母数字或 `-`（kebab-case 的
                // `--no-bearer-check` 不是 Bearer scheme）
                && !after
                    .as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-')
            {
                // 保留 Bearer，值部分替换
                note(pos + kw.len());
                break;
            }
            // 词首但既非键值也非独立 bearer（`--key-file`）——继续找后面的
            // 命中，不得 break
        }
    }
    // §53：`sk-` 前缀密钥形态取第一个**词首**命中（保留 sk-，值部分替换）——
    // `--task-file /x (current: sk-…)` 里词中的 `task-` 不再挡住后面的真密钥。
    for (pos, _) in lower.match_indices("sk-") {
        if is_word_start(pos) {
            note(pos + 3);
            break;
        }
    }
    // §51/§53：Authorization: 后按 scheme 脱敏（任意 scheme）——scheme 词
    // 保留，其后的值整体替换；`Authorization: <单段值>`（无 scheme 形态）
    // 从冒号后直接整体替换。§53：遍历命中（`reauthorization` 的词中命中
    // 不挡真头），且冒号必须紧跟关键字（中间只许空白）。
    for (pos, _) in lower.match_indices("authorization") {
        if !is_word_start(pos) {
            continue;
        }
        let after_kw = &lower[pos + "authorization".len()..];
        let trimmed = after_kw.trim_start_matches([' ', '\t']);
        if !trimmed.starts_with(':') {
            // 不是 Authorization 头（`authorization failed, see https://…`）——
            // 不得拿后面任意位置的冒号（`https:`）误伤
            continue;
        }
        let credentials = &trimmed[1..];
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
            let colon_off = after_kw.len() - trimmed.len() + 1;
            note(pos + "authorization".len() + colon_off + value_start);
        }
        break;
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
/// §53 F-SP2-6：VALUE_FLAGS（`--auth`）要求**前导 `-`**——裸位置参数
/// （`gh auth login` 的 `auth`、裸 `h`）不是 flag，不得吞掉下一个 arg；
/// `-H` **区分大小写**（curl 的 header flag 是大写 `-H`，`-h` 是 help），
/// 且任何以 `-H` / `--header` 开头的 arg（含 `-HAuthorization: …` 连写）
/// 都按敏感处理。
pub fn sanitize_args(args: &[String]) -> Vec<String> {
    const KEYWORDS: &[&str] = &["key", "token", "secret", "password"];
    const VALUE_FLAGS: &[&str] = &["auth"];
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
        let is_flag = arg.starts_with('-');
        if let Some(eq) = bare.find('=') {
            let name = &bare[..eq];
            if KEYWORDS.iter().any(|kw| name.ends_with(kw))
                || (is_flag && VALUE_FLAGS.contains(&name))
            {
                out.push(format!(
                    "{}=***",
                    &arg[..arg.find('=').unwrap_or(arg.len())]
                ));
                continue;
            }
        }
        if let Some(prefix_len) = header_flag_prefix(arg) {
            if arg.len() > prefix_len {
                // 连写形态（`-HAuthorization: …` / `--header=…`）：flag 前缀
                // 保留，其余整体替换
                out.push(format!("{}***", &arg[..prefix_len]));
            } else {
                out.push(arg.clone());
                redact_next = true;
            }
            continue;
        }
        if KEYWORDS.iter().any(|kw| bare.ends_with(kw)) || (is_flag && VALUE_FLAGS.contains(&bare))
        {
            out.push(arg.clone());
            redact_next = true;
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// §53 F-SP2-6：curl 的 header flag——大写 `-H`（大小写敏感：`-h` 是
/// help）或 `--header`（大小写不敏感）。返回 flag 前缀的字节长度（连写
/// 值紧跟其后）；任何以二者开头的 arg 都按敏感处理（header 串可能带
/// `Authorization: Bearer …`，宁枉勿纵）。
fn header_flag_prefix(arg: &str) -> Option<usize> {
    let bytes = arg.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'-' && bytes[1] == b'H' {
        return Some(2);
    }
    if bytes.len() >= 8 && bytes[..8].eq_ignore_ascii_case(b"--header") {
        return Some(8);
    }
    None
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

    /// §53 F-SP2-6 表驱动·泄漏列：§53 列出的**每一个**泄漏样例——§51 词首
    /// 修复削弱脱敏造成的三组回归（sk- 只看首命中 / 分隔符前有空白 /
    /// 键值形态要求词首）+ Authorization 首命中与冒号错位 + 关键字循环
    /// break + §51 原始样本（回归锁定）。脱敏规则已来回两轮（§51→§53），
    /// 本表锁定行为：输出必须不含机密片段、且带 `***` 标记。
    #[test]
    fn sanitize_line_leak_table_every_entry_is_redacted() {
        const LEAKS: &[(&str, &str)] = &[
            // 回归 1：`sk-` 只看首次命中——词中的 `task-` 挡住后面的真 `sk-`
            ("--task-file /x (current: sk-live-abc123)", "live-abc123"),
            ("error in task-runner: used sk-proj-SECRET", "proj-SECRET"),
            // 回归 2：`=`/`:` 前有空白不再算键值
            ("password = hunter2", "hunter2"),
            ("token : abc", "abc"),
            ("api_key = abc", "abc"),
            // 回归 3：键值形态也要求词首（裸复合名不是 CLI flag——宁枉勿纵）
            ("apiKey=abc123", "abc123"),
            ("accessToken=abc123", "abc123"),
            ("PGPASSWORD=hunter2", "hunter2"),
            ("clientSecret: abc", "abc"),
            ("authToken: abc", "abc"),
            // Authorization 只看首次命中：`reauthorization` 的词中命中挡住真头
            (
                "reauthorization needed; Authorization: Basic SECRET",
                "SECRET",
            ),
            // 关键字循环在第一个词首非键值命中处 break
            ("--key-file /x --key=SECRET", "SECRET"),
            ("token refresh failed; token=SECRET", "SECRET"),
            // §51 原始样本（不得随本次修复回退）
            ("Authorization: Bearer xyz-secret-1", "xyz-secret-1"),
            ("Authorization: Basic dXNlcjpwYXNz", "dXNlcjpwYXNz"),
            ("Authorization: token ghp_abc123def", "ghp_abc123def"),
        ];
        for (input, secret) in LEAKS {
            let out = sanitize_line(input);
            assert!(
                !out.contains(secret),
                "§53 泄漏列：{input:?} 的输出 {out:?} 仍含机密 {secret:?}"
            );
            assert!(
                out.contains("***"),
                "§53 泄漏列：{input:?} 必须被脱敏，实际输出 {out:?}"
            );
        }
    }

    /// §53 F-SP2-6 表驱动·保留列：§51 的误伤样本必须原样保留（修复不得
    /// 以回退误伤修复为代价），外加 §53 的 Authorization 冒号紧跟规则
    ///（`https:` 的冒号不再触发）。
    #[test]
    fn sanitize_line_intact_table_is_untouched() {
        const INTACT: &[&str] = &[
            "--ask-for-approval",
            "--task-file /w/task-list.md",
            "task-list.md",
            "https://desk-api.example.com/v1",
            "--no-bearer-check",
            // flag 名（前导 `-`）里的词中键值命中按误伤处理
            "--monkey=banana",
            // desk- / risk- 词中命中 sk-：无真密钥时不得截断
            "risk-assessment of desk- usage",
            // §53：Authorization 的冒号必须紧跟关键字（中间只许空白）
            "authorization failed, see https://example.com/help",
            "plain log line",
        ];
        for input in INTACT {
            assert_eq!(
                sanitize_line(input),
                *input,
                "§53 保留列：{input:?} 不得被误伤"
            );
        }
    }

    /// §53 F-SP2-6（sanitize_args）：VALUE_FLAGS 必须要求前导 `-`、`-H`
    /// 区分大小写——裸位置参数 `auth` / `h` 不是 flag，`-h` 是 help：
    /// `gh auth login`、`-h foo`、裸 `h foo` 都不得吞掉下一个参数。
    #[test]
    fn sanitize_args_keeps_bare_positionals_and_lowercase_h() {
        let out = sanitize_args(&["gh".into(), "auth".into(), "login".into()]);
        assert_eq!(out, vec!["gh", "auth", "login"], "gh auth login 不得脱敏");
        let out = sanitize_args(&["-h".into(), "foo".into()]);
        assert_eq!(out, vec!["-h", "foo"], "小写 -h 是 help，不是 header flag");
        let out = sanitize_args(&["h".into(), "foo".into()]);
        assert_eq!(out, vec!["h", "foo"], "裸位置参数 h 不得吞掉下一个 arg");
    }

    /// §53 F-SP2-6（sanitize_args）：以 `-H`（大小写敏感——curl 的 header
    /// flag 是大写）或 `--header` 开头的 arg 都按敏感处理——独立形态的值
    /// （下一 arg）、`-H` 连写、`--header=` 连写整体替换；`--auth` 的值
    /// 照旧。
    #[test]
    fn sanitize_args_redacts_header_flag_forms_and_auth_values() {
        let out = sanitize_args(&[
            "-H".into(),
            "Authorization: Bearer abc.def".into(),
            "-HAuthorization: Bearer abc.def".into(),
            "--header".into(),
            "Authorization: Bearer abc.def".into(),
            "--header=Authorization: Bearer abc.def".into(),
            "--auth".into(),
            "ghp_abc123".into(),
        ]);
        assert_eq!(out[0], "-H");
        assert_eq!(out[1], "***", "-H 的值是 header 串");
        assert_eq!(out[2], "-H***", "-H 连写形态整体脱敏");
        assert_eq!(out[3], "--header");
        assert_eq!(out[4], "***", "--header 的值是 header 串");
        assert_eq!(out[5], "--header***", "--header= 连写形态整体脱敏");
        assert_eq!(out[6], "--auth");
        assert_eq!(out[7], "***", "--auth 的值是凭证");
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
