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
/// `sanitize_line_intact_table_is_untouched` /
/// `sanitize_line_acceptable_collateral_column_is_redacted_by_design` 锁定）：
/// - **键值形态不再要求词首**：「关键字 + 可选空白 + `=`/`:`」任意位置命中
///   （`apiKey=…`、`PGPASSWORD=…`、`clientSecret: …`）；分隔符前的空白
///   （`password = hunter2`、`token : abc`）重新算键值。
/// - `sk-` 用 `match_indices` 取第一个**词首**命中——词中的 `task-`/
///   `desk-` 不再挡住后面的真 `sk-` 密钥。
/// - Authorization 遍历命中（`reauthorization` 的词中命中不挡真头），且
///   冒号必须紧跟关键字——`… see https://…` 的 `https:` 冒号不再误伤。
/// - 关键字循环不再在首个词首非键值命中处 break（`--key-file /x
///   --key=SECRET` 的后者必须命中）。
///
/// §55 F-SP2-8 修订（§54 复审：dash 例外过宽、「冒号必须紧跟」破坏引号
/// 形态、空白只跳 ASCII 空格/tab——取最简单、宁枉勿纵的方案）：
/// - **dash 例外删除**：§54 的「命中所在 token 以 `-` 开头即跳过」只为保
///   `--monkey=banana`，却放过 `--apiKey=SECRET`、`-Dpassword=…`、
///   `-ePASSWORD=…`、`-PsigningKey=…`、diff 删除行 `-PGPASSWORD=`——键值
///   形态现在**任意位置**命中即脱敏；`--monkey=banana` 移入「可接受误伤」
///   列（测试按列区分）。
/// - 键值与 Authorization 的分隔符判定都跳过**任意 Unicode 空白和引号**
///   （`"`/`'`）——`{"headers":{"Authorization":"Basic SECRET"}}`、
///   `'authorization': 'Basic SECRET'`、`{"apiKey": "abc"}`（关键字与 `:`
///   之间隔着闭合引号的 JSON 形态）闭合；Authorization 的 scheme 值前也
///   跳过引号。
/// - 空白从「ASCII 空格/tab」回到 `char` 级：NBSP（U+00A0）、U+3000、`\r`
///   等都算键值分隔前的合法空白（`trim_start_matches` 的字符闭包保证偏移
///   算术仍在字节边界上）。
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
    // §55：关键字/Authorization 与分隔符（`=`/`:`）之间允许任意 Unicode
    // 空白**和引号**——JSON 键值（`{"apiKey": "abc"}`）、带引号的
    // Authorization 头都算命中。trim_start_matches 按字符跳过，偏移差
    //（字节）仍是字符边界。
    let skip_sep_ws = |c: char| c.is_whitespace() || c == '"' || c == '\'';
    for kw in KEYWORDS {
        // §53：遍历全部命中——词中命中、或词首但非键值（`--key-file`）都
        // 不得挡住同串后面的真键值形态（`--key-file /x --key=SECRET`）。
        // §55：dash 例外删除——键值命中不再要求词首，也不再看 flag 名。
        for (pos, _) in lower.match_indices(kw) {
            let after = &lower[pos + kw.len()..];
            let rest = after.trim_start_matches(skip_sep_ws);
            let ws = after.len() - rest.len();
            let is_kv = rest.starts_with('=') || rest.starts_with(':');
            if is_kv {
                // 值起点：跳过连续的分隔符/空白/引号（§55：引号也跳——
                // `{"token":"abc"}` 的值从 abc 起截）
                let value_start = rest
                    .find(|c: char| !skip_sep_ws(c) && c != '=' && c != ':')
                    .map(|off| pos + kw.len() + ws + off)
                    .unwrap_or(line.len());
                note(value_start);
                break;
            }
            if !is_word_start(pos) {
                // 词中且非键值形态——继续找后面的命中
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
    // 从冒号后直接整体替换。§53：遍历命中且冒号必须紧跟关键字；§55：
    // 关键字与冒号之间允许空白**和引号**（`"Authorization":"Basic …"`、
    // `'authorization': 'Basic …'`），scheme 值前也跳过引号。
    for (pos, _) in lower.match_indices("authorization") {
        if !is_word_start(pos) {
            continue;
        }
        let after_kw = &lower[pos + "authorization".len()..];
        let trimmed = after_kw.trim_start_matches(skip_sep_ws);
        if !trimmed.starts_with(':') {
            // 不是 Authorization 头（`authorization failed, see https://…`）——
            // 不得拿后面任意位置的冒号（`https:`）误伤
            continue;
        }
        let credentials = &trimmed[1..];
        if let Some(mut value_start) = credentials.find(|c: char| !skip_sep_ws(c)) {
            // 跳过一个 scheme 词（Basic / Bearer / token …）再取值；
            // scheme 词之后没有更多内容（单段值形态）则从值起点整体替换
            let scheme_and_rest = &credentials[value_start..];
            let scheme_len = scheme_and_rest
                .find(|c: char| c.is_whitespace())
                .unwrap_or(scheme_and_rest.len());
            let after_scheme = &scheme_and_rest[scheme_len..];
            if let Some(follow) = after_scheme.find(|c: char| !skip_sep_ws(c)) {
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
/// 且任何以 `-H` 开头的 arg（含 `-HAuthorization: …` 连写）都按敏感处理。
/// §55 P3：`--header` 精确匹配（独立 flag 或 `--header=…` 连写、保留 `=`），
/// 不再误伤 `--headers` / `--header-file`（见 [`header_flag_prefix`]）。
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
/// 值紧跟其后）。
/// §55 P3：`--header` 改**精确**匹配——只认独立 flag `--header` 与连写
/// `--header=…`（前缀含 `=`，输出保留 `=`）；`--headers` / `--header-file`
/// 不是 header flag（§54 的前缀匹配吞掉 flag 名，且 `--header-file /x`
/// 的值反而可见）。`-H` 仍按前缀连写（任何 `-H…` arg 都按敏感处理——
/// header 串可能带 `Authorization: Bearer …`，宁枉勿纵）。
fn header_flag_prefix(arg: &str) -> Option<usize> {
    let bytes = arg.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'-' && bytes[1] == b'H' {
        return Some(2);
    }
    if let Some(head) = arg.get(..8) {
        if head.eq_ignore_ascii_case("--header") {
            let rest = &arg[8..];
            if rest.is_empty() || rest.starts_with('=') {
                // 独立 flag 返回 8；`--header=…` 返回 9（含 `=`，输出保留）
                return Some(if rest.is_empty() { 8 } else { 9 });
            }
        }
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
    /// §55 F-SP2-8：`--monkey=banana` 移入「可接受误伤」列（dash 例外删除，
    /// 见 `sanitize_line_acceptable_collateral_column_is_redacted_by_design`）。
    #[test]
    fn sanitize_line_keeps_args_that_only_collide_mid_word() {
        for intact in [
            "--ask-for-approval",
            "--task-file /w/task-list.md",
            "/w/task-list.md",
            "https://desk-api.example.com/v1",
            "--no-bearer-check",
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

    /// §53 F-SP2-6 / §55 F-SP2-8 表驱动·泄漏列：§53 与 §55 列出的**每一个**
    /// 泄漏样例——§51 词首修复削弱脱敏造成的三组回归 + Authorization 首命中
    /// 与冒号错位 + 关键字循环 break + §51 原始样本（回归锁定）+ §55 的
    /// dash 例外泄漏（camelCase/-D/-P/-e/diff 删除行）、带引号的
    /// Authorization/JSON 键值、非 ASCII 空白分隔。脱敏规则已来回三轮
    /// （§51→§53→§55），本表锁定行为：输出必须不含机密片段、且带 `***`。
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
            // §55 F-SP2-8 回归 1：dash 例外过宽——flag 名（camelCase 复合 /
            // Java -D / gradle -P / docker -e / unified diff 删除行）内的
            // 键值形态必须照脱（§54 只为保 --monkey=banana，代价过高）。
            //（值取 S9/SECRET 等非前缀子串——`clientSecret` 自带大写 S，
            // 用单字符 S 断言会撞前缀。）
            ("--apiKey=SECRET", "SECRET"),
            ("--accessToken=S9", "S9"),
            ("--clientSecret=S9", "S9"),
            ("--dbPassword=S9", "S9"),
            ("mvn deploy -Dpassword=SECRET", "SECRET"),
            ("java -DapiKey=S -jar x.jar", "S"),
            ("docker run -ePASSWORD=SECRET img", "SECRET"),
            ("gradle -PsigningKey=S9", "S9"),
            // unified diff 的删除行（`+PGPASSWORD=` 本就会脱敏）
            ("-PGPASSWORD=hunter2", "hunter2"),
            // §55 F-SP2-8 回归 2：「冒号必须紧跟」破坏了带引号的形态——
            // 关键字/Authorization 与分隔符之间允许引号（JSON 键值缺口
            // `{"apiKey": "abc"}` 三版共有，一并闭合）
            (r#"{"headers":{"Authorization":"Basic SECRET"}}"#, "SECRET"),
            ("'authorization': 'Basic SECRET'", "SECRET"),
            (r#"{"apiKey": "abc"}"#, "abc"),
            (r#"{"token":"abc"}"#, "abc"),
            // §55 P3：分隔符前的空白只跳 ASCII 空格/tab 的回归——任意
            // Unicode 空白（NBSP / U+3000 / CR）都算键值
            ("password\u{a0}= S", "S"),
            ("token\u{3000}= S", "S"),
            ("password\r=S", "S"),
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
    /// §55 F-SP2-8：`--monkey=banana` 移入「可接受误伤」列（dash 例外删除）。
    /// 已知未覆盖（§55 记录，非回归，留脱敏专项）：`--bearer-token SECRET`
    /// 仍泄漏（独立 bearer 规则的副作用；`sanitize_line` 本就不覆盖空格
    /// 分隔的 `--flag value` 形态）——删除 dash 例外不影响它（非键值形态）。
    #[test]
    fn sanitize_line_intact_table_is_untouched() {
        const INTACT: &[&str] = &[
            "--ask-for-approval",
            "--task-file /w/task-list.md",
            "task-list.md",
            "https://desk-api.example.com/v1",
            "--no-bearer-check",
            // desk- / risk- 词中命中 sk-：无真密钥时不得截断
            "risk-assessment of desk- usage",
            // §53：Authorization 的冒号必须紧跟关键字（中间只许空白/引号）
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

    /// §55 F-SP2-8 表驱动·可接受误伤列：删除 dash 例外（宁枉勿纵）后，
    /// flag 名里的键值命中**按设计脱敏**——与保留列（必须原样）区分：
    /// 这里断言的是「确实被脱敏」，是 §55 明示接受的代价（换取上表全部
    /// dash 形态泄漏闭合）。
    #[test]
    fn sanitize_line_acceptable_collateral_column_is_redacted_by_design() {
        const COLLATERAL: &[(&str, &str)] = &[
            // §55 明示接受：`--monkey=banana → --monkey= ***`
            ("--monkey=banana", "banana"),
        ];
        for (input, value) in COLLATERAL {
            let out = sanitize_line(input);
            assert!(
                !out.contains(value),
                "§55 可接受误伤列：{input:?} 的输出 {out:?} 不得泄漏 {value:?}"
            );
            assert!(
                out.contains("***"),
                "§55 可接受误伤列：{input:?} 按设计脱敏，实际输出 {out:?}"
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
        assert_eq!(
            out[5], "--header=***",
            "--header= 连写保留 `=`，其余整体脱敏"
        );
        assert_eq!(out[6], "--auth");
        assert_eq!(out[7], "***", "--auth 的值是凭证");
    }

    /// §55 P3（sanitize_args）：header flag **精确**匹配——`--headers` /
    /// `--header-file` 不是 `--header`（§54 的前缀匹配吞掉 flag 名：
    /// `--header=…` 丢 `=`、`--headers a` / `--header-file /x` 被错误掩码
    /// 且后者的值反而可见）；`-H` 仍按前缀连写（大小写敏感，含 `-HL`
    /// 这类 `-H` 前缀族——宁枉勿纵，按设计并入）。
    #[test]
    fn sanitize_args_header_flag_matching_is_exact() {
        let out = sanitize_args(&[
            "--headers".to_string(),
            "a".to_string(),
            "--header-file".to_string(),
            "/x".to_string(),
            "--header=Authorization: Bearer abc".to_string(),
            "-HL".to_string(),
        ]);
        assert_eq!(out[0], "--headers", "--headers 不是 --header，不得误伤");
        assert_eq!(out[1], "a", "--headers 的值照旧可见");
        assert_eq!(
            out[2], "--header-file",
            "--header-file 不是 --header，不得误伤"
        );
        assert_eq!(out[3], "/x", "--header-file 的值照旧可见（不是 header 串）");
        assert_eq!(out[4], "--header=***", "--header= 连写保留 `=`");
        assert_eq!(out[5], "-H***", "-H 前缀（大小写敏感）仍连写——按设计并入");
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
