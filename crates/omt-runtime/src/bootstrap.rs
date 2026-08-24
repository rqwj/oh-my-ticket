//! Bootstrap lock — single-daemon election (U5a locked contract).
//!
//! `<runtime-dir>/bootstrap.lock`: O_EXCL create; the winner heartbeats
//! every 2 s IN PLACE (inode-preserving, like the home lock); a challenger
//! judges a contender stale after 10 s of silence (injectable clock) or an
//! immediately-dead pid. Stale locks are stolen (unlink + retry). The
//! winner then publishes the descriptor and serves. Losers poll the
//! descriptor for a live daemon up to `OMT_BOOTSTRAP_TIMEOUT_MS`
//! (default 15 s) and exit with problem code BOOTSTRAP_TIMEOUT otherwise.

use crate::descriptor;
use omt_storage::clock::{iso_from_ms, parse_iso_ms, MillisClock};
use serde::{Deserialize, Serialize};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const LOCK_SCHEMA_VERSION: i64 = 1;
/// Winner heartbeat cadence (ms).
pub const HEARTBEAT_INTERVAL_MS: i64 = 2_000;
/// A holder silent longer than this is dead (ms).
pub const DEFAULT_STALE_MS: i64 = 10_000;
/// Default loser descriptor-poll budget (ms).
pub const DEFAULT_POLL_TIMEOUT_MS: i64 = 15_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockBody {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    pub pid: i64,
    #[serde(rename = "bootToken")]
    pub boot_token: String,
    #[serde(rename = "acquiredAt")]
    pub acquired_at: String,
    #[serde(rename = "heartbeatAt")]
    pub heartbeat_at: String,
}

/// Outcome of one election attempt.
pub enum Election {
    /// This process won and holds the bootstrap lock.
    Winner(BootstrapGuard),
    /// A live daemon already serves this runtime dir.
    DaemonPresent(descriptor::Descriptor),
}

