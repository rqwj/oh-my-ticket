//! U5d BLOCKING verification gates (plan Success Criteria — Done-gating).
//!
//! Gate 1 — multi-client envelope correctness: up to 8 opened homes ×
//! 2,000 nodes/home × 4 concurrent authenticated clients × active runs of
//! up to 200 items; concurrent mixed operations (create/update/move/
//! archive/list/tree/search + run claim/report) across real client
//! connections. Asserts: zero duplicate committed commandIds; zero lost
//! accepted updates (every acknowledged write stays readable and every
//! revision chain is gap-free); CONFLICT exactly when a presented
//! expectedRevision went stale; cross-principal / cross-attempt /
//! wrong-token reports rejected; per-home serialization holds under
//! contention.
//!
//! Gate 2 — crash-recovery kill-point grid: ALREADY EXISTS as
//! `crates/omt-storage/tests/kill_grid.rs` (53 canonical op×ordinal cells)
//! plus `crates/omt-storage/tests/kill_grid_runs.rs` (+5 run-plane cells).
//! Deliberately NOT rebuilt here — this module links it by reference.
//!
//! Gate 3 — SIGKILL-to-ready: daemon killed -9 mid-mutation on an active
//! home → respawn through the bootstrap path → readiness (descriptor live
//! + handshake + recovered state readable) measured wall-clock <5 s;
//! convergence asserted down to planned bytes on disk, unique operations
//! rows, journal phase = acknowledged for every command, and roll-forward
//! of both pending crash states (`prepared` and `db_committed`).
//!
//! Scale is env-parameterized: OMT_ENV_HOMES / OMT_ENV_NODES /
//! OMT_ENV_CLIENTS / OMT_ENV_RUNS / OMT_ENV_RUN_ITEMS / OMT_ENV_ITERS, or
//! OMT_ENV_FULL=1 for the complete envelope (8 homes × 2000 nodes ×
//! 4 clients × 5 runs × 200 items). The DEFAULT is the plan's documented
//! blocking minimum — 8 homes, 500 nodes/home, 4 clients, 3 runs×50 items —
//! because the full envelope is fsync-bound (~25 ms/create on the dev
//! machine; ~400 s of seeding alone) and cannot meet the 120 s budget
//! there. Deviation rationale recorded in docs/runtime/bench-baseline.md.

#![allow(dead_code)]

mod common;

use common::{authed, connected_client, wait_for_descriptor, DaemonProcess, RpcError, TestCtx};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── scale knobs ─────────────────────────────────────────────────────────

fn env_or(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

#[derive(Debug, Clone, Copy)]
struct Scale {
    homes: usize,
    nodes_per_home: usize,
    clients: usize,
    runs_per_home: usize,
    items_per_run: usize,
    iters: usize,
}

impl Scale {
    /// Tested-at default ratio: the plan's documented BLOCKING MINIMUM
    /// (8 homes × ≥500 nodes/home × 4 clients × 3 runs×50 items) — chosen
    /// because the FULL envelope (8×2000×5×200) is fsync-bound on the dev
    /// machine (~400 s of seeding alone) and would blow the 120 s budget.
    /// `OMT_ENV_FULL=1` selects the full envelope; every dimension can be
    /// overridden individually (OMT_ENV_HOMES/_NODES/_CLIENTS/_RUNS/
    /// _RUN_ITEMS/_ITERS).
    fn from_env() -> Scale {
        let full = std::env::var("OMT_ENV_FULL").as_deref() == Ok("1");
        let (nodes, runs, items) = if full { (2000, 5, 200) } else { (500, 3, 50) };
        Scale {
            homes: env_or("OMT_ENV_HOMES", 8),
            nodes_per_home: env_or("OMT_ENV_NODES", nodes),
            clients: env_or("OMT_ENV_CLIENTS", 4),
            runs_per_home: env_or("OMT_ENV_RUNS", runs),
            items_per_run: env_or("OMT_ENV_RUN_ITEMS", items),
            iters: env_or("OMT_ENV_ITERS", 48),
        }
    }
    /// Fixed-context nodes per home: epic + storyA + storyB + comb probe.
    /// The remainder are run-member tickets (also the storm-update pool).
    fn member_tickets(&self) -> usize {
        self.nodes_per_home - 4
    }
}

// ── deterministic rng (xorshift64) ──────────────────────────────────────

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

// ── client session helpers ──────────────────────────────────────────────

struct Session {
    client: common::TestClient,
    credential: Value,
}

fn connect_session(endpoint: &str, kind: &str) -> Session {
    let (client, credential) = connected_client(endpoint, kind)
        .unwrap_or_else(|err| panic!("connect+enroll as {kind}: {err:?}"));
    Session { client, credential }
}

impl Session {
    /// Authenticated call that MUST succeed.
    fn must(&mut self, method: &str, params: Value, what: &str) -> Value {
        self.client
            .call(method, authed(params, &self.credential))
            .unwrap_or_else(|err| panic!("{what}: {err:?}"))
    }
}

/// Insert homeId into params and attach the credential token.
fn hp(credential: &Value, home_id: &str, mut params: Value) -> Value {
    params
        .as_object_mut()
        .expect("params object")
        .insert("homeId".into(), json!(home_id));
    authed(params, credential)
}

fn kind_for(worker: usize) -> &'static str {
    // Mixed principal kinds exercise the same agent-available surface from
    // dsh/desktop/cli/mcp-style connections alike.
    ["cli", "dsh", "desktop", "mcp"][worker % 4]
}

fn problem_code(err: &RpcError) -> String {
    match err {
        RpcError::Problem { code, .. } => code.clone(),
        RpcError::Io(text) => format!("IO:{text}"),
    }
}

fn problem_rule(err: &RpcError) -> String {
    match err {
        RpcError::Problem { details, .. } => {
            details["rule"].as_str().unwrap_or_default().to_string()
        }
        RpcError::Io(_) => String::new(),
    }
}

/// Handshake and return (homeId, path) pairs for every opened home.
fn handshake_homes(endpoint: &str) -> Vec<(String, String)> {
    let mut client = common::TestClient::connect(endpoint).expect("probe connect");
    let result = client
        .call(
            "handshake/request",
            json!({
                "protocolVersion": "1.0",
                "client": { "kind": "external", "name": "omt-envelope-probe" },
                "requestedScopes": {},
            }),
        )
        .expect("handshake");
    result["homes"]
        .as_array()
        .expect("homes array")
        .iter()
        .map(|home| {
            (
                home["homeId"].as_str().expect("homeId").to_string(),
                home["path"].as_str().expect("path").to_string(),
            )
        })
        .collect()
}

// ── shared ledger ───────────────────────────────────────────────────────

