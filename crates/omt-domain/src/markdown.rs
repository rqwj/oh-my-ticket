//! Node Markdown codec — Rust mirror of `src/host/markdown.ts`.
//!
//! Layout: YAML frontmatter (metadata mirror) + free-form user body + a
//! plugin-managed children block delimited by HTML comment markers. The
//! managed block regenerates from the edge list whenever relations change;
//! edits outside the markers stay authoritative user content.
//!
//! # Byte-stability contract (plan U4b, R23)
//!
//! Serialization mirrors `js-yaml` 4.3.1 `dump(attrs, {lineWidth: -1,
//! noRefs: true})` **exactly**, including its style-choice quirks:
//!
//! * empty string → `''`;
//! * YAML-1.1-deprecated booleans (`y Y yes Yes YES n N no No NO on On ON off
//!   Off OFF`) and base-60 shapes (`12:34`) are force-single-quoted
//!   (`noCompatMode` defaults to false);
//! * any non-printable code point per js-yaml `isPrintable` (excludes TAB,
//!   CR, DEL, C1 controls, U+0085, U+00A0, U+2028/9, BOM; LF is handled by
//!   block styles) switches to DOUBLE-quoted with js-yaml's escape table;
//! * values containing LF otherwise become LITERAL blocks (`|`, `|-`, `|+`,
//!   with a `2` indentation indicator when the first content line is
//!   indented), indented two spaces per line;
//! * plain style only when every code point passes js-yaml's simplified
//!   `ns-plain-char` walk (leading `- ? : , [ ] { } # & * ! | = > ' " % @ \``
//!   ALL refuse plain — including `-`/`?`/`:` even before non-space) and the
//!   text does not implicitly resolve as null/bool/int/float/timestamp/merge
//!   (`<<`) under DEFAULT_SCHEMA;
//! * SINGLE-quoted (`''` escaping) as the fallback for single-line strings;
//! * keys stay in insertion order (= `frontmatterOf` order in core.ts).
//!
//! The golden corpus (`corpus/yaml-goldens/`) pins the exact bytes produced
//! by the TypeScript serializer across adversarial fixtures; the differential
//! suite re-checks with randomized scalars. Conventions live in
//! `corpus/yaml-goldens/CONVENTIONS.md`.

use super::error::{self, Problem, Result};
use serde_json::{Map, Value};

/// Begin/end markers of the managed children block.
pub const CHILDREN_BEGIN: &str = "<!-- omt:children -->";
pub const CHILDREN_END: &str = "<!-- /omt:children -->";

/// Frontmatter attributes persisted inside the node Markdown file. Absent
/// keys stay `None`; serialization order follows `frontmatterOf` in core.ts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrontmatterAttrs {
    pub id: Option<String>,
    pub node_type: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub archived: Option<bool>,
    pub priority: Option<f64>,
    pub parent: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Split a node file into frontmatter attributes and body text.
/// Mirrors `/^---\n([\s\S]*?)\n---\n?/` + `yaml.load` with unknown-field
/// tolerance: unrecognized keys are preserved in [`Self::extra`].
#[derive(Debug, Clone, Default)]
pub struct ParsedNodeFile {
    pub attrs: FrontmatterAttrs,
    pub extra: Map<String, Value>,
    pub body: String,
}

/// `/^---\n([\s\S]*?)\n---\n?/` — returns `(inner_yaml, body_after_match)`.
///
/// Safe against literal-block frontmatter values: block content is always
/// indented, so a content line can never start at column 0 and collide with
/// the closing `---` fence search.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---\n")?;
    if let Some(rel) = rest.find("\n---") {
        let after = rel + 4; // past "---"
        let body_start = if rest.as_bytes().get(after) == Some(&b'\n') {
            after + 1
        } else {
            after
        };
        return Some((&rest[..rel], &rest[body_start..]));
    }
    None
}

/// Parse one node file. Invalid frontmatter YAML is an INVALID_INPUT
/// problem (matching `parseNodeFile`).
pub fn parse_node_file(content: &str) -> Result<ParsedNodeFile> {
    let Some((yaml_text, body)) = split_frontmatter(content) else {
        return Ok(ParsedNodeFile::default_with_body(content.to_string()));
    };
    let map = parse_flat_yaml(yaml_text)?;
    Ok(ParsedNodeFile {
        attrs: attrs_from_map(&map),
        extra: map,
        body: body.to_string(),
    })
}

impl ParsedNodeFile {
    fn default_with_body(body: String) -> Self {
        ParsedNodeFile {
            attrs: FrontmatterAttrs::default(),
            extra: Map::new(),
            body,
        }
    }
}

