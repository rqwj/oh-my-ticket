//! Event fan-out (F4/R11): outbox rows → protocol EventEnvelopes, backlog
//! resume, and live subscriber notification (`omt/event`).
//!
//! Mapping note (documented deviation): the U4a journal planners emit
//! storage-plane event kinds ("node.created" / "node.updated" /
//! "node.moved"); the protocol vocabulary (events.schema.json) is
//! "node.changed" with a `change` discriminator. The envelope builder
/// below maps storage kinds onto protocol kinds; run-plane and
/// attention/quarantine/resync events are authored directly in protocol
/// shape by this unit's handlers.
use omt_storage::clock::MillisClock;
use omt_storage::outbox::{self, StoredEvent};
use serde_json::{json, Value};
use std::sync::Arc;

pub const NOTIFICATION_METHOD: &str = "omt/event";

#[derive(Debug, Clone)]
pub struct Envelope {
    pub cursor: i64,
    pub value: Value,
}

impl Envelope {
    pub fn to_notification_line(&self) -> String {
        crate::jsonrpc::notification(NOTIFICATION_METHOD, &self.value)
    }
}

/// Map one stored outbox row onto a protocol EventEnvelope. Returns None
/// for kinds outside the protocol vocabulary.
pub fn build_envelope(home_id: &str, stored: &StoredEvent) -> Option<Envelope> {
    let payload = stored.payload.clone();
    let node_ref = || json!({ "homeId": home_id, "nodeId": payload.get("nodeId").cloned().unwrap_or(Value::Null) });
    let (event_type, wire_payload) = match stored.event_type.as_str() {
        "node.created" => (
            "node.changed",
            json!({ "kind": "node.changed", "ref": node_ref(), "change": "created" }),
        ),
        "node.updated" => {
            let change = if payload.get("archived").and_then(|v| v.as_bool()) == Some(true) {
                "archived"
            } else if payload.get("unarchived").and_then(|v| v.as_bool()) == Some(true)
                || payload.get("archived").and_then(|v| v.as_bool()) == Some(false)
            {
                "unarchived"
            } else {
                "updated"
            };
            (
                "node.changed",
                json!({ "kind": "node.changed", "ref": node_ref(), "change": change }),
            )
        }
        "node.moved" => (
            "node.changed",
            json!({ "kind": "node.changed", "ref": node_ref(), "change": "moved" }),
        ),
        // Protocol-native kinds pass through untouched.
        "run.changed" | "run.item_changed" | "attention.raised" | "snapshot.resync"
        | "node.quarantined" => (stored.event_type.as_str(), payload),
        _ => return None,
    };
    Some(Envelope {
        cursor: stored.seq,
        value: json!({
            "cursor": stored.seq,
            "homeId": home_id,
            "type": event_type,
            "occurredAt": stored.created_at,
            "payload": wire_payload,
        }),
    })
}

/// Read the backlog strictly after `cursor` as envelopes, capped at `limit`
/// rows per page (the caller loops).
pub fn backlog(
    conn: &rusqlite::Connection,
    home_id: &str,
    cursor: i64,
    limit: usize,
) -> omt_storage::Result<Vec<Envelope>> {
    let stored = outbox::resume_since(conn, cursor, limit)?;
    Ok(stored
        .iter()
        .filter_map(|row| build_envelope(home_id, row))
        .collect())
}

/// One live subscriber: a bounded line channel into its connection writer.
pub struct Subscriber {
    #[allow(dead_code)]
    pub id: String,
    pub sender: std::sync::mpsc::SyncSender<String>,
}

/// Per-home live subscription registry + broadcast watermark. The home
/// actor owns mutations of `last_seq`; subscribers register/deregister from
/// connection threads under the mutex.
#[derive(Default)]
pub struct Hub {
    subscribers: std::sync::Mutex<Vec<Subscriber>>,
    last_seq: std::sync::Mutex<i64>,
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub::default())
    }

    /// Initialize the watermark so only post-open commits fan out.
    pub fn set_watermark(&self, seq: i64) {
        *self.last_seq.lock().expect("hub") = seq;
    }

    pub fn subscribe(&self, sender: std::sync::mpsc::SyncSender<String>) -> String {
        let id = crate::problem::entropy::short_id();
        self.subscribers.lock().expect("hub").push(Subscriber {
            id: id.clone(),
            sender,
        });
        id
    }

    #[allow(dead_code)] // explicit unsubscribe path (connection teardown is channel-drop today)
    pub fn unsubscribe(&self, id: &str) {
        self.subscribers
            .lock()
            .expect("hub")
            .retain(|subscriber| subscriber.id != id);
    }

    /// Current watermark (used by events/resume to detect gaps).
    pub fn watermark(&self) -> i64 {
        *self.last_seq.lock().expect("hub")
    }

    /// Drain committed-but-unbroadcast outbox rows and push them to every
    /// live subscriber. Called on the HOME ACTOR thread right after each
    /// mutating job completes — single-writer, no missed or duplicated
    /// notifications. Dead receivers are dropped.
    pub fn publish_new(
        &self,
        conn: &rusqlite::Connection,
        home_id: &str,
    ) -> omt_storage::Result<()> {
        let since = self.watermark();
        let stored = outbox::resume_since(conn, since, usize::MAX >> 4)?;
        for row in &stored {
            if let Some(envelope) = build_envelope(home_id, row) {
                let line = envelope.to_notification_line();
                self.subscribers.lock().expect("hub").retain(|subscriber| {
                    // Bounded channel: a slow/stalled client gets dropped
                    // rather than blocking the whole home queue.
                    subscriber.sender.try_send(line.clone()).is_ok()
                });
            }
        }
        let latest = outbox::latest_seq(conn)?;
        *self.last_seq.lock().expect("hub") = latest.max(since);
        Ok(())
    }
}

/// Retention wiring is deferred to U5b (limits/logging unit): today the
/// daemon never prunes, so snapshot-resync payloads never carry
/// prunedThroughSeq/consumerCursor. The schema fields exist additively so
/// consumers can parse them when retention lands.
pub struct RetentionNote;

impl RetentionNote {
    #[allow(dead_code)]
    pub fn deferred(clock_note: &'static str) -> &'static str {
        clock_note
    }
}

/// Convenience for tests: current watermark via latest_seq.
#[allow(dead_code)]
pub fn latest_cursor(conn: &rusqlite::Connection) -> omt_storage::Result<i64> {
    outbox::latest_seq(conn)
}

/// Unused-today shim keeping the clock import honest for future retention
/// stamping (U5b will stamp prune-time resync envelopes).
pub fn _now_ms(clock: &Arc<dyn MillisClock>) -> i64 {
    clock.now_ms()
}
