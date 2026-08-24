//! Node Markdown codec — Rust mirror of `src/host/markdown.ts`.
//!
//! Layout: YAML frontmatter (metadata mirror) + free-form user body + a
//! plugin-managed children block delimited by HTML comment markers. The
//! managed block regenerates from the edge list whenever relations change;
//! edits outside the markers stay authoritative user content.
//!
//! Serialization conventions mirror `js-yaml.dump(attrs, {lineWidth:-1,
//! noRefs:true})`: keys in insertion order, plain scalars whenever they
//! round-trip as strings, SINGLE-quoted otherwise (`''` escaping). Strings
//! that would implicitly resolve as another YAML type (numbers, booleans,
//! nulls, timestamps like `'2026-08-19T00:00:00.000Z'`) are quoted — this
//! matches real plugin-written files (verified against `.omt/tickets/**`)
//! and is what U4's byte-stability goldens will lock further.
//!
//! The parser intentionally covers the flat `key: value` subset those files
//! use (plus quoted scalars); nested YAML structures belong to U4's storage
//! crate, which will swap in a full parser behind the same interface.

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
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---\n")?;
    if let Some(rel) = rest.find("\n---") {
        let after = rel + 4; // past "---"
        let body_start = if rest.as_bytes().get(after) == Some(&b'\n') { after + 1 } else { after };
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
    Ok(ParsedNodeFile { attrs: attrs_from_map(&map), extra: map, body: body.to_string() })
}

impl ParsedNodeFile {
    fn default_with_body(body: String) -> Self {
        ParsedNodeFile { attrs: FrontmatterAttrs::default(), extra: Map::new(), body }
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

/// Minimal flat-YAML parser: `key: value` lines, blank lines and `#`
/// comments ignored, single/double-quoted or plain scalars.
fn parse_flat_yaml(text: &str) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
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
        out.insert(key, parse_scalar(value_text));
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
    match text {
        "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => {
            if let Ok(int_value) = text.parse::<i64>() {
                Value::Number(int_value.into())
            } else if let Ok(float_value) = text.parse::<f64>() {
                serde_json::Number::from_f64(float_value)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            } else {
                Value::String(text.to_string())
            }
        }
    }
}

fn unescape_double(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

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
    let trimmed = dump.trim_end();
    let stripped_body = body.trim_start_matches('\n');
    format!("---\n{trimmed}\n---\n\n{stripped_body}")
}

/// Dump one scalar following js-yaml conventions (plain when it round-trips
/// as a string, single-quoted otherwise).
pub fn dump_scalar(value: Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => {
            if plain_scalar_safe(&text) {
                text
            } else {
                format!("'{}'", text.replace('\'', "''"))
            }
        }
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(&value).unwrap_or_else(|_| "''".to_string())
        }
    }
}

/// True when `text` can sit bare in YAML and still read back as a string.
fn plain_scalar_safe(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let first = text.chars().next().unwrap();
    let last = text.chars().last().unwrap();
    // Leading/trailing whitespace is never plain.
    if first.is_whitespace() || last.is_whitespace() {
        return false;
    }
    // Anything that would implicitly resolve as another YAML type is quoted.
    if resolves_as_non_string(text) {
        return false;
    }
    // Leading indicator characters.
    if matches!(
        first,
        ',' | '[' | ']' | '{' | '}' | '#' | '&' | '*' | '!' | '|' | '>' | '\'' | '"' | '%' | '@' | '`'
    ) {
        return false;
    }
    if matches!(first, '-' | '?' | ':') {
        let second = text.chars().nth(1);
        if second.is_none_or(|c| c.is_whitespace()) {
            return false;
        }
    }
    // Interior hazards: ": ", trailing ':', " #".
    if text.contains(": ") || text.ends_with(':') || text.contains(" #") {
        return false;
    }
    // Line breaks / control characters force quoting.
    !text.chars().any(|c| c.is_control())
}

/// Would this plain scalar parse back as null / bool / number / timestamp?
fn resolves_as_non_string(text: &str) -> bool {
    matches!(text, "" | "~" | "null" | "Null" | "NULL")
        || matches!(text, "true" | "True" | "TRUE" | "false" | "False" | "FALSE")
        || text.parse::<i64>().is_ok()
        || text.parse::<f64>().is_ok()
        || is_yaml_timestamp(text)
}

/// js-yaml's implicit timestamp shape — ISO dates (with or without time)
/// resolve as timestamps, so dumps single-quote them.
fn is_yaml_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    // Byte-level checks FIRST — never slice at a byte index that may fall
    // inside a multi-byte character (CJK titles reach this function).
    let is_digits = |slice: &[u8]| slice.iter().all(|byte| byte.is_ascii_digit());
    if !(is_digits(&bytes[0..4]) && bytes[4] == b'-' && is_digits(&bytes[5..7]) && bytes[7] == b'-' && is_digits(&bytes[8..10])) {
        return false;
    }
    if bytes.len() == 10 {
        return true;
    }
    let sep = bytes[10];
    if sep == b'T' || sep == b't' || sep == b' ' {
        // Time part must begin with an ASCII digit; check via char boundary.
        return text.get(11..).map_or(false, |rest| {
            rest.chars().next().map_or(false, |c| c.is_ascii_digit())
        }) || (bytes.len() == 11);
    }
    false
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
            return format!("{}{}{}", &body[..begin], block, &body[end + CHILDREN_END.len()..]);
        }
    }
    let trimmed = body.trim_end();
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
    body.trim().to_string()
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
    out.trim().to_string()
}

// ── layout helpers (files.ts) ───────────────────────────────────────────

/// Filesystem-safe slug; keeps CJK, strips path-hostile characters.
/// `title.slice(0, 40)` counts UTF-16 units in TypeScript; char-counting is
/// equivalent for every BMP title (corpus included) and documented as the
/// astral-plane divergence risk for U4's golden snapshots.
pub fn slugify(title: &str) -> String {
    let collapsed = collapse_whitespace(title.trim());
    let stripped: String = collapsed
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#'))
        .collect();
    let collapsed_dashes = collapse_dashes(&stripped);
    let trimmed = collapsed_dashes.trim_matches('-');
    let sliced: String = trimmed.chars().take(40).collect();
    if sliced.is_empty() { "untitled".to_string() } else { sliced }
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
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
