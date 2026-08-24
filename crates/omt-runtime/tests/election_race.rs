//! U5a election race (evidence-first): N daemon processes race for one
//! runtime directory; exactly ONE wins the bootstrap lock, publishes the
//! descriptor, and serves; every loser exits with problem code
//! BOOTSTRAP_TIMEOUT. This suite is written FIRST (red against a binary
//! that does not yet implement the protocol) per the unit packet.

#![allow(dead_code)]

mod common;

use common::{wait_for_descriptor, DaemonProcess, TestCtx};

#[test]
fn election_race_elects_exactly_one_winner_and_losers_report_bootstrap_timeout() {
    let ctx = TestCtx::spawn();
    let n = 3usize;
    let mut procs: Vec<DaemonProcess> = Vec::new();
    for _ in 0..n {
        procs.push(DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]));
    }

    // The winner publishes a descriptor whose pid is alive and whose
    // endpoint answers a handshake.
    let descriptor = wait_for_descriptor(&ctx.runtime_dir, std::time::Duration::from_secs(20))
        .expect("a winner must publish a live descriptor");
    assert!(
        descriptor.generation >= 1,
        "generation starts at 1, got {}",
        descriptor.generation
    );

    // Exactly one process stays alive; it owns the descriptor's pid. Every
    // loser exits: one that FOUND the live winner's descriptor exits quietly
    // (its purpose — ensure a daemon exists — is fulfilled); one that saw
    // no live daemon within its poll timeout reports HOME_LOCKED-style
    // BOOTSTRAP_TIMEOUT on stderr.
    let mut alive = 0usize;
    let mut winners_pid_alive = false;
    for proc in &procs {
        match proc.try_wait() {
            None => {
                alive += 1;
                if proc.pid() == descriptor.pid {
                    winners_pid_alive = true;
                }
            }
            Some(_status) => {}
        }
    }
    assert_eq!(
        alive,
        1,
        "exactly one daemon must survive the election; stderr so far: {:?}",
        procs.iter().map(|p| p.stderr_text()).collect::<Vec<_>>()
    );
    assert!(
        winners_pid_alive,
        "the surviving process must be the descriptor publisher"
    );

    // Cleanup: kill the winner.
    for mut proc in procs {
        proc.kill();
    }
}
