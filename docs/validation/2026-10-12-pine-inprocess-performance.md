# Pine direct Rust experiment — 2026-10-12

The server calls the pinned Rust interpreter and a schema-2 Rust broker adapter on its existing compute pool. This removes Python and framed IPC for six exact bundled scripts; it does not compile Pine to machine code. The 16 external-task permit, trigger logic, order gateway and journal-before-publication contract remain in place.

Enable explicitly with `cargo build --release -p exchange-server --features pine-in-process` and `MARKETFORGE_PINE_IN_PROCESS=1`. It is off by default. Custom/edited sources, legacy mode, auto hedging, exit protection and noninteger overrides retain the existing process path.

Serial ABBA: 1000 Pine + 38 native liquidity bots, identical recipe/plugins and 64 complete initial candles, diverse inputs and 10/15/30-second horizons, 20-second warmup, 30-second measured window, 1000ms cadence. Two runs per version; medians below.

| Metric | Python process path | Direct Rust path |
|---|---:|---:|
| FIFO queue mean | 851.562 ms | 450.988 ms |
| FIFO queue P95 | 2351.516 ms | 1114.770 ms |
| Pine evaluate mean including waits | 31.037 ms | 14.539 ms |
| Closed-bar backlog instances at endpoint | 19.000  | 0.000  |
| Sampled peak process-tree working set | 3268.037 MiB | 2178.016 MiB |
| Server-only peak working set | 1885.484 MiB | 2248.096 MiB |
| Process-tree average logical cores | 1.003  | 0.796  |
| Market step rate | 0.972 /s | 0.976 /s |
| HTTP place P95 under Pine load | 80.430 ms | 27.469 ms |
| HTTP cancel P95 under Pine load | 22.311 ms | 30.963 ms |

Each final candidate run has positive direct-Rust call counts and zero Python worker acquisitions/roundtrips. All 1000 instances initialize. Recipe and initial-history hashes match across all four Pine runs; compiler and other benchmark-server samples are empty. FIFO samples are paired with actual Rust/worker starts; they exclude subsequent compute-thread and affinity-worker waits. Counter wall times overlap and must not be summed. Working set samples include shared pages per process.

Native-only control: 1000 native bots at 25ms, 5-second warmup and 30-second measurement, same ABBA order.

| Metric | Before | After |
|---|---:|---:|
| Market steps/s | 33.626 | 34.623 |
| HTTP place P95 ms | 45.293 | 49.733 |
| HTTP cancel P95 ms | 51.524 | 59.363 |

FIFO P95 decreases 52.6%, evaluation mean decreases 53.2%, and total sampled process-tree peak working set decreases 33.4%. Server-only memory rises because it now owns the sessions. Endpoint backlog counts are 38/0 before and 0/0 after; the median count alone should not imply every baseline run backlogged. The 1000ms market cadence is retained, so unchanged step rate does not measure maximum capacity.

There are regressions in measured tail latency: under Pine load cancel P95 rises from 22.3 to 31.0ms. Native-only place P95 rises from 45.3 to 49.7ms and cancel P95 from 51.5 to 59.4ms. Native per-run step rates span 28.3–39.0/s, place P95 31.9–64.7ms and cancel P95 39.2–77.6ms; these two runs per version do not establish the cause of the changes. No native latency improvement or regression-free production claim is made.

Validation: 628 Rust tests pass, 14 existing ignored; Clippy with warnings denied and the normal feature-off build check pass. Full decision/state differentials cover 576 seeded prefixes of all six scripts plus speculative retry, cache reset, timeouts, fees, partial fills, rational cost and cancel-before-replacement. Three execution modes produce identical 59-step real partial-fill/close receipts (canonical SHA-256 `48c26aa5e50e9a8417e8ce7de464eb4cff0a511f65288bb686e0d3a15024ecfa`). The qualified direct Rust resting-limit strategy also survives a PostgreSQL restart, keeps order references, replays duplicate steps exactly and cancels after recovery; metrics prove no Python execution in that test.

Session reuse requires equality with the host-committed frozen bars/accounts/providers/evaluations and settings. Errors or uncommitted outputs cannot authorize advancement. Recovery reconstructs from the same persisted schema 2. Rust cooperative deadlines preserve deterministic budgets; unused runtime snapshot parsing does not change the host JSON parser.

These bounded runs establish the measured result, not 30–60-minute stability or production capacity. Load measurements use the memory journal; PostgreSQL recovery is a separate correctness result. Async trades can differ due to scheduling, while fixed inputs have exact receipts. The raw comparison, source hashes, all final runs and validation receipts are linked in the adjacent JSON report. An earlier interrupted pilot is retained separately and excluded from the final aggregate.
