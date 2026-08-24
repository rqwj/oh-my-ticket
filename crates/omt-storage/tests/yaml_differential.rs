//! U4b differential harness: ≥2000 seeded randomized adversarial scalars
//! across every golden category are pushed through BOTH serializers — the
//! Rust codec in-process, the TypeScript serializer through ONE
//! `node scripts/gen-yaml-goldens.mjs --stdin` invocation per test-binary
//! run — and every full-file byte sequence must match. Deterministic: a
//! fixed LCG seed means failures reproduce exactly. Budget: <60 s total.

use std::path::PathBuf;
use std::process::Command;

use omt_domain::markdown::{
    render_children_entries, replace_children_block, serialize_node_file, ChildEntry,
};
use serde_json::{json, Value};

/// Fixed LCG state (Numerical Recipes constants). Any failure prints the
/// case index + seed so it can be replayed by hand.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
    /// Uniform in [0, n).
    fn below(&mut self, n: usize) -> usize {
        self.next_u32() as usize % n
    }
    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

// ── adversarial scalar alphabet (mirrors the golden categories) ─────────

const CJK: &[&str] = &[
    "登录",
    "这是一个非常长的中文标题片段用于测试行为",
    "修复：构建失败（含「引号」）",
    "长",
];
const EMOJI: &[&str] = &["✅", "☃", "🦀", "x", "🏳️‍🌈", "👨‍👩‍👧‍👦", "①"];
const QUOTES: &[&str] = &["'", "\"", "it's", "say \"hi\"", "both ' and \""];
const COLONS: &[&str] = &[": ", ":", ":a", "a:b", "12:34", "12:34:56", "全角：不算"];
const INDICATORS: &[&str] = &[
    "-", "?", ":", ",", "[", "]", "{", "}", "#", "&", "*", "!", "|", ">", "=", "'", "\"", "%", "@",
    "`",
];
const BOOLISH: &[&str] = &[
    "true", "True", "TRUE", "false", "False", "FALSE", "~", "null", "Null", "NULL", "y", "Y",
    "yes", "Yes", "YES", "n", "N", "no", "No", "NO", "on", "On", "ON", "off", "Off", "OFF",
];
const NUMERIC: &[&str] = &[
    "007", "1e5", "0x1F", "0o17", "0b101", "+5", "-5", ".5", "5.", "-0", ".inf", ".NaN", "+.inf",
    "inf", "NaN", "Infinity", "2026", "-0x1F",
];
const TIMESTAMPS: &[&str] = &[
    "2026-08-19",
    "2026-08-19T00:00:00.000Z",
    "2026-8-9T05:00:00+08:00",
    "2026-08-19 05:00:00",
    "2026-08-19\t05:00:00",
    "2026-08-19t05:00:00z",
    "2026-08-19T05:00:00-0530",
    "2026-08-19T05:00:00.5",
    "1999-12-31",
];
const CONTROLS: &[&str] = &[
    "\u{7f}", "\u{85}", "\u{a0}", "\u{feff}", "\u{2028}", "\u{2029}", "\t", "\u{1}", "\r",
];
const PLAIN_WORDS: &[&str] = &[
    "alpha",
    "beta_gamma",
    "delta#hash",
    "epsilon:next",
    "zeta ",
    " eta-trailing-dash-",
    "normal title",
];

/// One random adversarial scalar assembled from 1–4 fragments.
fn random_scalar(rng: &mut Lcg) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let fragment_count = 1 + rng.below(4);
    for _ in 0..fragment_count {
        let pool: &[&str] = match rng.below(10) {
            0 => CJK,
            1 => EMOJI,
            2 => QUOTES,
            3 => COLONS,
            4 => INDICATORS,
            5 => BOOLISH,
            6 => NUMERIC,
            7 => TIMESTAMPS,
            8 => CONTROLS,
            _ => PLAIN_WORDS,
        };
        parts.push(rng.pick(pool));
    }
    // Occasionally append a long tail or newline structure.
    match rng.below(12) {
        0 => format!("{}{}", parts.concat(), "x".repeat(60 + rng.below(200))),
        1 => format!("{}\nline-two\n", parts.concat()),
        2 => format!("{}\n\nindented\n\n", parts.concat()),
        3 => format!("\n{} more-indented", parts.concat()),
        4 => format!("{}\n", parts.concat()),
        5 => format!("{}\n\t tabbed", parts.concat()),
        _ => parts.concat(),
    }
}