fn attrs_from_map(map: &Map<String, Value>) -> FrontmatterAttrs {
    let get_str = |k: &str| map.get(k).and_then(Value::as_str).map(str::to_string);
    FrontmatterAttrs {
        id: get_str("id"),
        node_type: get_str("type"),
        title: get_str("title"),
        status: get_str("status"),
        archived: map.get("archived").and_then(Value::as_bool),
        priority: map.get("priority").and_then(Value::as_f64),
        parent: get_str("parent"),
        created_at: get_str("created_at"),
        updated_at: get_str("updated_at"),
    }
}

// ── flat-YAML reader (covers everything the writer above emits) ─────────

/// Flat-YAML reader: `key: value` lines plus the block-scalar forms the
/// byte-stability writer emits (`key: |`, `|-`, `|+`, `|2`, `>…`). Quoted
/// scalars (single/double) and plain scalars are supported; comments and
/// blank lines between entries are skipped.
fn parse_flat_yaml(text: &str) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut index = 0usize;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            index += 1;
            continue;
        }
        let Some(colon) = find_key_colon(trimmed) else {
            return Err(Problem::new(
                error::INVALID_INPUT,
                format!("invalid frontmatter YAML: unsupported line: {trimmed:?}"),
            ));
        };
        let key = trimmed[..colon].trim().to_string();
        let value_text = trimmed[colon + 1..].trim();
        if is_block_header(value_text) {
            let (value, consumed) = read_block_scalar(&lines[index + 1..], value_text);
            out.insert(key, Value::String(value));
            index += 1 + consumed;
        } else {
            out.insert(key, parse_scalar(value_text));
            index += 1;
        }
    }
    Ok(out)
}

/// First unquoted `:` that separates key from value (a key never carries a
/// colon in practice; a quoted value's colons live past the separator).
fn find_key_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b':' {
            return Some(index);
        }
        if *byte == b'\'' || *byte == b'"' {
            break; // key starting with a quote is unsupported
        }
    }
    None
}

/// `|`, `|-`, `|+`, `|2`, `|2-`, `|2+` — the literal-block headers the
/// byte-stability writer emits. Folded (`>`) headers are deliberately NOT
/// routed here (never emitted with lineWidth -1; hand-written folded values
/// fall back to plain-scalar parsing).
fn is_block_header(text: &str) -> bool {
    let mut chars = text.chars();
    if chars.next() != Some('|') {
        return false;
    }
    let mut seen_indent = false;
    for c in chars {
        match c {
            '2' if !seen_indent => seen_indent = true,
            '-' | '+' => {}
            _ => return false,
        }
    }
    true
}

/// Read a literal block whose candidate lines follow the header.
/// Returns the decoded string and how many extra lines were consumed.
///
/// Chomping reconstruction mirrors what the dumper writes: trailing blank
/// content lines appear as blank physical lines before the next key, so a
/// `+` header restores `count(trailing blanks) + 1` newlines, clip restores
/// exactly one, strip restores none.
fn read_block_scalar(candidate_lines: &[&str], header: &str) -> (String, usize) {
    // Block indent = indentation of the first non-empty candidate line; a
    // first non-empty line at column 0 means the block is empty.
    let mut indent = usize::MAX;
    for line in candidate_lines {
        if line.trim().is_empty() {
            continue;
        }
        let lead = line.len() - line.trim_start_matches(' ').len();
        if lead > 0 {
            indent = lead;
        }
        break;
    }
    if indent == usize::MAX {
        return (String::new(), 0);
    }
    let mut content_lines: Vec<String> = Vec::new();
    let mut consumed = 0usize;
    for line in candidate_lines {
        let is_blank = line.trim().is_empty();
        let line_indent = line.len() - line.trim_start_matches(' ').len();
        if !is_blank && line_indent < indent {
            break;
        }
        content_lines.push(if is_blank || line.len() < indent {
            String::new()
        } else {
            line[indent..].to_string()
        });
        consumed += 1;
    }
    let trailing_blanks = content_lines
        .iter()
        .rev()
        .take_while(|line| line.is_empty())
        .count();
    let content_end = content_lines.len() - trailing_blanks;
    let joined = content_lines[..content_end].join("\n");
    let keep = header.contains('+');
    let strip = header.contains('-');
    let value = if keep {
        let mut value = joined;
        for _ in 0..=trailing_blanks {
            value.push('\n');
        }
        value
    } else if strip {
        joined
    } else {
        format!("{joined}\n")
    };
    (value, consumed)
}

