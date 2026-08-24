//! Problem codes introduced by the runtime plane (U5a). Registered
//! additively in `schema/problems.schema.json`; coarse codes reuse
//! [`omt_domain::error`].

/// No valid credential on a request that requires one, or an expired /
/// unknown token (coarse; details.reason ∈ missing-credential |
/// unknown-credential | expired-credential).
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Authenticated principal lacks the parity class / scope for this method
/// (coarse; details.reason ∈ parity-adapter-only | admin-required |
/// home-not-scoped | operation-not-granted).
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Bootstrap election gave up: no live daemon appeared within the poll
/// budget and no lock could be won (HOME_LOCKED-style coarse refusal).
pub const BOOTSTRAP_TIMEOUT: &str = "BOOTSTRAP_TIMEOUT";

/// Alias so runtime modules speak one Problem shape.
pub type ProblemShape = omt_storage::Problem;

/// Registered limit-code builder (U5b): RATE_LIMITED carries
/// `details.reason`, QUOTA_EXCEEDED carries `details.rule`; both merge the
/// caller's structured extras (limit/observed) into details.
pub fn limit_problem(
    code: &'static str,
    field: &'static str,
    value: &str,
    extra: serde_json::Value,
) -> ProblemShape {
    omt_storage::Problem::with_details(code, format!("{code}: {field}={value}"), |d| {
        d.insert(field.into(), serde_json::Value::String(value.to_string()));
        if let serde_json::Value::Object(map) = extra {
            for (name, item) in map {
                d.insert(name, item);
            }
        }
    })
}

/// Entropy helpers: hex tokens from the OS CSPRNG.
pub mod entropy {
    /// 32 random bytes as 64 lowercase hex chars (credential tokens,
    /// boot tokens, lease tokens).
    pub fn token_hex() -> String {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("OS entropy unavailable");
        let mut text = String::with_capacity(64);
        for byte in bytes {
            text.push_str(&format!("{byte:02x}"));
        }
        text
    }

    /// Short unstructured id (command ids, subscription ids).
    pub fn short_id() -> String {
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes).expect("OS entropy unavailable");
        let mut text = String::with_capacity(24);
        for byte in bytes {
            text.push_str(&format!("{byte:02x}"));
        }
        text
    }
}

/// Defense-in-depth redaction: replace any 64-hex-char run (credential
/// token shape) with "[redacted]" before a message crosses the wire or a
/// log. NO credential ever appears in argv/env/logs/errors.
pub fn redact(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for ch in text.chars() {
        if ch.is_ascii_hexdigit() {
            run.push(ch);
            if run.len() == 64 {
                out.push_str("[redacted]");
                run.clear();
            }
        } else {
            // Flush any partial hex run verbatim (it is not token-shaped).
            out.push_str(&run);
            run.clear();
            out.push(ch);
        }
    }
    out.push_str(&run);
    let _ = bytes;
    out
}