/// Build one differential input record. Frontmatter travels as ORDERED
/// `[key, value]` pairs (`attrsPairs`) because serde_json maps sort keys
/// alphabetically on the wire while js-yaml dumps insertion order — the
/// pairs pin the exact `frontmatterOf` sequence across the transport.
fn build_input(name: String, rng: &mut Lcg) -> Value {
    let mut attrs: Vec<Value> = Vec::new();
    let mut push = |key: &str, value: Value| {
        attrs.push(json!([key, value]));
    };
    push("id", json!(format!("TICKET-{:04}", rng.below(9000))));
    push("type", json!("ticket"));
    push("title", json!(random_scalar(rng)));
    push("status", json!(rng.pick(&["open", "in_progress", "done"])));
    if rng.below(4) == 0 {
        push("archived", json!(true));
    }
    push("priority", json!(rng.below(40) as i64 - 20));
    if rng.below(3) == 0 {
        push("parent", json!(format!("STORY-{:04}", rng.below(9000))));
    }
    push("created_at", json!("2026-08-24T05:00:00.000Z"));
    push("updated_at", json!("2026-08-24T05:00:00.000Z"));

    let body = match rng.below(6) {
        0 => String::new(),
        1 => "   \n\n  \n".to_string(),
        2 => "\n\nlead newlines then text".to_string(),
        3 => "# 标题\n\n段落。\n\n- 列表项\n".to_string(),
        4 => format!("user body {}", random_scalar(rng)),
        _ => "body with trailing newlines\n\n".to_string(),
    };
    let children = if rng.below(3) == 0 {
        json!([{
            "id": format!("TICKET-{:04}", rng.below(9000)),
            "title": random_scalar(rng),
            "status": rng.pick(&["open", "done"]),
            "type": "ticket",
            "dirName": format!("TICKET-0000-child-{}", rng.below(100)),
        }])
    } else {
        json!([])
    };
    json!({ "name": name, "attrsPairs": attrs, "body": body, "children": children })
}

fn rust_render(case: &Value) -> String {
    // attrsPairs arrive in exact frontmatterOf order (see build_input).
    let pairs: Vec<(&str, Value)> = case["attrsPairs"]
        .as_array()
        .expect("attrsPairs")
        .iter()
        .map(|pair| {
            let items = pair.as_array().expect("pair");
            (items[0].as_str().expect("key"), items[1].clone())
        })
        .collect();
    let body = case["body"].as_str().expect("body");
    let children = case["children"].as_array().cloned().unwrap_or_default();
    let entries: Vec<ChildEntry> = children
        .iter()
        .map(|child| ChildEntry {
            id: child["id"].as_str().expect("id").to_string(),
            title: child["title"].as_str().expect("title").to_string(),
            dir_name: child["dirName"].as_str().expect("dirName").to_string(),
            node_type: child["type"].as_str().expect("type").to_string(),
            status: child["status"].as_str().expect("status").to_string(),
        })
        .collect();
    let block = render_children_entries(&entries);
    let full_body = replace_children_block(body, &block);
    serialize_node_file(pairs, &full_body)
}

fn first_diff(actual: &[u8], expected: &[u8]) -> Option<(usize, String)> {
    let common = actual.len().min(expected.len());
    for index in 0..common {
        if actual[index] != expected[index] {
            let start = index.saturating_sub(24);
            let end = (index + 24).min(common);
            return Some((
                index,
                format!(
                    "actual  …{}…\nexpected…{}…",
                    String::from_utf8_lossy(&actual[start..end]),
                    String::from_utf8_lossy(&expected[start..end]),
                ),
            ));
        }
    }
    (actual.len() != expected.len()).then(|| {
        (
            common,
            format!(
                "length differs: actual {} vs expected {}",
                actual.len(),
                expected.len()
            ),
        )
    })
}

