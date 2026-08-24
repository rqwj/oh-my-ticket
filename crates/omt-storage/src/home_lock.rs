//! Kernel home ownership (U4a/R2): the daemon-side layer over the U2b marker
//! contract (`src/host/home-lock.ts`), byte-compatible with it.
//!
//! Contract (locked cross-language):
//! - Path `<home>/home.lock`, JSON body
//!   `{schemaVersion:1, ownerKind:"ts-bridge"|"daemon", pid, hostname?,
//!   acquiredAt, heartbeatAt, token}` — field order preserved.
//! - Refusal matrix: `ownerKind:"daemon"` → `DAEMON_OWNS_HOME` ALWAYS (even
//!   stale); unknown/future `schemaVersion` → `HOME_LOCKED` fail closed,
//!   never stolen; live holder → `HOME_LOCKED {pid, acquiredAt}`; stale
//!   (>30 s silent) → steal; corrupt/empty body → mtime liveness fallback.
//! - Heartbeat every 10 s, stale window 30 s, injectable clock.
//!
//! Kernel layer (closing the documented U2b TOCTOU residual): after winning
//! the marker the owner takes an exclusive advisory flock ON THE SAME FILE
//! (fs4 `try_lock_exclusive`; the lease lives on the open file description
//! until release, so the handle keeps the fd for its whole lifetime).
//! Stealing a stale marker first probes that flock — a live daemon's lease
//! refuses the steal (`HOME_LOCKED`) even though its heartbeat went silent.
//! Heartbeats rewrite the body IN PLACE (inode-preserving) so the held flock
//! never detaches. Release unlinks only when the token still matches.

use crate::clock::{iso_from_ms, parse_iso_ms};
use crate::files::DiskFiles;
use crate::{Problem, Result};
use omt_domain::error;
use serde::{Deserialize, Serialize};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

/// Cross-language fixed lock path inside the home.
pub const LOCK_FILE_NAME: &str = "home.lock";

/// Marker schema version written by this implementation.
pub const LOCK_SCHEMA_VERSION: i64 = 1;

/// A holder silent longer than this is dead (ms) — matches TS DEFAULT_STALE_MS.
pub const DEFAULT_STALE_MS: i64 = 30_000;

/// Heartbeat cadence while held (ms); 0 disables — matches TS.
pub const HEARTBEAT_INTERVAL_MS: i64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    TsBridge,
    Daemon,
}

impl OwnerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OwnerKind::TsBridge => "ts-bridge",
            OwnerKind::Daemon => "daemon",
        }
    }
}

impl std::fmt::Display for OwnerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One owner-lock document. Field ORDER matters: serde serializes in
/// declaration order to stay byte-compatible with the TS writer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockBody {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    #[serde(rename = "ownerKind")]
    pub owner_kind: String,
    pub pid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(rename = "acquiredAt")]
    pub acquired_at: String,
    #[serde(rename = "heartbeatAt")]
    pub heartbeat_at: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct LockConfig {
    pub owner_kind: OwnerKind,
    pub hostname: String,
    pub stale_ms: i64,
    pub heartbeat_ms: i64,
}

impl Default for LockConfig {
    fn default() -> Self {
        LockConfig {
            owner_kind: OwnerKind::Daemon,
            hostname: default_hostname(),
            stale_ms: DEFAULT_STALE_MS,
            heartbeat_ms: HEARTBEAT_INTERVAL_MS,
        }
    }
}

pub(crate) fn default_hostname() -> String {
    match std::env::var("HOSTNAME") {
        Ok(name) if !name.is_empty() => name,
        _ => "localhost".to_string(),
    }
}

enum Inspect {
    Gone,
    Empty,
    Corrupt,
    Body(LockBody),
}