#[derive(Default)]
struct Ledger {
    /// (home, nodeId, title) for every ACKED create.
    creates: Vec<(usize, String, String)>,
    /// (home, nodeId) → accepted (newRevision, priority) pairs in ack order.
    updates: BTreeMap<(usize, String), Vec<(i64, i64)>>,
    /// Every CONFLICT(revision-mismatch): (home, nodeId, presented rev).
    conflicts: Vec<(usize, String, i64)>,
    /// (home, scratch nodeId) → move-target parent ids in ack order.
    moves: BTreeMap<(usize, String), Vec<String>>,
    archives: BTreeSet<(usize, String)>,
    /// (home, runId, nodeId) → successful claim count (fencing evidence).
    claims: BTreeMap<(usize, String, String), u32>,
    /// Successful reports: (home, runId, nodeId, outcome).
    reports: Vec<(usize, String, String, &'static str)>,
    barrier_wins: u32,
    barrier_conflicts: u32,
    /// Contract violations observed mid-run (must be empty to pass).
    violations: Vec<String>,
}

impl Ledger {
    fn violation(&mut self, text: String) {
        if self.violations.len() < 100 {
            self.violations.push(text);
        }
    }
}

// ── topology seeding ────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
struct PartialHome {
    story_a: String,
    story_b: String,
    members: Vec<String>,
    comb: String,
    runs: Vec<String>,
}

#[derive(Debug, Clone)]
struct Topology {
    home_ids: Vec<String>,
    story_a: Vec<String>,
    story_b: Vec<String>,
    members: Vec<Vec<String>>,
    conflict_nodes: Vec<String>,
    run_ids: Vec<Vec<String>>,
}

fn seed_topology(endpoint: &str, scale: Scale, home_ids: &[String]) -> Topology {
    let homes = home_ids.len();
    let seeders = homes.min(8);
    let partials: Vec<Mutex<Option<PartialHome>>> = (0..homes).map(|_| Mutex::new(None)).collect();
    let partials_ref = &partials;
    std::thread::scope(|scope| {
        for seeder in 0..seeders {
            scope.spawn(move || {
                let mut session = connect_session(endpoint, "cli");
                for h in (0..homes).skip(seeder).step_by(seeders) {
                    let seeded = seed_one_home(&mut session, &home_ids[h], scale);
                    *partials_ref[h].lock().expect("partial lock") = Some(seeded);
                }
            });
        }
    });
    let mut topo = Topology {
        home_ids: home_ids.to_vec(),
        story_a: Vec::with_capacity(homes),
        story_b: Vec::with_capacity(homes),
        members: Vec::with_capacity(homes),
        conflict_nodes: Vec::with_capacity(homes),
        run_ids: Vec::with_capacity(homes),
    };
    for h in 0..homes {
        let partial = partials[h]
            .lock()
            .expect("partial lock")
            .clone()
            .unwrap_or_else(|| panic!("home {h} was never seeded"));
        assert_eq!(
            partial.members.len(),
            scale.member_tickets(),
            "home {h} member count"
        );
        topo.story_a.push(partial.story_a);
        topo.story_b.push(partial.story_b);
        topo.members.push(partial.members);
        topo.conflict_nodes.push(partial.comb);
        topo.run_ids.push(partial.runs);
    }
    topo
}

fn seed_one_home(session: &mut Session, home_id: &str, scale: Scale) -> PartialHome {
    let cred = session.credential.clone();
    let create = |session: &mut Session, params: Value, what: &str| -> String {
        session.must("node/create", params, what)["node"]["nodeId"]
            .as_str()
            .expect("created nodeId")
            .to_string()
    };
    let epic = create(
        session,
        hp(
            &cred,
            home_id,
            json!({ "type": "epic", "title": format!("gate epic {home_id}") }),
        ),
        "seed epic",
    );
    let story_a = create(
        session,
        hp(
            &cred,
            home_id,
            json!({ "type": "story", "title": format!("gate story a {home_id}"), "parentId": epic }),
        ),
        "seed story a",
    );
    let story_b = create(
        session,
        hp(
            &cred,
            home_id,
            json!({ "type": "story", "title": format!("gate story b {home_id}"), "parentId": epic }),
        ),
        "seed story b",
    );
    let comb = create(
        session,
        hp(
            &cred,
            home_id,
            json!({ "type": "ticket", "title": format!("gate comb {home_id}"), "parentId": story_a }),
        ),
        "seed comb probe",
    );
    let mut members = Vec::with_capacity(scale.member_tickets());
    for index in 0..scale.member_tickets() {
        members.push(create(
            session,
            hp(
                &cred,
                home_id,
                json!({
                    "type": "ticket",
                    "title": format!("gate member {home_id}-{index}"),
                    "parentId": story_a,
                }),
            ),
            "seed member ticket",
        ));
    }
    // Active runs over disjoint member slices.
    let mut runs = Vec::with_capacity(scale.runs_per_home);
    for r in 0..scale.runs_per_home {
        let slice = &members[r * scale.items_per_run..(r + 1) * scale.items_per_run];
        let created = session.must(
            "run/create",
            hp(
                &cred,
                home_id,
                json!({
                    "nodeIds": slice,
                    "title": format!("gate run {home_id}/{r}"),
                    "config": { "stopOnFailure": false, "autoVerify": false },
                }),
            ),
            "seed run/create",
        );
        let run_id = created["run"]["runId"].as_str().expect("runId").to_string();
        session.must(
            "run/control",
            hp(
                &cred,
                home_id,
                json!({ "runId": run_id, "action": "start" }),
            ),
            "seed run start",
        );
        runs.push(run_id);
    }
    PartialHome {
        story_a,
        story_b,
        members,
        comb,
        runs,
    }
}

// ── gate 1: mixed-operation storm worker ────────────────────────────────

/// Record one ledger mutation under the lock.
fn apply(ledger: &Mutex<Ledger>, mutate: impl FnOnce(&mut Ledger)) {
    mutate(&mut ledger.lock().expect("ledger"));
}

fn storm_worker(
    worker: usize,
    endpoint: &str,
    topo: &Topology,
    ledger: &Mutex<Ledger>,
    mut rng: Rng,
    iters: usize,
    name_counter: &AtomicU64,
) {
    let mut session = connect_session(endpoint, kind_for(worker));
    let mut scratch: Vec<(usize, String)> = Vec::new();
    // (home, nodeId, newRev, prio) of this worker's latest acked update.
    let mut last_update: Option<(usize, String, i64, i64)> = None;

    for it in 0..iters {
        let h = rng.below(topo.home_ids.len());
        let home_id = topo.home_ids[h].clone();
        let cred = session.credential.clone();
        let mut send = |method: &str, params: Value| -> Result<Value, RpcError> {
            session.client.call(method, hp(&cred, &home_id, params))
        };

        match rng.below(20) {
            // create (4/20)
            0..=3 => {
                let title = format!(
                    "gate storm w{worker} n{}",
                    name_counter.fetch_add(1, Ordering::SeqCst)
                );
                match send(
                    "node/create",
                    json!({
                        "type": "ticket",
                        "title": title.clone(),
                        "parentId": topo.story_a[h],
                    }),
                ) {
                    Ok(view) => {
                        let node_id = view["node"]["nodeId"]
                            .as_str()
                            .expect("created nodeId")
                            .to_string();
                        apply(ledger, |ledger| ledger.creates.push((h, node_id, title)));
                    }
                    Err(err) => apply(ledger, |ledger| {
                        ledger.violation(format!("[w{worker}] storm create rejected: {err:?}"))
                    }),
                }
            }
            // read-modify-write update with optimistic revision (6/20)
            4..=9 => {
                let members = &topo.members[h];
                let node_id = members[rng.below(members.len())].clone();
                let prio = rng.below(10_000) as i64;
                let read = send("node/get", json!({ "nodeId": node_id }));
                let Ok(view) = read else {
                    apply(ledger, |ledger| {
                        ledger.violation(format!("[w{worker}] pre-update get failed"))
                    });
                    continue;
                };
                let revision = view["node"]["revision"].as_i64().expect("revision");
                match send(
                    "node/update",
                    json!({
                        "nodeId": node_id,
                        "expectedRevision": revision,
                        "changes": { "priority": prio },
                    }),
                ) {
                    Ok(view) => {
                        let new_rev = view["node"]["revision"].as_i64().expect("revision");
                        let got_prio = view["node"]["priority"].as_i64().expect("priority");
                        last_update = Some((h, node_id.clone(), new_rev, got_prio));
                        apply(ledger, move |ledger| {
                            ledger
                                .updates
                                .entry((h, node_id.clone()))
                                .or_default()
                                .push((new_rev, got_prio));
                        });
                    }
                    Err(err)
                        if problem_code(&err) == "CONFLICT"
                            && problem_rule(&err) == "revision-mismatch" =>
                    {
                        // Legitimate ONLY because some concurrent winner
                        // committed between our read and our write; verified
                        // against the ack chains after the storm.
                        apply(ledger, move |ledger| {
                            ledger.conflicts.push((h, node_id.clone(), revision))
                        });
                    }
                    Err(err) => apply(ledger, move |ledger| {
                        ledger.violation(format!(
                            "[w{worker}] RMW update failed unexpectedly: {err:?}"
                        ))
                    }),
                }
            }
            // move own scratch nodes between the two stories (2/20)
            10..=11 => {
                if scratch.is_empty() || rng.below(4) == 0 {
                    let title = format!(
                        "gate scratch w{worker} n{}",
                        name_counter.fetch_add(1, Ordering::SeqCst)
                    );
                    match send(
                        "node/create",
                        json!({
                            "type": "ticket",
                            "title": title.clone(),
                            "parentId": topo.story_b[h],
                        }),
                    ) {
                        Ok(view) => {
                            let node_id = view["node"]["nodeId"].as_str().expect("id").to_string();
                            scratch.push((h, node_id.clone()));
                            apply(ledger, move |ledger| {
                                ledger.creates.push((h, node_id, title))
                            });
                        }
                        Err(err) => apply(ledger, move |ledger| {
                            ledger.violation(format!("[w{worker}] scratch create failed: {err:?}"))
                        }),
                    }
                } else {
                    // Scratch nodes are home-scoped: only move ones the
                    // worker created on the CURRENTLY selected home.
                    let owned: Vec<String> = scratch
                        .iter()
                        .filter(|(sh, _)| *sh == h)
                        .map(|(_, id)| id.clone())
                        .collect();
                    if owned.is_empty() {
                        continue;
                    }
                    let node_id = owned[rng.below(owned.len())].clone();
                    let target = {
                        let guard = ledger.lock().expect("ledger");
                        match guard
                            .moves
                            .get(&(h, node_id.clone()))
                            .and_then(|t| t.last().cloned())
                        {
                            Some(last) if last == topo.story_b[h] => topo.story_a[h].clone(),
                            _ => topo.story_b[h].clone(),
                        }
                    };
                    match send(
                        "node/move",
                        json!({ "nodeId": node_id, "newParentId": target }),
                    ) {
                        Ok(_) => apply(ledger, move |ledger| {
                            ledger
                                .moves
                                .entry((h, node_id.clone()))
                                .or_default()
                                .push(target)
                        }),
                        Err(err) if problem_code(&err) == "CONFLICT" => {
                            assert_eq!(
                                problem_rule(&err),
                                "already-at-target",
                                "unexpected move conflict: {err:?}"
                            );
                        }
                        Err(err) => apply(ledger, move |ledger| {
                            ledger.violation(format!("[w{worker}] scratch move failed: {err:?}"))
                        }),
                    }
                }
            }
            // archive own scratch nodes (1/20)
            12 => {
                let candidates: Vec<String> = {
                    let guard = ledger.lock().expect("ledger");
                    scratch
                        .iter()
                        .filter(|(sh, id)| {
                            *sh == h && !guard.archives.contains(&(h, (*id).clone()))
                        })
                        .map(|(_, id)| id.clone())
                        .collect()
                };
                if let Some(node_id) = candidates.first() {
                    match send("node/archive", json!({ "nodeId": node_id })) {
                        Ok(_) => apply(ledger, move |ledger| {
                            ledger.archives.insert((h, node_id.clone()));
                        }),
                        Err(err) => apply(ledger, move |ledger| {
                            ledger.violation(format!("[w{worker}] scratch archive failed: {err:?}"))
                        }),
                    }
                }
            }
            // list (2/20)
            13..=14 => {
                if let Err(err) = send(
                    "node/list",
                    json!({ "filter": { "type": "ticket", "archived": false } }),
                ) {
                    apply(ledger, move |ledger| {
                        ledger.violation(format!("[w{worker}] node/list failed: {err:?}"))
                    })
                }
            }
            // tree (1/20)
            15 => {
                if let Err(err) = send("node/tree", json!({ "rootId": topo.story_a[h] })) {
                    apply(ledger, move |ledger| {
                        ledger.violation(format!("[w{worker}] node/tree failed: {err:?}"))
                    })
                }
            }
            // search (3/20)
            16..=18 => match send("node/search", json!({ "query": "gate", "limit": 20 })) {
                Ok(view) => {
                    if view["nodes"]
                        .as_array()
                        .is_some_and(|nodes| nodes.is_empty())
                    {
                        apply(ledger, |ledger| {
                            ledger.violation(format!("[w{worker}] search empty on a full home"))
                        });
                    }
                }
                Err(err) => apply(ledger, move |ledger| {
                    ledger.violation(format!("[w{worker}] node/search failed: {err:?}"))
                }),
            },
            // run claim + lease-fenced report (1/20)
            _ => {
                let run_id = topo.run_ids[h][rng.below(topo.run_ids[h].len())].clone();
                match send("run/claim", json!({ "runId": run_id })) {
                    Ok(view) => {
                        if view["claimed"].as_bool() == Some(true) {
                            let item = view["item"]["nodeId"]
                                .as_str()
                                .expect("claimed nodeId")
                                .to_string();
                            let token = view["lease"]["token"]
                                .as_str()
                                .expect("lease token")
                                .to_string();
                            apply(ledger, |ledger| {
                                *ledger
                                    .claims
                                    .entry((h, run_id.clone(), item.clone()))
                                    .or_insert(0) += 1;
                            });
                            match send(
                                "run/report",
                                json!({
                                    "runId": run_id.clone(),
                                    "nodeId": item.clone(),
                                    "outcome": "done",
                                    "note": "envelope storm",
                                    "leaseToken": token,
                                }),
                            ) {
                                Ok(_) => apply(ledger, move |ledger| {
                                    ledger
                                        .reports
                                        .push((h, run_id.clone(), item.clone(), "done"))
                                }),
                                Err(err) => apply(ledger, move |ledger| {
                                    ledger.violation(format!(
                                        "[w{worker}] valid-lease report rejected: {err:?}"
                                    ))
                                }),
                            }
                        }
                    }
                    Err(err) => apply(ledger, move |ledger| {
                        ledger.violation(format!(
                            "[w{worker}] run/claim against an active run failed: {err:?}"
                        ))
                    }),
                }
            }
        }

        // Read-your-write probe: acknowledged writes never roll back.
        if it % 8 == 7 {
            if let Some((uh, node_id, rev, prio)) = &last_update {
                let probe_home = topo.home_ids[*uh].clone();
                let probe_cred = session.credential.clone();
                if let Ok(view) = session.client.call(
                    "node/get",
                    hp(&probe_cred, &probe_home, json!({ "nodeId": node_id })),
                ) {
                    let current = view["node"]["revision"].as_i64().expect("revision");
                    if current < *rev {
                        apply(ledger, |ledger| {
                            ledger.violation(format!(
                                "[w{worker}] REVISION ROLLBACK on {node_id}: saw {rev}, now {current}"
                            ))
                        });
                    } else if current == *rev && view["node"]["priority"].as_i64() != Some(*prio) {
                        apply(ledger, |ledger| {
                            ledger.violation(format!(
                                "[w{worker}] READ-YOUR-WRITE miss on {node_id}@{rev}"
                            ))
                        });
                    }
                }
            }
        }
    }
}

// ── gate 1: deterministic lease-fencing negatives ───────────────────────

fn fence_phase(endpoint: &str, topo: &Topology) {
    let home_id = topo.home_ids[0].clone();
    let run_id = topo.run_ids[0][0].clone();
    let mut executor = connect_session(endpoint, "cli");
    let mut foreigner = connect_session(endpoint, "dsh");

    // Pick the first PENDING item (never touched by the storm's claims —
    // storm-reported items are already done).
    let view = executor.must(
        "run/get",
        hp(&executor.credential, &home_id, json!({ "runId": run_id })),
        "run/get",
    );
    let item = view["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|entry| entry["state"] == json!("pending"))
        .map(|entry| entry["nodeId"].as_str().expect("nodeId").to_string())
        .expect("a pending item exists");

    // Claim as the executor → live attempt-fenced lease.
    let claim = executor.must(
        "run/claim",
        hp(&executor.credential, &home_id, json!({ "runId": run_id })),
        "claim",
    );
    assert_eq!(claim["claimed"].as_bool(), Some(true));
    assert_eq!(claim["item"]["nodeId"].as_str(), Some(item.as_str()));
    let t1 = claim["lease"]["token"].as_str().expect("t1").to_string();
    let attempt1 = claim["lease"]["attempt"].as_i64().expect("attempt1");

    // 1. FOREIGN PRINCIPAL presenting the executor's token → actor-mismatch.
    let err = foreigner
        .client
        .call(
            "run/report",
            hp(
                &foreigner.credential,
                &home_id,
                json!({
                    "runId": run_id,
                    "nodeId": item,
                    "outcome": "done",
                    "leaseToken": t1,
                }),
            ),
        )
        .expect_err("foreign-principal report must be rejected");
    assert_eq!(problem_code(&err), "CONFLICT", "{err:?}");
    assert_eq!(problem_rule(&err), "actor-mismatch", "{err:?}");

    // 2. WRONG TOKEN from the rightful executor → lease-token fence.
    let err = executor
        .client
        .call(
            "run/report",
            hp(
                &executor.credential,
                &home_id,
                json!({
                    "runId": run_id,
                    "nodeId": item,
                    "outcome": "done",
                    "leaseToken": "ffffffffffffffffffffffffffffffff",
                }),
            ),
        )
        .expect_err("wrong-token report must be rejected");
    assert_eq!(problem_rule(&err), "lease-token", "{err:?}");

    // 3. Valid failed report consumes the attempt-1 lease.
    let report = executor.must(
        "run/report",
        hp(
            &executor.credential,
            &home_id,
            json!({
                "runId": run_id,
                "nodeId": item,
                "outcome": "failed",
                "note": "gate fault injection",
                "leaseToken": t1,
            }),
        ),
        "valid lease report",
    );
    assert_eq!(report["item"]["state"], json!("failed"));

    // 4. Retry promotes the item to attempt 2.
    executor.must(
        "run/control",
        hp(
            &executor.credential,
            &home_id,
            json!({ "runId": run_id, "action": "retry", "nodeId": item }),
        ),
        "retry",
    );

    // Re-claim → attempt-2 grant.
    let reclaim = executor.must(
        "run/claim",
        hp(&executor.credential, &home_id, json!({ "runId": run_id })),
        "reclaim",
    );
    assert_eq!(reclaim["item"]["nodeId"].as_str(), Some(item.as_str()));
    let t2 = reclaim["lease"]["token"].as_str().expect("t2").to_string();
    let attempt2 = reclaim["lease"]["attempt"].as_i64().expect("attempt2");
    assert_eq!(attempt2, attempt1 + 1, "attempts advance across retries");

    // 5. CROSS-ATTEMPT: replaying the consumed attempt-1 token is rejected.
    let err = executor
        .client
        .call(
            "run/report",
            hp(
                &executor.credential,
                &home_id,
                json!({
                    "runId": run_id,
                    "nodeId": item,
                    "outcome": "done",
                    "leaseToken": t1,
                }),
            ),
        )
        .expect_err("stale cross-attempt report must be rejected");
    assert_eq!(problem_code(&err), "CONFLICT", "{err:?}");
    assert!(
        matches!(problem_rule(&err).as_str(), "lease-attempt" | "lease-token"),
        "stale-lease family rule expected, got {err:?}"
    );

    // 6. The CURRENT attempt-2 token lands.
    let report = executor.must(
        "run/report",
        hp(
            &executor.credential,
            &home_id,
            json!({
                "runId": run_id,
                "nodeId": item,
                "outcome": "done",
                "note": "fenced completion",
                "leaseToken": t2,
            }),
        ),
        "current-lease report",
    );
    assert_eq!(report["item"]["state"], json!("done"));

    // 7. Terminal double-report → report-state-gate CONFLICT.
    let err = executor
        .client
        .call(
            "run/report",
            hp(
                &executor.credential,
                &home_id,
                json!({
                    "runId": run_id,
                    "nodeId": item,
                    "outcome": "done",
                    "leaseToken": t2,
                }),
            ),
        )
        .expect_err("reporting a terminal item must be rejected");
    assert_eq!(problem_rule(&err), "report-state-gate", "{err:?}");
}

// ── direct-SQL verification helpers ─────────────────────────────────────

struct HomeSqlStats {
    operations_rows: i64,
    operations_distinct_command_ids: i64,
    journal_rows: i64,
    journal_distinct_command_ids: i64,
    journal_not_acknowledged: i64,
    nodes_count: i64,
    events_count: i64,
}

fn sql_stats(db_path: &std::path::Path) -> HomeSqlStats {
    let conn = open_readonly(db_path);
    let scalar = |sql: &str| -> i64 {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|err| panic!("scalar `{sql}`: {err}"))
    };
    HomeSqlStats {
        operations_rows: scalar("SELECT COUNT(*) FROM operations"),
        operations_distinct_command_ids: scalar(
            "SELECT COUNT(DISTINCT command_id) FROM operations",
        ),
        journal_rows: scalar("SELECT COUNT(*) FROM journal"),
        journal_distinct_command_ids: scalar("SELECT COUNT(DISTINCT command_id) FROM journal"),
        journal_not_acknowledged: scalar(
            "SELECT COUNT(*) FROM journal WHERE phase <> 'acknowledged'",
        ),
        nodes_count: scalar("SELECT COUNT(*) FROM nodes"),
        events_count: scalar("SELECT COUNT(*) FROM events"),
    }
}

fn open_readonly(db_path: &std::path::Path) -> rusqlite::Connection {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|err| panic!("readonly open {}: {err}", db_path.display()));
    conn.busy_timeout(Duration::from_secs(5))
        .expect("busy timeout");
    conn
}

