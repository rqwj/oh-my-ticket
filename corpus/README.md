# OMT Behavioral Corpus (U2)

Language-neutral characterization suite that pins the current TypeScript core
(`src/host/core.ts` + `src/host/store.ts`) as the port specification for the
Rust rewrite (plan `docs/plans/2026-08-24-1030-refactor-omt-rust-core-desktop-plan.md`,
unit U2). Each scenario is a plain, self-describing JSON document — there is
NO extensible runner framework; the operation and invariant vocabularies are
closed and documented here.

## Frozen baseline

The scenarios were extracted against, and the error-taxonomy pre-freeze was
landed on, this exact commit:

```
42cc095ab6ef62061f51661e9bd7bd2a1096dffd  feat(contracts): json-schema protocol source with rust/ts codegen (U1)
```

Any behavior divergence between a future Rust leg and these scenarios must be
resolved against this baseline, not against later TS drift.

## Layout

```
corpus/
  README.md            ← this file
  scenarios/*.json     ← 68 scenario documents (one per behavior cluster)
  runner/ts/harness.ts ← plain executor: loads a document, drives a REAL
                         OmtCore/OmtStore/OmtCorePool in a fresh temp home
  runner/ts/run.spec.ts← vitest wrapper (one test per scenario file)
```

Run:

```bash
pnpm exec vitest run corpus/runner/ts/run.spec.ts
```

Current counts: **68 scenarios · 493 operations · 319 invariants**, all green.

## Envelope

```jsonc
{
  "meta": {
    "name": "kebab-case-name",
    "source": ["tests/…spec.ts#…"],   // provenance of the pinned behavior
    "description": "what is pinned",
    "recordEvents": true              // optional: capture item run-events
  },
  "setup": {
    "mask": ["created_at", …],        // optional volatile-field mask override
    "nodes": [ { "type": "epic", "title": "…" } ],   // sequential core.create
    "activeSessionIds": ["s1"],       // liveness passed to the initial open
    "pool": { … }                     // optional workspace-routing mode
  },
  "operations": [ { "op": "<name>", "params": {…}, "label": "alias?" } ],
  "invariants": [ { "expect": "<kind>", … } ]
}
```

### Determinism rules

- **Volatile-field masking.** Wall-clock stamps are replaced with
  `"__MASKED__"` before any comparison. Default mask:
  `created_at`, `updated_at`, `nudged_at`, `started_at`, `finished_at`;
  a scenario may narrow/widen via `setup.mask` (the envelope defines the
  equality domain). Presence checks (`defined` / `notDefined`) still work —
  masking replaces values but keeps keys. This is what makes cross-language
  equality well-defined without a clock abstraction in the TS leg: the Rust
  leg injects a real deterministic clock and produces identical masked
  projections.
- **Errors are data.** Operations never abort the scenario; a throw is stored
  as `{ "error": { "code", "message", "details" } }`. Assertions key on
  `code` + structured `details` only (R5); message text is diagnostic and
  never asserted.
- **Ordering.** Every list the core returns is deterministically ordered
  (`ORDER BY id`, edge `ord`, item `position`); assertions rely on it.

### Operation vocabulary (closed)

| group | ops |
|---|---|
| nodes | `create`, `update`, `move`, `show`, `list`, `tree`, `getNode`, `reindex` |
| files | `readFile`, `writeFile` (hand-edit simulation), `deleteFile` |
| runs | `createRun`, `getRun`, `listRuns`, `runItems`, `getRunItem`, `runItemStateCounts`, `addRunMembers`, `runsOfNode`, `startRun`, `pauseRun`, `resumeRun`, `cancelRun`, `transitionItem`, `retryItem`, `replayItem`, `claimRunItem`, `reportRunItem`, `removeRunItem` |
| continuation | `nudge` (recordItemNudge×count), `stallCheck` (isRunItemStalled), `continuationCandidates`, `executorItems` |
| lifecycle | `seedRun` (explicit seed primitive replacing direct OmtStore writes), `sweep` (janitorSweep with injected session liveness), `reopen` (close [+ `freshDb`] → re-open with given live sessions) |
| routing | `resolveHome`, `openRouted` (pool mode: workspace-local `.omt` wins, global fallback) |

### Invariant vocabulary (closed)