fn base64_decode(input: &str) -> Vec<u8> {
    fn value_of(byte: u8) -> u32 {
        match byte {
            b'A'..=b'Z' => (byte - b'A') as u32,
            b'a'..=b'z' => (byte - b'a' + 26) as u32,
            b'0'..=b'9' => (byte - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("invalid base64 byte {byte}"),
        }
    }
    let cleaned: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(cleaned.len() * 3 / 4);
    let mut chunk = [0u8; 4];
    let mut filled = 0usize;
    for &byte in &cleaned {
        if byte == b'=' {
            break;
        }
        chunk[filled] = value_of(byte) as u8;
        filled += 1;
        if filled == 4 {
            out.push((chunk[0] << 2) | (chunk[1] >> 4));
            out.push((chunk[1] << 4) | (chunk[2] >> 2));
            out.push((chunk[2] << 6) | chunk[3]);
            filled = 0;
        }
    }
    match filled {
        0 => {}
        2 => out.push(chunk[0] << 2 | chunk[1] >> 4),
        3 => {
            out.push((chunk[0] << 2) | (chunk[1] >> 4));
            out.push((chunk[1] << 4) | (chunk[2] >> 2));
        }
        other => panic!("truncated base64 group of {other}"),
    }
    out
}

#[test]
fn differential_matches_typescript_on_random_adversarial_scalars() {
    const CASE_COUNT: usize = 2200;
    const SEED: u64 = 0x00DE_11E7_CAFE_2026;

    let mut rng = Lcg::new(SEED);
    let inputs: Vec<Value> = (0..CASE_COUNT)
        .map(|index| build_input(format!("diff-{index:05}"), &mut rng))
        .collect();

    // ONE TypeScript invocation for all inputs (CONVENTIONS.md contract).
    let scratch =
        std::env::temp_dir().join(format!("omt-yaml-diff-{SEED}-{}.json", std::process::id()));
    std::fs::write(
        &scratch,
        serde_json::to_string(&inputs).expect("serialize inputs"),
    )
    .expect("write scratch inputs");
    let output_path = scratch.with_extension("out.json");
    let stdout = std::fs::File::create(&output_path).expect("create expectations file");
    let script = repo_root().join("scripts").join("gen-yaml-goldens.mjs");
    let status = Command::new("node")
        .arg(&script)
        .arg("--stdin")
        .stdin(std::fs::File::open(&scratch).expect("reopen scratch inputs"))
        .stdout(stdout)
        .status()
        .expect("spawn node for TS leg (node must be on PATH)");
    assert!(status.success(), "TS differential leg failed: {status}");
    let raw = std::fs::read_to_string(&output_path).expect("read expectations");
    std::fs::remove_file(&scratch).ok();
    std::fs::remove_file(&output_path).ok();
    let expectations: Vec<Value> = serde_json::from_str(&raw).expect("parse expectations");
    assert_eq!(
        expectations.len(),
        CASE_COUNT,
        "TS leg returned a different number of expectations"
    );

    let mut failures: Vec<String> = Vec::new();
    for (case, expectation) in inputs.iter().zip(expectations.iter()) {
        let actual = rust_render(case);
        let encoded = expectation["expectedBase64"]
            .as_str()
            .expect("expectedBase64");
        let expected = base64_decode(encoded);
        if let Some((offset, context)) = first_diff(actual.as_bytes(), &expected) {
            let case_name = case["name"].as_str().unwrap_or("?").to_string();
            let dump_dir =
                std::env::temp_dir().join(format!("omt-diff-dump-{}", std::process::id()));
            std::fs::create_dir_all(&dump_dir).ok();
            std::fs::write(dump_dir.join(format!("{case_name}.actual")), &actual).ok();
            std::fs::write(dump_dir.join(format!("{case_name}.expected")), &expected).ok();
            std::fs::write(
                dump_dir.join(format!("{case_name}.input.json")),
                serde_json::to_vec_pretty(case).unwrap(),
            )
            .ok();
            failures.push(format!(
                "case `{case_name}` diverges at byte {offset}:\n{context}\ninput: {case}",
            ));
        }
        if failures.len() >= 5 {
            break; // cap the report; counts still quoted below
        }
    }

    if !failures.is_empty() {
        panic!(
            "{}/{} differential cases diverge from the TypeScript serializer \
             (first {} shown):\n\n{}",
            failures.len().max(6),
            CASE_COUNT,
            failures.len(),
            failures.join("\n\n"),
        );
    }
}
