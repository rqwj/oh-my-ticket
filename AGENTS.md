# Repository Agent Guide

This file is the canonical, tool-neutral guidance for coding agents working in this repository.
Tool-specific instruction files may add interface details, but they must not contradict this file.

## Architecture

Oh-My-Ticket has two runtime planes and four main source surfaces:

- The ticket/domain plane lives in `crates/`. Rust owns domain rules, SQLite and Markdown
  persistence, the JSON-RPC daemon, CLI, and MCP server. Do not duplicate domain logic in UI or
  TypeScript adapters.
- The desktop execution plane lives in `apps/desktop/src-tauri/`. It owns local harness discovery
  and configuration (`harness.rs`), agent and binding persistence (`agents.rs`), process lifecycle
  and dispatch (`executor.rs`), atomic JSON state (`atomic_json.rs`), and temporary MCP injection
  (`mcp_injection.rs`). This plane may coordinate the daemon, but it does not replace domain rules.
- `src/` is the DSH plugin adapter and browser UI.
- `packages/client-ts/` is the shared TypeScript daemon client.

`schema/*.schema.json` is the source of truth for the wire protocol. Edit schemas and run
`pnpm gen`; do not hand-edit generated TypeScript bindings. Rust bindings are generated at build
time.

For DSH-specific integration work, also read `docs/dsh-plugin-dev-notes.md`. Runtime directory and
home resolution are defined by `docs/runtime/config.md`.

## Commands

From the repository root:

```sh
pnpm install
pnpm typecheck
pnpm build
pnpm test
pnpm build:daemon
pnpm gen
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Desktop frontend checks run separately:

```sh
cd apps/desktop
pnpm build
pnpm test
```

Daemon-backed tests require a built `omt-daemon`; run `pnpm build:daemon` first. Tauri compilation
also requires staged sidecars; CI demonstrates the authoritative staging sequence.

## Working Agreements

- Keep changes inside the owning plane and reuse existing contracts before adding abstractions.
- Preserve secrets: environment values and credential-bearing arguments must not enter persisted
  agent records, events, logs, or error messages.
- Process-control commands must resolve and validate persisted execution identity server-side;
  never trust a renderer-supplied PID as authorization.
- Lifecycle changes must cover success, failure, cancellation, restart, and cleanup paths.
- Add focused regression tests for behavior changes. Then run the smallest relevant suite, followed
  by typecheck/build or workspace tests as appropriate.
- Keep generated files synchronized with their source schema and verify the generated diff.
- Do not commit build outputs, local runtime state, credentials, or staged sidecar binaries.
