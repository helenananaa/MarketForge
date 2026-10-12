# Pine state-copy and commit encoding optimization — 2026-10-12

The final ABBA shows a clear computation/CPU reduction but only a small and variable overall latency change: Pine evaluation averages 8.084 → 6.003 ms (-25.7%), measured logical cores 0.670 → 0.538 (-19.6%), and enqueue-to-commit-return P95 901.434 → 870.048 ms (-3.5%). Pine-load HTTP place P95 rises 26.638 → 42.071 ms (+57.9%). Therefore this is an efficiency improvement, not a demonstrated broad latency or capacity qualification. Native control is approximately 40 steps/s; its asynchronous order flow varies between runs.

This iteration keeps three source-file changes. Saturated encoding-cache eviction now uses an ordered LRU index instead of scanning all entries per miss. Existing JSON Value payloads serialize directly to raw JSON without parsing the just-emitted text again; enclosing typed mutations retain recovery integer-range validation. Pine evaluation transfers ownership of account/evaluation/provider/bar arrays instead of repeatedly cloning them, and counts integer JSON lengths without allocating per-number strings. Full durable state, message limits and history-prefix checks remain.

The initial shared-template scheduler publication experiment was withdrawn. Its native ABBA fell from 39.756 to 30.143 steps/s (-24.2%), despite publication total wall falling from 2865.584 to 484.057 ms. Final observation lag and RSS also worsened. These negative receipts remain in the JSON; reduced component time was not accepted as an overall win.

Final serial ABBA compares against the prior pipeline candidate: 1000 Pine plus 38 native liquidity bots, three bundled sources with diverse inputs and 10/15/30-second horizons, identical frozen plugins/recipes and 64 longest-period prewarm candles. Each Pine case uses 20s warmup plus 30s measurement at 1000ms cadence. Native control uses 1000 bots at 25ms cadence, 5s warmup plus 30s measurement. Table values are medians of two per-run measurements, not pooled percentiles.

| Metric | Before | Final | Change |
|---|---:|---:|---:|
| Enqueue to commit-return mean (ms) | 402.294 | 393.432 | -2.2% |
| Enqueue to commit-return P95 (ms) | 901.434 | 870.048 | -3.5% |
| FIFO P95 (ms) | 655.985 | 622.952 | -5.0% |
| Start to commit-return P95 (ms) | 297.895 | 339.541 | +14.0% |
| Pine evaluate mean (ms) | 8.084 | 6.003 | -25.7% |
| Pine journal validation/encoding total wall (ms) | 1251.347 | 1213.458 | -3.0% |
| Pine-load HTTP place P95 (ms) | 26.638 | 42.071 | +57.9% |
| Pine-load HTTP cancel P95 (ms) | 23.450 | 18.529 | -21.0% |
| Pine measured logical cores | 0.670 | 0.538 | -19.6% |
| Pine measured RSS (MiB) | 2226.625 | 2288.150 | +2.8% |
| Native-only steps/s | 39.114 | 39.986 | +2.2% |
| Native-only HTTP place P95 (ms) | 33.124 | 23.899 | -27.8% |
| Native-only HTTP cancel P95 (ms) | 37.043 | 30.379 | -18.0% |

The enqueue-to-return metric pairs actual computation starts with successful commit returns over simulation time [first dispatch +20s, +50s). It includes later compute/write waiting, unlike the FIFO-only metric; it does not independently prove per-intent admission. All qualification gates and unmatched-start counts are retained in the raw results. Stage wall-time totals can overlap and must not be added into overall elapsed time.

Isolated cache benchmark: 4500 retained immutable states, two cache-thrashing passes (9000 encodings, 158748780 encoded bytes per run), ABBA. Median 260.846 → 121.637 ms (2.14x). All 4500 payloads are byte-equal; the 64 MiB fragment bound is preserved. The harness extracts the actual before/after Cache implementations and stores their source alongside the result. This is not an end-to-end speedup.

Acceleration checks use the same final executable and Pine workload, 20s warmup plus 30s measurement, one run per tier:

| Target acceleration | Actual steps/s | Bots behind closed bars at cutoff | P95 / max backlog (bars) | Strict cadence / zero backlog |
|---|---:|---:|---:|---|
| 5x | 5.022 | 0 | 0 / 0 | True / True |
| 10x | 9.350 | 0 | 0 / 0 | False / True |
| 20x | 13.850 | 667 | 1 / 2 | False / False |

The ladder retains literal cadence and endpoint backlog failures. A last-bar cutoff lag alone does not establish sustained backlog growth; these short single-tier samples do not qualify maximum capacity or 30–60-minute stability.

Validation: 636 Rust tests pass (15 explicitly ignored diagnostics), default and all-feature Clippy pass with warnings denied, and format checks pass. Twenty Python/Rust/HTTP checks pass, including 576 seeded prefixes across six sources, unchanged fixed trade receipt `48c26aa5e50e9a8417e8ce7de464eb4cff0a511f65288bb686e0d3a15024ecfa`, actual partial fills/cancels and PostgreSQL restart. PostgreSQL uses separate fresh Rust/HTTP databases with fsync, synchronous_commit and full_page_writes on. The task cluster was stopped afterward.

A Windows launcher pipe issue caused the first database creation attempt to run only after the task cluster was stopped; no Rust tests had started. The retry used a hidden detached launcher and passed all checks. Both environment logs are retained; no recovery validation was relaxed.

At the 20x target (13.850x observed), the 30s measurement window records 22.382s cumulative room-lock-held wall time and 18.017s journal validation/encoding wall time, with 7.375 average logical cores and 3628.664 MiB sampled process-tree RSS. These stage times overlap. This points to full-state encoding and the serialized commit lane as remaining limits; it is memory-journal evidence, not PostgreSQL saturation.

Remaining limits: the complete Pine transcript still grows and must be copied/encoded, and durable commits still serialize through the room writer. Default Python-worker and arbitrary-script capacity, long-run memory and PostgreSQL throughput are unqualified. Rust remains opt-in for the existing allowlisted sources. Validation completed before this iteration was committed or pushed; pre-existing staged strategy research files were preserved.

Final binary SHA-256: `9f19880498df41f0086d15bd4c8b391ede02740496e3995b41a89e0fc76b804d`. See [raw metrics, hashes, attempts and qualification gates](2026-10-12-pine-commit-optimization.json).
