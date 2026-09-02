//! Signal handling (unix): mask SIGTERM/SIGINT before any thread spawns,
//! then a watcher thread blocks in sigwait. Receipt triggers graceful
//! shutdown: stop accepting → drain home queues → release home locks →
//! remove descriptor → exit 0.
//!
//! The wake strategy is deliberately portable: after setting the shutdown
//! flag the watcher SELF-CONNECTS to the endpoint, which deterministically
//! wakes a blocked accept() on every unix platform (shutdown(2) on
//! listening sockets is not reliable across kernels). Windows console-ctrl
//! wiring lands with the windows leg; teardown there relies on process
//! exit closing fds (documented in README).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Wake callback invoked on the watcher thread once a termination signal
/// arrives (self-connects to the endpoint so accept() unblocks).
pub type WakeListener = Box<dyn FnOnce() + Send>;

static WAKE: std::sync::Mutex<Option<WakeListener>> = std::sync::Mutex::new(None);

/// Reusable wake handle for NON-signal sources (U5b idle watchdog): the
/// last home actor exiting after the quiet period self-connects through
/// this so a blocked accept() observes an empty homes registry and starts
/// the clean shutdown path.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// THE shared idle-wake slot: writers (set_idle_wake) and readers (home
/// actors through idle_wake_handle clones) MUST observe the same instance.
fn idle_slot() -> &'static std::sync::Arc<std::sync::Mutex<Option<WakeFn>>> {
    static SLOT: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<Option<WakeFn>>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Arc::new(std::sync::Mutex::new(None)))
}

/// Register the idle-wake callback (called once during daemon startup,
/// before any actor can finish).
pub fn set_idle_wake(wake: WakeFn) {
    *idle_slot().lock().expect("idle wake slot") = Some(wake);
}

/// Shared handle handed to home actors.
pub fn idle_wake_handle() -> std::sync::Arc<std::sync::Mutex<Option<WakeFn>>> {
    idle_slot().clone()
}

/// Install signal masking + watcher. Returns after arming; does not block.
#[cfg(unix)]
pub fn install(wake: WakeListener) {
    *WAKE.lock().expect("wake slot") = Some(wake);
    unsafe {
        // Block SIGTERM/SIGINT in this thread; spawned threads inherit the
        // mask, so only sigwait consumes them.
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGTERM);
        libc::sigaddset(&mut mask, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
    }
    std::thread::Builder::new()
        .name("omt-signal".to_string())
        .spawn(|| {
            let received = unsafe {
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                libc::sigaddset(&mut mask, libc::SIGTERM);
                libc::sigaddset(&mut mask, libc::SIGINT);
                let mut sig: i32 = 0;
                let mut rc = libc::sigwait(&mask, &mut sig);
                while rc != 0 {
                    rc = libc::sigwait(&mask, &mut sig);
                }
                sig
            };
            let _ = received;
            SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
            if let Some(wake) = WAKE.lock().expect("wake slot").take() {
                wake();
            }
        })
        .expect("spawn signal watcher");
}

#[cfg(windows)]
pub fn install(_wake: WakeListener) {
    // Windows console-ctrl wiring lands with the windows leg (README).
}

#[cfg(unix)]
/// Self-connect to the endpoint so a blocked accept() returns promptly.
pub fn wake_accept(endpoint: &std::path::Path) {
    use std::os::unix::net::UnixStream;
    if let Ok(stream) = UnixStream::connect(endpoint) {
        // Immediately drop; the daemon treats post-shutdown connections as
        // noise and never answers them.
        drop(stream);
    }
}
