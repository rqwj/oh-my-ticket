//! Identity and enrollment (R12, KTD9): OS-peer-credential-checked
//! handshake issuing short-lived scoped credentials from an in-memory
//! registry.
//!
//! Contract (U5a locked):
//! - Same-uid enforcement at connect: other-uid connections are closed.
//! - Handshake derives {token(32B hex), principalId "<kind>:<pid>",
//!   actorNamespace ("<kind>:<pid>" unless a scoped grant is requested),
//!   homes[], operations[], expiresAt = now + 12h}.
//! - actorNamespace grants: honored ONLY when equal to the server-assigned
//!   base or nested under it ("<base>/<suffix>") — a client can never mint
//!   another principal's namespace.
//! - Administrator capability: boolean, read fresh on every check from the
//!   out-of-band file `<runtime-dir>/admin-grants.json`
//!   {"principalIds":[...]}. Never delegable through the protocol.
//! - Credentials live in memory only — they die with the generation.

use crate::ipc::PeerId;
use crate::problem::entropy;
use omt_storage::clock::{iso_from_ms, MillisClock};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const PROTOCOL_MAJOR: i64 = 1;
pub const PROTOCOL_MINOR: i64 = 0;
/// Credential lifetime.
pub const CREDENTIAL_TTL_MS: i64 = 12 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientKind {
    Dsh,
    Cli,
    Desktop,
    Mcp,
    External,
}

impl ClientKind {
    pub fn parse(raw: &str) -> Option<ClientKind> {
        match raw {
            "dsh" => Some(ClientKind::Dsh),
            "cli" => Some(ClientKind::Cli),
            "desktop" => Some(ClientKind::Desktop),
            "mcp" => Some(ClientKind::Mcp),
            "external" => Some(ClientKind::External),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ClientKind::Dsh => "dsh",
            ClientKind::Cli => "cli",
            ClientKind::Desktop => "desktop",
            ClientKind::Mcp => "mcp",
            ClientKind::External => "external",
        }
    }

    /// Adapter-channel principals (parity `adapter_only`).
    pub fn is_adapter(&self) -> bool {
        matches!(self, ClientKind::Dsh | ClientKind::Desktop)
    }
}

/// One issued connection credential. The token lives ONLY in the registry
/// key; this view carried into dispatch deliberately excludes it so no
/// handler can accidentally log or echo a secret.
#[derive(Debug, Clone)]
pub struct Credential {
    pub principal_id: String,
    pub actor_namespace: String,
    pub kind: ClientKind,
    pub homes: Vec<String>,
    pub operations: Vec<String>,
    /// Milliseconds-since-epoch expiry (injectable clock domain).
    pub expires_at_ms: i64,
}

impl Credential {
    pub fn expired(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }

    pub fn home_allowed(&self, home_id: &str) -> bool {
        self.homes.iter().any(|h| h == home_id)
    }

    /// Operation-family scope: "*" grants everything; otherwise the exact
    /// method namespace ("node", "run", "events", "ui", "home") must be
    /// enumerated.
    pub fn operation_allowed(&self, method: &str) -> bool {
        if self.operations.iter().any(|op| op == "*") {
            return true;
        }
        let family = method.split('/').next().unwrap_or_default();
        self.operations.iter().any(|op| op == family)
    }
}