`equals`, `matches` (subset), `contains` (string substring or array element),
`length`, `gte`, `defined`, `notDefined`, `fileContains`, `fileNotContains`,
`itemEvents` ([node_id, fromState, toState] sequence; requires
`meta.recordEvents`). Invariants reference operations by index or `label`;
`path` is a dotted lookup into the result; `valueFrom` compares against
another operation's result at the same `path`.

## Scenario inventory by source

- **tests/core.spec.ts** (full port): `create-*`, `update-*`,
  `ancestor-activation-*`, `move-*`, `reindex-*`, `search-*`, `list-filters`.
- **tests/run-core.spec.ts** (full port): `run-create-*`,
  `run-container-members`, `run-legacy-container-quarantine`,
  `run-state-transitions`, `run-terminal-absolutism`, `item-*`,
  `terminal-derivation-*`, `remove-last-pending-derives`, `stop-on-failure-*`,
  `pause-dispatch-gate`, `retry-*`, `replay-*`, `resume-after-stop-on-failure`,
  `cancel-freezes-items`, `claim-*`, `wedge-force-remove`, `janitor-*`,
  `reindex-preserves-runs`, `add-members-*`, `runs-of-node-links`,
  `report-write-order`, `report-double-write-status`.
- **tests/archived.spec.ts** (behavioral port): `archive-*`.
- **tests/pool.spec.ts** (workspace-routing half only):
  `pool-home-routing`, `pool-routed-isolation`. The per-home instance cache
  and janitor wiring stay permanent TS suites (TS-object semantics).
- **Hook specs, core-visible halves**: `nudge-record-bookkeeping`,
  `continuation-candidate-gating`, `sweep-explicit-outcomes`,
  `trust-gate-awaiting`, `trust-gate-bypasses`, `dakon-open-over-awaiting`,
  `observation-cross-run-broadcast`, `observation-ownership-kept`,
  `observation-open-replays`, `observation-direct-blocked-skipped`.
  Pure delivery/timing assertions (followup/inject texts, backoff timers,
  same-tick merging) remain outside the corpus on purpose.
- **tests/run-store.spec.ts** (portable half): covered via the explicit
  `seedRun` primitive inside `run-legacy-container-quarantine` /
  `reindex-preserves-runs` (RUN counter continuity, config round-trip through
  `run-create-ordering-defaults`). The raw v1→v2→v3 SQLite migration tests
  stay permanent TS suites: they encode storage-format knowledge that U4's
  migration ledger replaces.

## Error taxonomy pre-freeze (assertion surface)

Before freezing, the coarse codes were subdivided so corpus assertions bind
to codes/details. Registry: `schema/problems.schema.json`.

| new code | falls back to | thrown where | details |
|---|---|---|---|
| `ARCHIVED_READONLY` | `CONFLICT` | update seal; archived run member; archived report target | `{ nodeId, operation: 'update'\|'report'\|'run-membership' }` |
| `DUPLICATE_MEMBER` | `INVALID_INPUT` | createRun duplicate member | `{ nodeId }` |
| `INVALID_CONCURRENCY` | `INVALID_INPUT` | createRun concurrency validation | `{ value }` |

Coarse codes stay valid where genuinely generic. Structured `details` were
added across all throw sites: hierarchy violations carry
`{ rule, parentType?, childType?, parentId?, nodeId?, targetParentId? }`;
state gates carry `{ rule, runId, current|runStatus|itemState, required? }`;
NOT_FOUND carries `{ kind: 'node'|'run'|'run-item', id? , runId?, nodeId? }`.
Message text was left byte-identical everywhere (rpc/tools surfaces format
`${code}: ${message}` and some suites match Chinese substrings of messages).

Two behaviors the old message regexes had obscured are now pinned precisely
(see `move-hierarchy-guards.json`):

1. Moving a node under its own descendant is rejected by the **child-type**
   rule first; under the current HIERARCHY matrix the `descendant-cycle`
   branch is unreachable through the public API (defensive depth only).
2. A `failed` report touches only the run item — the ticket keeps whatever
   status it had (claim dispatch does not write the ticket), which the old
   `已归档/in_progress`-adjacent prose could suggest otherwise about.

## Post-cutover fate (plan §success criteria)

After three stable Rust releases this corpus becomes the canonical behavioral
specification and the legacy spec citations in `meta.source` are retired.