fn inspect(files: &DiskFiles) -> Result<Inspect> {
    match files.read_optional(LOCK_FILE_NAME)? {
        None => Ok(Inspect::Gone),
        Some(raw) if raw.trim().is_empty() => Ok(Inspect::Empty),
        Some(raw) => match serde_json::from_str::<LockBody>(&raw) {
            Ok(body) => Ok(Inspect::Body(body)),
            Err(_) => Ok(Inspect::Corrupt),
        },
    }
}

fn parse_body(raw: Option<String>) -> Option<LockBody> {
    raw.and_then(|text| serde_json::from_str(&text).ok())
}

fn home_locked(home: &str, details: serde_json::Value) -> Problem {
    Problem {
        code: error::HOME_LOCKED,
        message: format!("home {home} is already owned by another writer"),
        details: Some(details),
    }
}

fn daemon_owns(home: &str, body: &LockBody) -> Problem {
    Problem {
        code: error::DAEMON_OWNS_HOME,
        message: format!(
            "home {} is owned by an omt-daemon (pid {:?}); close the daemon or remove its owner marker",
            home, body.pid
        ),
        details: Some(serde_json::json!({ "owner": body })),
    }
}

/// Acquire the home lock: marker semantics exactly like the TS bridge, then
/// the kernel flock layered on the same file. Stale ts-bridge markers are
/// stolen transparently; a stale marker whose inode is STILL flocked by a
/// living daemon fails closed instead of double-owning.
pub fn acquire(
    home: &std::path::Path,
    config: &LockConfig,
    clock: Arc<dyn crate::clock::MillisClock>,
) -> Result<HomeLockHandle> {
    let home_text = home.to_string_lossy().into_owned();
    std::fs::create_dir_all(home).map_err(|err| {
        Problem::with_details(
            error::IO,
            format!("cannot create home {}: {err}", home.display()),
            |d| {
                d.insert("home".into(), home.display().to_string().into());
            },
        )
    })?;
    let files = DiskFiles::new(home);

    let mut steals = 0usize;
    let mut attempts = 0usize;
    while attempts < 200 {
        attempts += 1;
        if let Some(handle) = try_create(&files, config, &clock)? {
            return Ok(handle);
        }

        // Contender present: inspect and apply the refusal matrix.
        let mut verdict = inspect(&files)?;
        if matches!(verdict, Inspect::Gone) {
            continue;
        }
        // Empty body = a creator died between O_EXCL create and publish;
        // grace window, then judge by mtime like the TS reader.
        if matches!(verdict, Inspect::Empty) {
            for _ in 0..50 {
                std::thread::sleep(std::time::Duration::from_millis(2));
                verdict = inspect(&files)?;
                if !matches!(verdict, Inspect::Empty) {
                    break;
                }
            }
            match verdict {
                Inspect::Gone => continue,
                Inspect::Empty => verdict = Inspect::Corrupt,
                other => verdict = other,
            }
        }

        match verdict {
            Inspect::Body(ref body) if body.owner_kind == "daemon" => {
                return Err(daemon_owns(&home_text, body));
            }
            Inspect::Body(ref body) if body.schema_version != LOCK_SCHEMA_VERSION => {
                return Err(home_locked(
                    &home_text,
                    serde_json::json!({
                        "pid": body.pid,
                        "acquiredAt": body.acquired_at,
                        "schemaVersion": body.schema_version,
                    }),
                ));
            }
            Inspect::Body(ref body) => {
                // Live-holder decision on heartbeat age (injected clock).
                let age = clock
                    .now_ms()
                    .saturating_sub(parse_iso_ms(&body.heartbeat_at).unwrap_or(i64::MAX));
                if age <= config.stale_ms {
                    return Err(home_locked(
                        &home_text,
                        serde_json::json!({ "pid": body.pid, "acquiredAt": body.acquired_at }),
                    ));
                }
                // Stale marker: kernel probe BEFORE stealing. A surviving
                // flock means the previous daemon still lives — never
                // double-own behind its back (fail closed).
                if inode_is_flocked(&files)? {
                    return Err(home_locked(
                        &home_text,
                        serde_json::json!({
                            "pid": body.pid,
                            "acquiredAt": body.acquired_at,
                            "reason": "kernel-flock-held",
                        }),
                    ));
                }
                steals += 1;
                if steals > 8 {
                    return Err(home_locked(
                        &home_text,
                        serde_json::json!({ "pid": body.pid, "acquiredAt": body.acquired_at, "reason": "steal-thrash" }),
                    ));
                }
                let _ = std::fs::remove_file(files.resolve(LOCK_FILE_NAME)?);
            }
            corrupt_or_empty @ (Inspect::Corrupt | Inspect::Empty) => {
                let _ = corrupt_or_empty;
                // Corrupt/empty beyond grace: mtime liveness fallback.
                let mtime = crate::files::mtime_ms(&files, LOCK_FILE_NAME);
                let age = mtime.map_or(i64::MAX, |stamp| clock.now_ms().saturating_sub(stamp));
                if age <= config.stale_ms {
                    return Err(home_locked(
                        &home_text,
                        serde_json::json!({ "pid": serde_json::Value::Null, "acquiredAt": serde_json::Value::Null }),
                    ));
                }
                steals += 1;
                if steals > 8 {
                    return Err(home_locked(
                        &home_text,
                        serde_json::json!({ "pid": serde_json::Value::Null, "acquiredAt": serde_json::Value::Null, "reason": "steal-thrash" }),
                    ));
                }
                let _ = std::fs::remove_file(files.resolve(LOCK_FILE_NAME)?);
            }
            Inspect::Gone => continue,
        }
    }
    Err(home_locked(
        &home_text,
        serde_json::json!({ "reason": "attempts-exhausted" }),
    ))
}

