# Bot computation/commit pipeline — 2026-10-12

This iteration improves FIFO admission, but does **not** establish an end-to-end performance win. In the final ABBA, enqueue-to-commit-return P95 changes by +3.9%, native median step rate by -5.4%, and Pine-load HTTP place P95 by +53.6%. Correctness checks pass; these results do not establish production performance qualification.

The scheduler now refills independent external decisions while its single ordered writer commits. A bot remains in-flight until commit completion; immutable market and scheduler snapshots publish together after durable installation. External refill stops when the ready queue reaches 256; already-active work may still finish. Each writer batch contains at most 64 decisions.

Spot event subscriptions narrow candidates by market, candle boundary, changed account and explicit wake deadline. Exact append proofs cover old history prefixes; restores/corrections, perpetual risk, related markets and unsupported inputs fall back to full screening. Existing event fingerprints and exact predecessor/epoch checks remain authoritative. Native bots retain per-tick selection.

Final serial ABBA: 1000 Pine + 38 native liquidity bots, three bundled sources with diverse inputs and 10/15/30-second horizons, identical frozen plugins/recipes and 64 longest-period prewarm candles (192/128/64 bars). Each run uses 20s warmup + 30s measurement at 1000ms cadence. Native control: 1000 bots, 25ms cadence, 5s warmup + 30s measurement. Values are medians of two runs per version, not pooled request percentiles.

| Metric | Baseline | Final | Change |
|---|---:|---:|---:|
| Pine FIFO mean ms | 420.885 | 225.420 | -46.4% |
| Pine FIFO P95 ms | 1039.376 | 756.831 | -27.2% |
| Enqueue to commit-return mean ms | 458.982 | 470.930 | +2.6% |
| Enqueue to commit-return P95 ms | 1067.877 | 1109.251 | +3.9% |
| Pine prefilter total wall ms | 9039.765 | 1451.392 | -83.9% |
| Pine evaluate mean ms | 8.096 | 8.241 | +1.8% |
| Pine-load HTTP place P95 ms | 20.745 | 31.859 | +53.6% |
| Pine-load HTTP cancel P95 ms | 25.627 | 19.512 | -23.9% |
| Pine measured logical cores | 0.652 | 0.728 | +11.7% |
| Pine measured RSS MiB | 2166.906 | 2265.402 | +4.5% |
| Native-only steps/s | 39.217 | 37.116 | -5.4% |
| Native scheduler publication wall ms | 189.464 | 2655.351 | +1301.5% |
| Native-only HTTP place P95 ms | 40.656 | 44.183 | +8.7% |
| Native-only HTTP cancel P95 ms | 42.976 | 45.327 | +5.5% |

FIFO pairs actual Rust starts with enqueue/dispatch receipts over simulation time [first dispatch + 20s, +50s). It excludes subsequent compute/write waiting. The supplementary enqueue-to-commit-return metric includes that waiting, but a successful batch return is not independent proof that each individual intent was admitted. It is not bar-close-to-durable-order latency. Nested stage sums overlap; do not sum them into wall-clock duration.

The first attempt is retained in the JSON. It reduced Pine FIFO P95 by 41% but lowered native median throughput by 6.9%; publication consumed 2.5–5.5s across each native run. The final immutable vector stores Arc agent elements, so changing one entry shares unchanged neighbors instead of copying their full templates/state. A unit test verifies these identities and frozen predecessors.

Acceleration ladder on the final binary (same workload, 20s warmup + 30s measurement; one run per tier; one simulation step is one second):

| Requested speed | Actual steps/s | Bots behind final closed bars | Backlog P95 / max bars | Strict cadence / zero backlog |
|---|---:|---:|---:|---|
| 5x | 4.985 | 126 | 1 / 1 | False / False |
| 10x | 8.776 | 491 | 2 / 2 | False / False |
| 20x | 12.951 | 733 | 1 / 1 | False / False |
| 40x | 9.764 | 676 | 2 / 2 | False / False |

Endpoint backlog is measured immediately around pause, not after an unlimited drain. One last-bar lag can reflect the cutoff phase; these short runs do not establish whether backlog grows indefinitely. Failed cadence and backlog gates are preserved. No 20x/40x sustainable Pine capacity or maximum bot count is qualified.

Validation: 634 Rust tests pass (15 explicitly ignored diagnostics/benchmarks), both default and all-feature Clippy pass with warnings denied. Twenty Python/Rust/HTTP checks pass on the final executable, including 576 seeded prefixes across six sources, real partial fills/cancels, and PostgreSQL restart. The fixed 59-step replay/incremental/direct trade receipt remains SHA-256 `48c26aa5e50e9a8417e8ce7de464eb4cff0a511f65288bb686e0d3a15024ecfa`. The new gated-journal test holds a durable write, observes all 48 independent calculations finish exactly once without publishing speculative state, then checks exact recovery.

One retained rerun failed because builtins-only Rust recovery tests encountered Pine rooms left by the HTTP suite in a shared test database. Separate fresh Rust and HTTP databases resolved it; application recovery rules were not changed. PostgreSQL used fsync, synchronous_commit and full_page_writes on.

This is a memory-journal performance comparison and a separate PostgreSQL correctness check. The Rust backend remains opt-in and limited to its existing qualified sources. History bounds, generic session limits and full durable Pine transcript encoding remain; long-run memory, broad script compatibility, process-worker throughput and PostgreSQL capacity are unqualified. The next substantial optimization is incremental durable strategy state and less repeated history/state encoding, with recovery equivalence as a gate.

Baseline commit: `e3e6b199bd6ab0fef827d0010a1ec3c8e519661b`. Final measured binary: `a3fa070e9d3db449a84a091dc9c401cc6c6e8c15de4e6e272e64e370c4695499`. Exact sources, binary/plugin hashes, all first/final attempts, traces and validation logs are linked from [the JSON report](2026-10-12-bot-pipeline-performance.json). Validation completed before this iteration was committed or pushed.
