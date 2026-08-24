//! Shared fixtures for the omt-runtime suites: temp runtime dirs, spawned
//! daemon processes, and a minimal newline-delimited JSON-RPC test client.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub static COUNTER: AtomicU64 = AtomicU64::new(1);

/// One spawned `omt-daemon` process bound to a temp runtime dir.
pub struct DaemonProcess {
    child: std::sync::Mutex<Child>,
    stderr_path: PathBuf,
}

impl DaemonProcess {
    pub fn spawn(ctx: &TestCtx, extra_args: &[&str]) -> DaemonProcess {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let stderr_path = ctx.dir.path().join(format!("daemon-{id}.stderr"));
        let stderr_file = std::fs::File::create(&stderr_path).expect("create stderr capture");
        let child = Command::new(bin_path())
            .arg("--runtime-dir")
            .arg(ctx.runtime_dir_str())
            .args(extra_args)
            .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .expect("spawn omt-daemon");
        DaemonProcess {
            child: std::sync::Mutex::new(child),
            stderr_path,
        }
    }

    pub fn pid(&self) -> i64 {
        self.child.lock().expect("child lock").id() as i64
    }

    /// None while the process is still running.
    pub fn try_wait(&self) -> Option<std::process::ExitStatus> {
        self.child
            .lock()
            .expect("child lock")
            .try_wait()
            .expect("try_wait")
    }

    pub fn is_alive(&self) -> bool {
        self.try_wait().is_none()
    }

    /// Poll until the process exits or the timeout elapses; returns captured
    /// stderr when it exited.
    pub fn wait_with_timeout(&mut self, timeout: Duration) -> Option<ExitInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self
                .child
                .lock()
                .expect("child lock")
                .try_wait()
                .expect("try_wait")
            {
                return Some(ExitInfo {
                    code: status.code(),
                    stderr: self.stderr_text(),
                });
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn stderr_text(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    pub fn kill(&mut self) {
        let _ = self.child.lock().expect("child lock").kill();
        let _ = self.child.lock().expect("child lock").wait();
    }
}

pub struct ExitInfo {
    pub code: Option<i32>,
    pub stderr: String,
}

/// Per-suite context: one shared temp dir holding the runtime dir + home.
pub struct TestCtx {
    pub dir: tempfile::TempDir,
    pub runtime_dir: PathBuf,
    pub home: PathBuf,
    pub global_home: PathBuf,
}

impl TestCtx {
    /// Fresh isolated runtime dir + workspace home + global home.
    pub fn spawn_named(tag: &str) -> TestCtx {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_dir = dir.path().join("rt");
        std::fs::create_dir_all(&runtime_dir).expect("mkdir rt");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let global_home = dir.path().join("global-home");
        std::fs::create_dir_all(&global_home).expect("mkdir global");
        let _ = tag;
        TestCtx {
            dir,
            runtime_dir,
            home,
            global_home,
        }
    }

    pub fn spawn() -> TestCtx {
        TestCtx::spawn_named("t")
    }

    pub fn runtime_dir_str(&self) -> &str {
        self.runtime_dir.to_str().expect("utf8 runtime dir")
    }

    pub fn home_str(&self) -> &str {
        self.home.to_str().expect("utf8 home")
    }

    pub fn global_home_str(&self) -> &str {
        self.global_home.to_str().expect("utf8 global home")
    }
}

// ── descriptor ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Descriptor {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    pub endpoint: String,
    pub generation: i64,
    pub pid: i64,
    #[serde(rename = "bootToken")]
    pub boot_token: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
}

impl Descriptor {
    pub fn read(runtime_dir: &Path) -> Option<Descriptor> {
        let raw = std::fs::read_to_string(runtime_dir.join("descriptor.json")).ok()?;
        serde_json::from_str(&raw).ok()
    }
}

/// Poll until a descriptor appears whose pid is alive.
pub fn wait_for_descriptor(runtime_dir: &Path, timeout: Duration) -> Option<Descriptor> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(descriptor) = Descriptor::read(runtime_dir) {
            if pid_alive(descriptor.pid) {
                return Some(descriptor);
            }
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn wait_descriptor_gone(runtime_dir: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match Descriptor::read(runtime_dir) {
            None => return true,
            Some(d) => {
                if !pid_alive(d.pid) {
                    return true;
                }
            }
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn bin_path() -> PathBuf {
    // Integration tests get the freshly built binary via CARGO_BIN_EXE_.
    PathBuf::from(env!("CARGO_BIN_EXE_omt-daemon"))
}

#[cfg(unix)]
fn pid_alive(pid: i64) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: i64) -> bool {
    false
}

// ── minimal NDJSON JSON-RPC client ──────────────────────────────────────

pub struct TestClient {
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
    next_id: u64,
    /// Pending server-pushed notifications (method != response).
    pub notifications: Vec<String>,
}

#[derive(Debug)]
pub enum RpcError {
    Io(String),
    Problem {
        code: String,
        details: serde_json::Value,
    },
}

impl TestClient {
    pub fn connect(endpoint: &str) -> Result<TestClient, RpcError> {
        let stream = std::os::unix::net::UnixStream::connect(endpoint)
            .map_err(|err| RpcError::Io(format!("connect {endpoint}: {err}")))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|err| RpcError::Io(err.to_string()))?;
        let writer = stream
            .try_clone()
            .map_err(|err| RpcError::Io(err.to_string()))?;
        Ok(TestClient {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            notifications: Vec::new(),
        })
    }

    pub fn send_line(&mut self, line: &str) -> Result<(), RpcError> {
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.write_all(b"\n"))
            .and_then(|_| self.writer.flush())
            .map_err(|err| RpcError::Io(err.to_string()))
    }

    /// Send a request; return the raw result value. Errors map to
    /// RpcError::Problem keyed on error.data.code.
    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send_line(&line.to_string())?;
        loop {
            let mut buf = String::new();
            let read = self
                .reader
                .read_line(&mut buf)
                .map_err(|err| RpcError::Io(err.to_string()))?;
            if read == 0 {
                return Err(RpcError::Io("connection closed".into()));
            }
            let value: serde_json::Value =
                serde_json::from_str(buf.trim()).map_err(|err| RpcError::Io(err.to_string()))?;
            if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(error) = value.get("error") {
                    let code = error
                        .pointer("/data/code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("UNKNOWN")
                        .to_string();
                    let details = error
                        .pointer("/data/details")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    return Err(RpcError::Problem { code, details });
                }
                return Ok(value
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
            // Server-initiated notification — park it for event assertions.
            self.notifications.push(buf.trim().to_string());
        }
    }

    /// Wait until a notification with the given method arrives (draining).
    pub fn wait_notification(
        &mut self,
        method: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, RpcError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = self.notifications.clone();
            for line in remaining {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    if value.get("method").and_then(|v| v.as_str()) == Some(method) {
                        self.notifications.retain(|l| l != &line);
                        return Ok(value.get("params").cloned().unwrap_or_default());
                    }
                }
            }
            if Instant::now() > deadline {
                return Err(RpcError::Io(format!("timeout waiting for {method}")));
            }
            let mut buf = [0u8; 1];
            // Peek via read with timeout: rely on read_line blocking with the
            // socket read timeout; poll notifications buffer first.
            let _ = &mut buf;
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Err(RpcError::Io("connection closed".into())),
                Ok(_) => {
                    self.notifications.push(line.trim().to_string());
                }
                Err(err) => return Err(RpcError::Io(err.to_string())),
            }
        }
    }
}

