//! omt-client (U7/KTD4-KTD6): the thin shared client half of the OMT
//! runtime protocol. One small crate owns what every local surface needs
//! to talk to `omt-daemon` — descriptor discovery + liveness, UDS endpoint
//! connect, JSON-line request/response framing, handshake enrollment, and
//! optional on-disk credential reuse — so consumers (the `omt` CLI today;
//! doctor's online preamble; future mcp helpers) never regrow ad-hoc
//! plumbing. Deliberately synchronous and tiny: the daemon protocol is
//! request/response over a per-process connection.
//!
//! NOT in scope (stays consumer-owned): reconnect policy, subscription
//! replay, surface-specific home registries.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use omt_domain::error::Problem;
use serde::{Deserialize, Serialize};

/// Wire descriptor as published by the daemon under
/// `<runtime-dir>/descriptor.json` (schema additive; unknown fields ignored
/// per docs/runtime/config.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Descriptor {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    pub endpoint: String,
    pub generation: i64,
    pub pid: i64,
    #[serde(rename = "bootToken")]
    pub boot_token: String,
}

/// Filesystem location of the daemon descriptor for one runtime dir.
pub fn descriptor_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("descriptor.json")
}

/// Read and parse the descriptor; `None` when absent or unparsable (both
/// mean "no known daemon" to a client).
pub fn read_descriptor(runtime_dir: &Path) -> Option<Descriptor> {
    let raw = std::fs::read_to_string(descriptor_path(runtime_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// True when the descriptor's pid names a live process. EPERM from kill(0)
/// means "alive but not ours" — that still counts as live for probing.
#[cfg(unix)]
pub fn pid_live(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs error checking only, no signal is sent.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Quick endpoint reachability probe: connect and close.
#[cfg(unix)]
pub fn endpoint_live(endpoint: &str) -> bool {
    std::os::unix::net::UnixStream::connect(endpoint).is_ok()
}

/// Synchronous JSON-RPC client over one UDS connection: writes one
/// request line, skips interleaved notifications, returns the matching
/// response. One in-flight request at a time by construction (&mut self).
pub struct Client {
    stream: std::os::unix::net::UnixStream,
    next_id: u64,
    token: String,
    /// Optional cooperative-cancel probe checked between read lines (the
    /// CLI wires its Ctrl-C flag here; EOF right after a cancel surfaces
    /// as CANCELED, not IO).
    cancel_check: Option<Box<dyn Fn() -> bool + Send>>,
}

/// Options for [`Client::connect_and_enroll`].
pub struct EnrollOptions {
    /// Client kind on the handshake (`"cli"`, `"doctor"`, ...).
    pub kind: String,
    /// Display name; defaults to `omt-<kind>`.
    pub name: Option<String>,
    /// Optional requested actor namespace scope.
    pub actor_namespace: Option<String>,
    /// Optional requested operations families (e.g. ["node", "run",
    /// "events"] for the restricted mcp credential). Absent → the
    /// server's default grant for the kind.
    pub operations: Option<Vec<String>>,
    /// Credential persistence policy: when set, a previously stored token
    /// at this path is reused (probed once), and a fresh enrollment is
    /// persisted back with 0600 permissions. Absent → enroll per process.
    pub credential_path: Option<PathBuf>,
}

impl Default for EnrollOptions {
    fn default() -> Self {
        Self {
            kind: "external".into(),
            name: None,
            actor_namespace: None,
            operations: None,
            credential_path: None,
        }
    }
}

/// Outcome of a successful enrollment: the live client plus the raw
/// handshake payload (homes listing, features map, daemon version).
pub struct Enrollment {
    pub client: Client,
    pub handshake: serde_json::Value,
}

impl Client {
    /// Connect to a descriptor's endpoint and enroll. With
    /// `credential_path` set, a stored token is probed first; an
    /// UNAUTHORIZED/FORBIDDEN probe falls through to a fresh enrollment
    /// (daemon restarts rotate the credential registry).
    #[cfg(unix)]
    pub fn connect_and_enroll(
        descriptor: &Descriptor,
        options: &EnrollOptions,
    ) -> Result<Enrollment, Problem> {
        let stream = std::os::unix::net::UnixStream::connect(&descriptor.endpoint)
            .map_err(|err| Problem::new("IO", format!("endpoint connect: {err}")))?;
        let mut client = Client {
            stream,
            next_id: 1,
            token: String::new(),
            cancel_check: None,
        };

        if let Some(cred_path) = &options.credential_path {
            if let Ok(raw) = std::fs::read_to_string(cred_path) {
                if let Ok(stored) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(token) = stored["token"].as_str() {
                        client.token = token.to_string();
                        match client.call("node/list", serde_json::json!({ "filter": {} })) {
                            Ok(_) => {
                                let handshake = client.discovery_handshake(options)?;
                                return Ok(Enrollment { client, handshake });
                            }
                            Err(problem)
                                if problem.code == "UNAUTHORIZED"
                                    || problem.code == "FORBIDDEN" =>
                            {
                                // Stale credential: fall through to re-enroll.
                                client.next_id += 1;
                            }
                            Err(_) => {
                                // Transport problem unrelated to auth: keep
                                // the stored token and let the caller's verb
                                // surface it.
                                let handshake = client.discovery_handshake(options)?;
                                return Ok(Enrollment { client, handshake });
                            }
                        }
                    }
                }
            }
        }

        let handshake = client.enroll(options)?;
        if let Some(cred_path) = &options.credential_path {
            let credential = handshake["credential"].clone();
            if credential["token"].as_str().is_some_and(|t| !t.is_empty()) {
                let payload = serde_json::to_string(&credential).unwrap_or_default();
                let _ = std::fs::write(cred_path, payload);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(cred_path, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
        Ok(Enrollment { client, handshake })
    }

    /// Convenience: enroll with an explicit restricted operations set
    /// (the mcp server's node/run/events-only credential, R7).
    #[cfg(unix)]
    pub fn connect_and_enroll_with_operations(
        descriptor: &Descriptor,
        options: &EnrollOptions,
        operations: &[&str],
    ) -> Result<Enrollment, Problem> {
        let scoped = EnrollOptions {
            kind: options.kind.clone(),
            name: options.name.clone(),
            actor_namespace: options.actor_namespace.clone(),
            operations: Some(operations.iter().map(|op| op.to_string()).collect()),
            credential_path: options.credential_path.clone(),
        };
        Self::connect_and_enroll(descriptor, &scoped)
    }

    /// Fresh handshake/request enrollment; stores the issued token.
    pub fn enroll(&mut self, options: &EnrollOptions) -> Result<serde_json::Value, Problem> {
        let params = serde_json::json!({
            "protocolVersion": "1.0",
            "client": {
                "kind": options.kind,
                "name": options.name.clone().unwrap_or_else(|| format!("omt-{}", options.kind)),
                "version": env!("CARGO_PKG_VERSION"),
            },
            "requestedScopes": requested_scopes(options),
        });
        let handshake = self.request("handshake/request", params)?;
        self.token = handshake["credential"]["token"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        Ok(handshake)
    }

    /// Unauthenticated discovery handshake (server treats it as such):
    /// learns the homes listing + features without minting a credential.
    pub fn discovery_handshake(
        &mut self,
        options: &EnrollOptions,
    ) -> Result<serde_json::Value, Problem> {
        self.request(
            "handshake/request",
            serde_json::json!({
                "protocolVersion": "1.0",
                "client": {
                    "kind": options.kind,
                    "name": options.name.clone().unwrap_or_else(|| format!("omt-{}", options.kind)),
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )
    }

    /// Wrap an already-connected stream (test seams and callers that own
    /// their connect logic).
    #[cfg(unix)]
    pub fn from_stream(stream: std::os::unix::net::UnixStream) -> Client {
        Client {
            stream,
            next_id: 0,
            token: String::new(),
            cancel_check: None,
        }
    }

    /// The credential token minted by the last successful enrollment.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Raw fd of the underlying socket (Ctrl-C abort registries).
    #[cfg(unix)]
    pub fn stream_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.stream.as_raw_fd()
    }

    /// Replace the credential token (stored-token reuse paths).
    pub fn set_token(&mut self, token: String) {
        self.token = token;
    }

    /// Install a cooperative-cancel probe (checked between read lines).
    pub fn set_cancel_check(&mut self, check: Box<dyn Fn() -> bool + Send>) {
        self.cancel_check = Some(check);
    }

    fn canceled(&self) -> bool {
        self.cancel_check.as_ref().is_some_and(|check| check())
    }

    /// Authenticated business call: attaches `credential.token`.
    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, Problem> {
        self.request(method, params)
    }

    /// Raw request: credential token attached for every non-handshake
    /// method, JSON-line framed, interleaved notifications skipped.
    pub fn request(
        &mut self,
        method: &str,
        mut params: serde_json::Value,
    ) -> Result<serde_json::Value, Problem> {
        self.next_id += 1;
        let id = self.next_id;
        if !method.starts_with("handshake") {
            params["credential"] = serde_json::json!({ "token": self.token });
        }
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        {
            let mut writer = &self.stream;
            writeln!(writer, "{line}")
                .map_err(|err| Problem::new("IO", format!("request write: {err}")))?;
        }
        let reader = BufReader::new(
            self.stream
                .try_clone()
                .map_err(|err| Problem::new("IO", format!("stream clone: {err}")))?,
        );
        for line in reader.lines() {
            if self.canceled() {
                return Err(Problem::new("CANCELED", "canceled"));
            }
            let line = match line {
                Ok(line) => line,
                Err(err) => {
                    // Socket EOF right after a cancel IS the cancel.
                    if self.canceled() {
                        return Err(Problem::new("CANCELED", "canceled"));
                    }
                    return Err(Problem::new("IO", format!("connection closed: {err}")));
                }
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue; // interleaved omt/event notification
            }
            if let Some(error) = value.get("error") {
                let data = error.get("data").cloned().unwrap_or(serde_json::json!({}));
                return Err(Problem {
                    code: intern_code(data["code"].as_str().unwrap_or("UNKNOWN")),
                    message: data["message"].as_str().unwrap_or_default().to_string(),
                    details: data.get("details").cloned(),
                });
            }
            return Ok(value
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null));
        }
        if self.canceled() {
            return Err(Problem::new("CANCELED", "canceled"));
        }
        Err(Problem::new("IO", "connection closed before response"))
    }
}

fn requested_scopes(options: &EnrollOptions) -> serde_json::Value {
    let mut scopes = serde_json::Map::new();
    if let Some(ns) = &options.actor_namespace {
        scopes.insert("actorNamespace".into(), serde_json::json!(ns));
    }
    if let Some(operations) = &options.operations {
        scopes.insert("operations".into(), serde_json::json!(operations));
    }
    serde_json::Value::Object(scopes)
}

/// Problem.code is &'static str; wire codes are dynamic — intern the known
/// registry (unknown collapses to UNKNOWN, matching the CLI's legacy map).
fn intern_code(code: &str) -> &'static str {
    match code {
        "CONFLICT" => "CONFLICT",
        "INVALID_HIERARCHY" => "INVALID_HIERARCHY",
        "INVALID_INPUT" => "INVALID_INPUT",
        "NOT_FOUND" => "NOT_FOUND",
        "IO" => "IO",
        "UNSUPPORTED_PROTOCOL" => "UNSUPPORTED_PROTOCOL",
        "SCHEMA_TOO_NEW" => "SCHEMA_TOO_NEW",
        "UNAUTHORIZED" => "UNAUTHORIZED",
        "FORBIDDEN" => "FORBIDDEN",
        "BOOTSTRAP_TIMEOUT" => "BOOTSTRAP_TIMEOUT",
        "RATE_LIMITED" => "RATE_LIMITED",
        "QUOTA_EXCEEDED" => "QUOTA_EXCEEDED",
        "HOME_LOCKED" => "HOME_LOCKED",
        "DAEMON_OWNS_HOME" => "DAEMON_OWNS_HOME",
        "REINDEX_REQUIRED" => "REINDEX_REQUIRED",
        "CANCELED" => "CANCELED",
        _ => "UNKNOWN",
    }
}

/// Bounded wait for a descriptor to appear (spawn-adjacent consumers).
pub fn wait_for_descriptor(runtime_dir: &Path, timeout: Duration) -> Option<Descriptor> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(descriptor) = read_descriptor(runtime_dir) {
            return Some(descriptor);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
