//! Home management (R2/KTD9): the daemon opens each configured home via
//! `omt_storage::OpenConfig { acquire_lock: true, owner_kind: Daemon }` —
//! marker + kernel flock ownership — and serializes EVERY method that
//! touches a home through a per-home actor thread (mpsc command queue).
//! Mutations and reads alike execute on the owning thread, so two clients
//! can never interleave half-applied state; WAL snapshots keep reads
//! consistent.
//!
//! U5b additions:
//! - **Queue bound (R21):** each actor queue holds at most
//!   `limits.max_home_queue_depth` jobs; overflow degrades fairly with
//!   RATE_LIMITED (`reason=home-queue-depth`) instead of growing memory.
//! - **Opened-homes quota:** [`Homes::open`] refuses beyond
//!   `limits.max_open_homes` with QUOTA_EXCEEDED (`rule=open-homes`).
//! - **Cancellation:** jobs carry a cancel probe honored at the storage
//!   layer's linearization-safe points only.
//! - **Retention task (R11/F4):** the actor's tick runs the daemon-owned
//!   outbox prune (`prune_and_signal`) keyed by the oldest live subscriber
//!   cursor; an appended `snapshot.resync` fans out like any event.
//! - **Idle watchdog:** when no job arrives for `idle_quiet_ms`, the actor
//!   drains (queues empty by definition of quiet), releases its home lock,
//!   exits; the last exiting actor wakes the accept loop so the process
//!   shuts down cleanly (descriptor removed, locks released).

use crate::events::Hub;
use crate::limits::{self, Limits};
use omt_domain::error;
use omt_storage::clock::{MillisClock, SystemClock};
use omt_storage::journal::{OpenConfig, Storage};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;

/// Bounded wait for a concurrent declare of the SAME canonical path
/// (KTD2): waiters park on the opener's completion slot instead of
/// double-opening (which would leak DAEMON_OWNS_HOME). Tests shrink this
/// via `OMT_DECLARE_WAIT_TIMEOUT_MS`.
const DEFAULT_DECLARE_WAIT_TIMEOUT_MS: u64 = 15_000;

fn declare_wait_timeout_ms() -> u64 {
    std::env::var("OMT_DECLARE_WAIT_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_DECLARE_WAIT_TIMEOUT_MS)
}

/// One unit of home work.
///
/// `allow(large_enum_variant)`: the Rpc payload is the hot path and stays
/// unboxed so a submit is one channel send with no heap hop; Shutdown is
/// fieldless by design.
#[allow(clippy::large_enum_variant)]
pub enum Job {
    /// Dispatch one authorized method against this home. `subscribe`
    /// carries the caller's live-event channel when events/resume asked
    /// for gap-free registration ON the actor. `cancel` is flipped by the
    /// connection reader on $/cancelRequest and honored ONLY at the
    /// storage layer's linearization-safe points.
    Rpc {
        method: String,
        params: Value,
        auth: crate::auth::Credential,
        subscribe: Option<SyncSender<String>>,
        /// Resume cursor when this job opens a live subscription (used to
        /// key retention pruning), else None.
        subscribe_from_cursor: Option<i64>,
        cancel: Arc<AtomicBool>,
        reply: SyncSender<Result<Value, omt_storage::Problem>>,
    },
    /// Graceful drain: finish queued jobs, release locks, exit.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HomeKind {
    Workspace,
    Global,
}

impl HomeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            HomeKind::Workspace => "workspace",
            HomeKind::Global => "global",
        }
    }
}

/// Handle held by connection threads to reach a home's queue.
pub struct OpenedHome {
    pub home_id: String,
    pub path: PathBuf,
    /// Canonicalized path (U5): the dedupe key for `home/declare`, so
    /// alias paths (symlinks, macOS `/var` ↔ `/private/var`) collapse.
    /// Best-effort at startup opens; strict for declares (they validate).
    pub canonical: PathBuf,
    pub name: String,
    pub kind: HomeKind,
    tx: SyncSender<Job>,
    #[allow(dead_code)] // hub reaches the actor via clone; field kept for diagnostics
    pub hub: Arc<Hub>,
}

fn shutdown_problem() -> omt_storage::Problem {
    omt_storage::Problem::new(error::IO, "daemon is shutting down")
}

