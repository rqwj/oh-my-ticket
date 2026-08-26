//! omt-daemon server core: runtime-dir resolution, config load
//! (`daemon.json`, precedence defaults < file), rotating log install,
//! bootstrap election, home opening + journal recovery BEFORE descriptor
//! publish, accept loop with the connection-count gate, connection threads
//! (handshake/enrollment → dispatch, `$/cancelRequest` honored at
//! linearization-safe points), idle-watchdog exit, and graceful SIGTERM
//! drain.

use crate::auth;
use crate::bootstrap;
use crate::config::DaemonConfig;
use crate::descriptor;
use crate::homes::{HomeKind, Homes};
use crate::ipc;
use crate::jsonrpc;
use crate::limits::Limits;
use crate::paths;
use crate::problem;
use crate::signal;
use omt_storage::clock::MillisClock;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Process-wide limits snapshot (set once at startup; consulted by the
/// home-queue RATE_LIMITED details without re-reading the config file).
static CURRENT_LIMITS: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();

pub fn current_limits() -> &'static Limits {
    CURRENT_LIMITS.get_or_init(Limits::default)
}

/// Daemon entry point.
pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // U1 (R22): `--version` prints and exits before touching any runtime
    // dir, lock, or descriptor — safe inside install smoke on a clean machine.
    if args.iter().any(|a| a == "--version") {
        println!("omt-daemon {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let mut home_paths: Vec<PathBuf> = Vec::new();
    let mut runtime_dir_arg: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--home" => {
                index += 1;
                match args.get(index) {
                    Some(path) => home_paths.push(PathBuf::from(path)),
                    None => return fail("missing value for --home"),
                }
            }
            "--runtime-dir" => {
                index += 1;
                match args.get(index) {
                    Some(path) => runtime_dir_arg = Some(path.clone()),
                    None => return fail("missing value for --runtime-dir"),
                }
            }
            other => return fail(&format!("unknown argument: {other}")),
        }
        index += 1;
    }

    let runtime_dir = paths::resolve(runtime_dir_arg.as_deref());
    if let Err(err) = std::fs::create_dir_all(&runtime_dir) {
        return fail(&format!("runtime dir create failed: {err}"));
    }

    // Lifecycle configuration FIRST (fail closed on a malformed file before
    // touching locks or descriptors); then install the rotating log.
    let config = match DaemonConfig::load(&runtime_dir) {
        Ok(config) => config,
        Err(err) => return fail_problem(&err),
    };
    let _ = CURRENT_LIMITS.set(config.limits.clone());
    crate::logging::init(&runtime_dir, &config.log);
    // Defense-in-depth admin checks inside dispatch read this snapshot.
    crate::dispatch::set_runtime_dir(runtime_dir.clone());
    crate::logging::log(
        "info",
        "STARTUP",
        &format!(
            "omt-daemon pid {} runtime-dir {}",
            std::process::id(),
            runtime_dir.display()
        ),
    );

    // 0) Arm signal handling BEFORE ANY THREAD SPAWNS: pthread_sigmask
    //    inherits into every later thread (home actors, heartbeats,
    //    connections), so ONLY the sigwait watcher consumes SIGTERM/SIGINT.
    //    Masking late would let the kernel hand the signal to an unblocked
    //    actor thread and terminate the process without a drain.
    let endpoint_path = paths::endpoint_path(&runtime_dir);
    signal::install(Box::new({
        let endpoint = endpoint_path.clone();
        move || {
            #[cfg(unix)]
            signal::wake_accept(&endpoint);
        }
    }));

    // Global home path convention (src/host/pool.ts): OMT_HOME else ~/.omt.
    let global_path = match std::env::var("OMT_HOME") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".omt")
        }
    };

    // No --home defaults to the global home only.
    if home_paths.is_empty() {
        home_paths.push(global_path.clone());
    }

    let clock = crate::homes::system_clock();

    // Idle watchdog wake: the last exiting actor self-connects through this
    // handle so the blocked accept() observes an empty registry below.
    signal::set_idle_wake({
        let endpoint = endpoint_path.clone();
        Arc::new(move || {
            #[cfg(unix)]
            signal::wake_accept(&endpoint);
        })
    });

    // 1) Open every requested home UNDER THE DAEMON OWNER LOCK. Recovery of
    //    pending journal entries happens inside Storage::open — readiness
    //    implies recovered. Dead-daemon markers of OUR OWN predecessor are
    //    auto-recovered first (orchestrator ruling).
    let homes = Homes::new();
    for path in &home_paths {
        if let Err(err) = homes.open(
            path.clone(),
            HomeKind::Workspace,
            Some(global_path.as_path()),
            Arc::clone(&clock),
            &config,
        ) {
            crate::logging::log(
                "error",
                err.code,
                &format!("home open failed ({}): {}", path.display(), err.message),
            );
            return fail_problem_owned(err);
        }
    }

    // 2) Bootstrap election: winner takes the lock; a loser recognizes the
    //    published live daemon and exits quietly, or times out with
    //    BOOTSTRAP_TIMEOUT.
    let stale_ms = bootstrap_stale_ms();
    let poll_timeout = std::time::Duration::from_millis(bootstrap_poll_timeout_ms() as u64);
    let guard = match bootstrap::elect(&runtime_dir, Arc::clone(&clock), stale_ms, poll_timeout) {
        Ok(bootstrap::Election::Winner(guard)) => guard,
        Ok(bootstrap::Election::DaemonPresent(_existing)) => {
            // A daemon already serves this runtime dir — nothing to do.
            crate::logging::log(
                "info",
                "DAEMON_PRESENT",
                "a live daemon already serves this runtime dir",
            );
            eprintln!(
                "{}",
                serde_json::json!({
                    "code": "DAEMON_PRESENT",
                    "message": "a live daemon already serves this runtime dir",
                })
            );
            return;
        }
        Err(problem) => {
            crate::logging::log("error", problem.code, &problem.message);
            eprintln!(
                "{}",
                serde_json::json!({
                    "code": problem.code,
                    "details": problem.details,
                    "message": problem.message,
                })
            );
            std::process::exit(2);
        }
    };

    // 3) Bind the endpoint and publish the descriptor (winner only).
    #[cfg(unix)]
    let listener = match ipc::Listener::bind(&endpoint_path) {
        Ok(listener) => listener,
        Err(err) => {
            guard.release();
            return fail(&format!("endpoint bind failed: {err}"));
        }
    };
    #[cfg(windows)]
    let _listener_guard_placeholder = (); // windows leg wires its own server

    #[cfg(unix)]
    let endpoint_string = endpoint_path.to_string_lossy().into_owned();
    #[cfg(windows)]
    let endpoint_string = paths::pipe_name(&runtime_dir);

    let boot_token = guard.boot_token().to_string();
    let published = match descriptor::publish(&runtime_dir, &endpoint_string, &boot_token, &*clock)
    {
        Ok(descriptor) => descriptor,
        Err(err) => {
            guard.release();
            return fail(&format!("descriptor publish failed: {err}"));
        }
    };
    crate::logging::log(
        "info",
        "READY",
        &format!(
            "serving generation {} on {endpoint_string}",
            published.generation
        ),
    );

    // 4) Accept loop with the concurrent-connections gate (R21/U5b):
    //    excess connects are refused politely (RATE_LIMITED line + close)
    //    instead of growing unbounded threads.
    let registry = Arc::new(auth::Registry::new());
    let homes = Arc::new(homes);
    let live_connections = Arc::new(AtomicUsize::new(0));
    let idle_enabled = config.idle_enabled();
    loop {
        if signal::SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
            break;
        }
        if idle_enabled && homes.live_actor_count() == 0 && !home_paths.is_empty() {
            crate::logging::log(
                "info",
                "IDLE_EXIT",
                "every home actor exited after the quiet period",
            );
            break;
        }
        match listener.accept() {
            Ok(stream) => {
                let cap = config.limits.max_concurrent_connections;
                if live_connections.load(Ordering::SeqCst) >= cap {
                    refuse_connection(stream, cap);
                    continue;
                }
                live_connections.fetch_add(1, Ordering::SeqCst);
                let runtime = runtime_dir.clone();
                let registry_c = Arc::clone(&registry);
                let homes_c = Arc::clone(&homes);
                let clock_c = Arc::clone(&clock);
                let counter = Arc::clone(&live_connections);
                std::thread::Builder::new()
                    .name("omt-conn".to_string())
                    .spawn(move || {
                        serve_connection(stream, &registry_c, &homes_c, &clock_c, &runtime);
                        counter.fetch_sub(1, Ordering::SeqCst);
                    })
                    .ok();
            }
            Err(_) => {
                if signal::SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
                    break;
                }
                // Transient accept error: brief backoff.
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    // 6) Graceful drain: finish queued jobs, release every home lock, drop
    //    the election lease, remove OUR descriptor.
    homes.shutdown_all();
    guard.release();
    descriptor::remove_if_ours(&runtime_dir, &published.boot_token);
    crate::logging::log("info", "SHUTDOWN", "drain complete; exiting 0");
}