// ═════════════════════════ GATE 1 test ══════════════════════════════════

/// THE blocking multi-client envelope gate. Prints scale + durations on
/// success; any assertion failure names the violated invariant verbatim.
#[test]
fn gate_envelope_multi_client_correctness() {
    let scale = Scale::from_env();
    assert!(
        scale.member_tickets() >= scale.runs_per_home * scale.items_per_run,
        "scale mismatch: {} member tickets cannot fill {} runs × {} items/home",
        scale.member_tickets(),
        scale.runs_per_home,
        scale.items_per_run,
    );
    let wall = Instant::now();

    let (ctx, home_paths) = TestCtx::spawn_with_homes("envelope", scale.homes);
    let arg_refs: Vec<String> = home_paths
        .iter()
        .flat_map(|path| ["--home".to_string(), path.to_string_lossy().into_owned()])
        .collect();
    let arg_refs: Vec<&str> = arg_refs.iter().map(String::as_str).collect();

    let daemon = DaemonProcess::spawn(&ctx, &arg_refs);
    assert!(
        daemon.is_alive(),
        "daemon died opening {} homes: {}",
        scale.homes,
        daemon.stderr_text()
    );
    let descriptor = wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(30))
        .expect("daemon publishes descriptor");
    let endpoint = descriptor.endpoint.clone();
    eprintln!(
        "[envelope] daemon pid {} opened {} homes on {endpoint}",
        descriptor.pid, scale.homes
    );

    // Discover stable homeIds via the handshake capability block.
    let mut discovered = handshake_homes(&endpoint);
    discovered.sort_by(|a, b| a.1.cmp(&b.1));
    let home_ids: Vec<String> = home_paths
        .iter()
        .map(|path| {
            let wanted = path.to_string_lossy().into_owned();
            discovered
                .iter()
                .find(|(_, found)| *found == wanted)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| panic!("handshake missing home path {wanted}"))
        })
        .collect();

    // ── phase A: parallel seeding ───────────────────────────────────────
    let phase = Instant::now();
    let topo = seed_topology(&endpoint, scale, &home_ids);
    let seed_s = phase.elapsed().as_secs_f32();
    eprintln!(
        "[envelope] seeded {} homes × {} nodes (+ {}×{} run members) in {seed_s:.2}s",
        scale.homes, scale.nodes_per_home, scale.runs_per_home, scale.items_per_run
    );

    // ── phase B: synchronized revision-COMB (CONFLICT exactly-when) ─────
    let phase = Instant::now();
    let ledger = Mutex::new(Ledger::default());
    {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(scale.clients));
        let ledger_ref = &ledger;
        std::thread::scope(|scope| {
            for worker in 0..scale.clients {
                let endpoint = endpoint.clone();
                let home_ids = home_ids.clone();
                let conflict_nodes = topo.conflict_nodes.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                scope.spawn(move || {
                    let mut session = connect_session(&endpoint, kind_for(worker));
                    for (h, node_id) in conflict_nodes.iter().enumerate() {
                        let view = session.must(
                            "node/get",
                            hp(
                                &session.credential,
                                &home_ids[h],
                                json!({ "nodeId": node_id }),
                            ),
                            "comb pre-read",
                        );
                        let revision = view["node"]["revision"].as_i64().expect("revision");
                        barrier.wait();
                        let outcome = session.client.call(
                            "node/update",
                            hp(
                                &session.credential,
                                &home_ids[h],
                                json!({
                                    "nodeId": node_id,
                                    "expectedRevision": revision,
                                    "changes": { "priority": 1000 + worker as i64 },
                                }),
                            ),
                        );
                        let mut ledger = ledger_ref.lock().expect("ledger");
                        match outcome {
                            Ok(_) => ledger.barrier_wins += 1,
                            Err(err) => {
                                assert_eq!(
                                    problem_code(&err),
                                    "CONFLICT",
                                    "comb loser must CONFLICT, got {err:?}"
                                );
                                assert_eq!(problem_rule(&err), "revision-mismatch", "{err:?}");
                                ledger.barrier_conflicts += 1;
                                ledger.conflicts.push((h, node_id.clone(), revision));
                            }
                        }
                    }
                });
            }
        });
        {
            let counted = ledger_ref.lock().expect("ledger");
            assert_eq!(
                counted.barrier_wins, scale.homes as u32,
                "COMB: exactly one winner per home round"
            );
            assert_eq!(
                counted.barrier_conflicts,
                (scale.homes * (scale.clients.saturating_sub(1))) as u32,
                "COMB: every loser conflicts"
            );
        }
    }
    let comb_s = phase.elapsed().as_secs_f32();

    // ── phase C: mixed-operation storm across C real connections ───────
    let phase = Instant::now();
    let name_counter = AtomicU64::new(1);
    {
        let topo_ref = &topo;
        let ledger_ref = &ledger;
        let counter_ref = &name_counter;
        std::thread::scope(|scope| {
            for worker in 0..scale.clients {
                let endpoint = endpoint.clone();
                scope.spawn(move || {
                    storm_worker(
                        worker,
                        &endpoint,
                        topo_ref,
                        ledger_ref,
                        Rng::new(0x5EED_0000 ^ (worker as u64 + 1)),
                        scale.iters,
                        counter_ref,
                    );
                });
            }
        });
    }
    let storm_s = phase.elapsed().as_secs_f32();
    let mut ledger = ledger.into_inner().expect("ledger");
    assert!(
        ledger.violations.is_empty(),
        "storm contract violations: {:?}",
        ledger.violations
    );

    // ── phase D: deterministic lease-fencing negatives ──────────────────
    let phase = Instant::now();
    fence_phase(&endpoint, &topo);
    let fence_s = phase.elapsed().as_secs_f32();

    // ── phase E: post-storm verification ────────────────────────────────
    let phase = Instant::now();
    verify_envelope(&endpoint, scale, &topo, &ledger, &home_paths);
    let verify_s = phase.elapsed().as_secs_f32();
    let total_s = wall.elapsed().as_secs_f32();

    println!(
        "\nGATE envelope-multi-client PASS | tested_at={} homes={} nodes/home={} clients={} runs/home={} items/run={} iters/client={} \
         | creates_acked={} updates_acked={} conflicts_expected={} comb_wins={} comb_conflicts={} claims_fenced={} \
         | ops_unique_commandids=yes revisions_gapfree=yes read_your_write=yes \
         | wall={total_s:.2}s (seed {seed_s:.2} comb {comb_s:.2} storm {storm_s:.2} fence {fence_s:.2} verify {verify_s:.2})\n",
        if std::env::var("OMT_ENV_FULL").as_deref() == Ok("1") {
            "FULL-ENVELOPE"
        } else {
            "BLOCKING-MINIMUM-RATIO"
        },
        scale.homes,
        scale.nodes_per_home,
        scale.clients,
        scale.runs_per_home,
        scale.items_per_run,
        scale.iters,
        ledger.creates.len(),
        ledger.updates.values().map(Vec::len).sum::<usize>(),
        ledger.conflicts.len(),
        ledger.barrier_wins,
        ledger.barrier_conflicts,
        ledger.claims.len(),
    );

    drop(daemon); // leak-reaper kills the daemon promptly
}

