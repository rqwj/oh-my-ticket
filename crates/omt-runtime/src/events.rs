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

/// One live subscriber: a bounded line channel into its connection writer,
/// plus the resume cursor it registered from — the retention task keys
/// `snapshot.resync` emission on the OLDEST live subscriber cursor so a
/// prune is signaled exactly when a connected consumer would lose history.
pub struct Subscriber {
    #[allow(dead_code)]
    pub id: String,
    pub sender: std::sync::mpsc::SyncSender<String>,
    /// Cursor this subscriber resumed from (0 = full replay).
    pub from_cursor: i64,
    /// Owning connection (F-DAEMON-1): a keyed subscription is unique per
    /// key — re-registering replaces it, connection teardown removes it.
    pub key: Option<String>,
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
        self.subscribe_with_cursor(0, sender)
    }

    /// Register a live subscription remembering its resume cursor.
    pub fn subscribe_with_cursor(
        &self,
        from_cursor: i64,
        sender: std::sync::mpsc::SyncSender<String>,
    ) -> String {
        self.register(None, from_cursor, sender)
    }

    /// Register the live subscription owned by `key`, REPLACING any earlier
    /// one under the same key (F-DAEMON-1). A client polling events/resume
    /// on one connection thus holds one subscription per home instead of
    /// one per call; dropping the replaced sender disconnects its forwarder.
    pub fn subscribe_keyed(
        &self,
        key: &str,
        from_cursor: i64,
        sender: std::sync::mpsc::SyncSender<String>,
    ) -> String {
        self.register(Some(key.to_string()), from_cursor, sender)
    }

    fn register(
        &self,
        key: Option<String>,
        from_cursor: i64,
        sender: std::sync::mpsc::SyncSender<String>,
    ) -> String {
        let id = crate::problem::entropy::short_id();
        let mut subscribers = self.subscribers.lock().expect("hub");
        if let Some(key) = key.as_deref() {
            subscribers.retain(|subscriber| subscriber.key.as_deref() != Some(key));
        }
        subscribers.push(Subscriber {
            id: id.clone(),
            sender,
            from_cursor,
            key,
        });
        id
    }

    /// Drop every subscription owned by `key` (connection teardown): a dead
    /// connection must not keep pinning retention or the idle-quiet exit.
    pub fn unsubscribe_key(&self, key: &str) {
        self.subscribers
            .lock()
            .expect("hub")
            .retain(|subscriber| subscriber.key.as_deref() != Some(key));
    }

    /// Live subscriber count (diagnostics / tests).
    #[allow(dead_code)]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().expect("hub").len()
    }

    /// Oldest live subscriber cursor (None when nobody subscribes). The
    /// retention task passes this as the protected consumer cursor.
    pub fn oldest_subscriber_cursor(&self) -> Option<i64> {
        self.subscribers
            .lock()
            .expect("hub")
            .iter()
            .map(|subscriber| subscriber.from_cursor)
            .min()
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

/// Retention wiring (U5b, superseding the U5a deferral): the daemon-owned
/// pruning task lives in `homes::run_retention_tick`, running
/// `outbox::prune_and_signal` per open home each actor tick with the oldest
/// live subscriber cursor as the protected consumer. This stub remains only
/// as a documentation anchor.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// F-DAEMON-1: re-registering under the same key replaces the earlier
    /// subscription (its sender drops, so its forwarder disconnects).
    #[test]
    fn keyed_subscribe_replaces_same_key() {
        let hub = Hub::new();
        let (first_tx, first_rx) = std::sync::mpsc::sync_channel::<String>(4);
        let (second_tx, _second_rx) = std::sync::mpsc::sync_channel::<String>(4);
        hub.subscribe_keyed("conn-a", 3, first_tx);
        hub.subscribe_keyed("conn-a", 7, second_tx);
        assert_eq!(hub.subscriber_count(), 1);
        assert_eq!(hub.oldest_subscriber_cursor(), Some(7));
        assert!(matches!(
            first_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));

        let (other_tx, _other_rx) = std::sync::mpsc::sync_channel::<String>(4);
        hub.subscribe_keyed("conn-b", 1, other_tx);
        assert_eq!(hub.subscriber_count(), 2, "distinct keys coexist");
    }

    /// Teardown drops only the departing connection's subscriptions;
    /// unkeyed registrations are untouched.
    #[test]
    fn unsubscribe_key_drops_only_that_key() {
        let hub = Hub::new();
        let (a_tx, _a_rx) = std::sync::mpsc::sync_channel::<String>(4);
        let (b_tx, _b_rx) = std::sync::mpsc::sync_channel::<String>(4);
        let (plain_tx, _plain_rx) = std::sync::mpsc::sync_channel::<String>(4);
        hub.subscribe_keyed("conn-a", 2, a_tx);
        hub.subscribe_keyed("conn-b", 5, b_tx);
        hub.subscribe_with_cursor(9, plain_tx);
        hub.unsubscribe_key("conn-a");
        assert_eq!(hub.subscriber_count(), 2);
        assert_eq!(hub.oldest_subscriber_cursor(), Some(5));
        hub.unsubscribe_key("conn-b");
        assert_eq!(hub.oldest_subscriber_cursor(), Some(9));
    }
}
