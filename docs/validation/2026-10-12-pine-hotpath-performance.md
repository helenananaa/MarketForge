# Pine hot-path comparison — 2026-10-12

Both versions run the qualified in-process Rust interpreter. The candidate borrows the request, moves response state, counts exact wire bounds without full text buffers, caches complete stamps inside immutable waves, and releases the shared history mutex before hashing private event/receipt buffers.

Final serial ABBA: 1000 Pine + 38 native liquidity bots, 64 complete prewarm candles, identical frozen recipes/plugins/seeds, diverse inputs and 10/15/30s horizons, 20s warmup + 30s measurement at 1000ms cadence. Separate native ABBA: 1000 native bots, 25ms cadence, 5s warmup + 30s measurement. Medians of two runs per version; all attempted runs retained.

| Metric | Previous Rust path | Final candidate | Change |
|---|---:|---:|---:|
| FIFO queue mean ms | 513.960 | 483.568 | -5.9% |
| FIFO queue P95 ms | 1332.417 | 1234.544 | -7.3% |
| Pine-load HTTP place P95 ms | 27.354 | 20.605 | -24.7% |
| Pine-load HTTP cancel P95 ms | 23.796 | 27.790 | +16.8% |
| Pine evaluate mean ms | 15.868 | 8.310 | -47.6% |
| Prefilter mean ms | 0.356 | 0.294 | -17.4% |
| Prefilter total wall ms | 11262.533 | 9447.746 | -16.1% |
| Journal validation mean ms | 9.471 | 6.987 | -26.2% |
| Native-only steps/s | 33.875 | 33.772 | -0.3% |
| Native-only HTTP place P95 ms | 44.802 | 56.405 | +25.9% |
| Native-only HTTP cancel P95 ms | 61.519 | 57.963 | -5.8% |

FIFO timing measures enqueue-to-dispatch for calls paired with actual Rust starts, over simulation time [first dispatch + 20s, +50s); it excludes later compute-thread waiting. Phase deltas cover warmup-end through final quiescence and overlap. Smaller nested sums are not automatically an end-to-end speedup. Request-clone timing now includes qualification and is not a pure copy-only metric.

The initial borrowed-request/cache attempt reduced decision cost but kept FIFO P95 essentially flat (1211.5 -> 1214.4ms). It is retained at `.local/pine-hotpath-20261012/phase1-performance.json`; the final comparison adds immutable shared buffers and shorter history locking. Neither attempt is silently discarded.

Native order span results:

| Run | Room-wait P95 ms | Apply P95 ms | Journal P95 ms |
|---|---:|---:|---:|
| native-01-before | 11.372 | 2.148 | 0.362 |
| native-02-after | 48.914 | 2.782 | 0.336 |
| native-03-after | 17.785 | 0.661 | 0.263 |
| native-04-before | 38.641 | 3.053 | 0.324 |

Order room-wait spans include the preceding venue attempt/fallback and room admission. They show waiting before execution is much larger than apply in these sampled requests; they do not isolate a single mutex or explain every endpoint P95 fluctuation. Samples include warmup. Gateway apply includes validation, clearing and candidate work, beyond primitive book operations. No native latency improvement or absence of regressions is inferred from two rounds alone.

Validation: 630 Rust tests pass, 14 existing ignored; Clippy with warnings denied and feature-off check pass. Six Python/Rust differential tests cover 576 seeded prefixes across six sources, full states, uncommitted retry, timeout, exact 4MiB request/1MiB ASCII response bounds, rational costs, partial fills, fees, and cancel confirmation. Frozen-wave tests cover 128 input combinations, concurrent private-history hashing, new versions, full-prefix correction, fee and completeness changes. Replay/incremental/direct modes retain identical 59-step real trade receipts, SHA-256 `48c26aa5e50e9a8417e8ce7de464eb4cff0a511f65288bb686e0d3a15024ecfa`. Qualified direct Rust limit orders, references, duplicate responses and cancel confirmation survive PostgreSQL restart with fsync and synchronous_commit on.

Trading/recovery invariants: no changed trigger digest bytes, eligibility rules, action/state limits, exact history-prefix comparisons, speculative-session authorization, protocol schema, gateway order execution, journal-before-publication, or default enablement. Buffers/caches live only in frozen observation waves and are reconstructed after restart.

Remaining cost must be read from the final phase counters: trigger screening/history preparation and scheduler journal validation still compete with FIFO admission. One realtime owner loop consumes screened/ready results, awaits durable commits, then refills the FIFO; shortening calculation does not remove that serialized path. Rust append time includes runtime/context/intent/state work and is not a pure interpreter CPU measurement.

Both enabled and default Clippy pass. A final feature-off-only typing repair uses Infallible for the unavailable direct path. The rebuilt full binary hash differs, while .text/.data/.pdata/.reloc are byte-identical to the measured binary; .rdata differs. Numbers above belong to the preserved measured A979 binary. Rebuild identity and section hashes are retained separately in the JSON; no measurement is relabeled.

The load comparison uses the memory journal; PostgreSQL restart establishes correctness separately. Async trades may differ because scheduling differs. Short windows do not qualify maximum capacity, long-duration stability or production. Some literal cadence / native 40-step target checks remain false and are retained in raw reports.

Raw final cases, hashes, frozen plugin inventories, stage profiles, correctness receipts and the first attempt are referenced in [the JSON report](2026-10-12-pine-hotpath-performance.json). No commit or push was performed.
