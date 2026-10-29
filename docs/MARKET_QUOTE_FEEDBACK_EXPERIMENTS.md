# Executable basis and quote-feedback experiments

This follow-up preserves the rejected 2026-10-24 maker experiment. It investigates
two separate mechanisms without changing numeric strategy parameters, funded
assets, signals, demand schedules, the simulation clock, or chart construction.
Neither new policy is a default. They are independent ablations, not a combined
population or a claim of real-market qualification.

## Executable basis policy

`BasisLinkTrader` 1.0.0 remains frozen. Version 1.1.0:

- Enters from `perp bid / spot ask` or `perp ask / spot bid`, rather than midpoint
  ratios. Exits use the reverse executable pair. Existing entry/exit thresholds
  retain their meaning; the population already includes an allowance for fees.
- Caps lead quantity by both observed touches and the actual spot hedge budget.
  A lead IOC will not sweep beyond its observed touch price.
- Waits for actual spot and perpetual positions to be hedged before another lead
  order. The spot leg still responds only to the actual perpetual position.

Two legs settle separately. This is not an atomic arbitrage or a guarantee of
profit: counterpart liquidity and capital can change before the hedge arrives.
The recipe option is `--basis-execution executable`.

## Reservation innovations

The existing own-order exclusion avoids a lone maker referencing its own quote.
It does not exclude that quote's influence after it has passed through another
maker's quote. `maker_reference_probe` isolates this by warming makers identically,
then feeding only their own/peer quotes back with zero trades and unchanged
positions. It reuses the existing lone-maker test's settings; it is a synthetic
observation experiment, not a full matching or market-data experiment.
An optional recipe argument repeats the diagnostic with its spot-maker numerical
configurations. This uses fixed stress inventories of target plus one quarter of
the inventory cap and ample cash; it is not a replay of their funded starting
accounts. A reduced drift in this probe does not establish a realistic market.

`--reservation-innovations` enables a new optional AdaptiveLiquidityMaker 1.1.0
policy. Let `m` be the observed reference mid and `u` the current reservation
offset (index/microprice bias minus inventory risk adjustment), bounded using
the existing maximum half-spread. The experimental center before fresh-flow
impact is `m + u - u_previous`, where `u_previous` is the last confirmed quote's
bounded reservation offset. The existing fresh-flow response and final center
cap still apply. This also makes the bounded reservation component explicit
before adding flow; it is not an unchanged textbook GLFT formula.

The hypothesis is that the current endogenous mid may already contain the
previous bias. Pricing its change avoids repeatedly applying an unchanged
discount through peers. This is a closed-loop model assumption, not a unique
decomposition of the true fair value. It must be evaluated against actual market
outcomes, including undesirable volatility suppression or other regressions.

The optional state is persisted. A proposal alone does not advance it: either a
real `OrderRested` gateway result or a later authoritative observation confirming
the proposed quote is required. Cancellation-only and rejected proposals do not
invent accepted quotes, fills, released reservations, or new capital. Same-time
idempotence, finite account budgets, and the existing matching/risk rules apply.
The option has no descriptor default and is rejected by version 1.0.0. Legacy
snapshots omit all of its state fields.

## Frozen market comparisons

Each policy is compared separately with the previous `model_ack` population:
300 native bots; seeds 41, 7, 19 and 73; 12 simulated hours per run at one-second
steps; `[2h,12h)` is the primary window. All numeric inputs and role allocations
are unchanged. The original midpoint basis remains in the reservation study;
the original level-based reservation policy remains in the basis study.

Each study also reruns the full 12-hour seed-41 control under its new binary.
Its authoritative receipt must match the previously archived receipt byte for
byte. Previous controls are reused only after checking their recipe, receipt,
metric and plan identities. Every new result is kept, including failures.

The seeds have already been examined in the maker study. Seed 41 diagnoses a
known anomaly; these are not unseen-seed holdouts. No parameter search, seed
selection, or independent calibration claim follows from improvement here.
The historical reference remains one archived BTCUSDT day, and contains no
order book. Its two-hour ranges are descriptive, not confidence intervals.

Besides the prior return, quote-mid, spread, depth and activity diagnostics,
candle ranges and wick fractions are recorded. Wick fraction is
`1 - abs(close-open)/(high-low)` for nonzero-range candles; flat candles are
excluded only from this ratio. These shape summaries were specified before
the first follow-up result, and are not a target to fit.

An additional exploratory reference uses 2025-01-02, 2025-04-02, 2025-07-02 and
2025-10-02: the second UTC day of each quarter's first month, selected by calendar
after the first basis result. `scripts/market_reference_calendar.py` retrieves
the official spot and USD-M BTCUSDT aggregate-trade archives and checks each
published SHA-256. Source format and timestamp units follow the
[Binance public-data documentation](https://github.com/binance/binance-public-data).
This adds eight disjoint ten-hour reference windows per market, plus the daily
and two-hour descriptions. These ranges are not confidence intervals. The
original paired figures deliberately retain their labeled one-day reference;
the calendar expansion is reported separately and never changes the recipes.

`scripts/summarize_market_feedback.py` joins all four seeds from both completed
studies, includes the original touch default as separate context, and uses the
eight ten-hour calendar windows for its labeled reference bands. No historical
book comparison is inferred. `scripts/research_flow_visibility.py` and
`scripts/analyze_market_anomaly.py` are explicitly post hoc receipt diagnostics;
neither reconstructs the strategy's full intraday decision/feedback trace.

## Reproduction

```powershell
cargo test -p exchange-core basis_link --lib
cargo test -p exchange-core adaptive_liquidity --lib
cargo test -p exchange-core --example maker_reference_probe
cargo run -p exchange-core --example maker_reference_probe
python -m unittest discover -s python/tests -p 'test_basis_execution_recipe.py' -v
cargo build --release -p exchange-server --features pine-in-process,bench-execution-timing --example strategy_ecosystem_market
python scripts/research_basis_execution.py --source <original-study> --output <new-basis-directory>
python scripts/research_quote_reservation.py --source <original-study> --output <new-reservation-directory>
python scripts/research_market_mechanisms.py run --output <one-new-directory> --binary <frozen-binary> --workers 1
python scripts/analyze_basis_execution.py --output <same-directory> --source <original-study> --charts
python scripts/summarize_market_feedback.py
```

The paired analyzer handles either study using its frozen plan metadata. Do not
replace an old plan, completed metadata, executable or receipt to reuse a name.
For the recorded run, bounded native memory scheduling is retained in logs and
does not change market time. Wall times are not performance benchmarks.
