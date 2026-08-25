//! U5d BENCHMARK tier (non-blocking, `#[ignore]` by default).
//!
//! Run with:
//! ```text
//! cargo test --release -p omt-runtime -- --ignored --nocapture bench_
//! ```
//!
//! Each bench prints a human table AND a stable `BENCH ...` line; the
//! committed numbers live in `docs/runtime/bench-baseline.md`.
//!
//! - b1 — 10k-command mixed stress at 1:4 mutation:query mix; reports total
//!   throughput and p50/p95 latency.
//! - b2 — soak (default 1800 s; `OMT_SOAK_SECONDS` overrides, e.g. 60 s
//!   for CI/dev smoke): ≥20 cmd/s aggregate with p95 interactive latency
//!   <100 ms. The thresholds are asserted.
//! - b3 — 100k retained events; resume of a 10k delta must complete in
//!   <2 s (10 pages × maxEventBatch 1000).

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, wait_for_descriptor, DaemonProcess, RpcError, TestCtx};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── shared helpers ──────────────────────────────────────────────────────

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

fn percentile(sorted_ms: &mut Vec<f64>, p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    sorted_ms.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let index = (((sorted_ms.len() as f64) * p).ceil() as usize)
        .saturating_sub(1)
        .min(sorted_ms.len() - 1);
    sorted_ms[index]
}

/// An RMW update losing to a concurrent winner is CORRECT protocol
/// behavior (revision CONFLICT), not a bench failure.
fn is_rmw_conflict(err: &RpcError) -> bool {
    match err {
        RpcError::Problem { code, details } => {
            code == "CONFLICT"
                && details.get("rule").and_then(|r| r.as_str()) == Some("revision-mismatch")
        }
        _ => false,
    }
}

fn env_seconds(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

struct BenchSession {
    client: common::TestClient,
    credential: Value,
}

fn connect_session(endpoint: &str, kind: &str) -> BenchSession {
    let (client, credential) = connected_client(endpoint, kind)
        .unwrap_or_else(|err| panic!("connect+enroll as {kind}: {err:?}"));
    BenchSession { client, credential }
}

/// Spawn one daemon over one fresh home; returns the endpoint.
fn bench_daemon(ctx: &TestCtx, home_str: &str) -> (DaemonProcess, String) {
    let daemon = DaemonProcess::spawn(ctx, &["--home", home_str]);
    let descriptor = wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(30))
        .expect("bench daemon publishes descriptor");
    assert!(daemon.is_alive(), "daemon died: {}", daemon.stderr_text());
    let endpoint = descriptor.endpoint.clone();
    (daemon, endpoint)
}

/// Seed an epic/story root plus `count` tickets under the story.
/// Returns (storyId, ticketIds).
fn seed_tickets(session: &mut BenchSession, count: usize, prefix: &str) -> (String, Vec<String>) {
    let epic = session
        .client
        .call(
            "node/create",
            authed(
                json!({ "type": "epic", "title": format!("{prefix} epic") }),
                &session.credential,
            ),
        )
        .expect("bench epic");
    let story = session
        .client
        .call(
            "node/create",
            authed(
                json!({
                    "type": "story",
                    "title": format!("{prefix} story"),
                    "parentId": epic["node"]["nodeId"].as_str().unwrap(),
                }),
                &session.credential,
            ),
        )
        .expect("bench story");
    let story_id = story["node"]["nodeId"].as_str().unwrap().to_string();
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let view = session
            .client
            .call(
                "node/create",
                authed(
                    json!({
                        "type": "ticket",
                        "title": format!("{prefix} ticket {index}"),
                        "parentId": story_id,
                    }),
                    &session.credential,
                ),
            )
            .expect("bench ticket");
        ids.push(view["node"]["nodeId"].as_str().unwrap().to_string());
    }
    (story_id, ids)
}

// ── b1: 10k-command mixed stress ────────────────────────────────────────