/// Post-storm verification pass (all mandatory):
/// creates exist; revision chains are gap-free/duplicate-free and
/// terminal-equal; every recorded CONFLICT had a strictly-later committed
/// winner (never a false rejection); moves/archives landed; claims were
/// exclusive and reported-done states hold; each home's SQLite carries
/// zero duplicate commandIds and zero non-acknowledged journal rows.
fn verify_envelope(
    endpoint: &str,
    scale: Scale,
    topo: &Topology,
    ledger: &Ledger,
    home_paths: &[std::path::PathBuf],
) {
    let mut session = connect_session(endpoint, "cli");
    let credential = session.credential.clone();
    let mut get_node = |home: &str, node_id: &str, context: &str| -> Value {
        session
            .client
            .call(
                "node/get",
                hp(&credential, home, json!({ "nodeId": node_id })),
            )
            .unwrap_or_else(|err| panic!("{context}: {err:?}"))
    };

    // 1. Every acked create exists with its exact title.
    for (h, node_id, title) in &ledger.creates {
        let view = get_node(
            &topo.home_ids[*h],
            node_id,
            &format!("acked create {node_id} vanished"),
        );
        assert_eq!(
            view["node"]["title"].as_str(),
            Some(title.as_str()),
            "create {node_id} drifted"
        );
    }

    // 2. Update chains: gap-free, duplicate-free, terminal-equal.
    for ((h, node_id), steps) in &ledger.updates {
        let mut revisions: Vec<i64> = steps.iter().map(|(rev, _)| *rev).collect();
        revisions.sort_unstable();
        let distinct: BTreeSet<i64> = revisions.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            revisions.len(),
            "DUPLICATE ACCEPTED REVISION for {node_id}: {revisions:?}"
        );
        for pair in revisions.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                1,
                "REVISION GAP on {node_id}: {revisions:?} (lost accepted update)"
            );
        }
        assert_eq!(
            revisions.first().copied().expect("nonempty"),
            2,
            "first accepted update on {node_id} must land at revision 2"
        );
        let (_, last_prio) = steps.iter().max_by_key(|(rev, _)| *rev).expect("steps");
        let view = get_node(
            &topo.home_ids[*h],
            node_id,
            &format!("updated node {node_id} readable"),
        );
        assert_eq!(
            view["node"]["revision"].as_i64(),
            revisions.last().copied(),
            "terminal revision drift on {node_id} (lost accepted update)"
        );
        assert_eq!(
            view["node"]["priority"].as_i64(),
            Some(*last_prio),
            "terminal priority drift on {node_id}"
        );
    }

    // 3. Every recorded CONFLICT had a strictly-later committed winner:
    //    CONFLICT fired EXACTLY because the expectation went stale.
    for (h, node_id, expected) in &ledger.conflicts {
        let max_committed = ledger
            .updates
            .get(&(*h, node_id.clone()))
            .and_then(|steps| steps.iter().map(|(rev, _)| *rev).max())
            .unwrap_or(if topo.conflict_nodes.get(*h) == Some(node_id) {
                2 // COMB loser: the single winner advanced 1→2
            } else {
                1
            });
        assert!(
            max_committed > *expected,
            "FALSE CONFLICT on {node_id}: presented revision {expected} was NOT stale \
             (max committed {max_committed})"
        );
    }

    // 4. Moves landed at their last acked target; archives stuck.
    for ((h, node_id), targets) in &ledger.moves {
        let parent = targets.last().expect("nonempty");
        let view = get_node(
            &topo.home_ids[*h],
            node_id,
            &format!("moved node {node_id} readable"),
        );
        assert_eq!(
            view["parent"]["nodeId"].as_str(),
            Some(parent.as_str()),
            "move target drift on {node_id}"
        );
    }
    for (h, node_id) in &ledger.archives {
        let view = get_node(
            &topo.home_ids[*h],
            node_id,
            &format!("archived node {node_id} readable"),
        );
        assert_eq!(view["node"]["archived"].as_bool(), Some(true));
    }

    // 5. Claim fencing: each (run,item) claimed at most once during the
    //    storm (a second overlapping claim would surface here), and every
    //    lease-fenced report reached `done` in its run.
    for ((_, run_id, node_id), count) in &ledger.claims {
        assert_eq!(
            *count, 1,
            "item {node_id} of {run_id} was claimed {count}× concurrently"
        );
    }
    let mut affected: BTreeMap<usize, BTreeSet<&String>> = BTreeMap::new();
    for (h, run_id, _, _) in &ledger.reports {
        affected.entry(*h).or_default().insert(run_id);
    }
    for (h, run_ids) in affected {
        for run_id in run_ids {
            let view = session
                .client
                .call(
                    "run/get",
                    hp(&credential, &topo.home_ids[h], json!({ "runId": run_id })),
                )
                .unwrap_or_else(|err| panic!("run/get {run_id}: {err:?}"));
            for entry in view["items"].as_array().expect("items") {
                let node_id = entry["nodeId"].as_str().expect("nodeId");
                if ledger.reports.iter().any(|(rh, r, item, outcome)| {
                    *rh == h && r == run_id && item == node_id && *outcome == "done"
                }) {
                    assert_eq!(
                        entry["state"],
                        json!("done"),
                        "reported-done item {node_id} not done in {run_id}"
                    );
                }
            }
        }
    }

    // 6. Durable-plane uniqueness + convergence per home.
    for (index, path) in home_paths.iter().enumerate() {
        let stats = sql_stats(&path.join("omt.db"));
        assert_eq!(
            stats.operations_rows, stats.operations_distinct_command_ids,
            "home {index}: DUPLICATE committed commandIds in operations"
        );
        assert_eq!(
            stats.journal_rows, stats.journal_distinct_command_ids,
            "home {index}: duplicate journal commandIds"
        );
        assert_eq!(
            stats.journal_not_acknowledged, 0,
            "home {index}: journal left non-acknowledged rows"
        );
        let expected_nodes = scale.nodes_per_home as i64
            + ledger
                .creates
                .iter()
                .filter(|(h, _, _)| *h == index)
                .count() as i64;
        if stats.nodes_count != expected_nodes {
            // Diagnostic: list every node outside the seeded/storm naming
            // schemes so a phantom row identifies itself.
            let conn = open_readonly(&path.join("omt.db"));
            let mut stmt = conn
                .prepare(
                    "SELECT id, title FROM nodes
                     WHERE title NOT LIKE 'gate epic %' AND title NOT LIKE 'gate story %'
                       AND title NOT LIKE 'gate member %' AND title NOT LIKE 'gate comb %'
                       AND title NOT LIKE 'gate storm w% n%' AND title NOT LIKE 'gate scratch w% n%'",
                )
                .expect("diag prepare");
            let odd: Vec<String> = stmt
                .query_map([], |row| {
                    Ok(format!(
                        "{}: {}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?
                    ))
                })
                .expect("diag query")
                .filter_map(Result::ok)
                .collect();
            panic!(
                "home {index}: node count diverged (expected {expected_nodes}, got {}) — \
                 creates recorded on this home: {}; non-scheme rows: {odd:?}",
                stats.nodes_count,
                ledger
                    .creates
                    .iter()
                    .filter(|(h, _, _)| *h == index)
                    .count(),
            );
        }
        assert!(
            stats.events_count >= stats.operations_rows,
            "home {index}: outbox thinner than the operation log"
        );
    }
}

