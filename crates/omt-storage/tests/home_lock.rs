//! Home-ownership interop (plan U4 / AE8, U2b contract): the marker refusal
//! matrix must match `src/host/home-lock.ts` EXACTLY (DAEMON_OWNS_HOME always
//! even when stale; unknown schemaVersion never stolen; live holders refuse
//! with pid/acquiredAt; stale ts-bridge markers are stolen; corrupt/empty
//! bodies fall back to mtime liveness), plus the U4a kernel layer: advisory
//! flock double-daemon exclusion and flock-guarded steals.

#[path = "common/mod.rs"]
mod common;

use common::*;
use omt_storage::clock::MillisClock;
use omt_storage::clock::SystemClock;
use omt_storage::files::DiskFiles;
use omt_storage::home_lock::{acquire, inode_is_flocked_public, LockBody, LockConfig, OwnerKind};
use omt_storage::FixedClock;

fn daemon_cfg() -> LockConfig {
    LockConfig {
        owner_kind: OwnerKind::Daemon,
        hostname: "lock-test".to_string(),
        stale_ms: 30_000,
        heartbeat_ms: 10_000,
    }
}

fn ts_bridge_cfg() -> LockConfig {
    LockConfig {
        owner_kind: OwnerKind::TsBridge,
        ..daemon_cfg()
    }
}

fn write_marker(home: &std::path::Path, body: &LockBody) {
    let files = DiskFiles::new(home);
    files
        .atomic_write("home.lock", &serde_json::to_string(body).unwrap(), "marker")
        .unwrap();
}

fn marker(home: &std::path::Path) -> Option<LockBody> {
    let text = std::fs::read_to_string(home.join("home.lock")).ok()?;
    serde_json::from_str(&text).ok()
}

fn body_of(owner_kind: &str, acquired_at: i64, token: &str) -> LockBody {
    LockBody {
        schema_version: 1,
        owner_kind: owner_kind.to_string(),
        pid: Some(4242),
        hostname: Some("other".to_string()),
        acquired_at: iso_at(acquired_at),
        heartbeat_at: iso_at(acquired_at),
        token: token.to_string(),
    }
}

#[test]
fn daemon_tombstone_splits_by_acquirer_generation() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    // Stale by an hour, NO holder behind it (no flock): a TOMBSTONE.
    // TICKET-0124: new-world acquirers clear it automatically (the flock,
    // not the pid, is the liveness authority) — but legacy ts-bridge
    // writers cannot probe, so they are fenced with upgrade guidance.
    write_marker(
        &home,
        &body_of("daemon", T0_MS - 3_600_000, "tombstone-token"),
    );
    let problem = expect_problem(
        acquire(&home, &ts_bridge_cfg(), clock.clone()),
        "DAEMON_OWNS_HOME",
    );
    let details = problem.details.unwrap();
    assert_eq!(details["owner"]["ownerKind"], "daemon");
    let hint = details["hint"].as_str().expect("upgrade hint");
    assert!(hint.contains("upgrade"), "{hint}");
    assert_eq!(
        marker(&home).unwrap().token,
        "tombstone-token",
        "legacy attempt leaves the marker untouched"
    );

    // New-world acquirer takes the tombstone transparently.
    let handle = acquire(&home, &daemon_cfg(), clock).expect("daemon recovers tombstone");
    assert_eq!(handle.body().owner_kind, "daemon");
    handle.release().unwrap();
}

#[test]
fn future_schema_version_fails_closed_never_stolen() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let mut future = body_of("ts-bridge", T0_MS - 86_400_000 * 30, "future-holder");
    future.schema_version = 7;
    write_marker(&home, &future);

    let problem = expect_problem(acquire(&home, &daemon_cfg(), clock.clone()), "HOME_LOCKED");
    let details = problem.details.unwrap();
    assert_eq!(details["schemaVersion"], 7);
    assert_eq!(
        marker(&home).unwrap().token,
        "future-holder",
        "never stolen"
    );
}

