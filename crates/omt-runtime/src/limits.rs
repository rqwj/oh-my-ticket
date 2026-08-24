//! Resource limits (R21/U5b): every externally influenced quantity is
//! bounded, and each bound degrades FAIRLY — cheap checks run before
//! expensive ones and every refusal carries a registered Problem code
//! (`schema/problems.schema.json`):
//!
//! - `RATE_LIMITED` — transient capacity: concurrent connections at cap,
//!   per-home queue at depth. The request never started; retry after
//!   backoff can succeed.
//! - `QUOTA_EXCEEDED` — durable resource: opened-homes count, idempotent-
//!   operation registry at its retention cap. Retrying without changing
//!   usage cannot succeed until the resource shrinks.
//! - `INVALID_INPUT` — client-payload bounds (payload bytes over
//!   `maxPayloadBytes`, search term over `maxSearchTermBytes`, event page
//!   over `maxEventBatch`); these are caller mistakes, not load signals.
//!
//! Fair order along the request path (documented contract, asserted by
//! tests/limits_matrix.spec.rs):
//! 1. wire framing + payload size (byte count, pre-parse),
//! 2. connection admission (accept-time counter, no protocol work done),
//! 3. credential validation (registry lookup),
//! 4. parity / operation / home scope (pure lookups),
//! 5. per-home queue admission (`try_send`, O(1)),
//! 6. durable-quota probes (one indexed COUNT for the idempotency table).
//!
//! Defaults are compiled in; `daemon.json` `limits` overrides them
//! (precedence defaults < file, see [`crate::config`]).

use serde_json::{json, Value};

/// Compiled-in defaults (U5a advertised values kept stable; the rest sized
/// to the plan's blocking envelope: 8 opened homes × concurrent clients ×
/// active runs).
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// Maximum wire payload per JSON-RPC line (bytes).
    pub max_payload_bytes: usize,
    /// Upper bound of any list/page `limit` parameter.
    pub max_list_limit: i64,
    /// Upper bound of one events/resume page.
    pub max_event_batch: i64,
    /// Advertised run concurrency (fixed at 1 for v1).
    pub run_concurrency: i64,
    /// Maximum simultaneously opened homes per daemon process.
    pub max_open_homes: usize,
    /// Maximum live client connections; excess connects are refused with
    /// RATE_LIMITED before any protocol exchange.
    pub max_concurrent_connections: usize,
    /// Maximum jobs queued per home actor; overflow degrades with
    /// RATE_LIMITED instead of growing memory unboundedly.
    pub max_home_queue_depth: usize,
    /// Maximum UTF-8 byte length of a search term.
    pub max_search_term_bytes: usize,
    /// Maximum retained idempotent-operation registry rows.
    pub max_idempotency_entries: i64,
    /// Retention ceiling for the per-home outbox (R21 disk retention);
    /// enforced by the daemon-owned pruning task.
    pub max_retained_events: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_payload_bytes: 8 * 1024 * 1024,
            max_list_limit: 200,
            max_event_batch: 1000,
            run_concurrency: 1,
            max_open_homes: 8,
            max_concurrent_connections: 64,
            max_home_queue_depth: 1024,
            max_search_term_bytes: 512,
            max_idempotency_entries: 10_000,
            max_retained_events: 100_000,
        }
    }
}

// ── Problem builders ────────────────────────────────────────────────────

/// Transient-capacity degradation (registered coarse code).
pub fn rate_limited(reason: &str, extra: Value) -> crate::problem::ProblemShape {
    crate::problem::limit_problem("RATE_LIMITED", "reason", reason, extra)
}

/// Durable-quota degradation (registered coarse code).
pub fn quota_exceeded(rule: &str, extra: Value) -> crate::problem::ProblemShape {
    crate::problem::limit_problem("QUOTA_EXCEEDED", "rule", rule, extra)
}

