# corpus/runner/rs — Rust behavioral-corpus leg

Rust binding layer that executes the frozen scenario documents
(`corpus/scenarios/*.json`) against the pure domain crate `omt-domain`
using in-memory ports (virtual filesystem, fixed clock, lease table).

The runner code lives in `crates/omt-domain/tests/` because the workspace
`Cargo.toml` declares `members = ["crates/*"]`; this directory documents the
entry point and holds the convenience wrapper.

## Run

```bash
./corpus/runner/rs/run-corpus.sh
```

which is equivalent to:

```bash
CARGO_HOME=$PWD/.cargo-home cargo test -p omt-domain --test corpus -- --nocapture
```

`CARGO_HOME=$PWD/.cargo-home` is REQUIRED by this repository's offline-cargo
convention.

## What you should see

* one `PASS <file> — <name> (<n> checks)` line per scenario, in filename order;
* a final summary line:

  ```
  corpus: 68 scenarios, 319 invariant checks, 0 failed scenario file(s)
  ```

Any divergence FAILS the driver. Set `OMT_DEBUG=1` to dump every stored
operation result (volatile fields masked) for failing scenarios:

```bash
OMT_DEBUG=1 ./corpus/runner/rs/run-corpus.sh
```

## Files

| Path | Role |
|---|---|
| `crates/omt-domain/tests/corpus.rs` | driver: loads every sorted `*.json`, runs it, asserts zero failures and ≥40-scenario inventory |
| `crates/omt-domain/tests/common/mod.rs` | virtual FS + per-home cores + closed op vocabulary + comparison engine (masking, paths, deep-equal/subset-match) mirroring `corpus/runner/ts/harness.ts` |
| `crates/omt-domain/tests/properties.rs` | property tests over transitions/concurrency/trust-gate/lease authorization + LCG-randomized sweep-equivalence vs the session-membership oracle |

The scenario JSON documents are FROZEN — never edit them to make a leg pass.
