//! Daemon lifecycle configuration (U5b): `<runtime-dir>/daemon.json`.
//!
//! Precedence: COMPILED DEFAULTS < CONFIG FILE. The file is optional; when
//! present it must parse and every key must be well-typed — a malformed
//! value fails startup with INVALID_INPUT naming the key (fail closed: a
//! silently ignored limit or quiet period would mislead operators).
//!
//! Shape (all keys optional):
//! ```json
//! {
//!   "idleQuietMs": 1800000,          // 0 disables the idle watchdog
//!   "lockHeartbeatMs": 10000,        // per-home owner-lock heartbeat
//!   "log": {                         // rotating-log caps (maxFiles×maxBytes)
//!     "maxBytes": 5242880,
//!     "maxFiles": 3
//!   },
//!   "limits": {                      // overrides, see limits.rs
//!     "maxPayloadBytes": 8388608,
//!     "maxOpenHomes": 8,
//!     "maxRetainedEvents": 100000
//!   }
//! }
//! ```

use crate::limits::Limits;
use crate::logging::LogConfig;
use std::path::Path;

/// Compiled-in default quiet period: 30 minutes (plan §Daemon lifecycle).
pub const DEFAULT_IDLE_QUIET_MS: i64 = 1_800_000;
/// Default per-home owner-lock heartbeat (home_lock::HEARTBEAT_INTERVAL_MS).
pub const DEFAULT_LOCK_HEARTBEAT_MS: i64 = 10_000;

#[derive(Debug, Clone, PartialEq)]
pub struct DaemonConfig {
    /// Quiet-period idle shutdown. 0 = disabled (serve until SIGTERM).
    pub idle_quiet_ms: i64,
    /// Owner-lock heartbeat cadence while a home is held.
    pub lock_heartbeat_ms: i64,
    /// Rotating-log caps (`maxFiles × maxBytes` bound total log volume).
    pub log: LogConfig,
    /// Resource-limit overrides applied over [`Limits::default`].
    pub limits: Limits,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            idle_quiet_ms: DEFAULT_IDLE_QUIET_MS,
            lock_heartbeat_ms: DEFAULT_LOCK_HEARTBEAT_MS,
            log: LogConfig::default(),
            limits: Limits::default(),
        }
    }
}

impl DaemonConfig {
    /// Load `<runtime-dir>/daemon.json` over defaults. Absent file → pure
    /// defaults.
    pub fn load(runtime_dir: &Path) -> Result<DaemonConfig, omt_storage::Problem> {
        let path = runtime_dir.join(crate::paths::CONFIG_FILE);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DaemonConfig::default())
            }
            Err(err) => {
                return Err(omt_storage::Problem::with_details(
                    omt_domain::error::IO,
                    format!("config read failed: {err}"),
                    |d| {
                        d.insert("file".into(), path.display().to_string().into());
                    },
                ))
            }
        };
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|err| config_problem(&path, "parse", format!("{err}")))?;
        DaemonConfig::from_value(&value).map_err(|err| err.with_context(&path))
    }

    /// Apply the parsed document over `self` (defaults < file).
    pub fn from_value(value: &serde_json::Value) -> Result<DaemonConfig, ConfigError> {
        let mut config = DaemonConfig::default();
        let obj = value
            .as_object()
            .ok_or_else(|| ConfigError::new("root", "must be an object"))?;
        for (key, item) in obj {
            match key.as_str() {
                "idleQuietMs" => {
                    config.idle_quiet_ms = int_field(key, item)?;
                }
                "lockHeartbeatMs" => {
                    config.lock_heartbeat_ms = int_field(key, item)?;
                }
                "log" => {
                    let log_obj = item
                        .as_object()
                        .ok_or_else(|| ConfigError::new("log", "must be an object"))?;
                    apply_log_overrides(&mut config.log, log_obj)?;
                }
                "limits" => {
                    let limits_obj = item
                        .as_object()
                        .ok_or_else(|| ConfigError::new("limits", "must be an object"))?;
                    apply_limits_overrides(&mut config.limits, limits_obj)?;
                }
                other => {
                    return Err(ConfigError::new(other, "unknown configuration key"));
                }
            }
        }
        Ok(config)
    }

    /// Idle watchdog enabled?
    pub fn idle_enabled(&self) -> bool {
        self.idle_quiet_ms > 0
    }
}

