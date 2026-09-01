//! Outbox semantics (R11): monotonic per-home cursor, exclusive-cursor
//! resume, bounded retention with snapshot-resync signaling, and the
//! reserved `snapshot.resync` event type.

#[path = "common/mod.rs"]
mod common;

use common::*;
use omt_storage::clock::iso_from_ms;
use omt_storage::outbox::{
    append_with_clock, latest_seq, prune, resume_since, Retention, EVENT_SNAPSHOT_RESYNC,
};

#[test]
fn cursor_resume_is_exclusive_and_monotonic() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let storage = open_storage(&home, &clock);
    let conn = storage.conn();
    let home_id = storage.home_id().unwrap().to_string();

    let mut seqs = vec![];
    for i in 0..5 {
        seqs.push(
            append_with_clock(
                conn,
                &home_id,
                "node.updated",
                &serde_json::json!({ "n": i }),
                &*clock,
            )
            .unwrap(),
        );
    }
    assert_eq!(seqs, vec![1, 2, 3, 4, 5], "monotonic per-home cursor");
    assert_eq!(latest_seq(conn).unwrap(), 5);

    // Exclusive lower bound: seq > cursor, ascending.
    let batch: Vec<i64> = resume_since(conn, 2, 100)
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(batch, vec![3, 4, 5]);
    // Limit respected.
    let limited: Vec<i64> = resume_since(conn, 0, 2)
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(limited, vec![1, 2]);
    // Fully consumed cursor yields nothing.
    assert!(resume_since(conn, 5, 100).unwrap().is_empty());
}

#[test]
fn retention_is_bounded_and_flags_resync_when_passing_consumers() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let storage = open_storage(&home, &clock);
    let conn = storage.conn();
    let home_id = storage.home_id().unwrap().to_string();

    for i in 0..10 {
        append_with_clock(
            conn,
            &home_id,
            "tick",
            &serde_json::json!({ "i": i }),
            &*clock,
        )
        .unwrap();
    }

    // Caught-up consumer (cursor 8 ≥ eviction frontier 6): everything pruned
    // was already delivered → no resync needed.
    let report = prune(
        conn,
        &Retention {
            max_events: 4,
            consumer_cursor: Some(8),
        },
    )
    .unwrap();
    assert_eq!(report.pruned_count, 6);
    assert_eq!(report.pruned_through_seq, 6);
    assert!(!report.requires_resync);
    assert_eq!(latest_seq(conn).unwrap(), 10);

    // The reserved signaling event type exists exactly for this handoff.
    assert_eq!(EVENT_SNAPSHOT_RESYNC, "snapshot.resync");

    // Consumer inside retained history (cursor 9 ≥ retained floor 8): safe.
    let report = prune(
        conn,
        &Retention {
            max_events: 3,
            consumer_cursor: Some(9),
        },
    )
    .unwrap();
    assert!(!report.requires_resync);

    // LAGGING consumer (cursor below the new eviction frontier): history it
    // still needs is gone → the report demands a snapshot resync.
    let (_d2, home2) = temp_home();
    let storage2 = open_storage(&home2, &clock.clone());
    let conn2 = storage2.conn();
    let hid2 = storage2.home_id().unwrap().to_string();
    for i in 0..10 {
        append_with_clock(
            conn2,
            &hid2,
            "tick",
            &serde_json::json!({ "i": i }),
            &*clock,
        )
        .unwrap();
    }
    let report = prune(
        conn2,
        &Retention {
            max_events: 2,
            consumer_cursor: Some(3),
        },
    )
    .unwrap();
    assert_eq!(report.pruned_count, 8);
    assert_eq!(report.pruned_through_seq, 8);
    assert!(report.requires_resync, "lagging consumer lost events 4..=8");

    // No consumer tracked: pruning is silent.
    let report = prune(
        conn,
        &Retention {
            max_events: 1,
            consumer_cursor: None,
        },
    )
    .unwrap();
    assert!(!report.requires_resync);
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 1);
}

#[test]
fn events_are_appended_inside_finalize_transaction_only_once() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let ticket = node_row(
        "TICKET-0009",
        "ticket",
        "outbox probe",
        "open",
        "tickets/t/TICKET.md",
    );
    let mut storage = open_storage(&home, &clock);
    execute_simple(&mut storage, "ob-seed", &ticket, None, "body");

    let plan = storage
        .plan_update(
            "ob-cmd",
            &ticket,
            Some("renamed".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            vec![],
            serde_json::json!({"k": 1}),
        )
        .unwrap();
    storage.execute(&plan).unwrap();
    // Duplicate command id must not double-append.
    storage.execute(&plan).unwrap();

    let events = resume_since(storage.conn(), 0, 100).unwrap();
    let updates = events
        .iter()
        .filter(|e| e.event_type == "node.created")
        .count()
        + events
            .iter()
            .filter(|e| e.event_type == "node.updated")
            .count();
    assert_eq!(
        updates, 2,
        "exactly one created + one updated across retries"
    );
    assert_eq!(events.last().unwrap().created_at, iso_from_ms(T0_MS));
}