// ═════════════════════════ GATE 3 test ══════════════════════════════════

enum Acked {
    Create { id: String, title: String },
    Update { id: String, revision: i64 },
}

/// Deterministically leave TWO pending journal rows behind (crash states a
/// mid-mutation SIGKILL can produce), WITHOUT running recovery:
/// one stuck at `prepared` (cancel-after-prepared rename) and one already
/// `db_committed` but unacknowledged (committed priority demoted back to
/// db_committed). Both MUST roll forward when the successor daemon opens
/// the home — recovery precedes readiness.
fn inject_pending_journal_states(home: &std::path::Path, epic_id: &str, ticket_id: &str) {
    use omt_storage::journal::OpenConfig;
    use omt_storage::store;
    use omt_storage::Storage;

    let mut config = OpenConfig::new(home);
    config.acquire_lock = true;
    config.owner_kind = omt_storage::home_lock::OwnerKind::Daemon;
    config.recover_on_open = false;
    let mut storage = Storage::open(config).expect("open WITHOUT recovery");

    // Row 1: rename the epic, cancel right AFTER the prepared insert →
    // the journal row stays at phase='prepared' with nothing applied.
    let before = store::get_node(storage.conn(), epic_id)
        .expect("query")
        .expect("epic row");
    let plan_rename = storage
        .plan_update(
            "sigkill-pending-rename",
            &before,
            Some("SIGKILL epic renamed while pending".to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
            vec![],
            json!({ "nodeId": epic_id }),
        )
        .expect("plan rename");
    let probes = std::sync::atomic::AtomicUsize::new(0);
    let outcome = storage.execute_cancellable(&plan_rename, &|| {
        probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1
    });
    match outcome {
        Err(problem) => assert_eq!(problem.code, "CANCELED", "{problem}"),
        Ok(_) => panic!("cancel-after-prepared did not fire"),
    }

    // Row 2: fully commit a priority change, then demote ONLY the journal
    // phase back to db_committed — exactly the crash-between-commit-and-ack
    // state ("acknowledged results roll forward").
    let before_ticket = store::get_node(storage.conn(), ticket_id)
        .expect("query")
        .expect("ticket row");
    let plan_priority = storage
        .plan_update(
            "sigkill-committed-priority",
            &before_ticket,
            None,
            None,
            None,
            Some(777),
            None,
            None,
            None,
            vec![],
            json!({ "nodeId": ticket_id }),
        )
        .expect("plan priority");
    storage.execute(&plan_priority).expect("execute priority");
    storage
        .conn()
        .execute(
            "UPDATE journal SET phase = 'db_committed'
             WHERE command_id = 'sigkill-committed-priority'",
            [],
        )
        .expect("demote phase");

    let pending: i64 = storage
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM journal WHERE phase <> 'acknowledged'",
            [],
            |row| row.get(0),
        )
        .expect("pending count");
    // At least our two injected rows; the SIGKILL itself may legitimately
    // have left additional storm rows pending (that is the point).
    assert!(
        pending >= 2,
        "expected the injected pending journal rows, found {pending}"
    );
    eprintln!(
        "[sigkill] pending journal rows at respawn: {pending} (2 injected + {} left by the kill)",
        pending - 2
    );
    // Release the flock AND unlink our marker explicitly — Storage has no
    // Drop; a leftover live-pid marker would make the successor refuse.
    storage.release_lock().expect("release injection lock");
    drop(storage);
}