fn parse_scalar(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    if text.starts_with('\'') {
        let inner = text.trim_matches(|c| c == '\'');
        return Value::String(inner.replace("''", "'"));
    }
    if text.starts_with('"') {
        let inner = text.trim_matches(|c| c == '"');
        return Value::String(unescape_double(inner));
    }
    // Mirror the loader-side implicit types loosely: the writer guarantees
    // ambiguous scalars are quoted, so plain values that LOOK typed are the
    // hand-edited minority; resolve them the way js-yaml's DEFAULT_SCHEMA
    // would (subset relevant to frontmatter).
    if matches!(text, "~" | "null" | "Null" | "NULL" | "<<") {
        return Value::Null;
    }
    match text {
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if yaml_int_resolves(text) || yaml_float_resolves(text) {
        if let Ok(int_value) = text.parse::<i64>() {
            return Value::Number(int_value.into());
        }
        if let Ok(float_value) = text.parse::<f64>() {
            if let Some(number) = serde_json::Number::from_f64(float_value) {
                return Value::Number(number);
            }
        }
    }
    if yaml_timestamp_resolves(text) {
        // Timestamps stay strings in the mirror (created_at/updated_at).
        return Value::String(text.to_string());
    }
    Value::String(text.to_string())
}

/// Full YAML double-quoted escape decoding (`\n`, `\xHH`, `\uHHHH`,
/// `\UHHHHHHHH`, `\N`, `\_`, `\L`, `\P`, …) for reader robustness.
fn unescape_double(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('a') => out.push('\u{7}'),
            Some('b') => out.push('\u{8}'),
            Some('v') => out.push('\u{b}'),
            Some('f') => out.push('\u{c}'),
            Some('e') => out.push('\u{1b}'),
            Some('N') => out.push('\u{85}'),
            Some('_') => out.push('\u{a0}'),
            Some('L') => out.push('\u{2028}'),
            Some('P') => out.push('\u{2029}'),
            Some('x') => push_hex_escape(&mut chars, &mut out, 2),
            Some('u') => push_hex_escape(&mut chars, &mut out, 4),
            Some('U') => push_hex_escape(&mut chars, &mut out, 8),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn push_hex_escape(chars: &mut std::str::Chars<'_>, out: &mut String, width: usize) {
    let mut value: u32 = 0;
    for _ in 0..width {
        match chars.next().and_then(|c| c.to_digit(16)) {
            Some(digit) => value = value * 16 + digit,
            None => return,
        }
    }
    if let Some(decoded) = char::from_u32(value) {
        out.push(decoded);
    } else {
        out.push('\u{fffd}');
    }
}

// ── serializer: exact js-yaml dump conventions ──────────────────────────

/// Serialize frontmatter attributes + body into a full node file.
/// Mirrors `serializeNodeFile`: trimmed YAML dump between `---` fences, then
/// leading newlines stripped from the body.
pub fn serialize_node_file(attrs_lines: Vec<(&str, Value)>, body: &str) -> String {
    let mut dump = String::new();
    for (key, value) in attrs_lines {
        dump.push_str(key);
        dump.push_str(": ");
        dump.push_str(&dump_scalar(value));
        dump.push('\n');
    }
    // JS `.trimEnd()` over the JS whitespace class (includes U+FEFF).
    let trimmed = dump.trim_end_matches(js_is_space);
    let stripped_body = body.trim_start_matches('\n');
    format!("---\n{trimmed}\n---\n\n{stripped_body}")
}

/// Dump one scalar following js-yaml conventions byte-exactly.
pub fn dump_scalar(value: Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => js_number_repr(&number),
        Value::String(text) => write_scalar_string(&text, false),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(&value).unwrap_or_else(|_| "''".to_string())
        }
    }
}

