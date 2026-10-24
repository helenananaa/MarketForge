# Research optimizations — 2026-10-23

Branch: `feat/endogenous-market-tuning`; baseline: `b494d636fbe45ee3efd0480eec0a560857b2f4a1`. All changes remain uncommitted.

## Decisions

- Retained: one bounded persistent Rayon pool in the research runner, preserving FIFO plugin dispatch, immutable observations, input-order results and serial gateway submission. Rayon is a dev dependency; the production scheduler is unchanged.
- Rejected: candidate account filtering. Both versions preserved receipts, but the new structural diff and fallback work cost more than the reads they removed. All candidate APIs and their runner path were removed. Patches and tests remain under the evidence directory.
- Rejected: Pine committed-prefix certificates. Long-history ABBA wall means were about 41.987 s vs 41.669 s (0.76%); plugin wall means were 24.6 s vs 24.65 s. This is not a convincing gain. Production Python code is byte-identical to the baseline.
- Retained: armed protections return after updating the scan cursor and trailing watermark when there is no exit reason. Triggered exits, retries, caps, cancellation and matching paths retain their prior behavior.

## Measurement boundaries

All comparisons use frozen release binaries, fixed input recipes, the same runtime and Python executable, and sequential runs. The first pool experiment using parallel chunks was rejected because instance-affine worker contention offset the gains; retained dispatch uses the original FIFO behavior.

No build or test ran alongside the reported timing batches. An accidentally overlapping interrupted candidate run (`screen-runs`) is excluded. This shared Windows host is not an isolated performance machine: unrelated phase times and a no-Pine control also moved. Do not attribute every wall-time fluctuation to the patch.
`wall_seconds` includes the complete subprocess. `screen`, `plugins_wall`, `submit`, etc. are phase measurements; `plugins_busy` sums worker times and must not be added to wall time. The runner rounds ecosystem phase values to 0.1 s.
The isolated protection case has 128 armed positions and 5,000 scans. `uncapped` has one book level and no notional cap; `capped` has 64 book levels and a notional cap. `uncapped_stable` repeats the uncapped case for 50,000 scans because the original sub-second run was sensitive to startup and host noise. Its timed loop does not advance the clock or execute orders. The whole actor JSON before/after scans must be identical. Its improvement is not an ecosystem throughput estimate.

## Workloads and receipt checks

| Case | Population | Steps / clock seconds | Wake mode |
|---|---|---|---|
| scale | 300 built-ins + 3,000 Pine | 1,800 / 1 | events |
| related | 300 built-ins + 241 Pine, peer-market inputs | 600 / 1 | events |
| poll | 300 built-ins + 241 Pine | 600 / 1 | 60-second poll |
| live | 300 built-ins, no Pine (synthetic control) | 600 / 1 | default |
| long | 300 built-ins + 24 Pine, long retained history | 1,500 / 3 | events |

Each ecosystem comparison hashes the entire emitted JSON byte stream: trades and their order, fills, rejections, bot state, account assets and sampled books. Step count and clock increment must match the requested completion boundary. Hashes are also checked across experiment batches, not only within a pair.

The frozen manifests replace the unavailable `python3` Windows stub with `E:\projects\MarketForge\.venv\Scripts\python.exe` in both command paths, retaining 16 workers. No trading configuration is changed. Runtime: `0.3.1+mf1`; hashes and frozen plugin files are recorded in the companion JSON.

## Measurements

### rejected_dispatch_chunks

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| scale | 54.924, 58.885 | 55.877, 58.120 | -0.17% reduction | identical |
| related | 7.120 | 6.455 | +9.34% reduction | identical |
| poll | 8.277 | 7.582 | +8.39% reduction | identical |
| live | 4.941 | 4.550 | +7.91% reduction | identical |
| long | 57.003 | 56.249 | +1.32% reduction | identical |

Raw evidence: `.local\optimization-20261023\pool-runs`; exact phase times and hashes are in the companion JSON.

### retained_persistent_fifo_pool

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| scale | 66.556, 62.937 | 63.379, 51.637 | +11.18% reduction | identical |
| related | 9.303 | 7.391 | +20.55% reduction | identical |
| poll | 8.950 | 8.979 | -0.33% reduction | identical |
| live | 5.688 | 4.501 | +20.87% reduction | identical |
| long | 51.179 | 53.497 | -4.53% reduction | identical |

Raw evidence: `.local\optimization-20261023\pool-queue-runs`; exact phase times and hashes are in the companion JSON.

scale / `screen` seconds: [5.5, 5.4] → [2.9, 2.3]; mean reduction 52.29%.

### rejected_private_candidates_v1

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| scale | 59.288, 62.147 | 66.328, 68.837 | -11.31% reduction | identical |
| related | 7.923 | 8.299 | -4.75% reduction | identical |
| poll | 7.358 | 8.008 | -8.83% reduction | identical |
| live | 5.708 | 5.267 | +7.73% reduction | identical |
| long | 58.711 | 61.770 | -5.21% reduction | identical |

