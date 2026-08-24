# omt-runtime / omt-daemon (plan U5a)

The runnable OMT runtime: a single-writer local daemon speaking JSON-RPC 2.0
over OS-local IPC. This unit delivers the daemon skeleton plus a working
dispatch for every command in `schema/commands.schema.json`; CLI/MCP/desktop
consumers land in later units (U5b–U9).

## Endpoint and discovery

Per-user runtime directory resolution (`src/paths.rs`):

1. `--runtime-dir <dir>` argument,
2. `OMT_RUNTIME_DIR` environment variable (tests/sandboxes),
3. otherwise `~/.omt/run/`.

**Choice note:** the existing TypeScript host has no runtime-directory
convention (`src/host` resolves only ticket homes), so the daemon pins its
files beside the global home root instead of a platform temp dir. Every
daemon-owned file stays under one user-visible location.

Layout inside `<runtime-dir>`:

| File | Purpose |
|---|---|
| `descriptor.json` | Atomic generation descriptor `{schemaVersion:1, endpoint, generation, pid, bootToken, startedAt}`; published tmp+rename |
| `bootstrap.lock` | Single-daemon election lock (O_EXCL create, 2 s heartbeat, 10 s staleness with injectable clock) |
| `admin-grants.json` | Out-of-band administrator principal list `{"principalIds":[...]}`, re-read fresh on every check |
| `omt/daemon.sock` | Unix domain socket endpoint (windows: named pipe `\\.\pipe\omt\<hash>\omt-daemon.pipe`, compiled per-target) |

Descriptor staleness = PID liveness **and** a connect probe against the
endpoint; each replacement publishes `generation = previous + 1`. Startup
performs pending-journal recovery while opening homes **before** publishing
the descriptor, so readiness implies recovered.

## Framing (pinned)

**Newline-delimited JSON**: exactly one JSON-RPC 2.0 message object per
line, `\n` terminated, UTF-8, no batch arrays, no content-length headers.
Payloads above 8 MiB are refused. The server pushes `omt/event`
notifications interleaved with responses over the same ordered connection.

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
(default `*`), expiresAt now+12h. Every subsequent request carries
`params.credential.token`. Credentials live only in memory and die with the
process generation. No credential appears in argv, env, logs, or error text
(`problem::redact` scrubs 64-hex runs from anything crossing the wire).

Parity enforcement (`schema/parity.schema.json`): agent_available is the
default; adapter_only (`node/execute`, `ui/*`) requires a dsh/desktop
principal; human_administrative (`home/reindex`) requires the principal id
to be listed in `admin-grants.json`.

## Home ownership and serialization

Each home opens under the daemon owner lock
(`OpenConfig { acquire_lock: true, owner_kind: Daemon }`) with journal
recovery on open, then all of its work funnels through one actor thread
(mpsc queue): mutations and reads serialize, WAL snapshots serve reads, and
the lock heartbeats between jobs. SIGTERM stops accepting, drains every
queue, releases locks, removes our descriptor, exits 0.

## Deviations (deliberate, revisited by later units)

- **Windows transport** compiles as a skeleton (pipe-name derivation +
  cfg-gated stubs); peer credentials via `GetNamedPipeClientProcessId` land
  with the windows release leg (U10). All suites here run on unix.
- **Run-plane writes** (`run/create`, `run/control`, `run/claim`,
  `run/report`) commit through immediate single transactions gated by
  domain decisions; the U4a phased-journal vocabulary currently covers node
  files only. Journal-backed run rows are an additive U4/U5b follow-up;
  node writes (`node/create|update|move|archive|execute`, report file
  patches) already flow through the journal.
- **Event retention** never prunes in U5a, so `snapshot.resync`
  `prunedThroughSeq`/`consumerCursor` (added to `events.schema.json`
  additively this unit) stay optional until retention ships.
- **Idle shutdown** after a quiet period is deferred to U5b lifecycle
  configuration; SIGTERM drain is implemented and tested.