/// JS `Number.prototype.toString(10)` for the JSON-number surface the
/// frontmatter carries: integers print bare (17, never 17.0); negative zero
/// prints `0`; non-integral magnitudes print shortest-roundtrip (identical
/// between V8 and Ryu for realistic priorities).
fn js_number_repr(number: &serde_json::Number) -> String {
    if let Some(int_value) = number.as_i64() {
        return int_value.to_string();
    }
    if let Some(uint_value) = number.as_u64() {
        return uint_value.to_string();
    }
    if let Some(float_value) = number.as_f64() {
        if float_value == 0.0 {
            return "0".to_string(); // covers -0.0 like JS
        }
        if float_value.fract() == 0.0 && float_value.abs() < 9_007_199_254_740_992.0 {
            return format!("{}", float_value as i64);
        }
        return number.to_string();
    }
    number.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Style {
    Plain,
    Single,
    Literal,
    Double,
}

/// js-yaml `writeScalar` for our surface: quotingType SINGLE, noCompatMode
/// false, forceQuotes false, noRefs true, lineWidth -1, level 1 (so block
/// indent = 2), inblock = true.
fn write_scalar_string(text: &str, is_key: bool) -> String {
    if text.is_empty() {
        return "''".to_string();
    }
    // Compat mode: deprecated booleans and base-60 shapes force quotes.
    if deprecated_booleans_contains(text) || deprecated_base60_matches(text) {
        return format!("'{}'", text.replace('\'', "''"));
    }
    match choose_scalar_style(text, is_key) {
        Style::Plain => text.to_string(),
        Style::Single => format!("'{}'", text.replace('\'', "''")),
        Style::Literal => format!(
            "|{}{}",
            block_header(text),
            drop_ending_newline(&indent_string(text, 2))
        ),
        Style::Double => format!("\"{}\"", js_escape_string(text)),
    }
}

/// js-yaml `chooseScalarStyle` with lineWidth = -1 (never folds), indent
/// per level 2, forceQuotes false.
fn choose_scalar_style(text: &str, single_line_only: bool) -> Style {
    let cps: Vec<char> = text.chars().collect();
    let mut plain = plain_safe_first(cps[0]) && plain_safe_last(cps[cps.len() - 1]);
    let mut has_line_break = false;
    let mut prev: Option<char> = None;

    if single_line_only {
        for &c in &cps {
            if !js_is_printable(c) {
                return Style::Double;
            }
            plain = plain && plain_safe(c, prev);
            prev = Some(c);
        }
    } else {
        for &c in &cps {
            if c == '\n' {
                has_line_break = true;
            } else if !js_is_printable(c) {
                return Style::Double;
            }
            plain = plain && plain_safe(c, prev);
            prev = Some(c);
        }
    }

    if !has_line_break {
        if plain && !test_implicit_resolving(text) {
            return Style::Plain;
        }
        // quotingType SINGLE.
        return Style::Single;
    }
    // Block styles permitted (indentPerLevel 2 ≤ 9; forceQuotes false).
    Style::Literal
}

/// js-yaml `isPrintable` (code points writable without escaping; TAB, CR,
/// DEL, C1 controls, U+0085, U+00A0, U+2028/9 and BOM are excluded; LF is
/// handled by the caller's line-break branch).
fn js_is_printable(c: char) -> bool {
    let cp = c as u32;
    (0x20..=0x7E).contains(&cp)
        || ((0xA1..=0xD7FF).contains(&cp) && cp != 0x2028 && cp != 0x2029)
        || ((0xE000..=0xFFFD).contains(&cp) && cp != 0xFEFF)
        || (0x10000..=0x10FFFF).contains(&cp)
}

/// js-yaml `isWhitespace`: s-white = space | tab ONLY.
fn js_is_s_white(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Simplified first-char test for plain style: every c-indicator refuses,
/// INCLUDING `-`, `?` and `:` even when followed by non-space, plus `=`.
fn plain_safe_first(c: char) -> bool {
    js_is_printable(c)
        && !js_is_s_white(c)
        && !matches!(
            c,
            '-' | '?'
                | ':'
                | ','
                | '['
                | ']'
                | '{'
                | '}'
                | '#'
                | '&'
                | '*'
                | '!'
                | '|'
                | '='
                | '>'
                | '\''
                | '"'
                | '%'
                | '@'
                | '`'
        )
}

/// Last-char test: not s-white, not a colon.
fn plain_safe_last(c: char) -> bool {
    !js_is_s_white(c) && c != ':'
}

/// js-yaml `isPlainSafe(c, prev, inblock=true)`.
fn plain_safe(c: char, prev: Option<char>) -> bool {
    let c_ns_or_ws = js_is_printable(c) && c != '\r' && c != '\n';
    let c_is_ns = c_ns_or_ws && !js_is_s_white(c);
    if c_ns_or_ws && c != '#' && !(prev == Some(':') && !c_is_ns) {
        return true;
    }
    let Some(prev) = prev else { return false };
    let prev_is_ns = js_is_printable(prev) && prev != '\r' && prev != '\n' && !js_is_s_white(prev);
    (prev_is_ns && c == '#') || (prev == ':' && c_is_ns)
}

/// `testImplicitResolving` over DEFAULT_SCHEMA implicit types:
/// null, bool, merge (`<<`), int, float, timestamp.
fn test_implicit_resolving(text: &str) -> bool {
    yaml_null_resolves(text)
        || yaml_bool_resolves(text)
        || text == "<<"
        || yaml_int_resolves(text)
        || yaml_float_resolves(text)
        || yaml_timestamp_resolves(text)
}

fn yaml_null_resolves(text: &str) -> bool {
    matches!(text, "~" | "null" | "Null" | "NULL")
}

fn yaml_bool_resolves(text: &str) -> bool {
    matches!(text, "true" | "True" | "TRUE" | "false" | "False" | "FALSE")
}

const DEPRECATED_BOOLEANS: [&str; 16] = [
    "y", "Y", "yes", "Yes", "YES", "on", "On", "ON", "n", "N", "no", "No", "NO", "off", "Off",
    "OFF",
];

fn deprecated_booleans_contains(text: &str) -> bool {
    DEPRECATED_BOOLEANS.contains(&text)
}

/// `/^[-+]?[0-9_]+(?::[0-9_]+)+(?:\.[0-9_]*)?$/`
fn deprecated_base60_matches(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    if bytes.first() == Some(&b'-') || bytes.first() == Some(&b'+') {
        i += 1;
    }
    let digits = |b: u8| b.is_ascii_digit() || b == b'_';
    let mut saw_digits = false;
    while i < bytes.len() && digits(bytes[i]) {
        saw_digits = true;
        i += 1;
    }
    if !saw_digits {
        return false;
    }
    let mut groups = 0usize;
    while i < bytes.len() && bytes[i] == b':' {
        i += 1;
        let mut group_digits = false;
        while i < bytes.len() && digits(bytes[i]) {
            group_digits = true;
            i += 1;
        }
        if !group_digits {
            return false;
        }
        groups += 1;
    }
    if groups == 0 {
        return false;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && digits(bytes[i]) {
            i += 1;
        }
    }
    i == bytes.len()
}

/// Exact port of js-yaml `resolveYamlInteger` (binary/octal/hex/decimal,
/// optional sign, `0` alone, finiteness of the parsed value).
fn yaml_int_resolves(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut index = 0usize;
    if matches!(bytes[0], b'-' | b'+') {
        index += 1;
    }
    if bytes.get(index) == Some(&b'0') && index + 1 < bytes.len() {
        index += 1;
        let prefix = bytes[index];
        if matches!(prefix, b'b' | b'x' | b'o') {
            index += 1;
            let mut has_digits = false;
            for &b in &bytes[index..] {
                let ok = match prefix {
                    b'b' => b == b'0' || b == b'1',
                    b'x' => b.is_ascii_digit() || matches!(b, b'a'..=b'f' | b'A'..=b'F'),
                    _ => (b'0'..=b'7').contains(&b),
                };
                if !ok {
                    return false;
                }
                has_digits = true;
            }
            if !has_digits {
                return false;
            }
            // isFinite(parseInt(data, radix)): accumulate in f64; only the
            // finiteness threshold matters (JS parseInt converts the full
            // digit string to a double).
            let radix: f64 = match prefix {
                b'b' => 2.0,
                b'x' => 16.0,
                _ => 8.0,
            };
            let mut value = 0.0f64;
            for &b in &bytes[index..] {
                let digit = match prefix {
                    b'b' | b'o' => (b - b'0') as f64,
                    _ => (b as char).to_digit(16).unwrap_or(0) as f64,
                };
                value = value * radix + digit;
            }
            let signed = if bytes[0] == b'-' { -value } else { value };
            return signed.is_finite();
        }
        // Not a radix prefix: fall through to the decimal walk from `index`.
    }
    // Decimal walk.
    let mut has_digits = false;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            return false;
        }
        has_digits = true;
        index += 1;
    }
    if !has_digits {
        return false;
    }
    // isFinite(parseInt(decimal)): the f64 conversion must stay finite
    // (a >309-digit literal overflows to Infinity and resolves as a STRING).
    let digits_start = usize::from(matches!(bytes[0], b'-' | b'+'));
    let magnitude: f64 = text[digits_start..].parse().unwrap_or(f64::INFINITY);
    if bytes[0] == b'-' {
        (-magnitude).is_finite()
    } else {
        magnitude.is_finite()
    }
}

