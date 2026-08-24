//! omt-daemon entry point (U5a): runtime-dir resolution, bootstrap
//! election, home opening + journal recovery BEFORE descriptor publish,
//! accept loop, connection threads (handshake/enrollment → dispatch), and
//! graceful SIGTERM drain.

mod auth;
mod bootstrap;
mod descriptor;
mod dispatch;
mod events;
mod homes;
mod ipc;
mod jsonrpc;
mod paths;
mod problem;
mod signal;
mod views;

use homes::{HomeKind, Homes};
use omt_storage::clock::MillisClock;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;

/// Payload limits advertised in the handshake (R21 seed; config wiring is
/// U5b).
const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_LIST_LIMIT: i64 = 200;
const MAX_EVENT_BATCH: i64 = 1000;
const RUN_CONCURRENCY: i64 = 1;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    dispatch::set_runtime_dir(runtime_dir.clone());

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

    let clock = homes::system_clock();

    // 1) Open every requested home UNDER THE DAEMON OWNER LOCK. Recovery of
    //    pending journal entries happens inside Storage::open — readiness
    //    implies recovered.
    let homes = Homes::new();
    for path in &home_paths {
        if let Err(err) = homes.open(
            path.clone(),
            HomeKind::Workspace,
            Some(global_path.as_path()),
            Arc::clone(&clock),
        ) {
            return fail(&format!(
                "home open failed ({}): {} {}",
                path.display(),
                err.code,
                problem::redact(&err.message)
            ));
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

    // 4) Accept loop.
    let registry = Arc::new(auth::Registry::new());
    let homes = Arc::new(homes);
    loop {
        if signal::SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok(stream) => {
                let runtime = runtime_dir.clone();
                let registry_c = Arc::clone(&registry);
                let homes_c = Arc::clone(&homes);
                let clock_c = Arc::clone(&clock);
                std::thread::Builder::new()
                    .name("omt-conn".to_string())
                    .spawn(move || {
                        serve_connection(stream, &registry_c, &homes_c, &clock_c, &runtime);
                    })
                    .ok();
            }
            Err(_) => {
                if signal::SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
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
}

fn serve_connection(
    stream: std::os::unix::net::UnixStream,
    registry: &auth::Registry,
    homes: &Homes,
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

    let mut conn_credential: Option<auth::Credential> = None;
    let reader = std::io::BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // Payload limit (R21 seed).
        if trimmed.len() > MAX_PAYLOAD_BYTES {
            respond(
                &line_tx,
                jsonrpc::error_response(
                    &serde_json::Value::Null,
                    jsonrpc::CODE_INVALID_REQUEST,
                    "INVALID_INPUT",
                    serde_json::json!({ "field": "payload", "maxPayloadBytes": MAX_PAYLOAD_BYTES }),
                    "payload exceeds maxPayloadBytes",
                ),
            );
            continue;
        }

        let request = match jsonrpc::Request::parse(trimmed) {
            Ok(Some(request)) => request,
            Ok(None) => continue, // notification from client: ignored
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
                continue;
            }
        };

        // ── handshake / enrollment ────────────────────────────────────
        if request.method == "handshake/request" {
            handle_handshake(
                request,
                peer,
                registry,
                homes,
                clock,
                &line_tx,
                &mut conn_credential,
            );
            continue;
        }

        // ── credential check ──────────────────────────────────────────
        let Some(credential) = &conn_credential else {
            respond(
                &line_tx,
                jsonrpc::error_response(
                    &request.id,
                    jsonrpc::CODE_SERVER_ERROR,
                    problem::UNAUTHORIZED,
                    serde_json::json!({ "reason": "missing-handshake" }),
                    "handshake required before any method",
                ),
            );
            continue;
        };
        let validated = match auth::presented_token(&request.params)
            .and_then(|token| registry.validate(&token, clock))
        {
            Ok(credential) => credential,
            Err(problem) => {
                respond(&line_tx, error_response_for_problem(&request.id, problem));
                continue;
            }
        };
        let _ = credential;

        route_method(request, &validated, homes, runtime_dir, &line_tx);
    }
    drop(line_tx); // closes the writer thread when notifications drain
}

fn handle_handshake(
    request: jsonrpc::Request,
    peer: ipc::PeerId,
    registry: &auth::Registry,
    homes: &Homes,
    clock: &Arc<dyn MillisClock>,
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
    let scopes = request
        .params
        .get("requestedScopes")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let open_ids = homes.open_ids();
    let (token, credential) = match registry.issue(peer, kind, &scopes, &open_ids, clock) {
        Ok(issued) => issued,
        Err(problem) => {
            respond(line_tx, error_response_for_problem(&request.id, problem));
            return;
        }
    };

    // Handshake result per capabilities.schema.json plus the additive
    // credential block (registration in capabilities.schema.json lands in
    // U5b's schema pass — README deviation note).
    let homes_info: Vec<serde_json::Value> = homes
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
        "limits": {
            "maxPayloadBytes": MAX_PAYLOAD_BYTES,
            "maxListLimit": MAX_LIST_LIMIT,
            "maxEventBatch": MAX_EVENT_BATCH,
            "runConcurrency": RUN_CONCURRENCY,
        },
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

fn route_method(
    request: jsonrpc::Request,
    credential: &auth::Credential,
    homes: &Homes,
    runtime_dir: &std::path::Path,
    line_tx: &std::sync::mpsc::Sender<String>,
) {
    // Parity pre-check OUTSIDE the home queue so denials never occupy an
    // actor; the actor re-checks cheaply (defense in depth).
    match dispatch::parity_of(&request.method) {
        dispatch::Parity::AgentAvailable => {}
        dispatch::Parity::AdapterOnly => {
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
        dispatch::Parity::HumanAdministrative => {
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
        match home.call_subscribing(
            &request.method,
            request.params.clone(),
            credential,
            Some(sub_tx),
        ) {
            Ok(value) => value,
            Err(problem) => {
                respond(line_tx, error_response_for_problem(&request.id, problem));
                return;
            }
        }
    } else {
        match home.call(&request.method, request.params.clone(), credential) {
            Ok(value) => value,
            Err(problem) => {
                respond(line_tx, error_response_for_problem(&request.id, problem));
                return;
            }
        }
    };
    respond(line_tx, jsonrpc::response(&request.id, &result));
}

fn respond(line_tx: &std::sync::mpsc::Sender<String>, line: String) {
    let _ = line_tx.send(line);
}

fn error_response_for_problem(id: &serde_json::Value, problem: omt_storage::Problem) -> String {
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