#[test]
fn live_ts_bridge_refuses_with_identity() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    write_marker(&home, &body_of("ts-bridge", T0_MS - 5_000, "live-bridge"));

    let problem = expect_problem(acquire(&home, &daemon_cfg(), clock.clone()), "HOME_LOCKED");
    let details = problem.details.unwrap();
    assert_eq!(details["pid"], 4242);
    assert_eq!(details["acquiredAt"], iso_at(T0_MS - 5_000));
    assert_eq!(marker(&home).unwrap().token, "live-bridge");
}

#[test]
fn stale_ts_bridge_marker_is_stolen() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    write_marker(&home, &body_of("ts-bridge", T0_MS - 31_000, "stale-bridge"));

    let handle = acquire(&home, &daemon_cfg(), clock.clone()).expect("steal succeeds");
    let current = marker(&home).expect("marker present");
    assert_eq!(current.owner_kind, "daemon");
    assert_eq!(current.token, handle.token().to_string());
    handle.release().expect("release");
    assert!(
        std::fs::metadata(home.join("home.lock")).is_err(),
        "released lock unlinked"
    );
}

#[test]
fn corrupt_body_falls_back_to_mtime_liveness() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    std::fs::write(home.join("home.lock"), "{not json at all").unwrap();

    // Fresh mtime → refused with null identity.
    let problem = expect_problem(acquire(&home, &daemon_cfg(), clock.clone()), "HOME_LOCKED");
    assert_eq!(problem.details.unwrap()["pid"], serde_json::Value::Null);
    assert_eq!(
        std::fs::read_to_string(home.join("home.lock")).unwrap(),
        "{not json at all",
        "corrupt body not overwritten while fresh"
    );

    // Anchor the injected clock ~1 minute BEHIND the file's real mtime so
    // the mtime-fallback liveness window is deterministically expired.
    let real_now = omt_storage::clock::SystemClock.now_ms();
    let backdated = std::sync::Arc::new(FixedClock::at_ms(real_now + 60_000));
    let handle = acquire(&home, &daemon_cfg(), backdated).expect("corrupt stale stolen");
    assert_eq!(handle.body().schema_version, 1);
    handle.release().unwrap();

    // Empty file beyond its liveness window behaves the same (clock anchored
    // ahead of the real mtime so the fallback window is deterministically
    // expired without sleeping).
    let (_dir2, home2) = temp_home();
    std::fs::write(home2.join("home.lock"), "").unwrap();
    let real_now2 = SystemClock.now_ms();
    let ahead = std::sync::Arc::new(FixedClock::at_ms(real_now2 + 60_000));
    let h = acquire(&home2, &daemon_cfg(), ahead).expect("empty stale stolen");
    h.release().unwrap();
}

#[test]
fn heartbeat_refreshes_body_in_place_and_survives_steal_window() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let mut handle = acquire(&home, &daemon_cfg(), clock.clone()).expect("acquire");

    clock.advance(HEARTBEAT_TICK_MS);
    handle.heartbeat().expect("heartbeat");
    let current = marker(&home).expect("marker");
    assert_eq!(current.heartbeat_at, iso_at(T0_MS + HEARTBEAT_TICK_MS));
    assert_eq!(current.token, handle.token().to_string());
    assert!(!handle.is_lost());

    // A contender looking NOW still sees a holder; since our own marker says
    // "daemon" AND the inode is flocked, even the new-world flock-authority
    // rule refuses (a live lease never loses its home).
    expect_problem(
        acquire(&home, &daemon_cfg(), clock.clone()),
        "DAEMON_OWNS_HOME",
    );

    // After the stale window WITHOUT heartbeats the fd is closed (no flock
    // behind the marker) — the marker is a TOMBSTONE: the next daemon
    // acquirer recovers it automatically (TICKET-0124), while a legacy
    // ts-bridge writer would still be fenced.
    let token = handle.token().to_string();
    drop(handle);
    clock.advance(31_000);
    let successor = acquire(&home, &daemon_cfg(), clock.clone()).expect("tombstone recovered");
    assert_ne!(successor.token(), token);
    successor.release().unwrap();
}

