# Pine qualification metadata optimization — 2026-10-23

Branch: `feat/endogenous-market-tuning`. This round starts from the retained result of [round 1](2026-10-23-research-optimizations.md), not from clean HEAD. The same frozen `final.exe` is used before and after. Changes remain uncommitted.

## Retained change

`incremental.qualified()` uses one file metadata query to check both regular-file status and modification time. It also reuses the qualification-file Path. Previously each evaluation constructed the Path and called both `is_file()` and `stat()`.

The modification time is still checked on every evaluation. Missing/non-file inputs remain unqualified; metadata errors retain the `Path.is_file()` fallback/error behavior. Qualification changes, source hashes, incremental-session admission, replay, committed-history validation, decisions, order submission, risk checks and receipt serialization retain their existing rules.

A sampled profile of 554 decisions identified 1,660 file-stat calls taking 0.456 s out of 1.698 s of profiled calls. This is diagnostic elapsed time with profiler overhead, not an ecosystem throughput claim. The independent metadata microbenchmark uses the same existing file, source and Python process for both implementations, two ABBA blocks, 5,000 queries per measurement. All 40,000 qualification results are true.

Metadata-only mean: **0.592149 s → 0.279888 s (52.73% less time)**. This percentage applies only to the repeated qualification lookup.

## Full research comparison

The fixed workloads and exact compressed recipe inputs are those recorded in round 1. `scale` is 3,000 Pine + 300 built-in bots; `long` is 24 Pine + 300 built-ins, 1,500 steps with retained history; `related` exercises peer-market input, `poll` uses polling, and `live` is a synthetic no-Pine control. The two large cases use ABBA; the other cases use one before/after pair.

Runs are sequential with no simultaneous builds or tests. The Windows host is shared and phase times vary; figures below are observed results, not confidence-bounded capacity estimates. `plugins_busy` is summed across callers and is not added to wall time. Diagnostic runs are excluded from performance comparisons.

| Case | Before runs (s) | After runs (s) | Mean before → after (s) | Wall reduction | Complete receipt |
|---|---:|---:|---:|---:|---|
| scale | 46.864, 48.187 | 45.218, 43.680 | 47.526 → 44.449 | +6.47% | byte-identical |
| long | 41.278, 37.452 | 38.492, 36.958 | 39.365 → 37.725 | +4.17% | byte-identical |
| related | 4.448 | 4.369 | 4.448 → 4.369 | +1.76% | byte-identical |
| poll | 5.080 | 5.120 | 5.080 → 5.120 | -0.79% | byte-identical |
| live | 3.172 | 3.213 | 3.172 → 3.213 | -1.29% | byte-identical |

| Case | Plugin wall before → after (s) | Submit before → after (s) |
|---|---:|---:|
| scale | 24.30 → 22.15 | 9.05 → 8.55 |
| long | 23.15 → 21.60 | 7.80 → 7.80 |
| related | 1.30 → 1.20 | 1.80 → 1.80 |
| poll | 1.20 → 1.10 | 2.50 → 2.50 |
| live | 0.00 → 0.00 | 2.20 → 2.20 |

## Dispatch experiment not retained

A separate experiment used 32 research caller threads with the existing 16 Python worker processes. No production source change was made for this experiment. It retained receipt equality, but the long-history plugin phase did not improve and unrelated submission timings moved substantially. The default remains unchanged.

| Case | Baseline runs (s) | 32-caller runs (s) | Plugin mean before → after (s) |
|---|---:|---:|---:|
| scale | 57.041, 52.621 | 48.362, 42.999 | 25.05 → 20.70 |
| long | 49.617, 47.296 | 41.526, 54.647 | 27.30 → 27.30 |

## Correctness and provenance

- 27 complete research runs, including diagnostic/dispatch runs, match the prior round's expected full receipt SHA-256 for their case. The optimized before/after matrix has 14 runs across all five cases. Step and clock increments are checked. Hashes cover the complete emitted JSON, including trade order, fills, rejections, assets, bot state and sampled books.
- Python: **76 passed**, including new qualification-file missing/directory/change/revocation/permission tests and existing qualified-source replay, history edits, worker restart, state delta and bounded-response tests. Both Pine and incremental runtime requirements are enabled.
- Server library: **339 passed, 9 pre-existing ignored**. The child test shell uses the existing local Python executable alias; no system Python/environment configuration changed.
- This round changes two Python source/test files. All source hashes from the retained round-1 baseline were rechecked and remain unchanged. The candidate frozen `incremental.py` matches the tested worktree source. All other frozen plugin files are byte-identical. Runtime remains `0.3.1+mf1` with its hash in the companion JSON.
- No Rust/core trading source changed this round. Round-1 Rust core and clippy validation is recorded in its report; it is not presented as a new run. `git diff --check` passed.
- Evidence directory: `.local/optimization2-20261023/`. The companion JSON retains plans, raw per-run measurements, hashes, diagnostic summaries and microbenchmark samples. Frozen binaries/packages and raw receipts/logs remain in the local evidence directories. Inputs are retained under `docs/validation/2026-10-23-research-optimizations-inputs/`.
- These are finite-capital synthetic research runs and regression checks. They do not establish live-market capacity or durable production-server performance.
