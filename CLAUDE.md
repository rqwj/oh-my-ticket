# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Oh-My-Ticket (OMT): a ticket-management plugin for DSH (DeepSeek Harness) implementing an
`Epic → Story → [SubStory] → Ticket → [SubTicket]` hierarchy, plus a "run" mechanism for
batch-handing tickets to a model for execution with a state machine, progress tracking, and a
trust policy backstop.

The repo is **four surfaces sharing one Rust runtime**:

1. **`crates/`** — the Rust core (`omt-domain`, `omt-storage`, `omt-client`, `omt-runtime`). This is
   the single source of truth for domain logic, persistence, and the daemon. Ships two binaries:
   `omt-daemon` (JSON-RPC server over UDS) and `omt` (operator CLI).
2. **`src/`** (repo root) — the DSH plugin (`dsh-oh-my-ticket` npm package): a *thin* TS adapter
   over the daemon (host half `src/host/`, browser half `src/client/`). It does **not** touch
   SQLite/Markdown directly — everything routes through `@omt/client-ts` → `omt-daemon`.
3. **`packages/client-ts/`** (`@omt/client-ts`) — shared TS protocol bindings + daemon
   discover-or-spawn/handshake/reconnect client, generated from `schema/*.schema.json`. Consumed
   by both the DSH plugin and the desktop app.
4. **`apps/desktop/`** — a Tauri 2 desktop app (React + Rust `src-tauri`, using `omt-client`
   directly) that bundles the daemon as a sidecar binary.

`schema/*.schema.json` is the single source of truth for the wire protocol; both the Rust bindings
(`omt-contracts`, via `typify`/`schemars`) and `@omt/client-ts` are generated from it — never
hand-edit generated bindings, edit the schema and regenerate.

Read `README.md` (Chinese) for the user-facing feature tour, and
`docs/dsh-plugin-dev-notes.md` for hard-won DSH-plugin integration lessons (slot system, RPC/SSE,
packaging pitfalls) — check it before touching DSH-specific plumbing.

## Commands

### Node/TS side (repo root)

```sh
pnpm install                 # install deps (workspace: root + packages/client-ts + apps/desktop)
pnpm build                   # tsdown → lib/index.js (host half) + lib/client.js (browser half, CJS)
pnpm watch                   # incremental rebuild during dev
pnpm typecheck                # tsc --noEmit
pnpm test                    # vitest run (root suite; excludes corpus/, apps/, .dsh-checkout/)
pnpm build:daemon             # cargo build -p omt-runtime (needed before daemon-fixture tests)
pnpm sync-version 0.6.2       # bump version in lockstep across Cargo.toml/package.json/tauri.conf.json
pnpm gen                      # regenerate @omt/client-ts bindings from schema/*.schema.json
```

Run a single vitest file/test: `pnpm vitest run tests/running.spec.ts -t "test name"`.

Daemon-backed tests spawn a real `omt-daemon` — run `pnpm build:daemon` first or they'll be slow/fail
against a stale binary. `vitest.config.ts` has generous `hookTimeout`/`testTimeout` for this reason.

### Rust side

