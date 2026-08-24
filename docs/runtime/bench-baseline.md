# Runtime benchmark baseline (U5d)

Recorded from the U5d verification suite (`crates/omt-runtime/tests/bench_tier.rs`,
`#[ignore]` tier). Numbers below are the initial baseline captured on the dev
machine; re-run the commands to compare against them. Latency is measured
client-side (full JSON-RPC round-trip over UDS) and includes daemon fsync
costs — this environment has unusually slow fsync (~20–60 ms per durable
mutation), which dominates mutation latency.

## Environment

| item | value |
| --- | --- |
| machine | Apple Silicon (arm64), 16 cores, 64 GB RAM |
| OS | macOS 26.6 |
| build | `cargo test --release -p omt-runtime` |
| date | 2026-08-24 (U5d verification) |

## Reproduce

```sh
export CARGO_HOME=$PWD/.cargo-home
# full soak uses its default of 1800 s; OMT_SOAK_SECONDS=60 is the smoke used here
OMT_SOAK_SECONDS=60 cargo test --release -p omt-runtime --test bench_tier -- --ignored --nocapture --test-threads=1
```

`--test-threads=1` keeps benches from contending with each other; run it when
comparing numbers.

## b1 · 10k-command mixed stress (1:4 mutation:query)

4 concurrent authenticated clients (cli/dsh/mcp/external kinds) issue 10 000
commands total: 2 000 mutations (create + RMW priority update) and 8 000
queries (search/tree/list rotation).

<!-- BENCH-b1 -->

```
commands     10000   mutations  2000   queries  8000
clients          4   failed        0   rmw-conflicts    1
wall         114.93 s
throughput           87.0 cmd/s
overall    p50    40.93 ms · p95    97.68 ms
mutations p50 63.09 / p95 119.60 ms · queries p50 36.22 / p95 88.77 ms

BENCH b1 commands=10000 mutations=2000 queries=8000 failed=0 conflicts=1 wall_ms=114928 throughput_cps=87.0 p50_us=40927 p95_us=97680
```

RMW conflicts are counted separately: losing an update to a concurrent
winner is correct protocol behavior, not a failure.

## b2 · soak

4 clients loop the same 1:4 discipline until the deadline. Assertions:
aggregate ≥20 cmd/s and interactive p95 <100 ms. `OMT_SOAK_SECONDS` overrides
the duration (default 1800 s); the committed baseline used a 60 s smoke.

<!-- BENCH-b2 -->

```
soak_seconds=60
commands      9447   failed     0   rmw-conflicts    3
throughput          157.2 cmd/s (floor 20)
overall    p50    26.75 ms · p95    76.78 ms
interactive p95       55.24 ms (ceiling 100)
mutations n=1942 p50 56.71 / p95 92.66 ms

BENCH b2 soak_seconds=60 commands=9447 failed=0 conflicts=3 throughput_cps=157.2 p95_interactive_us=55244
```

Both thresholds asserted in-test: ≥20 cmd/s aggregate, interactive p95
<100 ms. The committed run is a 60 s smoke (`OMT_SOAK_SECONDS=60`); the full
1800 s default is available on the same command without the env var.

## b3 · 100k retained events → resume 10k delta

100 000 events are bulk-appended through the storage outbox, then served by a
real daemon; a client resumes the newest 10 000 envelopes in pages of 1 000.
Assertion: full delta consumed in <2 s.

<!-- BENCH-b3 -->

```
retained      100000   delta  10000
pages             10 × limit 1000
consumed       10000 envelopes
resume wall      53.8 ms (ceiling 2000)

BENCH b3 retained=100000 delta=10000 pages=10 consumed=10000 resume_ms=53.8
```

## Blocking-gate scale deviation note

The blocking envelope gate
(`crates/omt-runtime/tests/envelope_gates.rs::gate_envelope_multi_client_correctness`)
defaults to the plan's documented **blocking minimum** ratio — 8 homes ×
500 nodes/home × 4 clients × 3 runs×50 items — rather than the full envelope
(8 × 2000 × 4 × 5 × 200). Measured cause: seeding is fsync-bound at roughly
40 creates/s aggregate on this machine (~400 s for the full 16 000-node seed
alone), which cannot meet the plan's 120 s budget. Every dimension remains
env-tunable (`OMT_ENV_HOMES/_NODES/_CLIENTS/_RUNS/_RUN_ITEMS/_ITERS`) and
`OMT_ENV_FULL=1` selects the full envelope on machines with faster disks.
Measured at the default ratio: wall ≈103 s (seed ≈95 s of that), all
correctness assertions green.