/// Fair degradation when a per-home queue is at depth (R21).
pub fn queue_depth_problem(limit: usize) -> omt_storage::Problem {
    limits::rate_limited("home-queue-depth", serde_json::json!({ "limit": limit }))
}

impl OpenedHome {
    /// Submit a job without blocking. Queue-at-depth degrades with
    /// RATE_LIMITED; a draining actor degrades with IO/shutdown.
    fn try_send_job(&self, job: Job) -> Result<(), omt_storage::Problem> {
        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(queue_depth_problem(self.queue_capacity())),
            Err(TrySendError::Disconnected(_)) => Err(shutdown_problem()),
        }
    }

    fn queue_capacity(&self) -> usize {
        // The channel was created with the configured depth; recover it for
        // problem details from the runtime config snapshot.
        crate::server::current_limits().max_home_queue_depth
    }

    /// Send an RPC job and wait for the actor's answer.
    pub fn call(
        &self,
        method: &str,
        params: Value,
        auth: &crate::auth::Credential,
    ) -> Result<Value, omt_storage::Problem> {
        self.call_full(
            method,
            params,
            auth,
            None,
            None,
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Like [`OpenedHome::call`] with a live-event subscription channel;
    /// events/resume registers it on the actor between backlog read and
    /// watermark advance so no committed event falls into the gap.
    pub fn call_subscribing(
        &self,
        method: &str,
        params: Value,
        auth: &crate::auth::Credential,
        subscribe: Option<SyncSender<String>>,
    ) -> Result<Value, omt_storage::Problem> {
        self.call_full(
            method,
            params,
            auth,
            subscribe,
            None,
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Full-fidelity call: subscription channel, its starting cursor (keys
    /// retention pruning), and a cancellable execution probe.
    #[allow(clippy::too_many_arguments)]
    pub fn call_full(
        &self,
        method: &str,
        params: Value,
        auth: &crate::auth::Credential,
        subscribe: Option<SyncSender<String>>,
        subscribe_from_cursor: Option<i64>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value, omt_storage::Problem> {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        self.try_send_job(Job::Rpc {
            method: method.to_string(),
            params,
            auth: auth.clone(),
            subscribe,
            subscribe_from_cursor,
            cancel,
            reply: reply_tx,
        })?;
        match reply_rx.recv() {
            Ok(result) => result,
            Err(_) => Err(shutdown_problem()),
        }
    }

    /// Ask this home's actor to drain and release its lock. Best-effort:
    /// a bounded channel may be full during heavy load, in which case the
    /// process-exit fd drop releases the kernel lock anyway.
    pub fn request_shutdown(&self) -> bool {
        match self.tx.try_send(Job::Shutdown) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

/// Registry of opened homes keyed by stable HomeId.
///
/// U5 (KTD2): `inner` holds REAL entries only. In-flight `home/declare`
/// opens park in `opening` (keyed by canonical path) as placeholders, so
/// every registry reader (handshake listing, quota counting, lock-release
/// polling, shutdown snapshot) is placeholder-aware by construction —
/// placeholders are structurally invisible to them.
#[derive(Default)]
pub struct Homes {
    /// Known-homes catalog (runtime-dir SQLite; None in tests that don't
    /// care). Recorded on every successful open/declare.
    known: Option<crate::known_homes::KnownHomes>,
    /// Shared with each home actor so it can remove its own entry on exit
    /// (idle AND shutdown paths alike): quota counts only live entries and
    /// an exited home disappears from the handshake listing.
    inner: Arc<std::sync::Mutex<Vec<Arc<OpenedHome>>>>,
    /// Live actor threads (idle exits decrement; the server polls this to
    /// notice an all-homes-idle shutdown).
    live_actors: Arc<AtomicUsize>,
    /// Opening placeholders keyed by canonical path (declare in flight).
    opening: Arc<std::sync::Mutex<HashMap<PathBuf, Arc<OpeningSlot>>>>,
}

/// Outcome of one in-flight declare, delivered to same-path waiters
/// through the completion slot (KTD2: "wake waiters with the Problem").
enum SlotOutcome {
    Opened(Arc<OpenedHome>),
    Failed(omt_storage::Problem),
}

/// Per-path completion slot for concurrent declares of one canonical path.
struct OpeningSlot {
    outcome: std::sync::Mutex<Option<SlotOutcome>>,
    signal: std::sync::Condvar,
}

impl OpeningSlot {
    fn new() -> Arc<OpeningSlot> {
        Arc::new(OpeningSlot {
            outcome: std::sync::Mutex::new(None),
            signal: std::sync::Condvar::new(),
        })
    }

    fn complete(&self, outcome: SlotOutcome) {
        *self.outcome.lock().expect("opening slot") = Some(outcome);
        self.signal.notify_all();
    }

    /// Block until the opener completes or the deadline passes; None means
    /// the waiter timed out (structured retryable problem upstream).
    fn wait_until(&self, deadline: std::time::Instant) -> Option<SlotOutcome> {
        let mut guard = self.outcome.lock().expect("opening slot");
        loop {
            match guard.as_ref() {
                Some(SlotOutcome::Opened(home)) => {
                    return Some(SlotOutcome::Opened(Arc::clone(home)))
                }
                Some(SlotOutcome::Failed(problem)) => {
                    return Some(SlotOutcome::Failed(problem.clone()))
                }
                None => {}
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, _) = self
                .signal
                .wait_timeout(guard, deadline - now)
                .expect("opening slot");
            guard = next;
        }
    }
}

impl Homes {
    pub fn new() -> Homes {
        Homes {
            known: None,
            inner: Arc::new(std::sync::Mutex::new(Vec::new())),
            live_actors: Arc::new(AtomicUsize::new(0)),
            opening: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Production constructor: attach the runtime-dir catalog.
    pub fn with_known(known: crate::known_homes::KnownHomes) -> Homes {
        Homes {
            known: Some(known),
            ..Homes::new()
        }
    }

    /// Best-effort catalog write (a catalog failure must never fail a
    /// home open — discovery data is auxiliary).
    fn record_known(&self, opened: &OpenedHome, clock: &Arc<dyn MillisClock>) {
        if let Some(known) = &self.known {
            let now = omt_storage::clock::iso_from_ms(clock.now_ms());
            let _ = known.record(
                &opened.canonical,
                &opened.name,
                opened.kind.as_str(),
                &opened.home_id,
                &now,
            );
        }
    }

    pub fn list(&self) -> Vec<std::sync::Arc<OpenedHome>> {
        self.inner.lock().expect("homes").clone()
    }

    pub fn by_home_id(&self, home_id: &str) -> Option<std::sync::Arc<OpenedHome>> {
        self.list().into_iter().find(|home| home.home_id == home_id)
    }

    pub fn global(&self) -> Option<std::sync::Arc<OpenedHome>> {
        self.list()
            .into_iter()
            .find(|home| home.kind == HomeKind::Global)
    }

    pub fn open_ids(&self) -> Vec<String> {
        self.list()
            .into_iter()
            .map(|home| home.home_id.clone())
            .collect()
    }

    /// How many actor threads are still alive (0 once every home exited,
    /// e.g. through the idle watchdog).
    pub fn live_actor_count(&self) -> usize {
        self.live_actors.load(Ordering::SeqCst)
    }

    /// True when every home's lock marker has been released on disk.
    pub fn all_locks_released(&self) -> bool {
        self.list().iter().all(|home| {
            !home
                .path
                .join(omt_storage::home_lock::LOCK_FILE_NAME)
                .exists()
        })
    }

    /// Open one home under the daemon owner lock and start its actor.
    /// Recovery of pending journal entries happens inside Storage::open —
    /// BEFORE the caller publishes the readiness descriptor.
    ///
    /// `dead-daemon recovery` (orchestrator ruling): a stale marker whose
    /// `ownerKind` is `"daemon"` with a DEAD pid is auto-recovered here —
    /// it can only be OUR OWN predecessor (second daemons and ts-bridge
    /// markers are refused by the lock matrix below us).
    pub fn open(
        &self,
        path: PathBuf,
        kind: HomeKind,
        global_path: Option<&std::path::Path>,
        clock: Arc<dyn MillisClock>,
        config: &crate::config::DaemonConfig,
    ) -> Result<std::sync::Arc<OpenedHome>, omt_storage::Problem> {
        // Opened-homes quota BEFORE doing any IO (fair order: quota probes
        // precede expensive open work).
        config
            .limits
            .check_open_homes(self.list().len())
            .map_err(|mut details| {
                let extra = details.as_object_mut().expect("object");
                let rule = extra.remove("rule").unwrap_or_default();
                limits::quota_exceeded(rule.as_str().unwrap_or("open-homes"), details)
            })?;

        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let opened = self.open_storage_and_start(
            &path,
            canonical,
            kind,
            global_path,
            Arc::clone(&clock),
            config,
        )?;

        // Startup opens are sequential; registry insertion still lands
        // before the caller proceeds (readiness descriptor ordering).
        self.inner
            .lock()
            .expect("homes")
            .push(std::sync::Arc::clone(&opened));
        self.record_known(&opened, &clock);
        Ok(opened)
    }

    /// Declare an existing on-disk home into this RUNNING daemon (U5/R6).
    /// Idempotent by canonical path; concurrent declares of one path are
    /// serialized through an opening-placeholder + completion-slot protocol
    /// (KTD2) so exactly one opener touches the store and losers get the
    /// winner's homeId — never DAEMON_OWNS_HOME.
    ///
    /// Lock discipline: `fs::canonicalize` and ALL filesystem IO run
    /// outside the registry mutexes; inside them only map lookups/inserts
    /// happen. Mutex order is always `inner` → `opening`.
    pub fn declare(
        &self,
        raw_path: PathBuf,
        kind_hint: HomeKind,
        global_path: Option<&std::path::Path>,
        clock: Arc<dyn MillisClock>,
        config: &crate::config::DaemonConfig,
    ) -> Result<std::sync::Arc<OpenedHome>, omt_storage::Problem> {
        // Pre-open validation OUTSIDE every registry lock (R9): structured
        // rejection for missing paths and non-directories, never a panic.
        let metadata = std::fs::metadata(&raw_path)
            .map_err(|err| invalid_declare_path(&raw_path, "unreadable", &err.to_string()))?;
        if !metadata.is_dir() {
            return Err(invalid_declare_path(
                &raw_path,
                "not-a-directory",
                "declare target exists but is not a directory",
            ));
        }
        let canonical = std::fs::canonicalize(&raw_path).map_err(|err| {
            invalid_declare_path(&raw_path, "uncanonicalizable", &err.to_string())
        })?;

        enum Admitted {
            /// Present-and-open: idempotent replay of the live entry.
            Existing(Arc<OpenedHome>),
            /// This caller performs the open (placeholder inserted).
            Opener(Arc<OpeningSlot>),
            /// Another declare holds the placeholder: wait on its slot.
            Waiter(Arc<OpeningSlot>),
        }

        let admitted = {
            // Lock order inner → opening (documented above). The dedupe
            // probe and quota probe both count REAL entries only.
            let entries = self.inner.lock().expect("homes");
            if let Some(existing) = entries.iter().find(|home| home.canonical == canonical) {
                Admitted::Existing(Arc::clone(existing))
            } else {
                let mut opening = self.opening.lock().expect("homes-opening");
                match opening.get(&canonical) {
                    Some(slot) => Admitted::Waiter(Arc::clone(slot)),
                    None => {
                        // Quota BEFORE admitting the opener (fair order:
                        // cheap probes precede expensive IO).
                        config
                            .limits
                            .check_open_homes(entries.len())
                            .map_err(|mut details| {
                                let extra = details.as_object_mut().expect("object");
                                let rule = extra.remove("rule").unwrap_or_default();
                                limits::quota_exceeded(
                                    rule.as_str().unwrap_or("open-homes"),
                                    details,
                                )
                            })?;
                        let slot = OpeningSlot::new();
                        opening.insert(canonical.clone(), Arc::clone(&slot));
                        Admitted::Opener(slot)
                    }
                }
            }
        };

        match admitted {
            Admitted::Existing(home) => Ok(home),
            Admitted::Waiter(slot) => {
                let timeout_ms = declare_wait_timeout_ms();
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                match slot.wait_until(deadline) {
                    Some(SlotOutcome::Opened(home)) => Ok(home),
                    Some(SlotOutcome::Failed(problem)) => Err(problem),
                    // Bounded wait exhausted: structured RETRYABLE problem
                    // (RATE_LIMITED semantics — retry after backoff can
                    // succeed once the in-flight declare settles).
                    None => Err(limits::rate_limited(
                        "home-declare-in-progress",
                        serde_json::json!({
                            "waitTimeoutMs": timeout_ms,
                            "retryable": true,
                        }),
                    )),
                }
            }
            Admitted::Opener(slot) => {
                // Deterministic test window between placeholder insertion
                // and Storage::open (starvation/race suites); production
                // never sets the env.
                test_declare_delay_hook();
                let outcome = self.open_storage_and_start(
                    &raw_path,
                    canonical.clone(),
                    kind_hint,
                    global_path,
                    Arc::clone(&clock),
                    config,
                );
                match outcome {
                    Ok(home) => {
                        // Registry insertion BEFORE waking waiters or
                        // returning (KTD2): subsequent requests see the
                        // home immediately.
                        self.inner.lock().expect("homes").push(Arc::clone(&home));
                        self.opening
                            .lock()
                            .expect("homes-opening")
                            .remove(&canonical);
                        slot.complete(SlotOutcome::Opened(Arc::clone(&home)));
                        self.record_known(&home, &clock);
                        Ok(home)
                    }
                    Err(problem) => {
                        // ANY failure/cancel removes the placeholder under
                        // the lock and wakes waiters with the Problem, so
                        // the same path is immediately declarable again.
                        self.opening
                            .lock()
                            .expect("homes-opening")
                            .remove(&canonical);
                        slot.complete(SlotOutcome::Failed(problem.clone()));
                        Err(problem)
                    }
                }
            }
        }
    }

    /// Shared tail of [`Homes::open`] and [`Homes::declare`]: dead-marker
    /// recovery + journal recovery + `Storage::open`, hub watermark, then
    /// actor spawn. Runs OUTSIDE any registry mutex (KTD2 heavy work).
    #[allow(clippy::too_many_arguments)]
    fn open_storage_and_start(
        &self,
        path: &std::path::Path,
        canonical: PathBuf,
        kind: HomeKind,
        global_path: Option<&std::path::Path>,
        clock: Arc<dyn MillisClock>,
        config: &crate::config::DaemonConfig,
    ) -> Result<std::sync::Arc<OpenedHome>, omt_storage::Problem> {
        // Dead-marker recovery BEFORE any lock acquisition (orchestrator
        // ruling, shared by startup opens AND declares since U5): our own
        // dead predecessor's daemon marker is auto-cleared; a LIVE foreign
        // daemon refuses with DAEMON_OWNS_HOME; a ts-bridge marker refuses
        // with takeover guidance (never an automatic steal).
        crate::ownership::recover_own_dead_daemon_marker(path)?;

        let mut open_config = OpenConfig::new(path);
        open_config.clock = Arc::clone(&clock);
        open_config.acquire_lock = true;
        open_config.owner_kind = omt_storage::home_lock::OwnerKind::Daemon;
        open_config.recover_on_open = true;
        open_config.lock_heartbeat_ms = config.lock_heartbeat_ms;
        // Fail closed on hostile stores: a corrupt/schema-violating home
        // surfaces here as a structured Problem — no partial actor start.
        let storage = Storage::open(open_config)?;
        let home_id = storage
            .home_id()
            .ok_or_else(|| omt_storage::Problem::new(error::IO, "opened home carries no id"))?
            .to_string();

        let hub = Hub::new();
        hub.set_watermark(omt_storage::outbox::latest_seq(storage.conn())?);

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "omt".to_string());
        // The kind passed in is a hint; the canonical rule is path equality
        // with the resolved global home (OMT_HOME / ~/.omt).
        let effective_kind = if global_path.map(|global| global == path).unwrap_or(false) {
            HomeKind::Global
        } else {
            kind
        };

        let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(config.limits.max_home_queue_depth);
        let opened = std::sync::Arc::new(OpenedHome {
            home_id: home_id.clone(),
            path: path.to_path_buf(),
            canonical,
            name,
            kind: effective_kind,
            tx,
            hub: std::sync::Arc::clone(&hub),
        });

        // Actor thread: owns Storage; serializes all work; heartbeats the
        // home lock on the configured cadence; prunes the outbox each tick;
        // exits through the idle watchdog after the quiet period.
        let clock_actor = Arc::clone(&clock);
        let hub_actor = Arc::clone(&hub);
        let home_id_actor = home_id.clone();
        let limits = config.limits.clone();
        let idle_quiet_ms = config.idle_quiet_ms;
        let heartbeat_ms = config.lock_heartbeat_ms;
        let idle_wake = crate::signal::idle_wake_handle();
        self.live_actors.fetch_add(1, Ordering::SeqCst);
        let live = Arc::clone(&self.live_actors);
        // Registry handle for SELF-EVICTION (U5 review fix): whichever way
        // this loop ends (idle quiet-exit, Shutdown job, daemon drain), the
        // actor removes its own entry so quota counts only live homes.
        let entries = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name(format!("omt-home-{home_id_actor}"))
            .spawn(move || {
                let mut storage = storage;
                let mut last_heartbeat_ms = 0i64;
                let mut last_activity_ms = clock_actor.now_ms();
                loop {
                    let now_ms = clock_actor.now_ms();
                    let tick_ms = heartbeat_ms.clamp(50, 60_000);
                    let recv_timeout = if idle_quiet_ms > 0 {
                        let elapsed = now_ms.saturating_sub(last_activity_ms);
                        let remaining = idle_quiet_ms.saturating_sub(elapsed);
                        std::cmp::min(tick_ms, remaining.clamp(1, i64::MAX)) as u64
                    } else {
                        tick_ms as u64
                    };
                    match rx.recv_timeout(std::time::Duration::from_millis(recv_timeout)) {
                        Ok(Job::Rpc {
                            method,
                            params,
                            auth,
                            subscribe,
                            subscribe_from_cursor,
                            cancel,
                            reply,
                        }) => {
                            last_activity_ms = clock_actor.now_ms();
                            test_delay_hook(&method);
                            let outcome = crate::dispatch::dispatch_cancellable(
                                &mut storage,
                                &hub_actor,
                                &method,
                                params,
                                &auth,
                                subscribe,
                                subscribe_from_cursor,
                                &cancel,
                                &limits,
                            );
                            let _ = reply.send(outcome);
                            heartbeat_if_due(
                                &mut storage,
                                &mut last_heartbeat_ms,
                                &*clock_actor,
                                heartbeat_ms,
                            );
                        }
                        Ok(Job::Shutdown)
                        | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            heartbeat_if_due(
                                &mut storage,
                                &mut last_heartbeat_ms,
                                &*clock_actor,
                                heartbeat_ms,
                            );
                            run_retention_tick(&mut storage, &hub_actor, &limits, &*clock_actor);
                            if idle_quiet_ms > 0
                                && clock_actor.now_ms().saturating_sub(last_activity_ms)
                                    >= idle_quiet_ms
                            {
                                // Subscriber keep-alive (U5 review fix): a
                                // live event subscription IS activity even
                                // when no RPC arrives — suppress the quiet
                                // exit while the hub holds subscribers.
                                if hub_actor.oldest_subscriber_cursor().is_some() {
                                    last_activity_ms = clock_actor.now_ms();
                                    continue;
                                }
                                crate::logging::log(
                                    "info",
                                    "IDLE_SHUTDOWN",
                                    &format!(
                                        "home {home_id_actor} quiet {}ms exceeded; draining",
                                        idle_quiet_ms
                                    ),
                                );
                                break;
                            }
                        }
                    }
                }
                // Drain complete: release the kernel lock + marker, then
                // evict our own registry entry (both exit paths converge).
                let _ = storage.release_lock();
                drop(hub_actor); // subscribers die with the hub clone
                if let Ok(mut entries) = entries.lock() {
                    entries.retain(|home| home.home_id != home_id_actor);
                }
                if live.fetch_sub(1, Ordering::SeqCst) == 1 {
                    // Last actor leaving: wake the accept loop so the
                    // process finishes its clean shutdown.
                    if let Some(wake) = idle_wake.lock().expect("idle wake").as_ref() {
                        wake();
                    }
                }
            })
            .expect("spawn home actor");

        Ok(opened)
    }

    /// Ask every actor to finish its queue, release its lock, and exit,
    /// then wait a bounded grace window so locks land released BEFORE the
    /// process drops its fds (SIGTERM drain test asserts marker removal).
    pub fn shutdown_all(&self) {
        let homes = self.inner.lock().expect("homes").clone();
        let mut released = 0usize;
        for home in &homes {
            if home.request_shutdown() {
                released += 1;
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while released > 0 && std::time::Instant::now() < deadline {
            // Poll lock markers: once each home's marker file is gone the
            // actor finished its release path.
            if self.all_locks_released() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        // Fallback grace for slow fsyncs even when markers vanished early.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Structured pre-open rejection for a declare path that is missing,
/// unreadable, or not a directory (R9: validation happens BEFORE any
/// lock/marker work, so a bad path can never wedge the registry).
fn invalid_declare_path(
    path: &std::path::Path,
    reason: &str,
    detail: &str,
) -> omt_storage::Problem {
    omt_storage::Problem::with_details(
        error::INVALID_INPUT,
        format!("cannot declare {}: {reason} ({detail})", path.display()),
        |d| {
            d.insert("field".into(), serde_json::json!("path"));
            d.insert("reason".into(), serde_json::json!(reason));
            d.insert("path".into(), serde_json::json!(path.display().to_string()));
        },
    )
}

/// Deterministic slowdown inside `home/declare` between placeholder
/// insertion and Storage::open (opt-in via `OMT_TEST_DECLARE_DELAY_MS`);
/// gives suites a window where one path's declare is provably in flight
/// while other connections keep making progress. Production never sets it.
fn test_declare_delay_hook() {
    let Ok(raw) = std::env::var("OMT_TEST_DECLARE_DELAY_MS") else {
        return;
    };
    if let Ok(ms) = raw.parse::<u64>() {
        std::thread::sleep(std::time::Duration::from_millis(ms.min(30_000)));
    }
}

/// Daemon-owned retention pass (orchestrator ruling): prune the outbox to
/// `max_retained_events`, keying the protected consumer cursor on the
/// OLDEST LIVE SUBSCRIBER so a resync is emitted exactly when a connected
/// consumer would lose history. Any appended `snapshot.resync` fans out
/// through the normal publish path.
fn run_retention_tick(
    storage: &mut Storage,
    hub: &Arc<Hub>,
    limits: &Limits,
    clock: &dyn MillisClock,
) {
    let Some(home_id) = storage.home_id().map(str::to_string) else {
        return;
    };
    let retention = omt_storage::outbox::Retention {
        max_events: limits.max_retained_events,
        consumer_cursor: hub.oldest_subscriber_cursor(),
    };
    let report =
        match omt_storage::outbox::prune_and_signal(storage.conn(), &home_id, &retention, clock) {
            Ok(report) => report,
            Err(err) => {
                crate::logging::log("warn", "RETENTION", &err.message);
                return;
            }
        };
    if report.pruned_count > 0 {
        crate::logging::log(
            "info",
            "RETENTION",
            &format!(
                "home {home_id} pruned {} events through seq {}",
                report.pruned_count, report.pruned_through_seq
            ),
        );
    }
    // Fan out the resync event (and anything else committed) to subscribers.
    let _ = hub.publish_new(storage.conn(), &home_id);
}

/// Deterministic slowdown for integration suites (opt-in via env):
/// `OMT_DELAY_BEFORE_METHOD="<method>:<milliseconds>"` sleeps BEFORE the
/// named method executes, giving tests a window to enqueue concurrent jobs
/// or deliver $/cancelRequest. Production never sets it.
fn test_delay_hook(method: &str) {
    let Ok(spec) = std::env::var("OMT_DELAY_BEFORE_METHOD") else {
        return;
    };
    let Some((target, ms_raw)) = spec.rsplit_once(':') else {
        return;
    };
    if target != method {
        return;
    }
    if let Ok(ms) = ms_raw.parse::<u64>() {
        std::thread::sleep(std::time::Duration::from_millis(ms.min(30_000)));
    }
}

fn heartbeat_if_due(
    storage: &mut Storage,
    last_heartbeat_ms: &mut i64,
    clock: &dyn MillisClock,
    heartbeat_ms: i64,
) {
    let now = clock.now_ms();
    let interval = heartbeat_ms
        .clamp(50, 60_000)
        .saturating_sub(1_000)
        .max(1_000);
    if now.saturating_sub(*last_heartbeat_ms) >= interval || *last_heartbeat_ms == 0 {
        *last_heartbeat_ms = now;
        if let Some(handle) = storage.lock_handle() {
            let _ = handle.heartbeat();
        }
    }
}

/// Production default clock re-exported so main stays lean.
pub fn system_clock() -> Arc<dyn MillisClock> {
    Arc::new(SystemClock)
}