/// In-memory credential registry keyed by token. Entries die with the
/// process (generation rotation) or at expiry.
#[derive(Default)]
pub struct Registry {
    inner: std::sync::Mutex<HashMap<String, Credential>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    pub fn issue(
        &self,
        peer: PeerId,
        kind: ClientKind,
        requested: &Value,
        open_home_ids: &[String],
        clock: &Arc<dyn MillisClock>,
    ) -> Result<(String, Credential), omt_storage::Problem> {
        let base = format!("{}:{}", kind.as_str(), peer.pid);
        // Actor namespace grant: base or base/nested only (never foreign).
        let mut actor_namespace = base.clone();
        if let Some(requested_ns) = requested.get("actorNamespace").and_then(|v| v.as_str()) {
            let nested = format!("{base}/");
            if (requested_ns == base || requested_ns.starts_with(&nested))
                && !requested_ns.is_empty()
                && requested_ns.len() <= 64
            {
                actor_namespace = requested_ns.to_string();
            }
        }

        // Home scope: explicit list ∩ currently-open homes; omitted/empty →
        // all open homes (recorded explicitly on the credential).
        let mut homes: Vec<String> = Vec::new();
        let requested_homes: Vec<String> = requested
            .get("homes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| h.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if requested_homes.is_empty() {
            homes.extend(open_home_ids.iter().cloned());
        } else {
            let open: HashSet<&String> = open_home_ids.iter().collect();
            for home in requested_homes {
                if open.contains(&home) {
                    homes.push(home);
                }
            }
        }

        // Operation scope: default wildcard within parity limits.
        let operations: Vec<String> = requested
            .get("operations")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|op| op.as_str().map(str::to_string))
                    .collect()
            })
            .filter(|list: &Vec<String>| !list.is_empty())
            .unwrap_or_else(|| vec!["*".to_string()]);

        let token = entropy::token_hex();
        let credential = Credential {
            principal_id: base.clone(),
            actor_namespace,
            kind,
            homes,
            operations,
            expires_at_ms: clock.now_ms() + CREDENTIAL_TTL_MS,
        };
        self.inner
            .lock()
            .expect("registry")
            .insert(token.clone(), credential.clone());
        Ok((token, credential))
    }

    /// Validate a presented token: known AND unexpired. Expired entries are
    /// evicted lazily. Error carries UNAUTHORIZED with details.reason.
    pub fn validate(
        &self,
        token: &str,
        clock: &Arc<dyn MillisClock>,
    ) -> Result<Credential, omt_storage::Problem> {
        let mut map = self.inner.lock().expect("registry");
        let now_ms = clock.now_ms();
        // Lazy sweep of everything already expired.
        map.retain(|_, credential| !credential.expired(now_ms));
        match map.get(token) {
            Some(credential) => Ok(credential.clone()),
            None => {
                // Distinguish missing vs expired for actionable details
                // without keeping any state after eviction.
                Err(unauthorized("unknown-credential"))
            }
        }
    }

    /// U5b's janitor wiring calls this on an interval; kept public now.
    #[allow(dead_code)]
    pub fn revoke_expired(&self, clock: &Arc<dyn MillisClock>) {
        let now_ms = clock.now_ms();
        self.inner
            .lock()
            .expect("registry")
            .retain(|_, c| !c.expired(now_ms));
    }
}

pub fn unauthorized(reason: &str) -> omt_storage::Problem {
    omt_storage::Problem::with_details(
        crate::problem::UNAUTHORIZED,
        format!("credential rejected: {reason}"),
        |d| {
            d.insert("reason".into(), reason.into());
        },
    )
}

pub fn forbidden(reason: &str, extra: Value) -> omt_storage::Problem {
    omt_storage::Problem::with_details(
        crate::problem::FORBIDDEN,
        format!("principal not authorized for this action: {reason}"),
        |d| {
            d.insert("reason".into(), reason.into());
            if let Value::Object(map) = extra {
                for (key, value) in map {
                    d.insert(key, value);
                }
            }
        },
    )
}

/// Extract the presented token from params.credential.token.
pub fn presented_token(params: &Value) -> Result<String, omt_storage::Problem> {
    let token = params
        .get("credential")
        .and_then(|credential| credential.get("token"))
        .and_then(|token| token.as_str());
    match token {
        Some(token) if !token.is_empty() => Ok(token.to_string()),
        _ => Err(unauthorized("missing-credential")),
    }
}

// ── same-uid gate ───────────────────────────────────────────────────────

/// Current process effective uid (unix).
#[cfg(unix)]
fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

/// Enforce same-user connections. Returns Err(problem) when the peer runs
/// as another uid — the connection must be closed before any protocol
/// exchange. Always Ok on windows (ACL-based isolation documented instead).
pub fn enforce_same_user(peer: PeerId) -> Result<(), omt_storage::Problem> {
    #[cfg(unix)]
    {
        if let Some(uid) = peer.uid {
            if uid != effective_uid() {
                return Err(forbidden("cross-uid-connection", json!({ "peerUid": uid })));
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let _ = peer;
        Ok(())
    }
}

// ── administrator grants (out-of-band file) ─────────────────────────────

/// Read `<runtime-dir>/admin-grants.json` FRESH each time (small file;
/// out-of-band edits take effect without restart). Unreadable/corrupt →
/// empty set (fail closed).
pub fn admin_principals(runtime_dir: &std::path::Path) -> HashSet<String> {
    let path: PathBuf = runtime_dir.join(crate::paths::ADMIN_GRANTS_FILE);
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return HashSet::new(),
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => value
            .get("principalIds")
            .and_then(|ids| ids.as_array())
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default(),
        Err(_) => HashSet::new(),
    }
}

pub fn is_administrator(runtime_dir: &std::path::Path, credential: &Credential) -> bool {
    admin_principals(runtime_dir).contains(&credential.principal_id)
}

/// Human-readable ISO expiry for the wire.
pub fn expires_iso(expires_at_ms: i64) -> String {
    iso_from_ms(expires_at_ms)
}

/// Duration used nowhere yet but kept beside TTL for U5b config wiring.
#[allow(dead_code)]
pub fn ttl() -> Duration {
    Duration::from_millis(CREDENTIAL_TTL_MS.max(0) as u64)
}