/// Public probe for interop tests and takeover tooling: does any process
/// hold an advisory flock on `<home>/home.lock` right now?
pub fn inode_is_flocked_public(home: &std::path::Path) -> Result<bool> {
    inode_is_flocked(&DiskFiles::new(home))
}

/// True when some process holds an advisory flock on the CURRENT inode at the
/// lock path (non-blocking probe).
fn inode_is_flocked(files: &DiskFiles) -> Result<bool> {
    use fs4::{FileExt, TryLockError};
    let path = files.resolve(LOCK_FILE_NAME)?;
    let probe = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(Problem::new(
                error::IO,
                format!("lock probe open failed: {err}"),
            ))
        }
    };
    match FileExt::try_lock(&probe) {
        Ok(()) => Ok(false), // nobody else holds it (our probe just did)
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(err)) => {
            Err(Problem::new(error::IO, format!("lock probe failed: {err}")))
        }
    }
}

/// O_EXCL create + body publish + kernel flock + confirm-token, mirroring the
/// TS `tryCreate` plus U4a's flock layer.
fn try_create(
    files: &DiskFiles,
    config: &LockConfig,
    clock: &Arc<dyn crate::clock::MillisClock>,
) -> Result<Option<HomeLockHandle>> {
    use fs4::{FileExt, TryLockError};
    let token = crate::files::entropy_token();
    let path: PathBuf = files.resolve(LOCK_FILE_NAME)?;
    let now_text = iso_from_ms(clock.now_ms());
    let mut body = LockBody {
        schema_version: LOCK_SCHEMA_VERSION,
        owner_kind: config.owner_kind.as_str().to_string(),
        pid: Some(std::process::id() as i64),
        hostname: Some(config.hostname.clone()),
        acquired_at: now_text.clone(),
        heartbeat_at: now_text,
        token: token.clone(),
    };
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(err) => {
            return Err(Problem::new(
                error::IO,
                format!("lock create failed: {err}"),
            ))
        }
    };
    // Publish the body first (readers expect a complete document ASAP), then
    // take the kernel lease.
    publish_body(&mut file, &body)?;
    match FileExt::try_lock(&file) {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            // Another daemon won this inode's lease between our create and
            // our lock request: withdraw OUR marker entirely and retry
            // through the matrix loop (never leave a phantom live holder).
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        Err(TryLockError::Error(err)) => {
            return Err(Problem::new(error::IO, format!("lock flock failed: {err}")))
        }
    }

    // Confirm we still own the visible file (a stealer may have replaced it
    // between create and publish): the token must match.
    let current = parse_body(files.read_optional(LOCK_FILE_NAME)?);
    match current {
        Some(visible) if visible.token == token => {
            body.hostname = visible.hostname.clone();
            Ok(Some(HomeLockHandle {
                home: files.root().to_path_buf(),
                token,
                body: visible,
                file: Some(file),
                clock: std::sync::Arc::clone(clock),
                heartbeat_ms: config.heartbeat_ms,
                lost: std::sync::atomic::AtomicBool::new(false),
                released: std::sync::atomic::AtomicBool::new(false),
            }))
        }
        _ => {
            // Withdraw ours (best effort) and retry.
            drop(file);
            let _ = std::fs::remove_file(&path);
            Ok(None)
        }
    }
}

