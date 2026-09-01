//! Ports (U3): the narrow interfaces storage (U4) will implement on disk and
//! the corpus runner implements in memory. The domain never touches wall
//! clocks, files, or lease tables directly — every side effect crosses a
//! port so decision logic stays pure and deterministic.

use std::collections::BTreeMap;

/// Wall clock. The Rust corpus leg injects a fixed stamp; volatile fields
/// are masked before comparison, so only *presence* is observable.
pub trait Clock {
    fn now_iso(&self) -> String;
}

/// Fixed deterministic clock (`FIXED_NUDGE_AT` from the TS harness — every
/// stamp it produces is masked anyway; this keeps runs reproducible).
#[derive(Debug, Default, Clone, Copy)]
pub struct FixedClock;

impl Clock for FixedClock {
    fn now_iso(&self) -> String {
        "2026-08-19T00:00:00.000Z".to_string()
    }
}

/// Fenced execution leases (R10) as consulted by janitor/claim decisions.
///
/// Ratified U3 semantics decision 1: the TypeScript core decides liveness by
/// `executor_session_id ∈ activeSessions` snapshots. Rust replaces that with
/// real leases — an item is live iff its executor holds an unexpired lease
/// bound to the item's attempt. Corpus ops keep the same scenario JSON:
/// `reopen {activeSessionIds}` / `sweep {activeSessions}` map to
/// `mark_exclusive(...)` at the port layer (issue far-future leases for the
/// listed sessions, expire everything else), which makes the lease predicate
/// decide exactly what the session-set membership decided on the TS leg.
pub trait LeaseTable {
    /// True when `session_id` holds an unexpired lease bound to `attempt`.
    fn lease_alive(&self, session_id: &str, attempt: i64) -> bool;
    /// Issue/replace the grant bound to one session (the claim/dispatch
    /// path). Default no-op for read-only tables.
    fn issue(&mut self, _session_id: &str, _grant: LeaseGrant) {}
    /// Force-expire one session's lease. Default no-op.
    fn expire(&mut self, _session_id: &str) {}
    /// Scenario-op binding (ratified decision 1): exactly `sessions` hold
    /// far-future leases; every other lease is expired. Default no-op.
    fn mark_exclusive(&mut self, _sessions: &[String]) {}
}

/// One issued grant (the claim path hands these out).
#[derive(Debug, Clone, PartialEq)]
pub struct LeaseGrant {
    pub token: String,
    pub attempt: i64,
    pub principal: String,
    /// Expiry in masked-domain milliseconds (any totally ordered number).
    pub expires_at: i64,
}

/// In-memory lease table used by the corpus leg and unit tests.
///
/// Two modes mirror the two liveness sources above:
/// - *exclusive* — exactly the sessions listed hold far-future,
///   attempt-wildcard leases (the scenario-op binding);
/// - *dynamic* — explicit [`MemoryLeases::issue`] / [`MemoryLeases::expire`]
///   grants bound to an attempt, the shape the daemon's claim/report flow
///   uses.
#[derive(Debug, Clone, Default)]
pub struct MemoryLeases {
    exclusive: Option<std::collections::BTreeSet<String>>,
    dynamic: BTreeMap<String, LeaseGrant>,
    now_ms: i64,
}

/// Far-future expiry for port-layer-issued leases (masked anyway).
pub const FAR_FUTURE_MS: i64 = i64::MAX / 4;

impl MemoryLeases {
    pub fn new() -> Self {
        MemoryLeases::default()
    }

    /// Scenario binding: exactly `sessions` are live; every other lease is
    /// expired. Called before each sweep/reopen with the op's list.
    pub fn mark_exclusive(&mut self, sessions: &[String]) {
        self.dynamic.clear();
        self.exclusive = Some(sessions.iter().cloned().collect());
    }

    /// Issue one real lease bound to an attempt (claim/dispatch path).
    pub fn issue(&mut self, session_id: &str, grant: LeaseGrant) {
        self.exclusive = None;
        self.dynamic.insert(session_id.to_string(), grant);
    }

    /// Force-expire one session's lease.
    pub fn expire(&mut self, session_id: &str) {
        if self.exclusive.is_some() {
            self.exclusive = None;
        }
        self.dynamic.remove(session_id);
    }

    pub fn grant(&self, session_id: &str) -> Option<&LeaseGrant> {
        self.dynamic.get(session_id)
    }

    pub fn set_now_ms(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }
}

impl LeaseTable for MemoryLeases {
    /// Sweep liveness is SESSION-level (the TS leg's oracle is pure
    /// session-membership, and scenario seeds bind legacy sessions that
    /// never claimed): a session is alive iff it holds ANY unexpired grant.
    /// Attempt-binding fences explicit reports instead — see
    /// `runs::authorize_report`, which compares the stored grant's attempt.
    fn lease_alive(&self, session_id: &str, _attempt: i64) -> bool {
        match &self.exclusive {
            Some(live) => live.contains(session_id),
            None => self
                .dynamic
                .get(session_id)
                .is_some_and(|g| self.now_ms < g.expires_at),
        }
    }

    fn issue(&mut self, session_id: &str, grant: LeaseGrant) {
        self.exclusive = None;
        self.dynamic.insert(session_id.to_string(), grant);
    }

    fn expire(&mut self, session_id: &str) {
        self.exclusive = None;
        self.dynamic.remove(session_id);
    }

    fn mark_exclusive(&mut self, sessions: &[String]) {
        self.dynamic.clear();
        self.exclusive = Some(sessions.iter().cloned().collect());
    }
}
