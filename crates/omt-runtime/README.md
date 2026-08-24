# omt-runtime / omt-daemon / omt (plan U5a–U5c)

The runnable OMT runtime: a single-writer local daemon speaking JSON-RPC 2.0
over OS-local IPC, plus the `omt` operator CLI (second binary of this crate).
U5a delivered the daemon skeleton and dispatch; U5b added resource limits,
lifecycle configuration, rotating logs, daemon-owned retention, run-plane
journal vocabulary, and cancellation; U5c added this CLI and the TypeScript
client's reconnect/cancel polish.

## Bins

- `omt-daemon` — the single-writer runtime (`src/server.rs`).
- `omt` — the operator CLI (`src/cli/`), see below.

## Endpoint and discovery

Per-user runtime directory resolution (`src/paths.rs`):

1. `--runtime-dir <dir>` argument,
2. `OMT_RUNTIME_DIR` environment variable (tests/sandboxes),
3. otherwise `~/.omt/run/`.

Layout inside `<runtime-dir>`:

| File | Purpose |
|---|---|
| `descriptor.json` | Atomic generation descriptor `{schemaVersion:1, endpoint, generation, pid, bootToken, startedAt}`; published tmp+rename |
| `bootstrap.lock` | Single-daemon election lock (O_EXCL create, 2 s heartbeat, 10 s staleness with injectable clock) |
| `admin-grants.json` | Out-of-band administrator principal list `{"principalIds":[...]}`, re-read fresh on every check |
| `daemon.json` | Lifecycle + limits configuration (see below); absent file means compiled defaults |
| `logs/omt-daemon.log(.N)` | Size-capped rotating daemon log (`maxFiles × maxBytes` bound total volume) |
| `cli-credential.json` | Persisted CLI enrollment (0600) so leases fence consistently across invocations |
| `omt/daemon.sock` | Unix domain socket endpoint (windows: named pipe `\\.\pipe\omt\<hash>\omt-daemon.pipe`, compiled per-target) |

Descriptor staleness = PID liveness **and** a connect probe against the
endpoint; each replacement publishes `generation = previous + 1`. Startup
performs pending-journal recovery while opening homes **before** publishing
the descriptor, so readiness implies recovered.

## Lifecycle configuration (`daemon.json`, U5b)

Precedence: compiled defaults < config file; unknown keys or ill-typed
values fail startup closed with `INVALID_INPUT` naming the field.

```json
{
  "idleQuietMs": 1800000,
  "lockHeartbeatMs": 10000,
  "log": { "maxBytes": 5242880, "maxFiles": 3 },
  "limits": { "maxOpenHomes": 8, "maxRetainedEvents": 100000 }
}
```

- `idleQuietMs` — quiet-period idle watchdog (default 30 min). After this
  much silence a home actor drains, releases its lock, and exits; the last
  exiting actor wakes the accept loop so the process shuts down cleanly.
  `0` disables the watchdog.
- `lockHeartbeatMs` — per-home owner-lock heartbeat cadence.
- `log.maxBytes` × `log.maxFiles` — rotation caps; total on-disk log volume
  stays under their product by construction (oldest generation deleted at
  rollover). Every logged line passes secret redaction before touching disk.
- `limits` — overrides for the R21 bounds advertised in the handshake
  (`Limits` in `schema/capabilities.schema.json`).

## Resource limits (R21, U5b)

Every externally influenced quantity is bounded (`src/limits.rs`) and each
bound degrades FAIRLY — cheap checks before expensive ones — returning a
registered Problem code:

- `RATE_LIMITED` (transient capacity): concurrent connections at cap
  (refused pre-protocol), per-home queue at depth (`try_send`, O(1)).
- `QUOTA_EXCEEDED` (durable resource): opened-homes count, idempotency-table
  entries at retention cap (committed replays always stay allowed).
- `INVALID_INPUT` (caller mistakes): payload bytes over `maxPayloadBytes`,
  search term over `maxSearchTermBytes`, event page over `maxEventBatch`.

The negative matrix lives in `tests/limits_matrix.rs`; it asserts both each
bound tripping and the fair order along the request path.

## Home ownership, serialization, cancellation

Each home opens under the daemon owner lock
(`OpenConfig { acquire_lock: true, owner_kind: Daemon }`) with journal
recovery on open, then all of its work funnels through one actor thread
(mpsc queue): mutations and reads serialize, WAL snapshots serve reads, and
the lock heartbeats between jobs.

**Owner markers (binding rulings):** a restart may auto-recover ONLY its own
homes — a `home.lock` whose `ownerKind:"daemon"` carries a DEAD pid (kernel
flock probed first) — while any ts-bridge marker requires explicit takeover
(U6) and a live daemon marker refuses second writers with
`DAEMON_OWNS_HOME`.

**Cancellation (U5b):** `$/cancelRequest` flips an in-flight probe honored
ONLY at linearization-safe points in `Storage::execute_cancellable`: before
the journal row exists → clean abort; after `prepared` /
`files_applied` → the op aborts with `CANCELED` while its journal row stays
pending-recovery (identical to a crash; recovery rolls it forward); after
`db_committed` → the cancel is ignored and the op completes.

## Retention (R11/F4, U5b)

The daemon owns pruning: each home actor tick runs
`outbox::prune_and_signal` to `maxRetainedEvents`, keying the protected
consumer cursor on the OLDEST LIVE SUBSCRIBER cursor. When retained history
no longer covers that consumer, a keyed `snapshot.resync`
(`prunedThroughSeq`, `consumerCursor`) fans out through the normal publish
path.