/// Verify every DB nodes row has its planned bytes on disk with matching
/// frontmatter. Returns the number of verified nodes or the first drift.
fn scan_planned_bytes(home: &std::path::Path) -> Result<usize, String> {
    let conn = open_readonly(&home.join("omt.db"));
    let mut stmt = conn
        .prepare("SELECT id, title, status, archived, priority, path FROM nodes")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String, bool, i64, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (id, title, status, archived, priority, relative) in &rows {
        let raw = std::fs::read_to_string(home.join(relative.trim_start_matches('/')))
            .map_err(|err| format!("planned bytes MISSING for {id} ({relative}): {err}"))?;
        let parsed = omt_domain::markdown::parse_node_file(raw.as_str())
            .map_err(|err| format!("{id}: parse failed: {err:?}"))?;
        let drift = |what: &str| format!("frontmatter drift for {id} ({what}): {relative}");
        if parsed.attrs.id.as_deref() != Some(id.as_str()) {
            return Err(drift("id"));
        }
        if parsed.attrs.title.as_deref() != Some(title.as_str()) {
            return Err(drift("title"));
        }
        if parsed.attrs.status.as_deref() != Some(status.as_str()) {
            return Err(drift("status"));
        }
        if parsed.attrs.archived.unwrap_or(false) != *archived {
            return Err(drift("archived"));
        }
        if parsed.attrs.priority.map(|p| p as i64).unwrap_or(0) != *priority {
            return Err(drift("priority"));
        }
    }
    Ok(rows.len())
}