impl Limits {
    /// Wire payload bound (framing stage). INVALID_INPUT: a caller bug, not
    /// load. Kept byte-count based exactly like the U5a seed check.
    pub fn check_payload(&self, line_len: usize) -> Result<(), Value> {
        if line_len > self.max_payload_bytes {
            return Err(json!({
                "field": "payload",
                "maxPayloadBytes": self.max_payload_bytes,
                "observed": line_len,
            }));
        }
        Ok(())
    }

    /// Event page bound: reject pages above the advertised batch ceiling
    /// with INVALID_INPUT (details.limit names the bound).
    pub fn check_event_page(&self, requested: i64) -> Result<(), Value> {
        if requested > self.max_event_batch {
            return Err(json!({
                "field": "limit",
                "limit": self.max_event_batch,
                "observed": requested,
            }));
        }
        Ok(())
    }

    /// Search-term bound (UTF-8 bytes).
    pub fn check_search_term(&self, term: &str) -> Result<(), Value> {
        let len = term.len();
        if len > self.max_search_term_bytes {
            return Err(json!({
                "field": "query",
                "maxLength": self.max_search_term_bytes,
                "observed": len,
            }));
        }
        Ok(())
    }

    /// Idempotency-table quota: called only when a NEW operation row would
    /// be inserted (committed replays bypass it — replaying an acknowledged
    /// command is always allowed regardless of table pressure).
    pub fn check_idempotency_capacity(&self, current_rows: i64) -> Result<(), Value> {
        if current_rows >= self.max_idempotency_entries {
            return Err(json!({
                "rule": "idempotency-table",
                "limit": self.max_idempotency_entries,
            }));
        }
        Ok(())
    }

    /// Opened-homes quota.
    pub fn check_open_homes(&self, current_open: usize) -> Result<(), Value> {
        if current_open >= self.max_open_homes {
            return Err(json!({
                "rule": "open-homes",
                "limit": self.max_open_homes,
            }));
        }
        Ok(())
    }

    /// Clamp helper for list-shaped reads within [1, max_list_limit].
    pub fn clamp_list_limit(&self, requested: i64) -> i64 {
        requested.clamp(1, self.max_list_limit)
    }

    /// Handshake `limits` block (capabilities.schema.json#/$defs/Limits).
    pub fn to_handshake_json(&self) -> Value {
        json!({
            "maxPayloadBytes": self.max_payload_bytes,
            "maxListLimit": self.max_list_limit,
            "maxEventBatch": self.max_event_batch,
            "runConcurrency": self.run_concurrency,
            "maxOpenHomes": self.max_open_homes,
            "maxConcurrentConnections": self.max_concurrent_connections,
            "maxHomeQueueDepth": self.max_home_queue_depth,
            "maxSearchTermBytes": self.max_search_term_bytes,
            "maxIdempotencyEntries": self.max_idempotency_entries,
            "maxRetainedEvents": self.max_retained_events,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_bound_trips_at_exact_ceiling() {
        let limits = Limits {
            max_payload_bytes: 16,
            ..Limits::default()
        };
        assert!(limits.check_payload(16).is_ok());
        assert!(limits.check_payload(17).is_err());
    }

    #[test]
    fn event_page_and_search_bounds_carry_registered_shapes() {
        let limits = Limits::default();
        assert!(limits.check_event_page(1000).is_ok());
        let page_err = limits.check_event_page(1001).unwrap_err();
        assert_eq!(page_err["field"], "limit");
        assert_eq!(page_err["limit"], 1000);
        let term_err = limits.check_search_term(&"x".repeat(513)).unwrap_err();
        assert_eq!(term_err["field"], "query");
        assert_eq!(term_err["observed"], 513);
    }

    #[test]
    fn quota_helpers_name_rule_and_limit() {
        let limits = Limits::default();
        let homes = limits.check_open_homes(8).unwrap_err();
        assert_eq!(homes["rule"], "open-homes");
        let idem = limits.check_idempotency_capacity(10_000).unwrap_err();
        assert_eq!(idem["rule"], "idempotency-table");
    }
}