/// Serialize the body into an ALREADY-OPEN file WITHOUT replacing the inode
/// (an atomic rename here would silently detach the held flock), then fsync.
fn publish_body(file: &mut std::fs::File, body: &LockBody) -> Result<()> {
    let text = serde_json::to_string(body)
        .map_err(|err| Problem::new(error::IO, format!("lock serialize: {err}")))?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(text.as_bytes()))
        .and_then(|_| file.set_len(text.len() as u64))
        .and_then(|_| file.sync_all())
        .map_err(|err| Problem::new(error::IO, format!("lock write failed: {err}")))
}

/// A held home ownership: marker + advisory flock on `<home>/home.lock`.
pub struct HomeLockHandle {
    home: PathBuf,
    token: String,
    body: LockBody,
    /// The locked open file description — holding this IS holding the flock.
    file: Option<std::fs::File>,
    clock: Arc<dyn crate::clock::MillisClock>,
    heartbeat_ms: i64,
    lost: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
}

impl HomeLockHandle {
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn body(&self) -> &LockBody {
        &self.body
    }

    pub fn home(&self) -> &std::path::Path {
        &self.home
    }

    pub fn heartbeat_interval_ms(&self) -> i64 {
        self.heartbeat_ms
    }

    /// True once another writer replaced/removed our lock (refreshing stops).
    pub fn is_lost(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Refresh `heartbeatAt` IN PLACE under the held fd — the inode (and thus
    /// our flock) survives. Silently becomes inert when ownership was lost,
    /// exactly like the TS handle.
    pub fn heartbeat(&mut self) -> Result<()> {
        if self.released.load(std::sync::atomic::Ordering::SeqCst)
            || self.lost.load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let files = DiskFiles::new(&self.home);
        match parse_body(files.read_optional(LOCK_FILE_NAME)?) {
            Some(body) if body.token == self.token => {}
            _ => {
                self.lost.store(true, std::sync::atomic::Ordering::SeqCst);
                return Ok(());
            }
        }
        self.body.heartbeat_at = iso_from_ms(self.clock.now_ms());
        let Some(file) = self.file.as_mut() else {
            self.lost.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        };
        publish_body(file, &self.body)
    }

    /// Stop heartbeating; unlink ONLY when the on-disk body still carries our
    /// token (a successor's lock is never touched). Idempotent; dropping the
    /// fd releases the kernel flock.
    pub fn release(mut self) -> Result<()> {
        if self
            .released
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let files = DiskFiles::new(&self.home);
        let path = files.resolve(LOCK_FILE_NAME)?;
        let parsed = parse_body(std::fs::read_to_string(&path).ok());
        if parsed.is_some_and(|body| body.token == self.token) {
            let _ = std::fs::remove_file(&path);
        }
        self.file.take(); // close fd → kernel releases the flock
        Ok(())
    }
}