#[test]
fn flock_double_daemon_kernel_exclusion() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();

    let a = acquire(&home, &daemon_cfg(), clock.clone()).expect("first daemon owns");

    // Second acquisition attempt fails at the MARKER layer already.
    expect_problem(
        acquire(&home, &daemon_cfg(), clock.clone()),
        "DAEMON_OWNS_HOME",
    );

    // KERNEL layer proof: even bypassing marker semantics entirely, a second
    // exclusive flock on the SAME inode is impossible while A holds it.
    assert!(
        inode_is_flocked_public(&home).expect("probe"),
        "held inode reports flocked"
    );

    // Steal-path protection against a LYING marker: forge a stale TS-BRIDGE
    // marker over the live daemon's inode. B's matrix sees a stealable stale
    // bridge — the kernel probe must refuse because A's lease still lives.
    let forged = body_of("ts-bridge", T0_MS - 120_000, "forged-bridge");
    let files = DiskFiles::new(&home);
    files
        .write_in_place("home.lock", &serde_json::to_string(&forged).unwrap())
        .unwrap();

    let problem = expect_problem(acquire(&home, &daemon_cfg(), clock.clone()), "HOME_LOCKED");
    assert_eq!(problem.details.unwrap()["reason"], "kernel-flock-held");

    // Once A releases (fd closed → flock dropped), the SAME forged stale
    // marker is legitimately stolen.
    a.release().expect("A releases");
    let b = acquire(&home, &daemon_cfg(), clock.clone()).expect("stale steal after release");
    assert_ne!(b.token(), "forged-bridge");
    b.release().expect("B releases");
}

#[test]
fn dead_daemon_marker_auto_recovers_for_new_world() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let a = acquire(&home, &daemon_cfg(), clock.clone()).expect("a acquires");
    drop(a); // crash: fd closed (flock released), marker remains

    clock.advance(3_600_000); // an hour later
                              // TICKET-0124: the un-flocked marker is a tombstone — the next daemon
                              // acquirer recovers it without manual tooling (D1: no user-visible gap).
    let b = acquire(&home, &daemon_cfg(), clock.clone()).expect("auto-recovered");
    b.release().unwrap();

    // A LEGACY writer over a daemon marker is still fenced with guidance:
    write_marker(&home, &body_of("daemon", T0_MS - 3_600_000, "fence-token"));
    let problem = expect_problem(acquire(&home, &ts_bridge_cfg(), clock), "DAEMON_OWNS_HOME");
    assert!(problem.details.unwrap()["hint"].is_string());
}

#[test]
fn release_only_unlinks_own_token() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let a = acquire(&home, &daemon_cfg(), clock.clone()).expect("a acquires");

    // Simulate loss: someone else replaced the marker.
    write_marker(&home, &body_of("ts-bridge", T0_MS, "someone-else"));
    a.release().expect("release is inert for foreign tokens");
    assert_eq!(
        marker(&home).unwrap().token,
        "someone-else",
        "successor untouched"
    );
}

#[test]
fn lost_ownership_heartbeats_become_inert() {
    let (_dir, home) = temp_home();
    let clock = fixed_clock();
    let mut a = acquire(&home, &daemon_cfg(), clock.clone()).expect("a acquires");
    write_marker(&home, &body_of("ts-bridge", T0_MS, "usurper"));
    a.heartbeat().expect("no error");
    assert!(a.is_lost(), "handle notices replacement");
    assert_eq!(
        marker(&home).unwrap().token,
        "usurper",
        "heartbeat did not clobber"
    );
}
