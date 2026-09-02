# omt-contracts

Rust bindings for the OMT JSON-RPC protocol, generated from the JSON Schema
documents in [`schema/`](../../schema) — the single source of truth for
commands, queries, events, problems, handshake/capabilities, and the
action-parity classification (KTD2, R3).

## Code generation: the one deterministic path

**Build-time generation via `build.rs` + typify.** There is no committed
generated Rust source and no external generator tool to install; every
`cargo build` / `cargo test` of this crate deterministically regenerates
`$OUT_DIR/contracts_generate.rs` from the current schema documents and compiles
it in. Determinism comes from:

1. Files are read in sorted order (`schema/*.schema.json`).
2. All `$defs` are merged into **one flat namespace**. Definition names are
   globally unique across documents — a duplicate fails the build loudly.
   Cross-file `$ref`s such as `"common.schema.json#/$defs/HomeId"` are rewritten
   by `build.rs` into internal pointers (`"#/$defs/HomeId"`), which is lossless
   under the flat merge because typify resolves refs by their final path
   segment.
3. `prepare_for_typify` strips three keyword groups that carry no type shape or
   that typify does not implement — they remain fully in force at the
   schema-validation layer (see `tests/contract.rs`, which validates against the
   unmodified documents through the `jsonschema` crate):
   - `if` / `then` / `else` (conditional validation; e.g. run/control retry and
     remove requiring `nodeId`);
   - `format` (typify hard-codes `date-time` → chrono, which would drag chrono
     into every consumer; timestamps stay validated ISO 8601 strings);
   - nothing else: patterns, lengths, bounds, enums, requireds, recursion all
     survive into the generated types.

Regeneration happens implicitly; there is no separate step to remember.

## Wire conventions

- JSON fields are **camelCase** (`homeId`, `nodeId`, `expectedRevision`);
  enum **values** keep the domain's snake_case spelling (`in_progress`,
  `awaiting_confirmation`, `completed_with_failures`). Generated serde code
  renames accordingly.
- Every public reference carries a stable `homeId` matching
  `^h_[a-z0-9]{6,}$` (R4). Bare ids are a DSH-adapter compatibility input,
  resolved before anything crosses the wire.
- Errors travel as JSON-RPC error objects whose `data` is a `Problem`:
  stable machine-readable `code` + structured `details`; `message` is
  diagnostic only and never part of assertions (R5). The seeded coarse codes
  live in `schema/problems.schema.json`; the registry map is open so U2 can
  subdivide additively.
- Wire objects do not set `additionalProperties: false`, so additive schema
  changes are tolerated by older peers; generated structs ignore unknown
  fields. (The two envelope response variants deliberately pin themselves.)

## Protocol constants

`omt_contracts::protocol` holds semantic anchors (`PROTOCOL_VERSION`,
`UNSUPPORTED_PROTOCOL_CODE`, `SEED_PROBLEM_CODES`, `SCHEMA_ID_BASE`) that mirror
values declared in the schema documents; `tests/contract.rs` re-reads the
schemas and fails if they drift.

## Layout

- `build.rs` — codegen entry point (see above).
- `src/lib.rs` — includes generated module, re-exports it, declares constants.
- `tests/contract.rs` — focused contract suite: round-trips, unknown-field
  tolerance, unsupported-major negotiation, malformed `homeId` rejection,
  code/details-only error assertions, parity-matrix coverage.
- TypeScript bindings for the same schemas are generated separately by
  `scripts/gen-bindings.mjs` into `packages/client-ts/src/generated/`.

## Sandbox note

In environments where `~/.cargo` is not writable, point cargo at a
workspace-local home: `CARGO_HOME="$PWD/.cargo-home"`. The directory is
gitignored.