/// Politely refuse an over-capacity connection (RATE_LIMITED, registered
/// code) and close it before any protocol exchange.
fn refuse_connection(stream: std::os::unix::net::UnixStream, cap: usize) {
    use std::io::Write;
    let limit_line = jsonrpc::error_response(
        &serde_json::Value::Null,
        jsonrpc::CODE_SERVER_ERROR,
        "RATE_LIMITED",
        serde_json::json!({ "reason": "concurrent-connections", "limit": cap }),
        "concurrent connection limit reached; retry after backoff",
    );
    let mut writer = std::io::BufWriter::new(stream);
    let _ = writeln!(writer, "{limit_line}");
    let _ = writer.flush();
    crate::logging::log(
        "warn",
        "RATE_LIMITED",
        &format!("connection refused at cap {cap}"),
    );
}

struct ConnState {
    registry: Arc<auth::Registry>,
    homes: Arc<Homes>,
    #[allow(dead_code)] // kept for parity checks in future handshake paths
    clock: Arc<dyn MillisClock>,
    runtime_dir: PathBuf,
    /// Kernel-verified peer identity for this connection (handshake input).
    peer: ipc::PeerId,
    /// In-flight request id → cancel flag ($/cancelRequest flips these).
    inflight: Arc<Mutex<std::collections::HashMap<serde_json::Value, Arc<AtomicBool>>>>,
}

