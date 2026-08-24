//! Home management (R2/KTD9): the daemon opens each configured home via
//! `omt_storage::OpenConfig { acquire_lock: true, owner_kind: Daemon }` —
//! marker + kernel flock ownership — and serializes EVERY method that
//! touches a home through a per-home actor thread (mpsc command queue).
//! Mutations and reads alike execute on the owning thread, so two clients
//! can never interleave half-applied state; WAL snapshots keep reads
//! consistent. A recv-timeout heartbeat loop refreshes each held home lock
//! every 10 s (the lock's own cadence).

use crate::events::Hub;
use omt_domain::error;
use omt_storage::clock::{MillisClock, SystemClock};
use omt_storage::journal::{OpenConfig, Storage};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;

/// One unit of home work.
pub enum Job {
    /// Dispatch one authorized method against this home. `subscribe`
    /// carries the caller's live-event channel when events/resume asked
    /// for gap-free registration ON the actor.
    Rpc {
        method: String,
        params: Value,
        auth: crate::auth::Credential,
        subscribe: Option<SyncSender<String>>,
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
    pub name: String,
    pub kind: HomeKind,
    tx: SyncSender<Job>,
    #[allow(dead_code)] // hub reaches the actor via clone; field kept for diagnostics
    pub hub: Arc<Hub>,
}

impl OpenedHome {
    fn send(&self, job: Job) -> Result<(), omt_storage::Problem> {
        self.tx.send(job).map_err(|_| shutdown_problem())
    }

    /// Send an RPC job and wait for the actor's answer.
    pub fn call(
        &self,
        method: &str,
        params: Value,
        auth: &crate::auth::Credential,
    ) -> Result<Value, omt_storage::Problem> {
        self.call_subscribing(method, params, auth, None)
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
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        self.send(Job::Rpc {
            method: method.to_string(),
            params,
            auth: auth.clone(),
            subscribe,
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

fn shutdown_problem() -> omt_storage::Problem {
    omt_storage::Problem::new(error::IO, "daemon is shutting down")
}

/// Registry of opened homes keyed by stable HomeId.
#[derive(Default)]
pub struct Homes {
    inner: std::sync::Mutex<Vec<std::sync::Arc<OpenedHome>>>,
}

impl Homes {
    pub fn new() -> Homes {
        Homes::default()
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

    /// Open one home under the daemon owner lock and start its actor.
    /// Recovery of pending journal entries happens inside Storage::open —
    /// BEFORE the caller publishes the readiness descriptor.
    pub fn open(
        &self,
        path: PathBuf,
        kind: HomeKind,
        global_path: Option<&std::path::Path>,
        clock: Arc<dyn MillisClock>,
    ) -> Result<std::sync::Arc<OpenedHome>, omt_storage::Problem> {
        let mut config = OpenConfig::new(&path);
        config.clock = Arc::clone(&clock);
        config.acquire_lock = true;
        config.owner_kind = omt_storage::home_lock::OwnerKind::Daemon;
        config.recover_on_open = true;
        let storage = Storage::open(config)?;
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
        let effective_kind = if global_path
            .map(|global| global == path.as_path())
            .unwrap_or(false)
        {
            HomeKind::Global
        } else {
            kind
        };

        let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(1024);
        let opened = std::sync::Arc::new(OpenedHome {
            home_id: home_id.clone(),
            path: path.clone(),
            name,
            kind: effective_kind,
            tx,
            hub: std::sync::Arc::clone(&hub),
        });

        // Actor thread: owns Storage; serializes all work; heartbeats the
        // home lock between jobs; drains outbox after successful jobs.
        let heartbeat_ms = 10_000i64; // matches HEARTBEAT_INTERVAL_MS contract
        let clock_actor = Arc::clone(&clock);
        let hub_actor = Arc::clone(&hub);
        let home_id_actor = home_id.clone();
        std::thread::Builder::new()
            .name(format!("omt-home-{home_id_actor}"))
            .spawn(move || {
                let mut storage = storage;
                let mut last_heartbeat_ms = 0i64;
                loop {
                    let timeout =
                        std::time::Duration::from_millis(heartbeat_ms.clamp(50, 60_000) as u64);
                    match rx.recv_timeout(timeout) {
                        Ok(Job::Rpc {
                            method,
                            params,
                            auth,
                            subscribe,
                            reply,
                        }) => {
                            let outcome = crate::dispatch::dispatch(
                                &mut storage,
                                &hub_actor,
                                &method,
                                params,
                                &auth,
                                subscribe,
                            );
                            let _ = reply.send(outcome);
                            heartbeat_if_due(&mut storage, &mut last_heartbeat_ms, &*clock_actor);
                        }
                        Ok(Job::Shutdown)
                        | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            heartbeat_if_due(&mut storage, &mut last_heartbeat_ms, &*clock_actor);
                        }
                    }
                }
                // Drain complete: release the kernel lock + marker.
                let _ = storage.release_lock();
                let _ = hub_actor; // subscribers die with the hub clone
            })
            .expect("spawn home actor");

        self.inner.lock().expect("homes").push(Arc::clone(&opened));
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
            let all_clear = homes.iter().all(|home| !home.lock_marker_present());
            if all_clear {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        // Fallback grace for slow fsyncs even when markers vanished early.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

impl OpenedHome {
    fn lock_marker_present(&self) -> bool {
        self.path.join("home.lock").exists()
    }
}

fn heartbeat_if_due(storage: &mut Storage, last_heartbeat_ms: &mut i64, clock: &dyn MillisClock) {
    let now = clock.now_ms();
    if now.saturating_sub(*last_heartbeat_ms) >= 9_000 || *last_heartbeat_ms == 0 {
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
