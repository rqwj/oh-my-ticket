# omt-domain

Pure OMT domain model (plan unit **U3**): hierarchy / status / run / lease-decision
semantics ported from the TypeScript core (`src/host/core.ts` + `src/host/store.ts`)
with **zero harness dependencies** — no filesystem access, no wall clock, no sockets,
no async runtime. Every side effect crosses a port; every state-machine rule is a
total function over row snapshots. The executable specification is the frozen
behavioral corpus (`corpus/scenarios/*.json`, 68 scenarios · 493 ops · 319
invariants): `cargo test -p omt-domain --test corpus` must execute all of it with
**zero divergence** from the TypeScript leg.

```
CARGO_HOME=$PWD/.cargo-home cargo test -p omt-domain
CARGO_HOME=$PWD/.cargo-home cargo test -p omt-domain --test corpus -- --nocapture
```

## Module map (TS concern → Rust module)

| TS source | Rust module | Notes |
|---|---|---|
| `src/host/types.ts` | [`types`] | Rows (`NodeRow`, `RunRow`, `RunItemRow`), transition tables, predicates. Node/run/item vocabularies are **re-used** from `omt-contracts` (typify-generated enums, serde snake_case). |
| `src/host/errors.ts` (Problem codes) | [`error`] | `Problem { code, message, details? }`; coarse codes + pre-freeze subdivisions; `NOT_FOUND` helpers carry the TS details shapes (`kind/id`, `runId/nodeId`). |
| `src/host/markdown.ts` + file layout of `files.ts` | [`markdown`] | Frontmatter split/parse/serialize, managed children block, slug/path rules. |
| `src/host/store.ts` | [`store`] | In-memory store whose query functions mirror the SQL `ORDER BY` clauses **normatively**: nodes by id, children by (ord, child_id), runs by id, items by (position, node_id), per-node items by run_id. Search = token AND substring, ASCII-case-insensitive, title-hit-first rank, limit 20. |
| `core.ts` create/move guards | [`hierarchy`] | root-requires-epic, child-type, self-parent, descendant-cycle, ancestor walk, descendants BFS. |
| `core.ts` run/item transitions, derivation, trust gate | [`runs`] | Transition matrices verbatim from `core.ts` L147–176; terminal derivation; narrow trust gate; lease-fenced report authorization (`authorize_report`). |
| `core.ts` janitor sweep | [`janitor`] | Two-pass pure planner (`plan_sweep`) + the six ratified U3 decisions in doc comments. |
| clock / leases / files | [`ports`] | `Clock`, `LeaseTable` (+ `MemoryLeases`), `FileStore` (+ in-memory VFS in tests). |
| `core.ts` orchestration | [`core`] | `OmtCore`: dual-write sequencing, event emission, observation, claims, reports, startup sweep. |

[`types`]: src/types.rs
[`error`]: src/error.rs
[`markdown`]: src/markdown.rs
[`store`]: src/store.rs
[`hierarchy`]: src/hierarchy.rs
[`runs`]: src/runs.rs
[`janitor`]: src/janitor.rs
[`ports`]: src/ports.rs
[`core`]: src/core.rs

## The six ratified U3 semantics decisions

Recorded normatively in `src/janitor.rs`; summarized here:

1. **Liveness is real leases.** The TypeScript leg's "live sessions" set maps onto
   the port layer as an exclusive set of far-future lease grants
   (`LeaseTable::mark_exclusive`). A session is alive for the sweep iff it holds an
   unexpired grant (**session-level** liveness — matching the TS membership oracle;
   legacy seeded executors that never claimed stay alive this way).
   Attempt-binding fences *explicit reports* instead (`runs::authorize_report`).
   Scenario `openSession/closeSession` → issue/expire at the port layer; the same
   scenario JSON drives both legs. An item whose executor is undefined is always
   orphaned.
2. **The sweep is two-pass, and order is normative.** Pass 1 demotes orphaned
   running items across `running|paused` runs; pass 2 then derives terminal states
   BEFORE falling back to `interrupted`. Paused runs keep their status unless
   derivation applies.
3. **Claim emits drained-member events first:** every pending-but-unclaimable
   member (archived node / container type) emits `[pending→skipped]` BEFORE the
   claimed item's `[pending→running]`.
4. **Observation ignores fully-terminal runs.** Derivation seals them before late
   reports can arrive.
5. **Trust gate is narrow:** only the executing principal's own bare `done`
   (`omt_update`, not `omt_run_report`) on its own RUNNING item gates to
   `awaiting_confirmation`. Repeated bare dones never bypass confirmation
   (`awaiting_confirmation` short-circuits to false).
6. **Seeding without an explicit id consumes the RUN counter**
   (`RUN-0001`, `RUN-0002`, …).

## TypeScript subtleties discovered during the port

These behaviors are easy to misread as bugs; they are load-bearing:

* **child-type check precedes descendant-cycle**, so a public move can never
  surface `descendant-cycle` — it is defensively unreachable behind the
  child-type guard (kept in [`hierarchy`] with the same precedence).
* **A failed report touches only the item.** `failed` reports transition the item
  and append the note to the ticket body, but never change ticket status; the
  note lands in `last_error`. The transition lands FIRST so a failing note-append
  cannot strand an untransitioned item.
* **Timestamp single-quoting.** js-yaml dumps ISO strings single-quoted because
  they resolve as implicit timestamps; plain scalars stay bare; `'` inside strings
  becomes `''`. The Rust codec reproduces this exactly (see `is_yaml_timestamp`,
  which checks bytes before slicing so CJK titles never panic at char boundaries).
* **UTF-16 vs chars.** `title.slice(0, 40)` counts UTF-16 units; Rust counts
  chars. Identical for BMP titles (the whole corpus); astral-plane titles are a
  documented divergence risk for U4 golden snapshots.
* **finished_at preservation.** `set_run_status` preserves the existing stamp on
  paused/pending transitions, clears it re-entering running, stamps absolute
  terminals only.
* **Store persistence across reopen.** The DB outlives one `OmtCore` instance
  (shared handle); only `reopen {freshDb:true}` wipes it, after which the
  open-flow sees missing schema_version + existing files and reindexes.
* **Two distinct claim gates.** `pending` (dispatchable) vs non-running (already
  dispatched) produce the same CONFLICT shape but are separate checks, mirroring
  the TS branch order.
* **Event visibility.** Item demotions performed by the janitor write the store
  directly and emit NO item events; listeners attach post-open, so startup
  demotions stay invisible unless a scenario re-attaches collectors after reopen.

## Layout

```
crates/omt-domain/
├── src/
│   ├── lib.rs        crate wiring + module map
│   ├── types.rs      rows, vocabularies, transition tables
│   ├── error.rs      Problem codes (R5 taxonomy)
│   ├── markdown.rs   frontmatter codec + children block + slugs/paths
│   ├── store.rs      query-semantics store + FileStore port
│   ├── hierarchy.rs  create/move guards
│   ├── runs.rs       transitions, derivation, trust gate, report authorization
│   ├── janitor.rs    two-pass sweep planner (+ ratified decisions)
│   ├── ports.rs      Clock / LeaseTable / MemoryLeases
│   └── core.rs       OmtCore dual-write orchestration
└── tests/
    ├── common/mod.rs  virtual FS, executor, comparison engine (TS harness mirror)
    ├── corpus.rs      the 68-scenario zero-divergence driver
    └── properties.rs  decision-layer property tests
```