#[allow(clippy::type_complexity)]
pub(crate) fn serve_connection(
    stream: std::os::unix::net::UnixStream,
    registry: &Arc<auth::Registry>,
    homes: &Arc<Homes>,
    clock: &std::sync::Arc<dyn MillisClock>,
    runtime_dir: &std::path::Path,
) {
    // Same-user gate BEFORE any protocol exchange: cross-uid connections
    // close immediately.
    let peer = match ipc::peer_id(&stream) {
        Ok(peer) => peer,
        Err(_) => return,
    };
    if auth::enforce_same_user(peer).is_err() {
        return;
    }

    let writer_stream = match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    };
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    // Writer thread: the ONLY writer to the socket; carries responses AND
    // live event notifications through one ordered channel.
    std::thread::Builder::new()
        .name("omt-write".to_string())
        .spawn(move || {
            use std::io::Write;
            let mut writer = std::io::BufWriter::new(writer_stream);
            for line in line_rx {
                if writeln!(writer, "{line}")
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        })
        .ok();

    let state = Arc::new(ConnState {
        registry: Arc::clone(registry),
        homes: Arc::clone(homes),
        clock: Arc::clone(clock),
        runtime_dir: runtime_dir.to_path_buf(),
        peer,
        inflight: Arc::new(Mutex::new(std::collections::HashMap::new())),
    });

    // Dispatcher thread: processes requests SEQUENTIALLY (per-connection
    // FIFO linearization is preserved) while the reader keeps consuming
    // lines — cancellation notifications stay responsive during long ops.
    let (job_tx, job_rx) = std::sync::mpsc::channel::<jsonrpc::Request>();
    {
        let state = Arc::clone(&state);
        let line_tx = line_tx.clone();
        std::thread::Builder::new()
            .name("omt-dispatch".to_string())
            .spawn(move || {
                let mut conn_credential: Option<auth::Credential> = None;
                for request in job_rx {
                    process_request(request, &state, &line_tx, &mut conn_credential);
                }
            })
            .ok();
    }

    let reader = std::io::BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // Payload bound FIRST (fair order stage 1): byte count pre-parse.
        let limits = current_limits();
        if let Err(details) = limits.check_payload(trimmed.len()) {
            respond(
                &line_tx,
                jsonrpc::error_response(
                    &serde_json::Value::Null,
                    jsonrpc::CODE_INVALID_REQUEST,
                    "INVALID_INPUT",
                    details,
                    "payload exceeds maxPayloadBytes",
                ),
            );
            continue;
        }

        match jsonrpc::parse_message(trimmed) {
            Ok(Some(jsonrpc::Incoming::Request(request))) => {
                if job_tx.send(request).is_err() {
                    break;
                }
            }
            Ok(Some(jsonrpc::Incoming::Notification { method, params })) => {
                if method == jsonrpc::CANCEL_METHOD {
                    if let Some(target) = jsonrpc::cancel_target(&params) {
                        if let Some(flag) = state.inflight.lock().expect("inflight").get(&target) {
                            flag.store(true, Ordering::SeqCst);
                        }
                    }
                }
                // Other client notifications are accepted and ignored.
            }
            Ok(None) => {}
            Err((code, message)) => {
                respond(
                    &line_tx,
                    jsonrpc::error_response(
                        &serde_json::Value::Null,
                        code,
                        "INVALID_INPUT",
                        serde_json::json!({}),
                        &message,
                    ),
                );
            }
        }
    }
    drop(line_tx); // closes the writer thread when notifications drain
}