Raw evidence: `.local\optimization-20261023\screen-runs-clean`; exact phase times and hashes are in the companion JSON.

### rejected_private_candidates_v2

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| scale | 47.666, 54.617 | 56.693, 54.306 | -8.52% reduction | identical |
| related | 6.653 | 6.955 | -4.55% reduction | identical |
| poll | 6.995 | 6.202 | +11.34% reduction | identical |
| live | 4.389 | 4.681 | -6.66% reduction | identical |
| long | 42.173 | 42.955 | -1.85% reduction | identical |

Raw evidence: `.local\optimization-20261023\screen-v2-runs`; exact phase times and hashes are in the companion JSON.

### rejected_committed_prefix_certificate

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| long | 41.714, 42.261 | 41.181, 42.157 | +0.76% reduction | identical |
| scale | 43.242 | 42.842 | +0.93% reduction | identical |
| related | 5.411 | 5.490 | -1.47% reduction | identical |
| poll | 5.902 | 5.603 | +5.07% reduction | identical |
| live | 3.528 | 3.283 | +6.96% reduction | identical |

Raw evidence: `.local\optimization-20261023\prefix-runs`; exact phase times and hashes are in the companion JSON.

### retained_armed_protection_early_return

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| uncapped | 0.245, 0.307 | 0.564, 0.208 | -40.09% reduction | identical |
| capped | 2.715, 2.658 | 0.226, 0.212 | +91.86% reduction | identical |

Raw evidence: `.local\optimization-20261023\protection-runs`; exact phase times and hashes are in the companion JSON.

uncapped / `protections` seconds: [0.182631, 0.243567] → [0.164066, 0.163413]; mean reduction 23.16%.

capped / `protections` seconds: [2.660074, 2.611555] → [0.169697, 0.161152]; mean reduction 93.72%.

### retained_armed_protection_uncapped_repeat

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| uncapped_stable | 1.788, 1.734 | 1.613, 1.593 | +8.94% reduction | identical |

Raw evidence: `.local\optimization-20261023\protection-repeat-runs`; exact phase times and hashes are in the companion JSON.

uncapped_stable / `protections` seconds: [1.725824, 1.692795] → [1.56485, 1.553321]; mean reduction 8.79%.

### final_vs_original

| Case | Before (s) | After (s) | Wall change | Receipt |
|---|---:|---:|---:|---|
| scale | 43.172, 43.718 | 41.943, 41.440 | +4.04% reduction | identical |
| long | 40.473, 40.233 | 40.806, 39.711 | +0.23% reduction | identical |
| related | 5.155 | 4.715 | +8.54% reduction | identical |
| poll | 5.231 | 5.070 | +3.07% reduction | identical |
| live | 3.231 | 3.323 | -2.85% reduction | identical |

Raw evidence: `.local\optimization-20261023\final-runs`; exact phase times and hashes are in the companion JSON.

scale / `screen` seconds: [3.3, 3.3] → [1.9, 1.9]; mean reduction 42.42%.

## Validation and reproduction

Core: 444 passed, 12 pre-existing ignored; server: 339 passed, 9 pre-existing ignored; Python: 75 passed. The existing 1,000-step protection reference comparison passed. Clippy and diff whitespace checks were run against the retained source. See captured result lines in the companion JSON. The rejected prefix experiment separately passed 36 tests, including real worker deltas, full requests, history correction, rollback and restart.

The first server run failed 12 process-plugin tests because Windows resolved python3 to the Store stub (exit 9009). The core suite passed in that same run. The server suite then passed with a task-local copy of the existing Anaconda Python executable named python3.exe and child-shell-only PATH/PYTHONHOME settings. No global environment, plugin manifest or product code was changed for this rerun. Both logs are preserved.

Two stale test fixtures were repaired: the runtime version allowlist now matches the adapter, and the diagnostics fake session returns the current result structure. The initial failing Python output is retained as `python-stale-fixture-failure.log`.

Compressed exact recipe inputs are under `2026-10-23-research-optimizations-inputs/`; decompress before running. Input hashes are for uncompressed bytes. Frozen binaries, rejected patches, package copies, plan files, raw receipts and complete logs remain in `.local/optimization-20261023/`.

Run a recorded plan with the existing venv interpreter:

```powershell
& E:\projects\MarketForge\.venv\Scripts\python.exe scripts\benchmark_research_optimizations.py .local\optimization-20261023\final-plan.json .local\optimization-20261023\fresh-final-runs
```

The output directory must be new. Plans record absolute frozen executable/package paths, recipe hashes, step counts and environment variables. On another machine, update those paths and use the same qualified runtime. SHA equality verifies the tested scenarios; it is not a proof over every possible market state or a live server qualification.
