# Rust Pine exit adapter: qualified single-ID cohort

## Result and decision

Retain an opt-in Rust broker implementation for **11 exact source hashes with one
fixed exit ID**, covering **138** instances in the unchanged 3,300-bot recipe.
Pine direct-route coverage is now **212/241 scripts and 2,635/3,000 instances**.
The original scripts, parameters, gateway, risk checks and matching rules remain
the reference. Python `broker.py` was not modified. The existing
`MARKETFORGE_PINE_IN_PROCESS=1` switch is still required; this is not a default-on
or production-capacity qualification.

The implementation preserves exit installation, absolute/relative price rounding,
partial quantities, entry ownership, triggered-protection latching, rejected
receipt retries, cancellation ownership checks, trailing activation, shared
reduction reservations and between-bar wake levels. New-bar `last_actions` now
includes the completed replacement/exit action list, matching Python's list alias.
Execution continues in schema 4 beyond 128 bars; no history-length fallback was
introduced. The replay/size/timeout limits documented in `PINE_BOTS.md` still apply.

The **plugin stage improves clearly on the scale workload**. Overall wall time
does not improve uniformly: the long-history mean is slower and host contention
is substantial. Keep this a bounded experimental admission expansion, not a claim
that removing Python always speeds up the whole exchange.

Machine-readable results, hashes and every sample: [JSON](2026-10-24-pine-exits.json).
Reproduction inputs and the incremental patch against the saved dirty baseline:
[input archive](2026-10-24-pine-exits-inputs.tar.gz).
Raw outputs remain in `.local/exits-20261024/`.

## Before/after research runs

Baseline is the previous round's **retained** journal/source-wave implementation,
copied before editing to `baseline.exe`. It does not contain the rejected archive
encoding cache. Both sides use the same frozen Python package, source/configuration,
recipe, seeds, clock, gateway and receipt projection. Release builds use
`pine-in-process,bench-execution-timing`.

Scale and long use three paired runs in AB/BA/AB order. Other cases use one pair.
All 18 runs completed and matched the frozen economic receipt hash, including
orders, fills, cancels, rejections, broker state and wake metadata. Only the existing
enumerated execution-storage/transport fields are projected across Python schema 2
and Rust schema 4; trading fields are not removed. There were no compiler-overlap
samples or sampler errors.

| Workload | Before wall s | After wall s | Change | Pairs |
|---|---:|---:|---:|---:|
| 3,300 bots, 1,800 steps × 1 s | 39.634 | 38.618 | -2.6% | 3 |
| Long history, 1,500 steps × 3 s | 43.631 | 45.269 | **+3.8%** | 3 |
| Related-market control, 600 steps | 8.150 | 6.358 | -22.0% | 1 |
| Poll control, 600 steps | 6.898 | 6.353 | -7.9% | 1 |
| No-Pine control, 600 steps | 5.517 | 5.447 | -1.3% | 1 |

The scale plugin wall stage changes **11.200 → 9.633 s (-14.0%)**; all three
pairs improve that stage. Sampled process-tree peak RSS averages
**2,343.3 → 2,275.1 MiB (-2.9%)**. Scale wall samples are
33.833/43.937/41.130 s before and 31.630/40.060/44.163 s after; the third pair is
slower overall. Unmodified native and gateway stages vary substantially too.
The host was concurrently running emulator/browser workloads; no unrelated
process was stopped, and no unfavorable sample was discarded. These runs do not
establish a statistically stable whole-run gain or universal non-regression.

The long recipe migrates only `acadb920444d17a1.pine`; four of its five previous
Python scripts remain on Python. Its plugin-stage mean is 21.033 → 21.767 s.
The single-pair controls are checks for gross changes, not independently qualified
speedup claims.

## HTTP service probe

Same **212 Pine sources plus 300 native agents** on both sides; one instance per
selected source, one-second bars, 5 s warmup, 45 s measurement, 250 ms clock
interval, in-memory journal, graceful trace shutdown. Run order: before, after,
after, before. This is a 512-agent service probe, not a 3,300-agent HTTP test.

| Metric | Before runs | After runs | Change of run means |
|---|---:|---:|---:|
| Accepted strategy bar-close → durable-submit P95, ms | 204.077 / 180.655 | 181.723 / 173.322 | **-7.7%** |
| Sampled process-tree peak RSS, MiB | 886.594 / 876.438 | 673.160 / 657.617 | **-24.5%** |

P95 numbers are per-run percentiles; their mean is not a pooled percentile.
The service cohort becomes entirely Rust for Pine, explaining the larger memory
change than the full recipe, which still contains 365 Python Pine instances.

All four runs have complete generated-action terminal receipts and decision
outcomes, no trace/harness/bot errors, no compiler overlap, and acknowledged
graceful shutdown. However, **neither candidate run passes the stricter durable
receipt qualification flag**: 4 and 12 generated actions in the measured bar
cohort belong to decisions discarded after the measurement window, when pause
changes the epoch (`epoch_changed_in_ready_queue`). They were not submitted or
silently lost. Both baseline runs pass that flag. Cutoff/post-pause backlog and
skipped-decision gates are not all green; this is not sustained zero-backlog or
all-generated-actions-submitted qualification. The full flags, reasons and wall
timestamps are retained in the companion JSON. Deterministic semantic equivalence
is established separately below.