```sh
cargo build --workspace
cargo test --workspace
cargo test -p omt-domain some_test_name     # single crate / single test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

### Desktop app (`apps/desktop/`)

```sh
cd apps/desktop
pnpm dev            # vite dev server
pnpm build          # tsc --noEmit && vite build
pnpm tauri:dev       # copies sidecar daemon binary, then `tauri dev`
pnpm tauri:build     # copies sidecar daemon binary, then `tauri build`
pnpm test           # vitest run (jsdom + tauri API mocks; own vitest config, separate from root)
```

### Packaging

```sh
pnpm build && npm pack       # → dsh-oh-my-ticket-<version>.tgz
pnpm dsh plugin --profile <profile> add /path/to/dsh-oh-my-ticket-<version>.tgz
```
The daemon binary rides along as an optional dependency (`@oh-my-ticket/darwin-arm64`); it can
also be installed independently via brew / `install.sh` / GitHub Release.

CI (`.github/workflows/ci.yml`) is macOS-only (Tauri desktop needs macOS to build) and runs two
jobs: `rust` (fmt, clippy, `cargo test --workspace`, plus a corpus `yaml_differential` parity check
that needs `pnpm install`) and `node` (`pnpm typecheck`, `pnpm build:daemon`, `pnpm test`).

## Architecture notes

### Config resolution (cross-surface contract)

`docs/runtime/config.md` is the **single authority** for how runtime dir / global home resolve
(`explicit arg > env var > default`) across all three surfaces (DSH plugin, desktop, CLI/MCP).
If an implementation disagrees with that doc, the doc wins and the implementation is the bug.
Authoritative implementations: `crates/omt-runtime/src/paths.rs::resolve` (Rust),
`packages/client-ts/src/client.ts::OmtClient.resolveRuntimeDir` (TS), consumed by
`src/host/service.ts` on the DSH side. Cross-surface parity is locked by
`tests/config-parity.spec.ts`. All UI-layer clients must resolve to the *same* runtime dir as the
daemon (discovery is descriptor-based) — never introduce a second directory convention.

### Data model

- SQLite (WAL) stores metadata/hierarchy; Markdown files store ticket body content. Markdown is
  the content authority, SQLite is a query index; `omt_reindex` rebuilds the index from disk after
  manual markdown edits. Managed regions inside markdown files are delimited by HTML comments
  (e.g. `<!-- omt:children -->`).
- Tickets live under a "home" directory: a workspace-local `.omt/` (checked into git with the
  project) or the global home (plugin config > `OMT_HOME` env > `~/.omt`). IDs are globally unique
  across homes.
- `omt-storage` implements crash-recoverable dual-store persistence: SQLite + a phased operation
  journal + ledgered migrations + kernel-level home-ownership locking (advisory file locks via
  `fs4`).

### Runtime / daemon plane

- `omt-daemon` serves JSON-RPC 2.0 over a Unix domain socket, with bootstrap election, peer-
  credential (`SO_PEERCRED`) enrollment, and per-home serialized mutation queues.
- The DSH plugin adapter (`src/host/service.ts` / `OmtService`) discovers-or-spawns the daemon
  (via `OMT_DAEMON` env, `PATH`, or npm platform package fallback — see "daemon binary override"
  in `docs/runtime/config.md`), handshakes as kind `"dsh"`, and reconnects with capped backoff.
  All `omt_*` tool calls and RPC/SSE traffic route through this one client lifecycle.
- The daemon binary is intentionally the *same* binary across the DSH plugin, the CLI, and the
  desktop sidecar — logic lives once in Rust; TS/Tauri layers are thin adapters. Do not port
  domain logic back into TypeScript.

### DSH plugin internals (`src/`)

- `src/index.ts` is the plugin entry: wires `OmtService` as the optional-inject Cordis service
  `omt`, registers the `omt_*` tool family (`src/host/tools.ts`), the `/omt` RPC channel
  (`src/host/rpc.ts`), SSE change push (`src/host/events.ts`), and three session hooks (idle,
  disposed, notify).
- Client half (`src/client/`) registers into DSH's slot system: `TicketPanel` is the shared tree
  view rendered by three shells — `Drawer` (overlay), `FloatWindow`, `TicketTab` — plus
  `DocPanel` (details), `RunsView`/`RunPicker` (run UI), `TurnTickets`/`ReferencedBar` (chat
  linkage). `controller.ts` centralizes async/store logic consumed by components.
- Two embedded skills ship with the plugin: `omt` (ticket operation conventions/status flow) and
  `omt-runs` (run-batch claim/report/resume discipline) — see `src/host/skill.ts`.
- Config precedence for the plugin's own `home` option mirrors the cross-surface contract:
  plugin config > `OMT_HOME` env > `~/.omt` (`resolveHome` in `src/index.ts`).

### Testing layout

- Root vitest suite (`tests/`) covers host logic (pure node, temp-dir fixtures) and client
  controller logic (platform modules aliased to local mocks in `tests/mocks/`; DOM-dependent specs
  need a leading `// @vitest-environment jsdom` comment).
- `corpus/` is a separate scenario corpus (TS/Rust runners under `corpus/runner/`) exercised by
  `scripts/gen-yaml-goldens.mjs` / the CI `yaml_differential` check, **not** by `pnpm test` — it's
  excluded in `vitest.config.ts`.
- `apps/desktop/` has its own vitest config (jsdom + Tauri API mocks) — run its tests from within
  that directory, not from the root.
- `.dsh-checkout` is a local symlink to a DeepSeek Harness checkout (set up via a local `pnpm run
  setup`, not present in fresh clones) — never treated as part of this repo's test suite.
