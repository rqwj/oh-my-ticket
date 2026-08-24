//! Durable outbox events (R11): monotonic per-home cursor (`events.seq`),
//! cursor resume, bounded retention with snapshot-resync signaling. Events
//! are appended INSIDE the finalize transaction — a committed mutation and
//! its event set are inseparable.

use crate::clock::MillisClock;
use crate::{Problem, Result};
use omt_domain::error;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Reserved event type signaling that retention dropped events some consumer
/// had not yet consumed (R11 snapshot-resync). Never emitted by normal flow;
/// retention surfaces the need and callers emit it.
pub const EVENT_SNAPSHOT_RESYNC: &str = "snapshot.resync";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    pub seq: i64,
    pub home_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: String,
}

pub fn append(
    conn: &Connection,
    home_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
    created_at: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO events (home_id, event_type, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![home_id, event_type, payload.to_string(), created_at],
    )
    .map_err(|err| {
        Problem::with_details(error::IO, format!("outbox append failed: {err}"), |d| {
            d.insert("eventType".into(), event_type.into());
        })
    })?;
    Ok(conn.last_insert_rowid())
}

/// Resume from an exclusive lower bound: strictly `seq > cursor`, ascending
/// (per-home cursor semantics; single home per database).
pub fn resume_since(conn: &Connection, cursor: i64, limit: usize) -> Result<Vec<StoredEvent>> {
    let mut stmt = conn
        .prepare("SELECT seq, home_id, event_type, payload, created_at FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2")
        .map_err(|err| Problem::new(error::IO, format!("outbox resume prepare: {err}")))?;
    let rows = stmt
        .query_map(params![cursor, limit as i64], |row| {
            let payload_text: String = row.get(3)?;
            Ok(StoredEvent {
                seq: row.get(0)?,
                home_id: row.get(1)?,
                event_type: row.get(2)?,
                payload: serde_json::from_str(&payload_text).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })
        .map_err(|err| Problem::new(error::IO, format!("outbox resume query: {err}")))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| Problem::new(error::IO, format!("outbox resume read: {err}")))
}

pub fn latest_seq(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|err| Problem::new(error::IO, format!("outbox latest: {err}")))?
        .unwrap_or(0))
}

/// Retention configuration (bounded, R21 seed).
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    /// Keep at most this many newest events.
    pub max_events: usize,
    /// The consumer cursor to protect: pruning never silently passes it
    /// without flagging a resync need.
    pub consumer_cursor: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PruneReport {
    /// Everything at or below this seq was removed.
    pub pruned_through_seq: i64,
    pub pruned_count: usize,
    /// True when retained history no longer covers `consumer_cursor` — the
    /// caller must emit [`EVENT_SNAPSHOT_RESYNC`] so consumers resnapshot.
    pub requires_resync: bool,
}

/// Bounded retention hook: delete oldest events beyond `max_events`; report
/// whether any protected consumer fell behind the new watermark.
pub fn prune(conn: &Connection, retention: &Retention) -> Result<PruneReport> {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .map_err(|err| Problem::new(error::IO, format!("retention count: {err}")))?;
    if total <= retention.max_events as i64 {
        return Ok(PruneReport {
            pruned_through_seq: 0,
            pruned_count: 0,
            requires_resync: false,
        });
    }
    // Retain exactly the newest `max_events`; everything strictly below the
    // retained window's floor is evicted. `pruned_through_seq` reports the
    // HIGHEST EVICTED seq so consumers can detect lost history precisely.
    let retained_floor: Option<i64> = conn
        .query_row(
            "SELECT MIN(seq) FROM (SELECT seq FROM events ORDER BY seq DESC LIMIT ?1)",
            params![retention.max_events],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| Problem::new(error::IO, format!("retention watermark: {err}")))?;
    let Some(floor) = retained_floor else {
        return Ok(PruneReport {
            pruned_through_seq: 0,
            pruned_count: 0,
            requires_resync: false,
        });
    };
    let pruned = conn
        .execute("DELETE FROM events WHERE seq < ?1", params![floor])
        .map_err(|err| Problem::new(error::IO, format!("retention prune: {err}")))?;
    let pruned_through = floor - 1;
    // A consumer lagging BEHIND the eviction frontier can no longer resume
    // from its cursor: it must full-snapshot (`snapshot.resync` handoff).
    let requires_resync = retention
        .consumer_cursor
        .is_some_and(|cursor| cursor > 0 && cursor < floor);
    Ok(PruneReport {
        pruned_through_seq: pruned_through,
        pruned_count: pruned,
        requires_resync,
    })
}

/// Convenience wrapper binding a clock for created_at stamps.
pub fn append_with_clock(
    conn: &Connection,
    home_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
    clock: &dyn MillisClock,
) -> Result<i64> {
    append(
        conn,
        home_id,
        event_type,
        payload,
        &crate::clock::iso_from_ms(clock.now_ms()),
    )
}