/// Exact port of js-yaml `resolveYamlFloat`.
fn yaml_float_resolves(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut i = 0usize;
    if matches!(bytes[0], b'-' | b'+') {
        i += 1;
    }
    // Alternatives: int-part [. frac-part] [exp] | . frac-part [exp] |
    // [-+].(inf|Inf|INF) | .(nan|NaN|NAN)
    let special_inf = |start: usize| -> Option<usize> {
        if bytes.get(start) != Some(&b'.') {
            return None;
        }
        for form in [&b"inf"[..], &b"Inf"[..], &b"INF"[..]] {
            if bytes[start + 1..].starts_with(form) {
                return Some(start + 1 + form.len());
            }
        }
        None
    };
    let special_nan = |start: usize| -> Option<usize> {
        if bytes.get(start) != Some(&b'.') {
            return None;
        }
        for form in [&b"nan"[..], &b"NaN"[..], &b"NAN"[..]] {
            if bytes[start + 1..].starts_with(form) {
                return Some(start + 1 + form.len());
            }
        }
        None
    };
    if let Some(end) = special_inf(i) {
        return end == bytes.len();
    }
    if let Some(end) = special_nan(i) {
        return end == bytes.len();
    }
    // Numeric forms.
    let mut j = i;
    let mut int_digits = 0usize;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
        int_digits += 1;
    }
    if int_digits == 0 {
        if bytes.get(j) != Some(&b'.') {
            return false;
        }
        j += 1;
        let mut frac = 0usize;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
            frac += 1;
        }
        if frac == 0 {
            return false;
        }
    } else if j < bytes.len() && bytes[j] == b'.' {
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
    }
    if j < bytes.len() && matches!(bytes[j], b'e' | b'E') {
        j += 1;
        if j < bytes.len() && matches!(bytes[j], b'-' | b'+') {
            j += 1;
        }
        let mut exp = 0usize;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
            exp += 1;
        }
        if exp == 0 {
            return false;
        }
    }
    if j != bytes.len() {
        return false;
    }
    // isFinite(parseFloat(data)): gated candidates parse cleanly in Rust.
    text.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
}