## Handshake and credentials

Connect → the kernel reports peer credentials (SO_PEERCRED on Linux,
LOCAL_PEERCRED/LOCAL_PEERPID on Darwin); other-uid connections close before
any protocol exchange. The client then sends:

```json
{"jsonrpc":"2.0","id":1,"method":"handshake/request","params":{
  "protocolVersion":"1.0",
  "client":{"kind":"dsh|cli|desktop|mcp|external","name":"..."},
  "requestedScopes":{"actorNamespace?":"...","homes?":["..."],"operations?":["..."]}
}}
```

The server derives a credential — token (32 random bytes hex),
principalId `<kind>:<pid>`, actorNamespace (`<kind>:<pid>` unless the
requested namespace equals it or nests under `<kind>:<pid>/…`; a client can
never mint another principal's namespace), homes ∩ open homes, operations
(default `*`), expiresAt now+12h — formally registered as
`$defs/CredentialGrant` in `schema/capabilities.schema.json`. Every
subsequent request carries `params.credential.token`. Credentials live only
in memory and die with the process generation. No credential appears in
argv, env, logs, or error text (`problem::redact` scrubs 64-hex runs from
anything crossing the wire or the log).

Parity enforcement (`schema/parity.schema.json`): agent_available is the
default; adapter_only (`node/execute`, `ui/*`) requires a dsh/desktop
principal; human_administrative (`home/reindex`) requires the principal id
to be listed in `admin-grants.json`.

## The `omt` CLI (U5c)

Online verbs connect exactly like the TypeScript client (descriptor
discovery → liveness probe → handshake/enrollment, kind `cli`,
actorNamespace `cli:<pid>` by default):

```
list · show · create · update · move · archive
run-create · run-get · run-list · run-control · run-claim · run-report
daemon-start · daemon-stop · daemon-status
```

Offline maintenance verbs take exclusive ownership themselves
(`OwnerKind::Daemon` marker + kernel flock, released afterwards) and REFUSE
while a live daemon serves the runtime dir (`HOME_LOCKED` with stop-the-
daemon guidance):

```
reindex <home-path>     # quarantine-preserving index rebuild
doctor  <home-path>     # local writer cohorts: ts-bridge live/stale markers,
                        # orphan recovery dirs, too-new schema; never steals
```

Contract: human summary on stdout (pretty JSON; `--json` for compact),
Problem code/details on stderr, exit codes `0` ok · `2` usage · `3` problem
· `130` canceled. Ctrl-C aborts the in-flight call (socket shutdown breaks
the blocked read) and exits 130.

## Deviations (deliberate, revisited by later units)

- **Windows transport** compiles as a skeleton (pipe-name derivation +
  cfg-gated stubs); peer credentials via `GetNamedPipeClientProcessId` land
  with the windows release leg (U10). All suites here run on unix.

## Verification gates & benchmark tier (U5d)

Blocking verification lives in `tests/envelope_gates.rs` and runs as part of
a normal `cargo test -p omt-runtime`:

- `gate_envelope_multi_client_correctness` — up to 8 homes × N nodes/home ×
  4 concurrent authenticated clients × R runs: mixed-op storm over real
  connections asserting zero duplicate committed commandIds, zero lost
  accepted updates (read-your-write + gap-free revision chains), revision
  CONFLICT exactly when a concurrent winner exists, lease-fencing negatives
  (`lease-stale`/`lease-attempt`/`lease-token`/`actor-mismatch`/
  `report-state-gate`), and per-home SQL invariants. Scale is env-tunable:
  `OMT_ENV_HOMES/_NODES/_CLIENTS/_RUNS/_RUN_ITEMS/_ITERS`, or
  `OMT_ENV_FULL=1` for the full envelope. The default is the plan's blocking
  minimum ratio (8 × 500 nodes/home × 4 clients × 3 runs×50 items) — the
  full envelope is fsync-bound (~400 s of seeding alone on the dev machine);
  see `docs/runtime/bench-baseline.md`.
- `gate_sigkill_to_ready_under_five_seconds` — SIGKILLs the daemon
  mid-mutation, injects one `prepared` and one demoted `db_committed`
  journal row, respawns via bootstrap and asserts readiness <5 s with full
  convergence (journal roll-forward/replay, planned-bytes scan of every node
  file, all pre-kill acknowledgments visible).
- The crash-recovery kill-point grid is NOT rebuilt here — see
  `crates/omt-storage/tests/kill_grid.rs` (53 cells) and `kill_grid_runs.rs`
  (5 cells).

Non-blocking benchmarks live in `tests/bench_tier.rs` (`#[ignore]`; run with
`cargo test --release -p omt-runtime --test bench_tier -- --ignored
--nocapture --test-threads=1`): b1 = 10k-command mixed stress, b2 = soak
(`OMT_SOAK_SECONDS` overrides duration, default 1800; asserts ≥20 cmd/s
aggregate and interactive p95 <100 ms), b3 = 100k-event resume <2 s.
Recorded numbers: `docs/runtime/bench-baseline.md`.

All suites share `tests/common`, which installs an exit reaper that kills
any daemon process leaked by a failing test (tracked pids plus descriptor
pids found under registered runtime dirs), so panics cannot strand
`omt-daemon` processes.