/// Perform the handshake/enrollment exchange; returns the credential object.
pub fn enroll(
    client: &mut TestClient,
    kind: &str,
    scopes: serde_json::Value,
) -> Result<(serde_json::Value, serde_json::Value), RpcError> {
    let params = serde_json::json!({
        "protocolVersion": "1.0",
        "client": { "kind": kind, "name": format!("omt-test-{kind}") },
        "requestedScopes": scopes,
    });
    let result = client.call("handshake/request", params)?;
    let credential = result
        .get("credential")
        .cloned()
        .ok_or_else(|| RpcError::Io("handshake returned no credential".into()))?;
    Ok((result, credential))
}

/// Convenience: connect + enroll as the given kind against a live endpoint.
pub fn connected_client(
    endpoint: &str,
    kind: &str,
) -> Result<(TestClient, serde_json::Value), RpcError> {
    let mut client = TestClient::connect(endpoint)?;
    let (_, credential) = enroll(&mut client, kind, serde_json::json!({}))?;
    Ok((client, credential))
}

/// Attach the credential token to method params (`params.credential.token`).
pub fn authed(params: serde_json::Value, credential: &serde_json::Value) -> serde_json::Value {
    let mut params = params;
    let obj = params.as_object_mut().expect("params object");
    obj.insert(
        "credential".into(),
        serde_json::json!({ "token": credential["token"].clone() }),
    );
    params
}

/// Read a file's contents (helper for lock-marker assertions).
pub fn read_maybe(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[allow(unused)]
fn _assert_reader_in_use(r: &mut impl Read) {}