#[test]
#[ignore = "benchmark tier: run explicitly via `cargo test --release -p omt-runtime -- --ignored bench_`"]
fn bench_b1_ten_k_mixed_stress() {
    const TOTAL: usize = 10_000;
    const CLIENTS: usize = 4;

    let ctx = TestCtx::spawn_named("bench-b1");
    let home_str = ctx.home_str().to_string();
    let (_daemon, endpoint) = bench_daemon(&ctx, &home_str);
    let mut setup = connect_session(&endpoint, "cli");
    let (story_id, tickets) = seed_tickets(&mut setup, 400, "b1");
    drop(setup);

    // 1:4 mutation:query mix across TOTAL commands; every mutation targets
    // the story plane (creates under the story, RMW priority updates on
    // tickets); queries rotate search/tree/list/get.
    let commands_per_client = TOTAL / CLIENTS;
    let latencies: Mutex<Vec<(f64, bool)>> = Mutex::new(Vec::new()); // (ms, was_mutation)
    let failures = AtomicU64::new(0);
    let conflicts = AtomicU64::new(0);
    let counter = AtomicU64::new(1);
    let started = Instant::now();

    std::thread::scope(|scope| {
        for worker in 0..CLIENTS {
            let endpoint = endpoint.clone();
            let tickets = tickets.clone();
            let story_id = story_id.clone();
            let latencies = &latencies;
            let failures = &failures;
            let conflicts = &conflicts;
            let counter = &counter;
            scope.spawn(move || {
                let mut session =
                    connect_session(&endpoint, ["cli", "dsh", "mcp", "external"][worker]);
                let mut rng = Rng::new(0xB10000 ^ (worker as u64 + 3));
                for step in 0..commands_per_client {
                    // Cycle of five: one mutation then four queries (1:4).
                    let is_mutation = step % 5 == 0;
                    let at = Instant::now();
                    let outcome: Result<Value, RpcError> = if is_mutation {
                        if rng.below(2) == 0 {
                            session.client.call(
                                "node/create",
                                authed(
                                    json!({
                                        "type": "ticket",
                                        "title": format!(
                                            "b1 storm w{worker} n{}",
                                            counter.fetch_add(1, Ordering::SeqCst)
                                        ),
                                        "parentId": story_id,
                                    }),
                                    &session.credential,
                                ),
                            )
                        } else {
                            let node_id = tickets[rng.below(tickets.len())].clone();
                            match session.client.call(
                                "node/get",
                                authed(json!({ "nodeId": node_id }), &session.credential),
                            ) {
                                Ok(view) => session.client.call(
                                    "node/update",
                                    authed(
                                        json!({
                                            "nodeId": node_id,
                                            "expectedRevision": view["node"]["revision"],
                                            "changes": { "priority": rng.below(9000) as i64 },
                                        }),
                                        &session.credential,
                                    ),
                                ),
                                Err(err) => Err(err),
                            }
                        }
                    } else {
                        match rng.below(3) {
                            0 => session.client.call(
                                "node/search",
                                authed(json!({ "query": "b1", "limit": 50 }), &session.credential),
                            ),
                            1 => session
                                .client
                                .call("node/tree", authed(json!({}), &session.credential)),
                            _ => session.client.call(
                                "node/list",
                                authed(
                                    json!({ "filter": { "type": "ticket", "archived": false } }),
                                    &session.credential,
                                ),
                            ),
                        }
                    };
                    if let Err(err) = &outcome {
                        if is_rmw_conflict(err) {
                            conflicts.fetch_add(1, Ordering::SeqCst);
                        } else {
                            failures.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    latencies
                        .lock()
                        .expect("latencies")
                        .push((at.elapsed().as_secs_f64() * 1000.0, is_mutation));
                }
            });
        }
    });

    let wall_s = started.elapsed().as_secs_f64();
    let samples = latencies.into_inner().expect("latencies");
    let mut all: Vec<f64> = samples.iter().map(|(ms, _)| *ms).collect();
    let mut mutations: Vec<f64> = samples
        .iter()
        .filter(|(_, m)| *m)
        .map(|(ms, _)| *ms)
        .collect();
    let mut queries: Vec<f64> = samples
        .iter()
        .filter(|(_, m)| !*m)
        .map(|(ms, _)| *ms)
        .collect();
    let throughput = TOTAL as f64 / wall_s;
    let failed = failures.load(Ordering::SeqCst);
    let conflicted = conflicts.load(Ordering::SeqCst);

    println!(
        "\n┌─ b1 · 10k-command mixed stress (1:4 mutation:query) ──────────────\n\
         │ commands   {TOTAL:>7}   mutations {:>5}   queries {:>5}\n\
         │ clients    {CLIENTS:>7}   failed    {:>5}   rmw-conflicts {:>4}\n\
         │ wall       {:>8.2} s\n\
         │ throughput {:>14.1} cmd/s\n\
         │ overall    p50 {:>8.2} ms · p95 {:>8.2} ms\n\
         └─ mutations p50 {:.2} / p95 {:.2} ms · queries p50 {:.2} / p95 {:.2} ms\n",
        mutations.len(),
        queries.len(),
        failed,
        conflicted,
        wall_s,
        throughput,
        percentile(&mut all, 0.50),
        percentile(&mut all, 0.95),
        percentile(&mut mutations, 0.50),
        percentile(&mut mutations, 0.95),
        percentile(&mut queries, 0.50),
        percentile(&mut queries, 0.95),
    );
    println!(
        "BENCH b1 commands={TOTAL} mutations={} queries={} failed={failed} conflicts={conflicted} \
         wall_ms={:.0} throughput_cps={throughput:.1} p50_us={:.0} p95_us={:.0}",
        mutations.len(),
        queries.len(),
        wall_s * 1000.0,
        percentile(&mut all, 0.50) * 1000.0,
        percentile(&mut all, 0.95) * 1000.0,
    );
    assert_eq!(failed, 0, "non-conflict command failures under the mix");
}

// ── b2: soak ────────────────────────────────────────────────────────────

#[test]
#[ignore = "benchmark tier: run explicitly via `cargo test --release -p omt-runtime -- --ignored bench_`"]
fn bench_b2_soak() {
    let soak_seconds = env_seconds("OMT_SOAK_SECONDS", 1800);
    const CLIENTS: usize = 4;
    /// Aggregate throughput floor (plan b2).
    const MIN_THROUGHPUT_CPS: f64 = 20.0;
    /// Interactive-latency ceiling (plan b2).
    const MAX_P95_INTERACTIVE_MS: f64 = 100.0;

    let ctx = TestCtx::spawn_named("bench-b2");
    let home_str = ctx.home_str().to_string();
    let (_daemon, endpoint) = bench_daemon(&ctx, &home_str);
    let mut setup = connect_session(&endpoint, "cli");
    let (_story_id, tickets) = seed_tickets(&mut setup, 200, "b2");
    drop(setup);

    let started = Instant::now();
    let deadline = started + Duration::from_secs(soak_seconds);
    let latencies: Mutex<Vec<(f64, bool)>> = Mutex::new(Vec::new());
    let failures = AtomicU64::new(0);
    let conflicts = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for worker in 0..CLIENTS {
            let endpoint = endpoint.clone();
            let tickets = tickets.clone();
            let latencies = &latencies;
            let failures = &failures;
            let conflicts = &conflicts;
            scope.spawn(move || {
                let mut session =
                    connect_session(&endpoint, ["cli", "dsh", "mcp", "desktop"][worker]);
                let mut rng = Rng::new(0xB20000 ^ (worker as u64 + 5));
                loop {
                    if Instant::now() >= deadline {
                        break;
                    }
                    // Same 1:4 discipline as b1 so the writer stays loaded
                    // while interactive latency is sampled.
                    let is_mutation = rng.below(5) == 0;
                    let at = Instant::now();
                    let outcome: Result<Value, RpcError> = if is_mutation {
                        let node_id = tickets[rng.below(tickets.len())].clone();
                        match session.client.call(
                            "node/get",
                            authed(json!({ "nodeId": node_id }), &session.credential),
                        ) {
                            Ok(view) => session.client.call(
                                "node/update",
                                authed(
                                    json!({
                                        "nodeId": node_id,
                                        "expectedRevision": view["node"]["revision"],
                                        "changes": { "priority": rng.below(9000) as i64 },
                                    }),
                                    &session.credential,
                                ),
                            ),
                            Err(err) => Err(err),
                        }
                    } else {
                        match rng.below(4) {
                            0 => session.client.call(
                                "node/search",
                                authed(json!({ "query": "b2", "limit": 30 }), &session.credential),
                            ),
                            1 | 2 => session.client.call(
                                "node/list",
                                authed(
                                    json!({ "filter": { "type": "ticket" } }),
                                    &session.credential,
                                ),
                            ),
                            _ => session.client.call(
                                "node/get",
                                authed(
                                    json!({ "nodeId": tickets[rng.below(tickets.len())] }),
                                    &session.credential,
                                ),
                            ),
                        }
                    };
                    if let Err(err) = &outcome {
                        if is_rmw_conflict(err) {
                            conflicts.fetch_add(1, Ordering::SeqCst);
                        } else {
                            failures.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    latencies
                        .lock()
                        .expect("latencies")
                        .push((at.elapsed().as_secs_f64() * 1000.0, is_mutation));
                }
            });
        }
    });

    let wall_s = started.elapsed().as_secs_f64();
    let samples = latencies.into_inner().expect("latencies");
    let total_commands = samples.len();
    let throughput = total_commands as f64 / wall_s;
    let mut all: Vec<f64> = samples.iter().map(|(ms, _)| *ms).collect();
    let mut interactive: Vec<f64> = samples
        .iter()
        .filter(|(_, m)| !*m)
        .map(|(ms, _)| *ms)
        .collect();
    let mut mutations: Vec<f64> = samples
        .iter()
        .filter(|(_, m)| *m)
        .map(|(ms, _)| *ms)
        .collect();
    let p95_interactive = percentile(&mut interactive, 0.95);
    let failed = failures.load(Ordering::SeqCst);
    let conflicted = conflicts.load(Ordering::SeqCst);

    println!(
        "\n┌─ b2 · soak ({soak_seconds}s configured) ─────────────────────────────────┐\n\
         │ commands   {total_commands:>7}   failed {:>5}   rmw-conflicts {:>4}\n\
         │ throughput {:>14.1} cmd/s (floor {MIN_THROUGHPUT_CPS})\n\
         │ overall    p50 {:>8.2} ms · p95 {:>8.2} ms\n\
         │ interactive p95 {:>11.2} ms (ceiling {MAX_P95_INTERACTIVE_MS})\n\
         └─ mutations n={} p50 {:.2} / p95 {:.2} ms\n",
        failed,
        conflicted,
        throughput,
        percentile(&mut all, 0.50),
        percentile(&mut all, 0.95),
        p95_interactive,
        mutations.len(),
        percentile(&mut mutations, 0.50),
        percentile(&mut mutations, 0.95),
    );
    println!(
        "BENCH b2 soak_seconds={soak_seconds} commands={total_commands} failed={failed} \
         conflicts={conflicted} throughput_cps={throughput:.1} p95_interactive_us={:.0}",
        p95_interactive * 1000.0,
    );
    assert_eq!(failed, 0, "soak observed NON-CONFLICT command failures");
    assert!(
        throughput >= MIN_THROUGHPUT_CPS,
        "aggregate throughput {throughput:.1} cmd/s below the {MIN_THROUGHPUT_CPS} floor"
    );
    assert!(
        p95_interactive < MAX_P95_INTERACTIVE_MS,
        "interactive p95 {p95_interactive:.2} ms above the {MAX_P95_INTERACTIVE_MS} ms ceiling"
    );
}

// ── b3: 100k retained events, sub-2s resume of a 10k delta ──────────────

#[test]
#[ignore = "benchmark tier: run explicitly via `cargo test --release -p omt-runtime -- --ignored bench_`"]
fn bench_b3_hundred_k_events_resume() {
    use omt_storage::journal::OpenConfig;
    use omt_storage::Storage;

    const RETAINED: i64 = 100_000;
    const DELTA: i64 = 10_000;
    const PAGE_LIMIT: i64 = 1000; // server maxEventBatch
    const BATCH: i64 = 10_000;

    // Build the event history DIRECTLY through the storage outbox in bulk
    // transactions: the bench measures RESUME cost, not fsync-bound event
    // production.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let config = OpenConfig::new(&home);
    {
        let storage = Storage::open(config).expect("open storage");
        let home_id = storage.home_id().expect("home id").to_string();
        let now_iso = omt_storage::clock::iso_from_ms(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_millis() as i64,
        );
        let conn = storage.conn();
        let mut appended = 0i64;
        while appended < RETAINED {
            conn.execute_batch("BEGIN IMMEDIATE").expect("begin batch");
            for offset in 0..BATCH.min(RETAINED - appended) {
                let seq = appended + offset;
                let payload = json!({
                    "nodeId": format!("TICKET-{:06}", seq % 50_000),
                    "path": format!("tickets/bulk/TICKET-{:06}.md", seq % 50_000),
                });
                omt_storage::outbox::append(conn, &home_id, "node.updated", &payload, &now_iso)
                    .expect("append event");
            }
            conn.execute_batch("COMMIT").expect("commit batch");
            appended += BATCH.min(RETAINED - appended);
        }
    }

    // Serve it through the REAL daemon (bootstrap path, recovery on open).
    let ctx = TestCtx::spawn_named("bench-b3");
    let home_str = home.to_str().expect("utf8").to_string();
    let (_daemon, endpoint) = bench_daemon(&ctx, &home_str);
    let mut session = connect_session(&endpoint, "cli");

    // Retention invariant: exactly RETAINED events exist, and a cursor
    // inside that window must never trigger a resync.
    {
        let conn = rusqlite::Connection::open_with_flags(
            home.join("omt.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("readonly open");
        let latest = omt_storage::outbox::latest_seq(&conn).expect("latest seq");
        assert_eq!(latest, RETAINED, "event history size mismatch");
    }

    let start_cursor = RETAINED - DELTA;
    let mut cursor = start_cursor;
    let mut pages = 0usize;
    let mut consumed = 0i64;
    let resumed_at = Instant::now();
    loop {
        let page = session
            .client
            .call(
                "events/resume",
                authed(
                    json!({ "cursor": cursor, "limit": PAGE_LIMIT }),
                    &session.credential,
                ),
            )
            .expect("resume page");
        let events = page["events"].as_array().expect("events array");
        if events.is_empty() {
            break;
        }
        cursor = events
            .last()
            .and_then(|e| e["cursor"].as_i64())
            .expect("cursor");
        consumed += events.len() as i64;
        pages += 1;
        if consumed >= DELTA || cursor >= RETAINED {
            break;
        }
    }
    let resume_ms = resumed_at.elapsed().as_secs_f64() * 1000.0;

    println!(
        "\n┌─ b3 · {RETAINED} retained events → resume {DELTA} delta ─────────────┐\n\
         │ retained    {RETAINED:>8}   delta {DELTA:>6}\n\
         │ pages       {pages:>8} × limit {PAGE_LIMIT}\n\
         │ consumed    {consumed:>8} envelopes\n\
         │ resume wall {resume_ms:>9.1} ms (ceiling 2000)\n\
         └─ {}\n",
        if resume_ms < 2000.0 { "PASS" } else { "FAIL" },
    );
    println!(
        "BENCH b3 retained={RETAINED} delta={DELTA} pages={pages} consumed={consumed} resume_ms={resume_ms:.1}"
    );
    assert_eq!(consumed, DELTA, "the full delta must be consumed");
    assert!(
        resume_ms < 2000.0,
        "resume took {resume_ms:.1} ms — above the 2 s ceiling"
    );
}
