//! U4b byte-stability gate: every golden case must serialize to EXACTLY the
//! bytes the real TypeScript serializer produced (`corpus/yaml-goldens/`,
//! conventions in CONVENTIONS.md). Proof-first harness — this ran RED against
//! the pre-parity codec before any `markdown.rs` fix was made.

use std::path::PathBuf;

use serde_json::Value;

use omt_domain::markdown::{
    render_children_entries, replace_children_block, serialize_node_file, slugify, ChildEntry,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn load_cases() -> Vec<serde_json::Value> {
    let path = repo_root()
        .join("corpus")
        .join("yaml-goldens")
        .join("cases.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let parsed: Value = serde_json::from_str(&text).expect("parse cases.json");
    parsed["cases"].as_array().expect("cases array").clone()
}

/// Rebuild the canonical `frontmatterOf` key order from the stored attrs
/// object (CONVENTIONS.md rule 1; JSON object order is not authoritative).
fn ordered_pairs(attrs: &serde_json::Map<String, Value>) -> Vec<(&'static str, Value)> {
    let mut pairs: Vec<(&'static str, Value)> = Vec::new();
    let mut push = |key: &'static str, attrs: &serde_json::Map<String, Value>| {
        if let Some(value) = attrs.get(key) {
            pairs.push((key, value.clone()));
        }
    };
    push("id", attrs);
    push("type", attrs);
    push("title", attrs);
    push("status", attrs);
    push("archived", attrs);
    push("priority", attrs);
    push("parent", attrs);
    push("created_at", attrs);
    push("updated_at", attrs);
    assert_eq!(pairs.len(), attrs.len(), "unknown frontmatter keys present");
    pairs
}

fn child_entries(children: &[Value]) -> Vec<ChildEntry> {
    children
        .iter()
        .map(|child| {
            let title = child["title"].as_str().expect("child title").to_string();
            // dirName mirrors what the TS leg passed to renderChildrenBlock:
            // an explicit (possibly hand-written) name when present, else
            // `<id>-<slug>` recomputed by the RUST slugify — which is where
            // the UTF-16 slice parity (decision 3) is actually proven.
            // Slug-generated names arrive as base64 bytes (a straddled cut
            // ends in a lone surrogate that cannot cross JSON; Node's writer
            // already turned it into U+FFFD inside the file).
            let dir_name = if let Some(explicit) = child.get("dirName").and_then(Value::as_str) {
                explicit.to_string()
            } else if let Some(encoded) = child.get("dirNameBase64").and_then(Value::as_str) {
                String::from_utf8_lossy(&base64_decode(encoded)).into_owned()
            } else {
                format!("{}-{}", child["id"].as_str().expect("id"), slugify(&title))
            };
            ChildEntry {
                id: child["id"].as_str().expect("id").to_string(),
                title,
                dir_name,
                node_type: child["type"].as_str().expect("type").to_string(),
                status: child["status"].as_str().expect("status").to_string(),
            }
        })
        .collect()
}

/// First differing byte offset + surrounding context for failure output.
fn first_diff(actual: &[u8], expected: &[u8]) -> Option<(usize, String)> {
    let common = actual.len().min(expected.len());
    for index in 0..common {
        if actual[index] != expected[index] {
            let start = index.saturating_sub(24);
            let end = index.saturating_add(24).min(common.max(index));
            return Some((
                index,
                format!(
                    "actual  …{}…\nexpected…{}…",
                    String::from_utf8_lossy(&actual[start..end]),
                    String::from_utf8_lossy(&expected[start..end.min(expected.len())]),
                ),
            ));
        }
    }
    if actual.len() != expected.len() {
        return Some((
            common,
            format!(
                "length differs: actual {} vs expected {}",
                actual.len(),
                expected.len()
            ),
        ));
    }
    None
}

#[test]
fn golden_cases_are_byte_identical() {
    let cases = load_cases();
    assert!(cases.len() >= 100, "golden corpus unexpectedly small");

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().expect("case name").to_string();
        let attrs = case["attrs"].as_object().expect("attrs object");
        let body = case["body"].as_str().expect("body").to_string();
        let children = case["children"].as_array().cloned().unwrap_or_default();

        let block = render_children_entries(&child_entries(&children));
        let full_body = replace_children_block(&body, &block);
        let actual = serialize_node_file(ordered_pairs(attrs), &full_body);

        let expected_base64 = case["expectedBase64"].as_str().expect("expectedBase64");
        let expected = base64_decode(expected_base64);

        match first_diff(actual.as_bytes(), &expected) {
            None => {}
            Some((offset, context)) => failures.push(format!(
                "case `{name}`: byte {offset} differs ({len_a} vs {len_e} bytes)\n{context}",
                len_a = actual.len(),
                len_e = expected.len(),
            )),
        }

        // Decision 3 hard check: for children whose dirName was GENERATED by
        // the real TS slugify (recorded as `dirNameBase64` bytes), the Rust
        // slug must match byte-for-byte — including the lone-surrogate
        // straddle case, where both sides must produce U+FFFD.
        for child in &children {
            if child.get("slugGenerated").and_then(Value::as_bool) == Some(true) {
                let encoded = child
                    .get("dirNameBase64")
                    .and_then(Value::as_str)
                    .expect("dirNameBase64");
                let ts_slug = String::from_utf8_lossy(&base64_decode(encoded)).into_owned();
                let rust_slug = format!(
                    "{}-{}",
                    child["id"].as_str().unwrap(),
                    slugify(child["title"].as_str().unwrap())
                );
                if rust_slug != ts_slug {
                    failures.push(format!(
                        "case `{name}`: astral/slice slug divergence: rust `{rust_slug:?}` vs ts `{ts_slug:?}`"
                    ));
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} of {} golden cases diverge from the TypeScript serializer:\n\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n\n")
        );
    }
}

/// Minimal standard base64 decoder (avoids adding a dependency for tests).
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