fn apply_limits_overrides(
    limits: &mut Limits,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigError> {
    for (key, item) in obj {
        match key.as_str() {
            "maxPayloadBytes" => limits.max_payload_bytes = positive(key, item)? as usize,
            "maxListLimit" => limits.max_list_limit = positive(key, item)?,
            "maxEventBatch" => limits.max_event_batch = positive(key, item)?,
            "runConcurrency" => limits.run_concurrency = positive(key, item)?,
            "maxOpenHomes" => limits.max_open_homes = positive(key, item)? as usize,
            "maxConcurrentConnections" => {
                limits.max_concurrent_connections = positive(key, item)? as usize
            }
            "maxHomeQueueDepth" => limits.max_home_queue_depth = positive(key, item)? as usize,
            "maxSearchTermBytes" => limits.max_search_term_bytes = positive(key, item)? as usize,
            "maxIdempotencyEntries" => limits.max_idempotency_entries = positive(key, item)?,
            "maxRetainedEvents" => limits.max_retained_events = positive(key, item)? as usize,
            other => {
                return Err(ConfigError::new(
                    format!("limits.{other}"),
                    "unknown limit key",
                ))
            }
        }
    }
    Ok(())
}

fn apply_log_overrides(
    log: &mut LogConfig,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigError> {
    for (key, item) in obj {
        match key.as_str() {
            "maxBytes" => log.max_bytes = positive(key, item)? as u64,
            "maxFiles" => {
                let files = positive(key, item)?;
                // At least the active file plus one rollover generation,
                // otherwise rotation would be a no-op or self-truncating.
                if files < 2 {
                    return Err(ConfigError::new("log.maxFiles", "must be 2 or greater"));
                }
                log.max_files = files as usize;
            }
            other => return Err(ConfigError::new(format!("log.{other}"), "unknown log key")),
        }
    }
    Ok(())
}

fn int_field(key: &str, item: &serde_json::Value) -> Result<i64, ConfigError> {
    item.as_i64()
        .ok_or_else(|| ConfigError::new(key, "must be an integer (milliseconds)"))
}

fn positive(key: &str, item: &serde_json::Value) -> Result<i64, ConfigError> {
    let raw = int_field(key, item)?;
    if raw <= 0 {
        return Err(ConfigError::new(key, "must be a positive integer"));
    }
    Ok(raw)
}

/// One bad configuration key/value; rendered with its file context.
#[derive(Debug, Clone)]
pub struct ConfigError {
    field: String,
    message: String,
}

impl ConfigError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> ConfigError {
        ConfigError {
            field: field.into(),
            message: message.into(),
        }
    }

    fn with_context(self, path: &Path) -> omt_storage::Problem {
        omt_storage::Problem::with_details(
            omt_domain::error::INVALID_INPUT,
            format!("config {}.{}: {}", path.display(), self.field, self.message),
            |d| {
                d.insert("field".into(), self.field.into());
                d.insert("file".into(), path.display().to_string().into());
            },
        )
    }
}

fn config_problem(path: &Path, rule: &str, message: String) -> omt_storage::Problem {
    omt_storage::Problem::with_details(
        omt_domain::error::INVALID_INPUT,
        format!("config {} unreadable ({rule}): {message}", path.display()),
        |d| {
            d.insert("rule".into(), rule.into());
            d.insert("file".into(), path.display().to_string().into());
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_hold_plan_values() {
        let config = DaemonConfig::default();
        assert_eq!(config.idle_quiet_ms, 1_800_000);
        assert!(config.idle_enabled());
        assert_eq!(config.lock_heartbeat_ms, 10_000);
    }

    #[test]
    fn file_overrides_win_over_defaults() {
        let config =
            DaemonConfig::from_value(&json!({ "idleQuietMs": 250, "lockHeartbeatMs": 50 }))
                .expect("config");
        assert_eq!(config.idle_quiet_ms, 250);
        assert_eq!(config.lock_heartbeat_ms, 50);
    }

    #[test]
    fn zero_disables_idle_and_limits_merge_shallow() {
        let config = DaemonConfig::from_value(&json!({
            "idleQuietMs": 0,
            "limits": { "maxOpenHomes": 2 }
        }))
        .expect("config");
        assert!(!config.idle_enabled());
        assert_eq!(config.limits.max_open_homes, 2);
        assert_eq!(
            config.limits.max_payload_bytes,
            Limits::default().max_payload_bytes
        );
    }

    #[test]
    fn unknown_keys_fail_closed() {
        let err = DaemonConfig::from_value(&json!({ "nope": true })).unwrap_err();
        assert_eq!(err.field, "nope");
        let err = DaemonConfig::from_value(&json!({ "limits": { "nope": 1 } })).unwrap_err();
        assert_eq!(err.field, "limits.nope");
        let err = DaemonConfig::from_value(&json!({ "log": { "nope": 1 } })).unwrap_err();
        assert_eq!(err.field, "log.nope");
        assert!(DaemonConfig::from_value(&json!({ "idleQuietMs": "soon" })).is_err());
    }

    #[test]
    fn log_overrides_shape_rotation_caps() {
        let config =
            DaemonConfig::from_value(&json!({ "log": { "maxBytes": 4096, "maxFiles": 5 } }))
                .expect("config");
        assert_eq!(config.log.max_bytes, 4096);
        assert_eq!(config.log.max_files, 5);
        // Defaults survive a partial override.
        assert_eq!(
            DaemonConfig::from_value(&json!({ "log": { "maxFiles": 4 } }))
                .expect("config")
                .log
                .max_bytes,
            crate::logging::DEFAULT_MAX_BYTES
        );
    }

    #[test]
    fn log_keys_are_well_typed_and_bounded() {
        let err = DaemonConfig::from_value(&json!({ "log": { "maxBytes": "big" } })).unwrap_err();
        assert_eq!(err.field, "maxBytes");
        // maxFiles < 2 would make rotation meaningless: refuse.
        let err = DaemonConfig::from_value(&json!({ "log": { "maxFiles": 1 } })).unwrap_err();
        assert_eq!(err.field, "log.maxFiles");
        assert!(
            DaemonConfig::from_value(&json!({ "log": "rot" })).is_err(),
            "non-object log section refuses"
        );
    }
}
