//! The `omt` operator CLI (U5c): online verbs speak the same protocol as
//! the TypeScript client (descriptor discovery → handshake/enrollment →
//! JSON-RPC), while reindex/doctor are OFFLINE maintenance verbs that
//! require NO live daemon serving the home (HOME_LOCKED refusal otherwise)
//! and take exclusive ownership themselves (`OwnerKind:"daemon"` marker +
//! kernel flock while running, released afterwards).
//!
//! Contract:
//! - stdout carries the human-readable result summary (pretty JSON);
//! - errors carry the registered Problem code/details on stderr;
//! - exit codes are stable: 0 ok · 2 usage · 3 problem · 130 canceled;
//! - Ctrl-C during a call cancels it ($/cancelRequest semantics apply
//!   server-side; the CLI aborts its socket read and exits 130).

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use omt_storage::clock::MillisClock;
use omt_storage::{Problem, Result};

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_PROBLEM: i32 = 3;
const EXIT_CANCELED: i32 = 130;

static CANCELED: AtomicBool = AtomicBool::new(false);
/// Active socket fd for Ctrl-C abort (shutdown breaks a blocked read).
static ACTIVE_FD: std::sync::Mutex<Option<i32>> = std::sync::Mutex::new(None);

pub const USAGE: &str = "\
omt — Oh My Ticket operator CLI

USAGE:
  omt [--runtime-dir DIR] [--home PATH] [--actor NS] <VERB> [ARGS] [--json]

ONLINE VERBS (connect to the serving daemon; enrollment is persisted at
  <runtime-dir>/cli-credential.json so leases survive across invocations):
  list      [--home ID] [--type T] [--status S] [--archived b] [--query Q]
  show      <nodeId>
  create    --type epic|story|substory|ticket|subticket --title TITLE
            [--parent ID] [--body TEXT] [--priority N] [--command-id ID]
  update    <nodeId> [--title T] [--status S] [--priority N]
            [--expected-revision R] [--body TEXT | --append TEXT]
  move      <nodeId> --to NEWPARENTID
  archive   <nodeId>
  run-create  <nodeId,nodeId,...> [--title T] [--stop-on-failure] [--auto-verify b]
  run-get     <runId>
  run-list    [--status S]
  run-control <runId> --action start|pause|resume|cancel|retry|remove [--node-id N]
  run-claim   <runId>
  run-report  <runId> <nodeId> --outcome done|failed|blocked|skipped [--note TEXT]
  daemon-start
  daemon-stop
  daemon-status

OFFLINE MAINTENANCE (refuses while a daemon serves the home):
  reindex   <home-path>
  doctor    <home-path>

EXIT CODES: 0 ok · 2 usage · 3 problem · 130 canceled";

fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct SystemClock;
impl MillisClock for SystemClock {
    fn now_ms(&self) -> i64 {
        system_now_ms()
    }
}

// ── entry ───────────────────────────────────────────────────────────────

pub fn run() -> std::process::ExitCode {
    let code = real_run();
    std::process::ExitCode::from(code as u8)
}

/// Integer exit-code surface for the thin `omt` binary entry.
pub fn real_run_code() -> i32 {
    real_run()
}

fn real_run() -> i32 {
    // Arm SIGINT/SIGTERM handling: the wake closure flips CANCELED and
    // shuts the active socket down so a blocked call aborts promptly.
    crate::signal::install(Box::new(|| {
        CANCELED.store(true, Ordering::SeqCst);
        if let Some(fd) = *ACTIVE_FD.lock().expect("fd slot") {
            unsafe {
                libc::shutdown(fd, libc::SHUT_RD);
            }
        }
    }));

    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch_args(args) {
        Ok(()) => EXIT_OK,
        Err(CliError::Usage(message)) => {
            eprintln!("{message}\n\n{USAGE}");
            EXIT_USAGE
        }
        Err(CliError::Problem(problem)) => {
            eprintln!(
                "{}",
                serde_json::json!({
                    "code": problem.code,
                    "details": problem.details,
                    "message": crate::problem::redact(&problem.message),
                })
            );
            EXIT_PROBLEM
        }
        Err(CliError::Canceled) => {
            eprintln!(
                "{}",
                serde_json::json!({"code": "CANCELED", "message": "call canceled by SIGINT"})
            );
            EXIT_CANCELED
        }
    }
}

enum CliError {
    Usage(String),
    Problem(Problem),
    Canceled,
}

impl From<Problem> for CliError {
    fn from(problem: Problem) -> CliError {
        if problem.code == "IO" && problem.message.contains("connection closed") {
            return CliError::Canceled;
        }
        CliError::Problem(problem)
    }
}

type CliResult<T> = std::result::Result<T, CliError>;

fn usage_err(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

// ── argument parsing ────────────────────────────────────────────────────

pub struct GlobalArgs {
    pub(crate) runtime_dir: Option<String>,
    homes: Vec<String>,
    json: bool,
    /// Stable cross-invocation actor namespace (leases fence to it).
    /// Defaults to "cli:<pid>"; set when separate invocations must share
    /// executor identity (e.g. claim in one call, report in another).
    actor: Option<String>,
}

struct Split {
    globals: GlobalArgs,
    rest: Vec<String>,
}

fn split_globals(args: &[String]) -> CliResult<Split> {
    let mut globals = GlobalArgs {
        runtime_dir: None,
        homes: Vec::new(),
        json: false,
        actor: None,
    };
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--runtime-dir" => {
                index += 1;
                globals.runtime_dir = Some(
                    args.get(index)
                        .ok_or_else(|| usage_err("missing value for --runtime-dir"))?
                        .clone(),
                );
            }
            "--home" => {
                index += 1;
                globals.homes.push(
                    args.get(index)
                        .ok_or_else(|| usage_err("missing value for --home"))?
                        .clone(),
                );
            }
            "--json" => globals.json = true,
            "--actor" => {
                index += 1;
                globals.actor = Some(
                    args.get(index)
                        .ok_or_else(|| usage_err("missing value for --actor"))?
                        .clone(),
                );
            }
            other => rest.push(other.to_string()),
        }
        index += 1;
    }
    Ok(Split { globals, rest })
}