fn process_request(
    request: jsonrpc::Request,
    state: &Arc<ConnState>,
    line_tx: &std::sync::mpsc::Sender<String>,
    conn_credential: &mut Option<auth::Credential>,
) {
    // ── handshake / enrollment ────────────────────────────────────────
    if request.method == "handshake/request" {
        handle_handshake(request, state, line_tx, conn_credential);
        return;
    }

    // ── credential check (fair order stages 3–4) ──────────────────────
    let validated = match auth::presented_token(&request.params)
        .and_then(|token| state.registry.validate(&token, &state.clock))
    {
        Ok(credential) => credential,
        Err(problem) => {
            respond(line_tx, error_response_for_problem(&request.id, problem));
            return;
        }
    };

    route_method(request, &validated, state, line_tx);
}

fn handle_handshake(
    request: jsonrpc::Request,
    state: &Arc<ConnState>,
    line_tx: &std::sync::mpsc::Sender<String>,
    conn_credential: &mut Option<auth::Credential>,
) {
    // Protocol major negotiation (F1): unsupported MAJOR →
    // UNSUPPORTED_PROTOCOL problem instead of a handshake result.
    let version_text = request
        .params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let major_ok = version_text
        .split('.')
        .next()
        .and_then(|major| major.parse::<i64>().ok())
        .map(|major| major == auth::PROTOCOL_MAJOR)
        .unwrap_or(false);
    if !major_ok {
        respond(
            line_tx,
            jsonrpc::error_response(
                &request.id,
                jsonrpc::CODE_SERVER_ERROR,
                "UNSUPPORTED_PROTOCOL",
                serde_json::json!({
                    "requested": version_text,
                    "supported": format!("{}.x", auth::PROTOCOL_MAJOR),
                }),
                "unsupported protocol major",
            ),
        );
        return;
    }
    let kind_text = request
        .params
        .pointer("/client/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("external");
    let Some(kind) = auth::ClientKind::parse(kind_text) else {
        respond(
            line_tx,
            jsonrpc::error_response(
                &request.id,
                jsonrpc::CODE_SERVER_ERROR,
                omt_domain::error::INVALID_INPUT,
                serde_json::json!({ "field": "client.kind", "value": kind_text }),
                "unknown client kind",
            ),
        );
        return;
    };
    let peer = state.peer;
    // Session-level principal nesting (TICKET-0130 item 3): a handshake that
    // presents client/sessionId derives the per-session actor namespace
    // "<base>/<sessionId>" (base = kind:pid), so concurrent model sessions
    // of one process are separately attributable at the trust gate while
    // staying inside the same principal (R12 nesting rule in auth::issue).
    let mut scopes = request
        .params
        .get("requestedScopes")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(session_id) = request
        .params
        .pointer("/client/sessionId")
        .and_then(|v| v.as_str())
    {
        let valid = !session_id.is_empty()
            && session_id.len() <= 64
            && session_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if valid {
            if !scopes.is_object() {
                scopes = serde_json::json!({});
            }
            let base = format!("{}:{}", kind.as_str(), peer.pid);
            if let Some(obj) = scopes.as_object_mut() {
                obj.insert(
                    "actorNamespace".into(),
                    serde_json::json!(format!("{base}/{session_id}")),
                );
            }
        }
    }
    let open_ids = state.homes.open_ids();
    let (token, credential) =
        match state
            .registry
            .issue(peer, kind, &scopes, &open_ids, &state.clock)
        {
            Ok(issued) => issued,
            Err(problem) => {
                respond(line_tx, error_response_for_problem(&request.id, problem));
                return;
            }
        };

    // Handshake result per capabilities.schema.json INCLUDING the formally
    // registered credential block ($defs/CredentialGrant, U5b).
    let homes_info: Vec<serde_json::Value> = state
        .homes
        .list()
        .iter()
        .filter(|home| credential.homes.iter().any(|h| h == &home.home_id))
        .map(|home| {
            serde_json::json!({
                "homeId": home.home_id,
                "name": home.name,
                "kind": home.kind.as_str(),
                "path": home.path.display().to_string(),
            })
        })
        .collect();
    let result = serde_json::json!({
        "protocolVersion": format!("{}.{}", auth::PROTOCOL_MAJOR, auth::PROTOCOL_MINOR),
        "daemon": { "name": "omt-daemon", "version": env!("CARGO_PKG_VERSION") },
        "homes": homes_info,
        "limits": current_limits().to_handshake_json(),
        "features": {
            "actionParityMatrix": true,
            "eventResume": true,
            "idempotencyKeys": true,
        },
        "credential": {
            "token": token,
            "principalId": credential.principal_id,
            "actorNamespace": credential.actor_namespace,
            "homes": credential.homes,
            "operations": credential.operations,
            "expiresAt": auth::expires_iso(credential.expires_at_ms),
        },
    });
    *conn_credential = Some(credential);
    respond(line_tx, jsonrpc::response(&request.id, &result));
}

#[allow(clippy::type_complexity)]
fn route_method(
    request: jsonrpc::Request,
    credential: &auth::Credential,
    state: &Arc<ConnState>,
    line_tx: &std::sync::mpsc::Sender<String>,
) {
    let runtime_dir = &state.runtime_dir;
    let homes = &state.homes;

    // Parity pre-check OUTSIDE the home queue so denials never occupy an
    // actor; the actor re-checks cheaply (defense in depth). Fair-order
    // stage 4 precedes any queue admission.
    match dispatch_parity(&request.method) {
        Parity::AgentAvailable => {}
        Parity::AdapterOnly => {
            if !credential.kind.is_adapter() {
                respond(
                    line_tx,
                    jsonrpc::error_response(
                        &request.id,
                        jsonrpc::CODE_SERVER_ERROR,
                        problem::FORBIDDEN,
                        serde_json::json!({
                            "reason": "parity-adapter-only",
                            "method": request.method,
                            "clientKind": credential.kind.as_str(),
                        }),
                        "principal not authorized for this action: parity-adapter-only",
                    ),
                );
                return;
            }
        }
        Parity::HumanAdministrative => {
            if !auth::is_administrator(runtime_dir, credential) {
                respond(
                    line_tx,
                    jsonrpc::error_response(
                        &request.id,
                        jsonrpc::CODE_SERVER_ERROR,
                        problem::FORBIDDEN,
                        serde_json::json!({
                            "reason": "admin-required",
                            "method": request.method,
                        }),
                        "principal not authorized for this action: admin-required",
                    ),
                );
                return;
            }
        }
    }

    // recent-* are global-home scoped without a homeId.
    let target_home = match request.params.get("homeId").and_then(|v| v.as_str()) {
        Some(home_id) => homes.by_home_id(home_id),
        None => homes.global().or_else(|| homes.list().first().cloned()),
    };
    let Some(home) = target_home else {
        respond(
            line_tx,
            jsonrpc::error_response(
                &request.id,
                jsonrpc::CODE_SERVER_ERROR,
                omt_domain::error::NOT_FOUND,
                serde_json::json!({ "kind": "home", "id": request.params.get("homeId").cloned().unwrap_or(serde_json::Value::String("global".into())) }),
                "home not opened",
            ),
        );
        return;
    };

    // Live subscription channel rides the job so registration happens ON
    // THE ACTOR between backlog page and watermark (gap-free).
    let wants_subscription = request.method == "events/resume";
    // Register the cancel probe for this request BEFORE execution so a
    // $/cancelRequest arriving mid-flight flips the storage-level probe.
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .inflight
        .lock()
        .expect("inflight")
        .insert(request.id.clone(), Arc::clone(&cancel));
    let resume_cursor = if wants_subscription {
        request.params.get("cursor").and_then(|v| v.as_i64())
    } else {
        None
    };
    let result = if wants_subscription {
        let (sub_tx, sub_rx) = std::sync::mpsc::sync_channel::<String>(4096);
        // Forwarder: hub pushes lines into this bounded channel; we forward
        // into the connection's writer channel.
        let forward_tx = line_tx.clone();
        std::thread::Builder::new()
            .name("omt-events".to_string())
            .spawn(move || {
                for line in sub_rx {
                    if forward_tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .ok();
        match home.call_full(
            &request.method,
            request.params.clone(),
            credential,
            Some(sub_tx),
            resume_cursor,
            cancel,
        ) {
            Ok(value) => value,
            Err(problem) => {
                state.inflight.lock().expect("inflight").remove(&request.id);
                respond(line_tx, error_response_for_problem(&request.id, problem));
                return;
            }
        }
    } else {
        match home.call_full(
            &request.method,
            request.params.clone(),
            credential,
            None,
            None,
            cancel,
        ) {
            Ok(value) => value,
            Err(problem) => {
                state.inflight.lock().expect("inflight").remove(&request.id);
                respond(line_tx, error_response_for_problem(&request.id, problem));
                return;
            }
        }
    };
    state.inflight.lock().expect("inflight").remove(&request.id);
    respond(line_tx, jsonrpc::response(&request.id, &result));
}

// Re-exported parity vocabulary so cli/server share one source of truth.
pub use crate::dispatch::{parity_of as dispatch_parity, Parity};

fn respond(line_tx: &std::sync::mpsc::Sender<String>, line: String) {
    let _ = line_tx.send(line);
}

pub(crate) fn error_response_for_problem(
    id: &serde_json::Value,
    problem: omt_storage::Problem,
) -> String {
    let details = problem.details.unwrap_or(serde_json::Value::Null);
    jsonrpc::error_response(
        id,
        jsonrpc::CODE_SERVER_ERROR,
        problem.code,
        details,
        &problem.message,
    )
}

fn bootstrap_stale_ms() -> i64 {
    std::env::var("OMT_BOOTSTRAP_STALE_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(bootstrap::DEFAULT_STALE_MS)
}

/// Loser descriptor-poll budget (tests shrink this via env).
fn bootstrap_poll_timeout_ms() -> i64 {
    std::env::var("OMT_BOOTSTRAP_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(bootstrap::DEFAULT_POLL_TIMEOUT_MS)
}

fn fail(message: &str) {
    eprintln!(
        "{}",
        serde_json::json!({ "code": "IO", "message": problem::redact(message) })
    );
    std::process::exit(2);
}

fn fail_problem(problem: &omt_storage::Problem) {
    eprintln!(
        "{}",
        serde_json::json!({
            "code": problem.code,
            "details": problem.details,
            "message": problem::redact(&problem.message),
        })
    );
    std::process::exit(2);
}

fn fail_problem_owned(problem: omt_storage::Problem) {
    fail_problem(&problem)
}
