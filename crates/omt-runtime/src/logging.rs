//! Size-capped rotating daemon log (U5b): `<runtime-dir>/logs/omt-daemon.log`
//! with `omt-daemon.log.1` … `omt-daemon.log.(maxFiles-1)` rollovers.
//! Total on-disk volume stays under `max_files × max_bytes` — the R21
//! "log volume" bound is enforced by construction, not by a janitor.
//!
//! Redaction (R12/KTD9): EVERY line passes through
//! [`crate::problem::redact`] before touching disk, so credential-shaped
//! hex can never land in a log even when a caller forgets to scrub. Log
//! entries record codes/details/counts — never request payloads, tokens,
//! or argv.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Default active-file cap: 5 MiB before rollover.
pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Default retained generations (active + this many rolled files).
pub const DEFAULT_MAX_FILES: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct LogConfig {
    pub max_bytes: u64,
    pub max_files: usize,
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

struct LogState {
    dir: PathBuf,
    config: LogConfig,
    file: Option<std::fs::File>,
    written: u64,
    dropped: bool,
}

static STATE: Mutex<Option<LogState>> = Mutex::new(None);

/// Install the process-wide logger writing under
/// `<runtime-dir>/logs/`. Best-effort: a failing log directory must never
/// take the daemon down; the logger then degrades to a no-op.
pub fn init(runtime_dir: &std::path::Path, config: &LogConfig) {
    let dir = runtime_dir.join("logs");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "{}",
            serde_json::json!({
                "code": "IO",
                "message": crate::problem::redact(&format!("log dir create failed: {err}")),
            })
        );
        return;
    }
    let file = open_active(&dir);
    let written = file
        .as_ref()
        .and_then(|file| file.metadata().ok())
        .map(|meta| meta.len())
        .unwrap_or(0);
    *STATE.lock().expect("log state") = Some(LogState {
        dir,
        config: config.clone(),
        file,
        written,
        dropped: false,
    });
}

fn open_active(dir: &std::path::Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("omt-daemon.log"))
        .ok()
}

/// Append one structured line. Level + code + message only; message text is
/// redacted defensively. Never panics, never blocks indefinitely, and
/// silently degrades once the disk refuses writes (bounded-log guarantee
/// outranks completeness).
pub fn log(level: &str, code: &str, message: &str) {
    let line = serde_json::json!({
        "ts": chrono_iso_now(),
        "level": level,
        "code": code,
        "message": crate::problem::redact(message),
    })
    .to_string();
    let mut guard = STATE.lock().expect("log state");
    let Some(state) = guard.as_mut() else { return };
    if state.dropped {
        return;
    }
    if state.written.saturating_add(line.len() as u64) > state.config.max_bytes {
        rotate(state);
    }
    let Some(file) = state.file.as_mut() else {
        return;
    };
    match file
        .write_all(line.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
    {
        Ok(()) => state.written += line.len() as u64 + 1,
        Err(_) => {
            // Disk refused (full/rotated away): stop trying rather than
            // growing unbounded or crashing the daemon.
            state.dropped = true;
        }
    }
}

/// Shift `omt-daemon.log.(n-1)` → `.n`, current → `.1`, and reopen a fresh
/// active file. Oldest generation beyond `max_files - 1` is deleted, so the
/// directory holds at most `max_files × max_bytes` bytes of log volume.
fn rotate(state: &mut LogState) {
    let keep = state.config.max_files.saturating_sub(1).max(1);
    // Delete oldest first.
    let oldest = state.dir.join(format!("omt-daemon.log.{keep}"));
    let _ = std::fs::remove_file(&oldest);
    for index in (1..keep).rev() {
        let from = state.dir.join(format!("omt-daemon.log.{index}"));
        let to = state.dir.join(format!("omt-daemon.log.{}", index + 1));
        let _ = std::fs::rename(&from, &to);
    }
    state.file = None; // close handle before renaming
    let _ = std::fs::rename(
        state.dir.join("omt-daemon.log"),
        state.dir.join("omt-daemon.log.1"),
    );
    state.file = open_active(&state.dir);
    state.written = 0;
}

/// Wall-clock ISO stamp for log lines (diagnostic only — protocol timestamps
/// come from the injected clock domain instead).
fn chrono_iso_now() -> String {
    omt_storage::clock::iso_from_ms(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    // The logger slot is process-global: serialize the tests over it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn install_fresh(max_bytes: u64, max_files: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = dir.path().join("rt");
        std::fs::create_dir_all(&runtime).expect("mkdir");
        // The global slot is process-wide: serialize rotation-proof tests
        // through the same mutex the logger uses.
        *STATE.lock().expect("log state") = None;
        init(
            &runtime,
            &LogConfig {
                max_bytes,
                max_files,
            },
        );
        dir
    }

    fn read_dir_logs(dir: &tempfile::TempDir) -> Vec<String> {
        let logs = dir.path().join("rt").join("logs");
        let mut names: Vec<String> = std::fs::read_dir(&logs)
            .expect("logs dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn rollover_keeps_volume_under_max_files_x_max_bytes() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = install_fresh(200, 3);
        for index in 0..40 {
            log("info", "TEST", &format!("line-{index}-{}", "x".repeat(30)));
        }
        let names = read_dir_logs(&dir);
        assert!(
            names.contains(&"omt-daemon.log".to_string()),
            "active file present: {names:?}"
        );
        assert!(
            names.contains(&"omt-daemon.log.1".to_string()),
            "first rollover present: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "omt-daemon.log.3"),
            "oldest beyond max_files pruned: {names:?}"
        );
        let total: u64 = names
            .iter()
            .map(|name| {
                std::fs::metadata(dir.path().join("rt").join("logs").join(name))
                    .map(|meta| meta.len())
                    .unwrap_or(0)
            })
            .sum();
        assert!(
            total <= 200 * 3 + /* line-boundary slack */ 400,
            "total log volume bounded: {total}"
        );
    }

    #[test]
    fn every_logged_line_is_redacted_token_shaped() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _dir = install_fresh(u64::MAX, 3); // owns the log dir until scope end
        let token = "a".repeat(64);
        log("warn", "TEST", &format!("credential {token} leaked?"));
        let guard = STATE.lock().expect("log state");
        let state = guard.as_ref().expect("installed");
        let active = std::fs::read_to_string(state.dir.join("omt-daemon.log")).expect("log");
        assert!(!active.contains(&token), "raw token reached the log");
        assert!(active.contains("[redacted]"), "redaction marker missing");
    }
}