/// Exact port of js-yaml's YAML_DATE_REGEXP / YAML_TIMESTAMP_REGEXP:
/// `^\d{4}-\d{2}-\d{2}$` or
/// `^\d{4}-\d{1,2}-\d{1,2}([Tt]|[ \t]+)\d{1,2}:\d{2}:\d{2}(\.\d*)?([ \t]*(Z|[-+]\d{1,2}(:\d{2})?))?$`.
fn yaml_timestamp_resolves(text: &str) -> bool {
    let b = text.as_bytes();
    let digits_at = |start: usize, count: usize| -> bool {
        start + count <= b.len() && b[start..start + count].iter().all(u8::is_ascii_digit)
    };
    // Date-only form.
    if b.len() == 10
        && digits_at(0, 4)
        && b[4] == b'-'
        && digits_at(5, 2)
        && b[7] == b'-'
        && digits_at(8, 2)
    {
        return true;
    }
    if b.len() < 10 || !digits_at(0, 4) || b.get(4) != Some(&b'-') {
        return false;
    }
    let mut i = 5usize;
    // Month: 1–2 digits.
    let month_start = i;
    if !b.get(i).map_or(false, u8::is_ascii_digit) {
        return false;
    }
    i += 1;
    if i - month_start < 2 && b.get(i).map_or(false, u8::is_ascii_digit) {
        i += 1;
    }
    if b.get(i) != Some(&b'-') {
        return false;
    }
    i += 1;
    // Day: 1–2 digits.
    let day_start = i;
    if !b.get(i).map_or(false, u8::is_ascii_digit) {
        return false;
    }
    i += 1;
    if i - day_start < 2 && b.get(i).map_or(false, u8::is_ascii_digit) {
        i += 1;
    }
    // Separator: [Tt] or one-or-more space/tab.
    match b.get(i) {
        Some(b'T' | b't') => i += 1,
        Some(b' ' | b'\t') => {
            while matches!(b.get(i), Some(b' ' | b'\t')) {
                i += 1;
            }
        }
        _ => return false,
    }
    // Hour: 1–2 digits.
    let hour_start = i;
    if !b.get(i).map_or(false, u8::is_ascii_digit) {
        return false;
    }
    i += 1;
    if i - hour_start < 2 && b.get(i).map_or(false, u8::is_ascii_digit) {
        i += 1;
    }
    // Minute and second: exactly two digits each, colon-separated.
    if b.get(i) != Some(&b':') || !digits_at(i + 1, 2) {
        return false;
    }
    i += 3;
    if b.get(i) != Some(&b':') || !digits_at(i + 1, 2) {
        return false;
    }
    i += 3;
    // Optional fraction: `.` then any run of digits.
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).map_or(false, u8::is_ascii_digit) {
            i += 1;
        }
    }
    // Optional timezone: [ \t]* ( Z | [-+] \d{1,2} (: \d{2})? )
    while matches!(b.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    match b.get(i) {
        Some(b'Z') => i += 1,
        Some(b'-' | b'+') => {
            i += 1;
            let tz_start = i;
            if !b.get(i).map_or(false, u8::is_ascii_digit) {
                return false;
            }
            i += 1;
            if i - tz_start < 2 && b.get(i).map_or(false, u8::is_ascii_digit) {
                i += 1;
            }
            if b.get(i) == Some(&b':') {
                i += 1;
                if !digits_at(i, 2) {
                    return false;
                }
                i += 2;
            }
        }
        _ => {}
    }
    i == b.len()
}

/// js-yaml `ESCAPE_SEQUENCES` + `encodeHex` for the DOUBLE style.
fn js_escape_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let cp = c as u32;
        if let Some(seq) = escape_sequence(cp) {
            out.push_str(seq);
        } else if js_is_printable(c) {
            out.push(c);
        } else {
            out.push_str(&encode_hex(cp));
        }
    }
    out
}

