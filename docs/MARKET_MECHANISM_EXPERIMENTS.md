# Fixed-seed market mechanism experiments

Recorded outcome: [2026-10-24 experiment report](validation/2026-10-24-market-mechanisms.md).
The four-seed native study did not qualify either candidate as an overall
realism improvement. The three scale runs failed on Pine compatibility before
the evaluation window; defaults remain unchanged.

The entry point is `scripts/research_market_mechanisms.py`. It freezes recipes and
evaluation windows before running a comparison. The experiment changes maker
quote construction and cancellation/replenishment scheduling, not a supplied
price path, chart smoothing, strategy signals, fees, assets or role allocations.

## Candidate mechanisms

`scripts/strategy_ecosystem.py --maker-quote-mode model` removes the final fast-flow
override that pins selected makers' half-spread to one tick and forces a 1-second,
zero-jitter refresh. The original GLFT/Avellaneda risk and arrival estimates,
pre-existing wider safety cap, and independently drawn refresh settings remain.
It does not restore a different population or increase its liquidity budget.
The old `touch` profile remains the default and is the experimental control.
The existing maximum half-spread also bounds how far the quote center can move
from the reference mid. Removing the one-tick override therefore restores that
center bound as well. The model arm tests the existing quote policy together
with its refresh settings, not an isolated spread-width coefficient.

`--requote-on-cancel-ack` enables an optional AdaptiveLiquidityMaker 1.1.0 behavior.
After a cancel-only decision, the next authoritative observation is eligible to
re-evaluate quotes without waiting another refresh interval. A still-visible
stale order is cancelled again. Funds and positions are read from the actual
account; no reservation is released locally, and no successful cancellation is
invented. Repeated observations at the same time remain idempotent. Restored
state retains the pending scheduling behavior. This option has no descriptor
default and is rejected by the frozen 1.0.0 version.

This is not atomic cancel/replace or a subsecond exchange simulator. A one-second
research clock still quantizes actions, and confirmation still requires a later
observation. Existing gateway authorization, budgets, post-only rejection,
matching, fees, liquidation and finite-asset rules remain authoritative.

## Experiment and interpretation

The three profiles are baseline, model-derived quotes, and model-derived quotes
plus prompt replenishment after cancellation visibility. The latter pair isolates
the scheduling change conditional on the model quote profile. It is not a full
factorial experiment; no claim is made for the scheduling-only profile.

The initial plan uses seeds 7, 19, 41 and 73 in the 300-agent native ecology,
12 simulated hours at one-second steps. The first two hours are a warmup window;
the remaining ten hours are the primary evaluation. Parent tasks retain their
original start times across ten hours and their finite target quantities. No
task is pulled forward to make the measured sample busier, and no account is
refilled. The previous 3,300-agent recipe receives a separate seed-7, six-hour
comparison with the same two-hour warmup. Its allocation differs from the native
cohort, so comparing the two is not a causal test of bot count. Actual native
warmup and Pine activity must be reported; elapsed time alone does not establish
that every Pine strategy has warmed up.
The saved readiness counters describe the final snapshot. Discarding the first
two hours does not prove every slow native or Pine strategy was already ready
at the start of the primary window.

The analyzer checks unique trade IDs, reconciles authoritative trade totals,
uses half-open time windows, and builds OHLCV only from actual trades. Returns
require adjacent nonempty closed bars. Missing bars are not zero returns.
Candlestick figures use common price/volume scales and fixed comparison windows.
Market statistics cover 1- and 5-minute returns, return/absolute-return
correlation, tails, inactivity, trend duration, volume relationships, actual
filled taker orders, role contributions, quote availability, spread and depth.
Book observations are taken once per second after the complete action wave.
Availability and outage durations refer to these samples, not continuous-time
liquidity or cancellations/depletion inside the wave.
Role contributions and active instances are attributed by the account owning
each actual fill. These receipt fields do not separately identify a strategy
decision versus a system-triggered liquidation of that account.
Pine activity counts both instances and distinct configured script paths;
different files are not assumed to be economically independent strategies.
Endpoint asset snapshots are checked for accounting identities and available
balances; their aggregate deltas remain visible. They are not a complete
intraday fee, insurance or reservation audit. An exploratory last-quote-midpoint
diagnostic was added after the first baseline/model pair to separate trade-price
bounce from movement of the quoted market. It is not a prespecified success
criterion, and it never fills unavailable quote samples.

The historical comparator is the already archived BTCUSDT spot/perpetual day
2025-01-02. The ZIP SHA-256 values must match the saved acquisition manifest.
It supplies twelve descriptive two-hour windows, not confidence intervals or
multi-date qualification. It contains aggregated trades, not an order book;
therefore no historical spread/depth match or exact taker-order comparison is
claimed. Internal lot volume is not equated with BTC volume.

Candidates are assessed by the full collection of paired results. A visually
more irregular chart, higher volatility, or one closer statistic is insufficient.
Worse quote availability, wider spreads and larger return tails remain visible
as regressions. This experiment does not search for another set of parameters
after inspecting the charts, and neither candidate is automatically promoted.

## Reproduction

The Python analysis uses NumPy and Matplotlib, available through the existing
`python[agents]` extra. It does not add a dependency to the core trading runtime.

```powershell
cargo test -p exchange-core adaptive_liquidity --lib
python -m unittest discover -s python/tests -p 'test_market_mechanism*.py' -v
cargo build --release -p exchange-server --features pine-in-process,bench-execution-timing --example strategy_ecosystem_market
python scripts/research_market_mechanisms.py plan --output <new-evidence-directory>
python scripts/research_market_mechanisms.py run --output <same-directory> --binary <frozen-executable> --workers 1
python scripts/analyze_market_mechanisms.py --output <same-directory> --reference --charts
```

Original Pine files and the qualified private runtime must be installed for the
scale cohort. Plans, per-run identities, logs, receipts, input hashes, extracted
candles and figures remain together in the evidence directory. A completed run
is reused only when recipe, executable, duration and receipt hashes agree.
The runner also freezes and validates interpreted Pine sources, plugin files and
private runtime hashes before cache reuse and after a batch. Existing runs
without such a manifest cannot acquire one retroactively. Cached analysis still
rehashes its original recipe and receipt. In this recorded study the dependency
manifest was captured before the scale runs and rechecked during analysis.
Incomplete runs and their errors are retained rather than silently replaced.

The recorded batch uses two workers and a local memory governor that can suspend
only native-only simulations between resource checks. Their market clocks and
seeded inputs are unchanged. Pine processes are excluded because their host
timeouts use wall time. Scheduling events are retained in the evidence directory;
recorded wall times are consequently not performance benchmarks. For a simple
reproduction, use the default single worker and adequate memory for the retained
authoritative execution history.