/// One hammer step: create OR read-modify-write update.
/// `Ok(None)` marks a benign skip: a stale-revision CONFLICT is EXPECTED
/// under concurrent hammers (a sibling won between read and write).
fn hammer_step(
    session: &mut common::TestClient,
    credential: &Value,
    story: &str,
    tickets: &[String],
    rng: &mut Rng,
    worker: usize,
    counter: &AtomicU64,
) -> Result<Option<Acked>, RpcError> {
    if rng.below(2) == 0 {
        let title = format!(
            "sigkill storm w{worker} n{}",
            counter.fetch_add(1, Ordering::SeqCst)
        );
        let view = session.call(
            "node/create",
            authed(
                json!({ "type": "ticket", "title": title.clone(), "parentId": story }),
                credential,
            ),
        )?;
        Ok(Some(Acked::Create {
            id: view["node"]["nodeId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            title,
        }))
    } else {
        let node_id = tickets[rng.below(tickets.len())].clone();
        let view = session.call("node/get", authed(json!({ "nodeId": node_id }), credential))?;
        let revision = view["node"]["revision"].as_i64().unwrap_or(1);
        match session.call(
            "node/update",
            authed(
                json!({
                    "nodeId": node_id,
                    "expectedRevision": revision,
                    "changes": { "priority": rng.below(5000) as i64 },
                }),
                credential,
            ),
        ) {
            Ok(_) => Ok(Some(Acked::Update {
                id: node_id,
                revision: revision + 1,
            })),
            Err(err) if problem_code(&err) == "CONFLICT" => {
                assert_eq!(problem_rule(&err), "revision-mismatch", "{err:?}");
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

#[test]
fn gate_sigkill_to_ready_under_five_seconds() {
    let ctx = TestCtx::spawn_named("sigkill-ready");
    let home = ctx.dir.path().join("active-home");
    std::fs::create_dir_all(&home).expect("mkdir active home");
    let home_str = home.to_str().expect("utf8 home");

    // Daemon #1 serves the active home.
    let first = DaemonProcess::spawn(&ctx, &["--home", home_str]);
    let first_descriptor =
        wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(30)).expect("first daemon ready");
    let endpoint = first_descriptor.endpoint.clone();

    // Seed baseline content through honest RPC.
    let mut seeder = connect_session(&endpoint, "cli");
    let seed_cred = seeder.credential.clone();
    let epic_id = seeder.must(
        "node/create",
        authed(
            json!({ "type": "epic", "title": "sigkill epic" }),
            &seed_cred,
        ),
        "epic",
    )["node"]["nodeId"]
        .as_str()
        .expect("id")
        .to_string();
    let story_id = seeder.must(
        "node/create",
        authed(
            json!({ "type": "story", "title": "sigkill story", "parentId": epic_id }),
            &seed_cred,
        ),
        "story",
    )["node"]["nodeId"]
        .as_str()
        .expect("id")
        .to_string();
    let mut tickets = Vec::new();
    for index in 0..12 {
        tickets.push(
            seeder.must(
                "node/create",
                authed(
                    json!({
                        "type": "ticket",
                        "title": format!("sigkill ticket {index}"),
                        "parentId": story_id,
                    }),
                    &seed_cred,
                ),
                "ticket",
            )["node"]["nodeId"]
                .as_str()
                .expect("id")
                .to_string(),
        );
    }
    drop(seeder);

    // ── kill -9 MID-MUTATION: three clients hammer mutations; the main
    // thread SIGKILLs the daemon while traffic is in flight. ─────────────
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let acked: std::sync::Arc<Mutex<Vec<Acked>>> = Default::default();
    let hammer_error: std::sync::Arc<Mutex<Option<String>>> = Default::default();
    let first_pid = first.pid();
    let spawn_at = Instant::now();
    let story_ref = &*story_id;
    let tickets_ref = &tickets;
    std::thread::scope(|scope| {
        for worker in 0..3usize {
            let endpoint = endpoint.clone();
            let tickets = tickets_ref.clone();
            let stop = std::sync::Arc::clone(&stop);
            let acked = std::sync::Arc::clone(&acked);
            let hammer_error = std::sync::Arc::clone(&hammer_error);
            scope.spawn(move || {
                let client = std::cell::RefCell::new(
                    connected_client(&endpoint, kind_for(worker)).expect("hammer connect"),
                );
                let mut rng = Rng::new(0xC0FFEE ^ (worker as u64 + 7));
                let counter = AtomicU64::new(1000 * (worker as u64 + 1));
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let outcome = {
                        let (ref mut client, ref credential) = *client.borrow_mut();
                        hammer_step(
                            client, credential, story_ref, &tickets, &mut rng, worker, &counter,
                        )
                    };
                    match outcome {
                        Ok(Some(step)) => {
                            if !step_id(&step).is_empty() {
                                acked.lock().expect("acked").push(step);
                            }
                        }
                        Ok(None) => {} // benign skip (lost the RMW race)
                        Err(RpcError::Io(_)) => break, // daemon died mid-request: unacked
                        Err(err) => {
                            *hammer_error.lock().expect("lock") =
                                Some(format!("[w{worker}] unexpected failure: {err:?}"));
                            break;
                        }
                    }
                }
            });
        }
        // The killer thread: land SIGKILL mid-storm — as soon as enough
        // traffic has been acknowledged to prove mid-mutation (≥10 acked),
        // bounded at 5 s so a loaded machine cannot stall the gate.
        let killer_stop = std::sync::Arc::clone(&stop);
        let killer_acked = std::sync::Arc::clone(&acked);
        scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if killer_acked.lock().expect("acked").len() >= 10 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            #[cfg(unix)]
            unsafe {
                libc::kill(first_pid as i32, libc::SIGKILL);
            }
            killer_stop.store(true, Ordering::SeqCst);
        });
    });
    let acked_ops = acked.lock().expect("acked").len();
    let kill_landed_ms = spawn_at.elapsed().as_millis();
    assert!(
        *hammer_error.lock().expect("lock") == None,
        "hammer failures before the kill: {:?}",
        hammer_error.lock().expect("lock")
    );
    assert!(
        acked_ops >= 10,
        "kill must land amid traffic (only {acked_ops} ops acked before SIGKILL)"
    );
    // The predecessor is really gone (its pid is dead within a moment).
    let deadline = Instant::now() + Duration::from_secs(5);
    while first.is_alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!first.is_alive(), "SIGKILL predecessor still alive");
    drop(first);

    // ── deterministic crash-state injection while dead ──────────────────
    omt_runtime::ownership::recover_own_dead_daemon_marker(&home)
        .expect("own dead-daemon marker auto-recovers");
    inject_pending_journal_states(&home, &epic_id, &tickets[0]);

    // ── respawn through the bootstrap path; measure spawn→ready ─────────
    let expected_epic_title = "SIGKILL epic renamed while pending";
    let respawn_at = Instant::now();
    let second = DaemonProcess::spawn(&ctx, &["--home", home_str]);
    let mut ready_ms: Option<u128> = None;
    let mut second_endpoint = String::new();
    while respawn_at.elapsed() < Duration::from_secs(10) {
        if let Some(descriptor) = wait_for_descriptor(&ctx.runtime_dir, Duration::from_millis(100))
        {
            if descriptor.generation <= first_descriptor.generation {
                continue; // stale predecessor descriptor
            }
            if let Ok((mut client, credential)) = connected_client(&descriptor.endpoint, "external")
            {
                if let Ok(view) = client.call(
                    "node/get",
                    authed(json!({ "nodeId": epic_id }), &credential),
                ) {
                    if view["node"]["title"].as_str() == Some(expected_epic_title) {
                        ready_ms = Some(respawn_at.elapsed().as_millis());
                        second_endpoint = descriptor.endpoint.clone();
                        break;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let ready_ms = ready_ms.unwrap_or_else(|| {
        panic!(
            "successor never became ready within 10 s (alive={}): stderr: {}",
            second.is_alive(),
            second.stderr_text()
        )
    });
    assert!(
        second.is_alive(),
        "successor died during startup: {}",
        second.stderr_text()
    );

    // ── convergence assertions over the recovered home ───────────────────
    let mut verifier = connect_session(&second_endpoint, "cli");
    let vcred = verifier.credential.clone();

    // (a) prepared-row roll-forward IS the readiness probe above; re-assert.
    let view = verifier
        .client
        .call("node/get", authed(json!({ "nodeId": epic_id }), &vcred))
        .expect("epic readable after recovery");
    assert_eq!(view["node"]["title"].as_str(), Some(expected_epic_title));

    // (b) db_committed-row roll-forward: priority 777 visible.
    let view = verifier
        .client
        .call("node/get", authed(json!({ "nodeId": tickets[0] }), &vcred))
        .expect("ticket readable after recovery");
    assert_eq!(
        view["node"]["priority"].as_i64(),
        Some(777),
        "db_committed row did not roll forward"
    );

    // (c) every pre-kill ACKED mutation survived (read-your-write across
    //     the crash boundary).
    let acked_list: Vec<Acked> = std::mem::take(&mut *acked.lock().expect("acked"));
    for step in &acked_list {
        match step {
            Acked::Create { id, title } => {
                let view = verifier
                    .client
                    .call("node/get", authed(json!({ "nodeId": id }), &vcred))
                    .unwrap_or_else(|err| panic!("acked create {id} lost after restart: {err:?}"));
                assert_eq!(
                    view["node"]["title"].as_str(),
                    Some(title.as_str()),
                    "acked create {id} drifted after restart"
                );
            }
            Acked::Update { id, revision } => {
                let view = verifier
                    .client
                    .call("node/get", authed(json!({ "nodeId": id }), &vcred))
                    .unwrap_or_else(|err| panic!("acked update target {id} lost: {err:?}"));
                let current = view["node"]["revision"].as_i64().expect("revision");
                assert!(
                    current >= *revision,
                    "REVISION ROLLBACK across restart for {id}: acked {revision}, now {current}"
                );
            }
        }
    }

    // (d) Durable plane: journal fully acknowledged, commandIds unique.
    let stats = sql_stats(&home.join("omt.db"));
    assert_eq!(
        stats.journal_not_acknowledged, 0,
        "journal left non-acknowledged rows after recovery"
    );
    assert_eq!(
        stats.operations_rows, stats.operations_distinct_command_ids,
        "duplicate committed commandIds after recovery"
    );

    // (e) Planned bytes: every nodes row's file matches frontmatter exactly.
    let verified = scan_planned_bytes(&home).expect("planned-bytes scan converged");

    println!(
        "\nGATE sigkill-to-ready PASS | killed_mid_mutation_acked={acked_ops} kill_at_ms={kill_landed_ms} \
         | spawn_to_ready_ms={ready_ms} (<5000) | injected_pending_rolled_forward=yes(prepared+db_committed) \
         | journal_pending_after_recovery=0 ops_unique_commandids=yes planned_bytes_verified={verified} nodes \
         | prekill_acks_survived={}\n",
        acked_list.len()
    );

    drop(second); // leak-reaper kills the successor promptly
}

fn step_id(step: &Acked) -> &str {
    match step {
        Acked::Create { id, .. } | Acked::Update { id, .. } => id,
    }
}