fn escape_sequence(cp: u32) -> Option<&'static str> {
    Some(match cp {
        0x00 => "\\0",
        0x07 => "\\a",
        0x08 => "\\b",
        0x09 => "\\t",
        0x0A => "\\n",
        0x0B => "\\v",
        0x0C => "\\f",
        0x0D => "\\r",
        0x1B => "\\e",
        0x22 => "\\\"",
        0x5C => "\\\\",
        0x85 => "\\N",
        0xA0 => "\\_",
        0x2028 => "\\L",
        0x2029 => "\\P",
        _ => return None,
    })
}

fn encode_hex(cp: u32) -> String {
    let digits = format!("{cp:X}");
    if cp <= 0xFF {
        format!("\\x{:0>2}", digits)
    } else if cp <= 0xFFFF {
        format!("\\u{:0>4}", digits)
    } else {
        format!("\\U{:0>8}", digits)
    }
}

/// js-yaml `indentString`: indent every non-empty line (empty lines stay
/// bare); lines are measured WITH their trailing newline (`"\n"` alone is
/// not indented).
fn indent_string(text: &str, spaces: usize) -> String {
    let ind = " ".repeat(spaces);
    let mut out = String::with_capacity(text.len() + ind.len());
    let mut rest = text;
    while let Some(pos) = rest.find('\n') {
        let line = &rest[..pos + 1];
        if !line.is_empty() && line != "\n" {
            out.push_str(&ind);
        }
        out.push_str(line);
        rest = &rest[pos + 1..];
    }
    if !rest.is_empty() {
        out.push_str(&ind);
        out.push_str(rest);
    }
    out
}

/// `/^\n* /` — a block-indentation indicator is required when the first
/// content line (after any leading newlines) starts with a space.
fn need_indent_indicator(text: &str) -> bool {
    text.trim_start_matches('\n').starts_with(' ')
}

/// js-yaml `blockHeader`: `indentIndicator + chomp + "\n"`.
fn block_header(text: &str) -> String {
    let indicator = if need_indent_indicator(text) { "2" } else { "" };
    let clip = text.ends_with('\n');
    let keep = clip && (text.ends_with("\n\n") || text == "\n");
    let chomp = if keep {
        "+"
    } else if clip {
        ""
    } else {
        "-"
    };
    format!("{indicator}{chomp}\n")
}

fn drop_ending_newline(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

// ── managed children block ──────────────────────────────────────────────

/// Render the managed children block: a `## 子节点` list of relative links,
/// ordered by edge ord; links stay parent-relative so subtree moves keep
/// them valid. Mirrors `renderChildrenBlock(children, childDirName)`.
pub fn render_children_entries(children: &[ChildEntry]) -> String {
    let list = if children.is_empty() {
        "（暂无子节点）".to_string()
    } else {
        children
            .iter()
            .map(|child| {
                format!(
                    "- [{} {}]({}/{}.md) — {}",
                    child.id, child.title, child.dir_name, child.node_type, child.status
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!("{CHILDREN_BEGIN}\n## 子节点\n\n{list}\n{CHILDREN_END}")
}

/// One rendered child link row.
#[derive(Debug, Clone)]
pub struct ChildEntry {
    pub id: String,
    pub title: String,
    pub dir_name: String,
    pub node_type: String,
    pub status: String,
}

/// Replace the managed children block inside a body, or append it when the
/// markers are absent.
pub fn replace_children_block(body: &str, block: &str) -> String {
    let begin = body.find(CHILDREN_BEGIN);
    let end = body.find(CHILDREN_END);
    if let (Some(begin), Some(end)) = (begin, end) {
        if begin < end {
            return format!(
                "{}{}{}",
                &body[..begin],
                block,
                &body[end + CHILDREN_END.len()..]
            );
        }
    }
    // `.replace(/\s+$/,'')` — the JS \s class, NOT Rust White_Space
    // (they disagree on U+FEFF and U+0085; differential case diff-00696).
    let trimmed = body.trim_end_matches(js_is_space);
    if trimmed.is_empty() {
        format!("{block}\n")
    } else {
        format!("{trimmed}\n\n{block}\n")
    }
}

/// Strip the managed children block, returning the user-owned body only.
pub fn strip_children_block(body: &str) -> String {
    let begin = body.find(CHILDREN_BEGIN);
    let end = body.find(CHILDREN_END);
    if let (Some(begin), Some(end)) = (begin, end) {
        if begin < end {
            let joined = format!("{}{}", &body[..begin], &body[end + CHILDREN_END.len()..]);
            return collapse_blank_runs(&joined);
        }
    }
    body.trim_matches(js_is_space).to_string()
}

/// `.replace(/\n{3,}/g, '\n\n').trim()` — collapse 3+ newlines, then trim.
fn collapse_blank_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newline_run = 0usize;
    for c in text.chars() {
        if c == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push(c);
            }
        } else {
            newline_run = 0;
            out.push(c);
        }
    }
    out.trim_matches(js_is_space).to_string()
}

// ── layout helpers (files.ts) ───────────────────────────────────────────

/// JavaScript's whitespace class (`\s` in regexes AND `String#trim`):
/// [\t\n\v\f\r \u{a0} \u{1680} \u{2000}-\u{200a} \u{2028} \u{2029}
/// \u{202f} \u{205f} \u{3000} \u{feff}] — deliberately NOT Rust's
/// `char::is_whitespace` (which adds U+0085 and drops U+FEFF).
fn js_is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// Greatest index `<= i` that is a `char` boundary of `s` (i > len → len).
/// Byte-budget truncation of arbitrary text MUST cut here, never on a raw
/// byte offset: a mid-char slice panics (TICKET-0130, CJK claim context).
pub fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

/// Filesystem-safe slug; keeps CJK, strips path-hostile characters.
/// Byte-parity with `files.ts` `slugify`: `.slice(0, 40)` truncates by
/// **UTF-16 code units**; a cut straddling a surrogate pair leaves a lone
/// surrogate that Node's UTF-8 writer replaces with U+FFFD, reproduced here
/// with `from_utf16_lossy` (orchestrator decision 3, proven by the
/// `children-astral-slug-parity` golden).
pub fn slugify(title: &str) -> String {
    let trimmed = title.trim_matches(js_is_space);
    let collapsed = collapse_whitespace(trimmed);
    let stripped: String = collapsed
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#'
            )
        })
        .collect();
    let collapsed_dashes = collapse_dashes(&stripped);
    let dash_trimmed = trimmed_dashes(&collapsed_dashes);
    let units: Vec<u16> = dash_trimmed.encode_utf16().take(40).collect();
    let sliced = String::from_utf16_lossy(&units);
    if sliced.is_empty() {
        "untitled".to_string()
    } else {
        sliced
    }
}