## Semantic and recovery gates

- **393 Rust tests passed**, 12 explicitly ignored; **43 Python tests passed**.
- **217 frozen Python-oracle vectors** cover exits, grid/quantity boundaries,
  long/short/flat positions, missing trades, shared reduction budgets, cancellation,
  replacement, pending/rejected retries, trigger latching and trailing wakes.
- **212 scripts / 6,795 comparisons passed**, through 512 bars, with cold recovery,
  speculative retries and Python-state migration at bar 129. There are 29 expected
  interpreter-error cases: three original scripts reject a nonnumeric order value
  under the synthetic held-position input. Rust rejects the same input in cached
  and cold sessions; only the typed error wrapper is normalized, not its message.
- Real gateway recovery uses the 11 new scripts plus 300 original native agents,
  210 steps, and new OS processes at steps 70/140. Python-only, continuous Rust,
  restarted Rust, and Python→Rust→restarted Rust have **identical 70,814-line command
  audit files** and identical economic receipts. Rust's entire final durable state
  is also exactly equal after OS restart.

Audit SHA256: `993187eb2e34b012e8c520764dba83a061bd1180c0403769adc09df1842ead50`.
Economic receipt SHA256:
`b36ba57086a09cb307acf1aafe3c6a91f7f5a86a064843153c6385e304405d54`.
Each direct segment executes all 11 sources (770 calls per 70-step segment), with
no history fallback. Real Pine gateway actions include 52 protected orders and 29
protection installations, of which 26 are accepted and 3 rejected. Seven of the
11 scripts submit actions in this short recovery fixture; the other four do not.
Synthetic vectors and the longer research runs supplement this bounded coverage;
it does not prove every possible strategy branch. No PostgreSQL durability or
production crash/capacity qualification was performed.

## Rejected broader admission and remaining bottleneck

The initial 32 exit-source candidates include four mixed conditional-entry/order
scripts; another lacks independent incremental qualification. A 27-source candidate
passes the bounded response differential but **fails real gateway equality**.
At market time 52,000 ms, `660e65aadf0a9b74.pine` (`pine-perp-50`) changes the order
of a protection installation and a triggered reduction. Its Python resident state
retains dictionary insertion order; Rust restored JSON maps use key order. The
candidate records 14 `InvalidPositionProtection` rejections against 13 in Python,
although final trade counts happen to match. Therefore equal trade counts alone
are insufficient.

That candidate is rejected. All multi-exit-ID scripts are excluded, not merely the
one exposing this fixture. The retained manifest has 11 independently qualified
exact hashes with one fixed literal exit ID, and nonempty restored exit state is
accepted only for that manifest with exactly one specification. Regression tests
keep the rejected script and malformed/multiple specifications outside admission.
Python's order behavior is not rewritten to make the comparison pass.

The next prerequisite for wider exit migration is explicit, durable ordering of
exit specifications across resident-cache reuse, eviction, cold restore and
Python/Rust handoff, with a compatibility policy for old states. Other remaining
capabilities include conditional entry and `strategy.order`. Current whole-run
cost still includes the order gateway and the remaining interpreter/state work;
language migration does not remove those costs.

## Reproduce and retained artifacts

Use the repository Python environment and private `pine_compat 0.3.1+mf1` runtime:

```powershell
$env:PATH='E:\projects\MarketForge\.venv\Scripts;'+$env:PATH
$env:PYTHONPATH='E:\projects\MarketForge-tuning\bot-plugins\pine-strategy\.runtime\strategy-ecosystem;E:\projects\MarketForge-tuning\python;E:\projects\MarketForge-tuning\python\tests'
cargo test -p exchange-server --features pine-in-process,bench-execution-timing --lib
cargo build --release -p exchange-server --features pine-in-process,bench-execution-timing --bin exchange-server --example strategy_ecosystem_market --example pine_inprocess_probe
python scripts/pine_exit_parity_fixtures.py
python scripts/qualify_pine_direct.py --probe .local/exits-20261024/selected-exits-probe.exe --manifest .local/exits-20261024/single-union-manifest.json --output .local/exits-rerun-qualification --bars 512 --replay-state --check-migration --check-errors --timeout-ms 5000
python scripts/benchmark_research_optimizations.py .local/exits-20261024/single-research-plan.json .local/exits-rerun-research
python scripts/validate_pine_direct_recovery.py --exe .local/exits-20261024/selected-exits.exe --cwd E:\projects\MarketForge-tuning\.local\pine-direct-20261023\python-canonical --recipe .local/exits-20261024/recovery-single.json --segment-steps 70 --replay-state --require-direct-segments --output .local/exits-rerun-recovery
```

The selected binaries are byte-identical copies of measured `single*.exe`.
Earlier `candidate*.exe` and `final*.exe` files in this evidence directory belong
to rejected broader cohorts; do not use them as the retained implementation.
The original dirty worktree was frozen before edits (4,479 source files hashed).
Seven pre-existing files changed in this round; no baseline file was removed.
Earlier work was preserved. Changes are local and uncommitted.