fn flag_value(args: &mut std::collections::VecDeque<String>, name: &str) -> CliResult<String> {
    args.pop_front()
        .ok_or_else(|| usage_err(format!("missing value for {name}")))
}

fn parse_bool(raw: &str) -> CliResult<bool> {
    match raw {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        other => Err(usage_err(format!("expected boolean, got {other:?}"))),
    }
}

// ── verb dispatch ───────────────────────────────────────────────────────

fn dispatch_args(all: Vec<String>) -> CliResult<()> {
    // U1 (R22): `--version` short-circuits before any verb/runtime-dir work —
    // the anchor for install smoke (`./omt --version`) and brew formula checks.
    if all.iter().any(|a| a == "--version") {
        println!("omt {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let split = split_globals(&all)?;
    let mut rest: std::collections::VecDeque<String> = split.rest.into();
    let Some(verb) = rest.pop_front() else {
        return Err(usage_err("a verb is required"));
    };
    match verb.as_str() {
        "list" => online(&split.globals, |client, home_id| {
            node_list(client, home_id, rest.make_contiguous())
        }),
        "show" => online(&split.globals, |client, home_id| {
            let node_id = rest
                .front()
                .ok_or_else(|| usage_err("show requires <nodeId>"))?;
            print_result(
                client.call(
                    "node/get",
                    serde_json::json!({ "homeId": home_id, "nodeId": node_id }),
                )?,
                split.globals.json,
            )
        }),
        "create" => online(&split.globals, |client, home_id| {
            node_create(client, home_id, rest.make_contiguous())
        }),
        "update" => online(&split.globals, |client, home_id| {
            node_update(client, home_id, rest.make_contiguous())
        }),
        "move" => online(&split.globals, |client, home_id| {
            node_move(client, home_id, rest.make_contiguous())
        }),
        "archive" => online(&split.globals, |client, home_id| {
            let node_id = rest
                .front()
                .ok_or_else(|| usage_err("archive requires <nodeId>"))?;
            print_result(
                client.call(
                    "node/archive",
                    serde_json::json!({ "homeId": home_id, "nodeId": node_id }),
                )?,
                split.globals.json,
            )
        }),
        "reindex" => offline_reindex(&split.globals, rest.make_contiguous()),
        "doctor" => offline_doctor(&split.globals, rest.make_contiguous()),
        "takeover" => offline_takeover(&split.globals, rest.make_contiguous()),
        "mcp" => crate::mcp::serve(&split.globals)
            .map(|_| ())
            .map_err(CliError::Problem),
        "run-create" => online(&split.globals, |client, home_id| {
            run_create(client, home_id, rest.make_contiguous())
        }),
        "run-get" => online(&split.globals, |client, home_id| {
            let run_id = rest
                .front()
                .ok_or_else(|| usage_err("run-get requires <runId>"))?;
            print_result(
                client.call(
                    "run/get",
                    serde_json::json!({ "homeId": home_id, "runId": run_id }),
                )?,
                split.globals.json,
            )
        }),
        "run-list" => online(&split.globals, |client, home_id| {
            let mut params = serde_json::json!({ "homeId": home_id });
            if let Some(position) = rest.iter().position(|a| a == "--status") {
                if let Some(status) = rest.get(position + 1) {
                    params["status"] = serde_json::json!(status);
                }
            }
            print_result(client.call("run/list", params)?, split.globals.json)
        }),
        "run-control" => online(&split.globals, |client, home_id| {
            run_control(client, home_id, rest.make_contiguous())
        }),
        "run-claim" => online(&split.globals, |client, home_id| {
            let run_id = rest
                .front()
                .ok_or_else(|| usage_err("run-claim requires <runId>"))?;
            print_result(
                client.call(
                    "run/claim",
                    serde_json::json!({ "homeId": home_id, "runId": run_id }),
                )?,
                split.globals.json,
            )
        }),
        "run-report" => online(&split.globals, |client, home_id| {
            run_report(client, home_id, rest.make_contiguous())
        }),
        "daemon-start" => daemon_start(&split.globals),
        "daemon-stop" => daemon_stop(&split.globals),
        "daemon-status" => daemon_status(&split.globals),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(usage_err(format!("unknown verb {other:?}"))),
    }
}

fn print_result(result: serde_json::Value, json: bool) -> CliResult<()> {
    if json {
        println!("{}", serde_json::to_string(&result).unwrap_or_default());
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    }
    Ok(())
}

// ── online plumbing ─────────────────────────────────────────────────────

/// U7 (KTD4): the CLI's online plumbing now lives in the shared omt-client
/// crate; OnlineClient is a thin wrapper adding the CLI's Ctrl-C semantics.
struct OnlineClient {
    inner: omt_client::Client,
    token: String,
}

impl OnlineClient {
    /// Connect like the TS client: descriptor discovery → endpoint probe →
    /// handshake/enrollment with kind=cli and actorNamespace "cli:<pid>".
    /// U7: transport + enrollment live in the shared omt-client crate; this
    /// wrapper keeps the CLI's Ctrl-C cancel semantics, stored-token
    /// policy, and CliError mapping.
    fn connect(globals: &GlobalArgs) -> CliResult<(OnlineClient, String)> {
        if CANCELED.load(Ordering::SeqCst) {
            return Err(CliError::Canceled);
        }
        let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());
        let descriptor = omt_client::read_descriptor(&runtime_dir).ok_or_else(|| {
            CliError::Problem(Problem::with_details(
                omt_domain::error::NOT_FOUND,
                format!(
                    "no live daemon descriptor under {}; start one with `omt daemon-start`",
                    runtime_dir.display()
                ),
                |d| {
                    d.insert("kind".into(), "daemon".into());
                    d.insert(
                        "runtimeDir".into(),
                        runtime_dir.display().to_string().into(),
                    );
                },
            ))
        })?;
        if !crate::descriptor::pid_live(descriptor.pid)
            || !crate::ipc::probe(&descriptor.endpoint, std::time::Duration::from_millis(500))
        {
            return Err(CliError::Problem(Problem::with_details(
                omt_domain::error::NOT_FOUND,
                "descriptor is stale (dead pid or unresponsive endpoint); respawn with `omt daemon-start`",
                |d| {
                    d.insert("kind".into(), "descriptor".into());
                },
            )));
        }

        let cred_path = runtime_dir.join("cli-credential.json");
        let options = omt_client::EnrollOptions {
            kind: "cli".into(),
            name: Some("omt-cli".into()),
            actor_namespace: Some(
                globals
                    .actor
                    .clone()
                    .unwrap_or_else(|| format!("cli:{}", std::process::id())),
            ),
            operations: None,
            credential_path: Some(cred_path.clone()),
        };
        let mut enrollment = omt_client::Client::connect_and_enroll(&descriptor, &options)
            .map_err(|problem| {
                if problem.code == "CANCELED" {
                    CliError::Canceled
                } else {
                    CliError::Problem(problem)
                }
            })?;

        // U7/R10: home-SCOPE denials stamped requiresRehandshake (declared
        // after enrollment, generation rotation) heal exactly once — delete
        // the stored credential and re-enroll, mirroring the UNAUTHORIZED
        // fallback inside connect_and_enroll.
        let needs_credential_heal = {
            let probe = enrollment
                .client
                .call("node/list", serde_json::json!({ "filter": {} }));
            matches!(probe, Err(ref problem) if home_scope_rehandshake_hint(problem))
        };
        if needs_credential_heal {
            let _ = std::fs::remove_file(&cred_path);
            enrollment = omt_client::Client::connect_and_enroll(&descriptor, &options)
                .map_err(CliError::Problem)?;
        }

        let token = enrollment.client.token().to_string();
        let mut client = OnlineClient {
            inner: enrollment.client,
            token,
        };
        // The inner client already consumed one handshake for enrollment;
        // resolve_home_id works off a fresh discovery handshake, so run the
        // CLI's own finish step over the wrapper (keeps wire behavior
        // identical to pre-U7).
        client
            .inner
            .set_cancel_check(Box::new(|| CANCELED.load(Ordering::SeqCst)));
        #[cfg(unix)]
        {
            *ACTIVE_FD.lock().expect("fd slot") = Some(client.raw_fd());
        }
        let handshake = client.request(
            "handshake/request",
            serde_json::json!({
                "protocolVersion": "1.0",
                "client": { "kind": "cli", "name": "omt-cli", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        let home_id = resolve_home_id(&handshake, globals)?;
        Ok((client, home_id))
    }

    /// Raw fd for the Ctrl-C abort registry (single-threaded CLI; the
    /// wrapper owns the only handle).
    #[cfg(unix)]
    fn raw_fd(&self) -> i32 {
        self.inner.stream_fd()
    }

    /// Authenticated business call (credential token attached in request).
    fn call(&mut self, method: &str, params: serde_json::Value) -> CliResult<serde_json::Value> {
        self.request(method, params)
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> CliResult<serde_json::Value> {
        self.inner.set_token(self.token.clone());
        match self.inner.request(method, params) {
            Ok(value) => {
                self.token = self.inner.token().to_string();
                Ok(value)
            }
            Err(problem) if problem.code == "CANCELED" => Err(CliError::Canceled),
            Err(problem) => Err(CliError::Problem(problem)),
        }
    }
}

/// U7/R10: is this problem a home-SCOPE denial stamped with the server's
/// requiresRehandshake hint (KTD3)? Only FORBIDDEN home-not-scoped and
/// NOT_FOUND kind:home carry it; op-family FORBIDDEN deliberately does not.
fn home_scope_rehandshake_hint(problem: &Problem) -> bool {
    let hint = problem
        .details
        .as_ref()
        .and_then(|d| d.get("requiresRehandshake"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    hint && (problem.code == "FORBIDDEN" || problem.code == "NOT_FOUND")
}

fn io_problem(context: &str, err: std::io::Error) -> Problem {
    Problem::new(omt_domain::error::IO, format!("{context}: {err}"))
}

fn resolve_home_id(handshake: &serde_json::Value, globals: &GlobalArgs) -> CliResult<String> {
    let homes = handshake["homes"].as_array().cloned().unwrap_or_default();
    if homes.is_empty() {
        return Err(CliError::Problem(Problem::new(
            omt_domain::error::NOT_FOUND,
            "daemon serves no homes",
        )));
    }
    if globals.homes.is_empty() {
        return Ok(homes[0]["homeId"].as_str().unwrap_or_default().to_string());
    }
    let wanted = std::fs::canonicalize(&globals.homes[0])
        .unwrap_or_else(|_| std::path::PathBuf::from(&globals.homes[0]));
    for home in &homes {
        let path = home["path"].as_str().map(std::path::PathBuf::from);
        if let Some(path) = path {
            let same = std::fs::canonicalize(&path)
                .map(|p| p == wanted)
                .unwrap_or(false)
                || path == wanted;
            if same {
                return Ok(home["homeId"].as_str().unwrap_or_default().to_string());
            }
        }
    }
    // Fall back to treating --home as a literal HomeId.
    for home in &homes {
        if home["homeId"].as_str() == Some(globals.homes[0].as_str()) {
            return Ok(globals.homes[0].clone());
        }
    }
    Err(CliError::Problem(Problem::with_details(
        omt_domain::error::NOT_FOUND,
        format!("--home {} is not among the opened homes", globals.homes[0]),
        |d| {
            d.insert("kind".into(), "home".into());
        },
    )))
}

fn online<F>(globals: &GlobalArgs, body: F) -> CliResult<()>
where
    F: FnOnce(&mut OnlineClient, &str) -> CliResult<()>,
{
    let (mut client, home_id) = OnlineClient::connect(globals)?;
    body(&mut client, &home_id)
}

// ── online verb bodies ──────────────────────────────────────────────────

fn node_list(client: &mut OnlineClient, home_id: &str, args: &[String]) -> CliResult<()> {
    let mut filter = serde_json::Map::new();
    let mut pairs = std::collections::VecDeque::from(args.to_vec());
    while let Some(flag) = pairs.pop_front() {
        match flag.as_str() {
            "--type" => filter.insert(
                "type".into(),
                serde_json::json!(flag_value(&mut pairs, "--type")?),
            ),
            "--status" => filter.insert(
                "status".into(),
                serde_json::json!(flag_value(&mut pairs, "--status")?),
            ),
            "--query" => filter.insert(
                "query".into(),
                serde_json::json!(flag_value(&mut pairs, "--query")?),
            ),
            "--archived" => filter.insert(
                "archived".into(),
                serde_json::json!(parse_bool(&flag_value(&mut pairs, "--archived")?)?),
            ),
            other => return Err(usage_err(format!("unknown list flag {other:?}"))),
        };
    }
    print_result(
        client.call(
            "node/list",
            serde_json::json!({ "homeId": home_id, "filter": filter }),
        )?,
        false,
    )
}

fn node_create(client: &mut OnlineClient, home_id: &str, args: &[String]) -> CliResult<()> {
    let mut params = serde_json::json!({ "homeId": home_id });
    let mut pairs = std::collections::VecDeque::from(args.to_vec());
    while let Some(flag) = pairs.pop_front() {
        match flag.as_str() {
            "--type" => params["type"] = serde_json::json!(flag_value(&mut pairs, "--type")?),
            "--title" => params["title"] = serde_json::json!(flag_value(&mut pairs, "--title")?),
            "--parent" => {
                params["parentId"] = serde_json::json!(flag_value(&mut pairs, "--parent")?)
            }
            "--body" => params["body"] = serde_json::json!(flag_value(&mut pairs, "--body")?),
            "--priority" => {
                params["priority"] = flag_value(&mut pairs, "--priority")?
                    .parse::<i64>()
                    .map_err(|_| usage_err("--priority expects an integer"))?
                    .into()
            }
            "--command-id" => {
                params["commandId"] = serde_json::json!(flag_value(&mut pairs, "--command-id")?)
            }
            other => return Err(usage_err(format!("unknown create flag {other:?}"))),
        }
    }
    print_result(client.call("node/create", params)?, false)
}

fn node_update(client: &mut OnlineClient, home_id: &str, args: &[String]) -> CliResult<()> {
    let node_id = args
        .first()
        .ok_or_else(|| usage_err("update requires <nodeId>"))?;
    let mut changes = serde_json::Map::new();
    let mut expected_revision: Option<i64> = None;
    let mut pairs = std::collections::VecDeque::from(args[1..].to_vec());
    while let Some(flag) = pairs.pop_front() {
        match flag.as_str() {
            "--title" => changes.insert(
                "title".into(),
                serde_json::json!(flag_value(&mut pairs, "--title")?),
            ),
            "--status" => changes.insert(
                "status".into(),
                serde_json::json!(flag_value(&mut pairs, "--status")?),
            ),
            "--body" => changes.insert(
                "body".into(),
                serde_json::json!(flag_value(&mut pairs, "--body")?),
            ),
            "--append" => changes.insert(
                "append".into(),
                serde_json::json!(flag_value(&mut pairs, "--append")?),
            ),
            "--priority" => changes.insert(
                "priority".into(),
                flag_value(&mut pairs, "--priority")?
                    .parse::<i64>()
                    .map_err(|_| usage_err("--priority expects an integer"))?
                    .into(),
            ),
            "--archived" => changes.insert(
                "archived".into(),
                serde_json::json!(parse_bool(&flag_value(&mut pairs, "--archived")?)?),
            ),
            "--expected-revision" => {
                expected_revision = Some(
                    flag_value(&mut pairs, "--expected-revision")?
                        .parse::<i64>()
                        .map_err(|_| usage_err("--expected-revision expects an integer"))?,
                );
                continue;
            }
            other => return Err(usage_err(format!("unknown update flag {other:?}"))),
        };
    }
    if changes.is_empty() {
        return Err(usage_err("update requires at least one change"));
    }
    let mut params = serde_json::json!({
        "homeId": home_id,
        "nodeId": node_id,
        "changes": changes,
    });
    if let Some(revision) = expected_revision {
        params["expectedRevision"] = revision.into();
    }
    print_result(client.call("node/update", params)?, false)
}

fn node_move(client: &mut OnlineClient, home_id: &str, args: &[String]) -> CliResult<()> {
    let node_id = args
        .first()
        .ok_or_else(|| usage_err("move requires <nodeId>"))?;
    let mut new_parent: Option<String> = None;
    let mut pairs = std::collections::VecDeque::from(args[1..].to_vec());
    while let Some(flag) = pairs.pop_front() {
        if flag == "--to" {
            new_parent = Some(flag_value(&mut pairs, "--to")?);
        } else {
            return Err(usage_err(format!("unknown move flag {flag:?}")));
        }
    }
    let new_parent = new_parent.ok_or_else(|| usage_err("move requires --to NEWPARENTID"))?;
    print_result(
        client.call(
            "node/move",
            serde_json::json!({ "homeId": home_id, "nodeId": node_id, "newParentId": new_parent }),
        )?,
        false,
    )
}

fn run_create(client: &mut OnlineClient, home_id: &str, args: &[String]) -> CliResult<()> {
    let members = args
        .first()
        .ok_or_else(|| usage_err("run-create requires a comma-separated nodeId list"))?;
    let node_ids: Vec<&str> = members
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if node_ids.is_empty() {
        return Err(usage_err("run-create requires at least one nodeId"));
    }
    let mut title: Option<String> = None;
    let mut stop_on_failure = false;
    let mut auto_verify = false;
    let mut pairs = std::collections::VecDeque::from(args[1..].to_vec());
    while let Some(flag) = pairs.pop_front() {
        match flag.as_str() {
            "--title" => title = Some(flag_value(&mut pairs, "--title")?),
            "--stop-on-failure" => stop_on_failure = true,
            "--auto-verify" => auto_verify = parse_bool(&flag_value(&mut pairs, "--auto-verify")?)?,
            other => return Err(usage_err(format!("unknown run-create flag {other:?}"))),
        }
    }
    let mut params = serde_json::json!({
        "homeId": home_id,
        "nodeIds": node_ids,
        "config": {
            "stopOnFailure": stop_on_failure,
            "autoContinue": true,
            "autoVerify": auto_verify,
            "concurrency": 1,
        },
    });
    if let Some(title) = title {
        params["title"] = serde_json::json!(title);
    }
    print_result(client.call("run/create", params)?, false)
}

fn run_control(client: &mut OnlineClient, home_id: &str, args: &[String]) -> CliResult<()> {
    let run_id = args
        .first()
        .ok_or_else(|| usage_err("run-control requires <runId>"))?;
    let mut action: Option<String> = None;
    let mut node_id: Option<String> = None;
    let mut pairs = std::collections::VecDeque::from(args[1..].to_vec());
    while let Some(flag) = pairs.pop_front() {
        match flag.as_str() {
            "--action" => action = Some(flag_value(&mut pairs, "--action")?),
            "--node-id" => node_id = Some(flag_value(&mut pairs, "--node-id")?),
            other => return Err(usage_err(format!("unknown run-control flag {other:?}"))),
        }
    }
    let action = action.ok_or_else(|| usage_err("run-control requires --action ACTION"))?;
    let mut params = serde_json::json!({
        "homeId": home_id,
        "runId": run_id,
        "action": action,
    });
    if let Some(node_id) = node_id {
        params["nodeId"] = serde_json::json!(node_id);
    }
    print_result(client.call("run/control", params)?, false)
}

fn run_report(client: &mut OnlineClient, home_id: &str, args: &[String]) -> CliResult<()> {
    if args.len() < 2 {
        return Err(usage_err("run-report requires <runId> <nodeId>"));
    }
    let run_id = &args[0];
    let node_id = &args[1];
    let mut outcome: Option<String> = None;
    let mut note: Option<String> = None;
    let mut lease_token: Option<String> = None;
    let mut pairs = std::collections::VecDeque::from(args[2..].to_vec());
    while let Some(flag) = pairs.pop_front() {
        match flag.as_str() {
            "--outcome" => outcome = Some(flag_value(&mut pairs, "--outcome")?),
            "--note" => note = Some(flag_value(&mut pairs, "--note")?),
            "--lease-token" => lease_token = Some(flag_value(&mut pairs, "--lease-token")?),
            other => return Err(usage_err(format!("unknown run-report flag {other:?}"))),
        }
    }
    let outcome = outcome.ok_or_else(|| usage_err("run-report requires --outcome OUTCOME"))?;
    let mut params = serde_json::json!({
        "homeId": home_id,
        "runId": run_id,
        "nodeId": node_id,
        "outcome": outcome,
    });
    if let Some(note) = note {
        params["note"] = serde_json::json!(note);
    }
    if let Some(token) = lease_token {
        params["leaseToken"] = serde_json::json!(token);
    }
    print_result(client.call("run/report", params)?, false)
}

// ── daemon lifecycle verbs ──────────────────────────────────────────────

/// Resolve the daemon binary for spawn: OMT_DAEMON env, the sibling of this
/// executable (cargo target layout), then PATH.
fn daemon_binary() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("OMT_DAEMON") {
        if !path.trim().is_empty() {
            return std::path::PathBuf::from(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("omt-daemon");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from("omt-daemon")
}

fn daemon_start(globals: &GlobalArgs) -> CliResult<()> {
    let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());
    // Idempotent: a live daemon already serving → report and exit 0.
    if let Some(descriptor) = crate::descriptor::read(&runtime_dir) {
        if crate::descriptor::pid_live(descriptor.pid)
            && crate::ipc::probe(&descriptor.endpoint, std::time::Duration::from_millis(300))
        {
            println!(
                "{}",
                serde_json::json!({
                    "status": "already-running",
                    "pid": descriptor.pid,
                    "generation": descriptor.generation,
                })
            );
            return Ok(());
        }
    }
    let binary = daemon_binary();
    let mut command = Command::new(&binary);
    command
        .arg("--runtime-dir")
        .arg(&runtime_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for home in &globals.homes {
        command.arg("--home").arg(home);
    }
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|err| CliError::Problem(io_problem("daemon spawn", err)))?;
    let pid = child.id() as i64;

    // Poll for the new generation's descriptor + responsive endpoint.
    let clock = SystemClock;
    let deadline = system_now_ms() + 15_000;
    loop {
        if CANCELED.load(Ordering::SeqCst) {
            return Err(CliError::Canceled);
        }
        if let Some(descriptor) = crate::descriptor::read(&runtime_dir) {
            if crate::descriptor::pid_live(descriptor.pid)
                && crate::ipc::probe(&descriptor.endpoint, std::time::Duration::from_millis(250))
            {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "started",
                        "pid": descriptor.pid,
                        "generation": descriptor.generation,
                        "endpoint": descriptor.endpoint,
                        "binary": binary.display().to_string(),
                        "spawnerPid": pid,
                        "startedAt": omt_storage::clock::iso_from_ms(clock.now_ms()),
                    })
                );
                return Ok(());
            }
        }
        if system_now_ms() > deadline {
            return Err(CliError::Problem(Problem::with_details(
                "BOOTSTRAP_TIMEOUT",
                "spawned daemon did not publish a live descriptor within the poll budget",
                |d| {
                    d.insert(
                        "runtimeDir".into(),
                        runtime_dir.display().to_string().into(),
                    );
                },
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn daemon_stop(globals: &GlobalArgs) -> CliResult<()> {
    let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());
    let Some(descriptor) = crate::descriptor::read(&runtime_dir) else {
        println!("{}", serde_json::json!({ "status": "not-running" }));
        return Ok(());
    };
    if !crate::descriptor::pid_live(descriptor.pid) {
        println!(
            "{}",
            serde_json::json!({ "status": "stale-descriptor", "pid": descriptor.pid })
        );
        return Ok(());
    }
    let pid = descriptor.pid;
    send_sigterm(pid)?;
    let deadline = system_now_ms() + 10_000;
    while crate::descriptor::pid_live(pid) && system_now_ms() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let drained = !crate::descriptor::pid_live(pid);
    println!(
        "{}",
        serde_json::json!({
            "status": if drained { "stopped" } else { "stop-timeout" },
            "pid": pid,
        })
    );
    if drained {
        Ok(())
    } else {
        Err(CliError::Problem(Problem::with_details(
            "IO",
            "daemon did not exit within the drain budget",
            |d| {
                d.insert("pid".into(), pid.into());
            },
        )))
    }
}

fn send_sigterm(pid: i64) -> CliResult<()> {
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(CliError::Problem(io_problem("SIGTERM", err)));
    }
    Ok(())
}

fn daemon_status(globals: &GlobalArgs) -> CliResult<()> {
    let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());
    let descriptor = crate::descriptor::read(&runtime_dir);
    let status = match &descriptor {
        None => serde_json::json!({
            "running": false,
            "reason": "no-descriptor",
            "runtimeDir": runtime_dir.display().to_string(),
        }),
        Some(d) => {
            let alive = crate::descriptor::pid_live(d.pid);
            let endpoint_alive =
                alive && crate::ipc::probe(&d.endpoint, std::time::Duration::from_millis(300));
            serde_json::json!({
                "running": alive && endpoint_alive,
                "pid": d.pid,
                "pidAlive": alive,
                "endpointAlive": endpoint_alive,
                "generation": d.generation,
                "endpoint": d.endpoint,
                "startedAt": d.started_at,
                "runtimeDir": runtime_dir.display().to_string(),
            })
        }
    };
    println!("{status}");
    Ok(())
}

// ── offline maintenance verbs ───────────────────────────────────────────

fn offline_home_path(args: &[String]) -> CliResult<std::path::PathBuf> {
    let raw = args
        .first()
        .ok_or_else(|| usage_err("this verb requires a <home-path> argument"))?;
    let path = std::path::PathBuf::from(raw);
    if !path.exists() {
        return Err(CliError::Problem(Problem::with_details(
            omt_domain::error::NOT_FOUND,
            format!("home path {} does not exist", path.display()),
            |d| {
                d.insert("kind".into(), "home".into());
                d.insert("path".into(), path.display().to_string().into());
            },
        )));
    }
    Ok(path)
}

fn open_offline_storage(home: &std::path::Path, recover: bool) -> Result<omt_storage::Storage> {
    crate::ownership::recover_own_dead_daemon_marker(home)?;
    let mut config = omt_storage::journal::OpenConfig::new(home);
    config.acquire_lock = true;
    config.owner_kind = omt_storage::home_lock::OwnerKind::Daemon;
    config.recover_on_open = recover;
    config.hostname = "omt-cli".to_string();
    omt_storage::Storage::open(config)
}

/// `omt reindex <home-path>` — OFFLINE maintenance: refuses while a live
/// daemon serves the runtime dir, takes the exclusive daemon owner lock,
/// rebuilds the index from Markdown, releases the lock.
fn offline_reindex(globals: &GlobalArgs, args: &[String]) -> CliResult<()> {
    let _ = globals;
    let home = offline_home_path(args)?;
    let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());
    crate::ownership::refuse_if_served(&runtime_dir).map_err(CliError::Problem)?;

    let mut storage = open_offline_storage(&home, true).map_err(CliError::Problem)?;
    let outcome = (|| -> Result<serde_json::Value> {
        // Dedicated short-lived connection (reindex::execute wants &mut
        // Connection; the CLI holds the exclusive owner lock so no second
        // writer can race this handle).
        let db_path = home.join(omt_storage::DB_FILE_NAME);
        let mut conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| Problem::new(omt_domain::error::IO, format!("reindex open: {e}")))?;
        omt_storage::store::apply_open_pragmas(&conn)?;
        let plan = omt_storage::reindex::dry_run(&conn, storage.files())?;
        let executed =
            omt_storage::reindex::execute(&mut conn, storage.files(), &plan, &SystemClock)?;
        let nodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
            .map_err(|e| Problem::new(omt_domain::error::IO, format!("count nodes: {e}")))?;
        let edges: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .map_err(|e| Problem::new(omt_domain::error::IO, format!("count edges: {e}")))?;
        Ok(serde_json::json!({
            "home": home.display().to_string(),
            "nodes": nodes,
            "edges": edges,
            "skipped": executed.quarantined.len(),
        }))
    })();
    let _ = storage.release_lock();
    print_result(outcome.map_err(CliError::Problem)?, globals.json)
}

/// U7/R10 preamble result, pre-serialized so both doctor report shapes
/// embed identical fields.
struct DoctorPreamble {
    runtime: serde_json::Value,
    admin_grants: serde_json::Value,
}

/// Online preamble (observation only — no locks, no ownership checks):
/// installed `omt` binary version vs the RUNNING daemon's handshake
/// version, plus the admin-grants cohort with dead-pid entries surfaced.
/// Degrades gracefully: no descriptor → not-running; probe/handshake
/// failure or missing version field → match:"unknown"; exit code stays 0.
fn doctor_online_preamble(runtime_dir: &std::path::Path) -> DoctorPreamble {
    // Test seam: e2e asserts the mismatch path without building a fake
    // old daemon (the comparison itself is the unit under test).
    let cli_version: String = std::env::var("OMT_DOCTOR_CLI_VERSION_OVERRIDE")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let runtime = match omt_client::read_descriptor(runtime_dir) {
        None => serde_json::json!({
            "descriptorFound": false,
            "daemonVersion": null,
            "cliVersion": cli_version,
            "match": "unknown",
            "generation": null,
            "note": "no daemon descriptor — daemon not running; offline checks below",
        }),
        Some(descriptor) => {
            let live = crate::descriptor::pid_live(descriptor.pid)
                && omt_client::endpoint_live(&descriptor.endpoint);
            if !live {
                serde_json::json!({
                    "descriptorFound": true,
                    "daemonVersion": null,
                    "cliVersion": cli_version,
                    "match": "unknown",
                    "generation": descriptor.generation,
                    "note": "descriptor stale (dead pid or unresponsive endpoint)",
                })
            } else {
                let options = omt_client::EnrollOptions {
                    kind: "cli".into(),
                    name: Some("omt-doctor".into()),
                    actor_namespace: None,
                    operations: None,
                    credential_path: None,
                };
                match omt_client::Client::connect_and_enroll(&descriptor, &options) {
                    Ok(enrollment) => {
                        let daemon_version = enrollment.handshake["daemon"]["version"].clone();
                        let match_value = match daemon_version.as_str() {
                            None => serde_json::json!("unknown"),
                            Some(v) if v == cli_version => serde_json::json!(true),
                            Some(_) => serde_json::json!(false),
                        };
                        serde_json::json!({
                            "descriptorFound": true,
                            "daemonVersion": daemon_version,
                            "cliVersion": cli_version,
                            "match": match_value,
                            "generation": descriptor.generation,
                        })
                    }
                    Err(_) => serde_json::json!({
                        "descriptorFound": true,
                        "daemonVersion": null,
                        "cliVersion": cli_version,
                        "match": "unknown",
                        "generation": descriptor.generation,
                        "note": "handshake failed — version undetermined",
                    }),
                }
            }
        }
    };

    // Admin-grants cohort: total entries + entries whose embedded actor
    // namespace pid is dead (namespaces shaped '<prefix>:<pid>').
    let principals = crate::auth::admin_principals(runtime_dir);
    let mut dead = Vec::new();
    for principal in &principals {
        if let Some(pid_str) = principal.rsplit(':').next() {
            if let Ok(pid) = pid_str.parse::<i64>() {
                if pid > 0 && !crate::descriptor::pid_live(pid) {
                    dead.push(serde_json::json!({ "principalId": principal, "pid": pid }));
                }
            }
        }
    }
    let admin_grants = serde_json::json!({
        "totalEntries": principals.len(),
        "deadPidEntries": dead,
    });

    DoctorPreamble {
        runtime,
        admin_grants,
    }
}

/// `omt doctor <home-path>` — diagnostics with a U7/R10 ONLINE PREAMBLE
/// (installed-binary vs running-daemon version + admin-grants cohort) and
/// OFFLINE cohort scans (ts-bridge live/stale markers, orphan recovery
/// directories, too-new schema). A live daemon skips deep probes with a
/// note instead of refusing; the exclusive scan holds the owner lock
/// during the scan and releases afterwards.
fn offline_doctor(globals: &GlobalArgs, args: &[String]) -> CliResult<()> {
    let home = offline_home_path(args)?;
    let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());

    // U7/R10: ONLINE PREAMBLE — installed-binary vs running-daemon
    // consistency, computed BEFORE any lock acquisition (and before
    // refuse_if_served: deep probes keep their served-daemon refusal, the
    // preamble is pure observation). Never hard-fails: every failure mode
    // degrades to match:"unknown" or not-running with exit code 0.
    let preamble = doctor_online_preamble(&runtime_dir);
    // A live daemon refuses the DEEP offline probes (they would race its
    // home ownership) — but the preamble's whole purpose is reporting on
    // exactly that situation (version drift after an upgrade), so report
    // the runtime fields and skip the deep probes instead of erroring out
    // (mirrors the ts-bridge cohort path below).
    if let Err(refusal) = crate::ownership::refuse_if_served(&runtime_dir) {
        let report = serde_json::json!({
            "home": home.display().to_string(),
            "healthy": null,
            "runtime": preamble.runtime,
            "adminGrants": preamble.admin_grants,
            "note": "deep probes skipped: live daemon serving this runtime dir",
            "refusal": { "code": refusal.code, "message": refusal.message },
        });
        return print_result(report, globals.json);
    }

    // Cohort scan BEFORE acquiring (acquisition replaces the marker).
    let mut cohorts =
        serde_json::json!({ "tsBridgeMarkers": [], "orphans": [], "schemaTooNew": false });
    let marker_path = home.join(omt_storage::home_lock::LOCK_FILE_NAME);
    if let Ok(raw) = std::fs::read_to_string(&marker_path) {
        if let Ok(body) = serde_json::from_str::<serde_json::Value>(&raw) {
            if body["ownerKind"] == "ts-bridge" {
                let pid = body["pid"].as_i64();
                let heartbeat_age_ms = body["heartbeatAt"]
                    .as_str()
                    .and_then(omt_storage::clock::parse_iso_ms)
                    .map(|hb| system_now_ms().saturating_sub(hb))
                    .unwrap_or(i64::MAX);
                let live = pid.map(crate::descriptor::pid_live).unwrap_or(false);
                cohorts["tsBridgeMarkers"] = serde_json::json!([{
                    "ownerKind": "ts-bridge",
                    "pid": pid,
                    "state": if live { "live" } else { "stale" },
                    "heartbeatAgeMs": heartbeat_age_ms,
                    "takeover": "explicit takeover required (U6); never auto-stolen",
                }]);
            }
        }
    }

    // A ts-bridge marker (live OR stale) blocks exclusive acquisition by
    // ruling — but doctor's JOB is to report exactly that cohort. Report
    // it and skip the deeper (lock-requiring) probes instead of failing.
    let bridge_present = cohorts["tsBridgeMarkers"]
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if bridge_present {
        let report = serde_json::json!({
            "home": home.display().to_string(),
            "healthy": false,
            "runtime": preamble.runtime,
            "adminGrants": preamble.admin_grants,
            "cohorts": cohorts,
            "note": "deeper probes skipped: ts-bridge marker requires explicit takeover (U6)",
        });
        return print_result(report, globals.json);
    }

    let mut storage = match open_offline_storage(&home, false) {
        Ok(storage) => storage,
        Err(problem) => return Err(CliError::Problem(problem)),
    };
    let outcome = (|| -> Result<serde_json::Value> {
        // Too-new schema cohort (fail closed upstream, reported here).
        let user_version = omt_storage::store::get_user_version(storage.conn())?;
        cohorts["schemaTooNew"] =
            serde_json::json!(user_version > omt_storage::store::KNOWN_SCHEMA_VERSION);

        // Orphan recovery dirs: non-empty `.omt/recovery/<commandId>/`.
        let recovery_root = home.join(omt_storage::RECOVERY_ROOT);
        let mut orphans = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&recovery_root) {
            for entry in entries.flatten() {
                let is_nonempty = std::fs::read_dir(entry.path())
                    .map(|mut inner| inner.next().is_some())
                    .unwrap_or(false);
                if is_nonempty {
                    orphans.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        orphans.sort();
        cohorts["orphans"] = serde_json::json!(orphans);

        let nodes: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
            .unwrap_or(0);
        Ok(serde_json::json!({
            "home": home.display().to_string(),
            "healthy": cohorts["orphans"].as_array().map(|a| a.is_empty()).unwrap_or(true)
                && cohorts["schemaTooNew"] == serde_json::json!(false),
            "nodes": nodes,
            "runtime": preamble.runtime,
            "adminGrants": preamble.admin_grants,
            "cohorts": cohorts,
        }))
    })();
    let _ = storage.release_lock();
    print_result(outcome.map_err(CliError::Problem)?, globals.json)
}

/// `omt takeover <home-path>` — quiescent takeover of a bridge-era home
/// (TICKET-0124): snapshot bundle → exclusive migrate → generation fence →
/// persistent legacy fence. Refusals carry actionable guidance.
fn offline_takeover(globals: &GlobalArgs, args: &[String]) -> CliResult<()> {
    let home = offline_home_path(args)?;
    let runtime_dir = crate::paths::resolve(globals.runtime_dir.as_deref());
    let backups_root = crate::paths::resolve(globals.runtime_dir.as_deref()).join("backups");
    let report = crate::takeover::takeover_home(&runtime_dir, &home, &backups_root)
        .map_err(CliError::Problem)?;
    print_result(report, globals.json)
}