/// `.replace(/^-|-$/g, '')` — removes one leading and one trailing dash
/// (after `-{2,}` collapsing there can be at most one of each).
fn trimmed_dashes(text: &str) -> &str {
    let without_lead = text.strip_prefix('-').unwrap_or(text);
    without_lead.strip_suffix('-').unwrap_or(without_lead)
}

/// `/\s+/g → '-'` over the JS whitespace class.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if js_is_space(c) {
            if !in_space {
                out.push('-');
                in_space = true;
            }
        } else {
            in_space = false;
            out.push(c);
        }
    }
    out
}

fn collapse_dashes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_dash = false;
    for c in text.chars() {
        if c == '-' {
            if !last_dash {
                out.push(c);
            }
            last_dash = true;
        } else {
            last_dash = false;
            out.push(c);
        }
    }
    out
}

/// Directory name for a node: `<ID>-<slug>` (stable once created).
pub fn node_dir_name(id: &str, title: &str) -> String {
    format!("{id}-{}", slugify(title))
}

/// Relative markdown path for a new node under its parent; root nodes land
/// directly under `tickets/`. Always forward slashes (home-relative POSIX
/// paths, matching the stored `path` column).
pub fn path_for(node_type: &str, id: &str, title: &str, parent_path: Option<&str>) -> String {
    let dir = node_dir_name(id, title);
    let base = match parent_path {
        Some(parent_path) => dirname(parent_path),
        None => "tickets".to_string(),
    };
    format!("{base}/{dir}/{node_type}.md")
}

/// Final path segment before the last `/` ("" when none).
pub fn dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[..index].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::floor_char_boundary;

    /// The U7a production crash shape: a byte budget cut inside a 3-byte
    /// CJK char. Raw `s[..i]` panics; the helper must return the previous
    /// boundary and never exceed `i`.
    #[test]
    fn floors_mid_char_cuts_for_every_residue() {
        let s = "用ab用体系x"; // boundaries: 0,3,4,5,8,9,12,13
        for i in 0..=s.len() {
            let floored = floor_char_boundary(s, i);
            assert!(s.is_char_boundary(floored), "i={i} -> {floored}");
            assert!(floored <= i);
            // Round-trip: slicing at the floored index is valid UTF-8.
            let _ = &s[..floored];
        }
    }

    #[test]
    fn pure_multibyte_body_matches_ticket_0130_geometry() {
        // "用"*6000 (18_000 B): every non-multiple-of-3 offset panicked
        // before the fix.
        let s = "用".repeat(6000);
        for take in [16_381usize, 16_297, 1, 2, 4, 17_999] {
            let floored = floor_char_boundary(&s, take);
            assert!(s.is_char_boundary(floored));
            assert_eq!(floored % 3, 0);
            assert!(take >= s.len() || floored <= take);
        }
        assert_eq!(floor_char_boundary(&s, usize::MAX), s.len());
        assert_eq!(floor_char_boundary("", 5), 0);
    }
}