/// Acquire the bootstrap lock or recognize the live winner.
///
/// `clock` is injectable for deterministic staleness tests; production
/// passes [`omt_storage::clock::SystemClock`]. `poll_timeout` bounds how
/// long a contender waits for the winner's descriptor before reporting
/// BOOTSTRAP_TIMEOUT.
pub fn elect(
    runtime_dir: &Path,
    clock: Arc<dyn MillisClock>,
    stale_ms: i64,
    poll_timeout: Duration,
) -> Result<Election, omt_storage::Problem> {
    let lock_path = runtime_dir.join(crate::paths::BOOTSTRAP_LOCK_FILE);
    // Overall election budget: a challenger that neither wins the lock nor
    // finds a published descriptor within this window exits with
    // HOME_LOCKED-style BOOTSTRAP_TIMEOUT.
    let deadline = std::time::Instant::now() + poll_timeout;
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        if attempts > 2000 || std::time::Instant::now() >= deadline {
            return Err(omt_storage::Problem::with_details(
                crate::problem::BOOTSTRAP_TIMEOUT,
                "no live daemon appeared within the bootstrap poll budget",
                |d| {
                    d.insert("rule".into(), "poll-timeout".into());
                    d.insert(
                        "runtimeDir".into(),
                        runtime_dir.display().to_string().into(),
                    );
                },
            ));
        }
        match try_create(&lock_path, Arc::clone(&clock)) {
            Ok(Some(guard)) => return Ok(Election::Winner(guard)),
            Ok(None) => {
                // Contender exists: judge liveness.
                let verdict = judge(&lock_path, &*clock, stale_ms);
                match verdict {
                    Verdict::LiveDaemon => {
                        if let Some(d) = wait_for_live_descriptor(runtime_dir, poll_timeout) {
                            return Ok(Election::DaemonPresent(d));
                        }
                        // No descriptor appeared while the lock holder kept
                        // heartbeating — keep contending until either side
                        // yields (the poll timeout governs loser exit).
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Verdict::Stale => {
                        // Steal: unlink and retry the O_EXCL create.
                        let _ = std::fs::remove_file(&lock_path);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Verdict::Gone => continue,
                }
            }
            Err(problem) => return Err(problem),
        }
    }
}

/// Poll for a live daemon's descriptor; used by losers before giving up.
#[allow(dead_code)]
pub fn poll_descriptor(runtime_dir: &Path, timeout: Duration) -> Option<descriptor::Descriptor> {
    wait_for_live_descriptor(runtime_dir, timeout)
}

fn wait_for_live_descriptor(
    runtime_dir: &Path,
    timeout: Duration,
) -> Option<descriptor::Descriptor> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(d) = descriptor::read(runtime_dir) {
            // Staleness = PID liveness AND a live connect probe (U5a locked
            // contract): pid reuse or a half-dead predecessor fails the
            // probe and stays stale.
            if d.schema_version == descriptor::DESCRIPTOR_SCHEMA_VERSION
                && descriptor::pid_live(d.pid)
                && crate::ipc::probe(&d.endpoint, std::time::Duration::from_millis(250))
            {
                return Some(d);
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

enum Verdict {
    Gone,
    LiveDaemon,
    Stale,
}

fn judge(lock_path: &Path, clock: &dyn MillisClock, stale_ms: i64) -> Verdict {
    let raw = match std::fs::read_to_string(lock_path) {
        Ok(raw) => raw,
        Err(_) => return Verdict::Gone,
    };
    if raw.trim().is_empty() {
        // Creator died between O_EXCL and publish: grace window, then mtime.
        std::thread::sleep(Duration::from_millis(20));
        let fresh = std::fs::read_to_string(lock_path).unwrap_or_default();
        if !fresh.trim().is_empty() {
            return judge_body(&fresh, lock_path, clock, stale_ms);
        }
        return mtime_verdict(lock_path, clock, stale_ms);
    }
    judge_body(&raw, lock_path, clock, stale_ms)
}

fn judge_body(raw: &str, lock_path: &Path, clock: &dyn MillisClock, stale_ms: i64) -> Verdict {
    match serde_json::from_str::<LockBody>(raw) {
        Ok(body) => {
            if !descriptor::pid_live(body.pid) {
                return Verdict::Stale;
            }
            match parse_iso_ms(&body.heartbeat_at) {
                Some(stamp) => {
                    if clock.now_ms().saturating_sub(stamp) <= stale_ms {
                        Verdict::LiveDaemon
                    } else {
                        Verdict::Stale
                    }
                }
                None => mtime_verdict(lock_path, clock, stale_ms),
            }
        }
        Err(_) => mtime_verdict(lock_path, clock, stale_ms),
    }
}

fn mtime_verdict(lock_path: &Path, clock: &dyn MillisClock, stale_ms: i64) -> Verdict {
    let age = std::fs::metadata(lock_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .map(|stamp| clock.now_ms().saturating_sub(stamp));
    match age {
        Some(age) if age <= stale_ms => Verdict::LiveDaemon,
        _ => Verdict::Stale,
    }
}

fn now_body(clock: &dyn MillisClock, boot_token: &str) -> LockBody {
    let now = iso_from_ms(clock.now_ms());
    LockBody {
        schema_version: LOCK_SCHEMA_VERSION,
        pid: std::process::id() as i64,
        boot_token: boot_token.to_string(),
        acquired_at: now.clone(),
        heartbeat_at: now,
    }
}

fn try_create(
    lock_path: &Path,
    clock: Arc<dyn MillisClock>,
) -> Result<Option<BootstrapGuard>, omt_storage::Problem> {
    let boot_token = crate::problem::entropy::token_hex();
    let body = now_body(&*clock, &boot_token);
    let text = serde_json::to_string(&body).map_err(|err| {
        omt_storage::Problem::new(omt_domain::error::IO, format!("bootstrap serialize: {err}"))
    })?;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(lock_path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(err) => {
            return Err(omt_storage::Problem::new(
                omt_domain::error::IO,
                format!("bootstrap lock create failed: {err}"),
            ))
        }
    };
    let mut guard = BootstrapGuard {
        file: Some(file),
        path: lock_path.to_path_buf(),
        boot_token,
        stop: Arc::new(AtomicBool::new(false)),
        clock,
        heartbeat_ms: HEARTBEAT_INTERVAL_MS,
    };
    guard.write_body(&text)?;
    guard.spawn_heartbeat();
    Ok(Some(guard))
}

/// A held bootstrap election lease: marker file + in-place heartbeats +
/// token-checked release.
pub struct BootstrapGuard {
    file: Option<std::fs::File>,
    path: std::path::PathBuf,
    boot_token: String,
    stop: Arc<AtomicBool>,
    clock: Arc<dyn MillisClock>,
    heartbeat_ms: i64,
}

impl BootstrapGuard {
    pub fn boot_token(&self) -> &str {
        &self.boot_token
    }

    fn write_body(&mut self, text: &str) -> Result<(), omt_storage::Problem> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(text.as_bytes()))
            .and_then(|_| file.set_len(text.len() as u64))
            .map_err(|err| {
                omt_storage::Problem::new(omt_domain::error::IO, format!("bootstrap write: {err}"))
            })?;
        Ok(())
    }

    fn spawn_heartbeat(&self) {
        let me = BootstrapGuard {
            file: None,
            path: self.path.clone(),
            boot_token: self.boot_token.clone(),
            stop: Arc::clone(&self.stop),
            clock: Arc::clone(&self.clock),
            heartbeat_ms: self.heartbeat_ms,
        };
        // The heartbeat thread rewrites the marker through its OWN handle so
        // the winner's fd stays untouched; inode-preserving writes keep any
        // watcher-visible identity stable.
        std::thread::spawn(move || loop {
            if me.stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(me.heartbeat_ms.max(0) as u64));
            if me.stop.load(Ordering::SeqCst) {
                return;
            }
            let body = now_body(&*me.clock, &me.boot_token);
            if let Ok(text) = serde_json::to_string(&body) {
                let _ = rewrite_in_place(&me.path, &text);
            }
        });
    }

    /// Stop heartbeating; unlink ONLY when the marker still carries our
    /// token (a successor's lock is never touched). Idempotent.
    pub fn release(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(current) = std::fs::read_to_string(&self.path) {
            if let Ok(body) = serde_json::from_str::<LockBody>(&current) {
                if body.boot_token == self.boot_token {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
        self.file.take(); // close fd
    }
}

fn rewrite_in_place(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)?;
    Ok(())
}
