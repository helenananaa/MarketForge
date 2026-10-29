# Economic and stop-execution repair qualification

**Rejected candidate.** The four-seed experiment below failed market-behavior
review and must not be promoted as an improvement. The experiment runner now
defaults to `legacy-stop`, restoring the previous population and quote/depth
configuration while the current runtime retains the market-stop repair.
This restores a comparison control; it does not qualify the old market as realistic.
Use `--profile rejected-economic` explicitly to reproduce the failed experiment.
Original receipts, charts and frozen plans remain unchanged.

This work changes executable behavior and the research population's demand
schedule. It does not select parameters against candle shape, volume/return
correlation, a desired price path, or a target volatility.

## Scope and reproducibility

The checkout is `MarketForge-endogenous`, based on commit
`e306311d242184e1d958f862aae0e8e62d6e69af`. The experiment declaration is
`2026-10-16-economic-market-plan.json`. The final experiment directory is
`.local/economic-market-20261016/final`; its frozen plan records executable,
recipe, roster, selected source and patch hashes before launching any case.

The declared cases are seeds **7, 19, 41 and 73**, each with **300 bots**, a
**four-hour session**, and **one-second observation waves**. Initial accounts,
cash pools, spot inventory and role counts are preserved relative to each
same-seed fast-flow recipe. This is a combined mechanism experiment; it does
not measure the independent causal contribution of each change.

To reproduce, build the release `strategy_market` example and run:

```powershell
python scripts/run_economic_market.py --profile rejected-economic --output-dir <new-directory> --binary target/release/examples/strategy_market.exe --seeds 7 19 41 73 --seconds 14400 --session-seconds 14400 --jobs 4 --probes
<analysis-python> scripts/analyze_economic_market.py <new-directory> --charts
```

Analysis uses Python 3.12, NumPy 2.3.5 and Matplotlib 3.10.7. This task's
compatible interpreter is
`.local/economic-market-20261016/plot-python/Scripts/python.exe`. It is an
isolated task environment; an incompatible system Matplotlib/NumPy pair was
not modified. A fresh reproduction can create its own venv and install those
two pinned analysis packages there.

The runner refuses to overwrite a frozen experiment. Analysis verifies the
binary, recipe, roster and receipt hashes and exact simulated duration.
Charts include every declared seed and both markets. They use actual one-minute
OHLC and traded quantity after the predeclared twenty-minute warmup; empty
minutes receive no invented trades.

## Mechanism changes

- `AdaptiveLiquidityMaker` **1.2.0** requires explicit venue maker fees and a
  minimum quote edge. Quotes must cover these costs at their observed midpoint
  after inventory/flow skew. Distinct depth levels survive fee clipping. Quote
  amendments pass through the gateway and retain quantity, avoiding a whole
  cancel-only wave or assumed collateral released by future cancellations.
  Versions 1.0.0 and 1.1.0 retain their original behavior.
- The economic population removes the inherited one-tick spread cap and the
  0.08/0.18 depth multipliers. The default quote size again comes from funded
  inventory capacity divided by sixteen. Heterogeneous refresh intervals and
  jitter replace identical touch refreshes. A fee floor is not a promise that
  adverse selection or inventory losses disappear.
- Every execution parent's planned start plus horizon fits the declared
  session. Endogenous value orders retain private beliefs, signals, sizes and
  funding, with execution horizons bounded by that session.
- Residual reactive traders have heterogeneous baseline arrival intervals;
  price-attention and activity-excitation arrival multipliers are disabled.
  No return/volume correlation is imposed as an acceptance condition.
- `BasisLinkTrader` **1.1.0** reserves both legs' round-trip fees, residual basis,
  observed closing half-spreads and a minimum edge against executable spot
  prices before a new entry. Hedges and unwinds are exempt from that new fee
  gate; existing budget and sequencing constraints still apply. A reversal
  first flattens the existing perpetual leg so the entry fee gate cannot block
  the risk-reducing portion. Version 1.0.0 remains available unchanged.
- Internal `StyleSwitchingTrader` stops now submit **market** exits; perpetual
  exits are **reduce-only**. A triggered exit stays latched through partial
  fills, an empty book, restored state and price recovery until a later account
  observation confirms flat. It bypasses the stochastic signal timer. Spot
  uses the venue's no-short risk constraint because spot rejects reduce-only.
  Internal trigger detection still uses the bot's observed midpoint; this does
  not turn it into an exchange-side last-trade trigger between observations.
  Ordinary discretionary rebalancing retains its existing limited execution.

## Verification and evidence boundaries

The final runtime passed **774 workspace library tests** with **21 explicitly
ignored**, three Python recipe tests, formatting and Clippy for core library
and examples with warnings denied. The worker subprocess tests used a local
Python environment exposing `python3`; no worker fallback or safety checks were
weakened to obtain this result.

The real-gateway stop regression opens ten perpetual lots, stops against only
three available lots, retries an empty book, restores bot state and closes the
remaining seven even after the price recovers. Its three submissions reconcile
to ten executed lots, one trigger and one confirmed completion. Separate tests
cover fee clipping after skew, distinct quote levels, amendments, explicit
plugin version/configuration contracts and fee-aware basis entries/unwinds.

The research runner records actual market exit commands and filled quantities,
and checks newly generated economic maker quotes against the observed midpoint
whenever both book sides are present. The quote counter excludes observations
without a two-sided book; the bot still applies its fee rule using its own
fallback reference. Analysis reconciles stop submissions and fills to bot state, checks
perpetual reduce-only flags, unique trade identities, positive prints, absence
of self trades and volume totals. It also reports actual role participation,
parent completion, spreads, sampled depth, return/order-sign statistics and
spot maker PnL. Absolute PnL uses the initial seed-book midpoint and final book
midpoint; excess PnL compares with holding the initial inventory to that same
final mark. Underperforming that hold benchmark alone does not establish an
absolute loss or an unviable market maker.

Role attribution counts actual taker quantity owned by each role, including
involuntary fills. The frozen native recipe starts ordinary gateway order IDs
at 100000, so analysis can separately identify the existing `room.rs` reserved
system namespaces: conditional orders at 7e18, position protection at 8e18 and
liquidation at 9e18. This classification is scoped to these recipes with no
external/user-supplied IDs; it is not a general inference from arbitrary IDs.

Seed 7 includes predeclared paired 10/100-lot buy/sell IOC interventions in each
market, each with a no-intervention branch and a 180-second follow-through.
Parent fingerprint checks protect the original path. These are finite actual
matching experiments with a price limit, not an assumption of full fills or a
claim about a real venue's impact law.

The separate `quote_reference_probe` example restores the first spot maker's
seed-41 state into an otherwise idle book, removes seed quotes, and compares
inventory at target with inventory one-quarter of capacity above target. Both
branches use the same unchanged plugin configuration and run 120 one-second
cycles; the probe asserts there are no new trades. Reproduce with:

```powershell
cargo run -p exchange-core --release --example quote_reference_probe -- .local/economic-market-20261016/final/seed-41.recipe.json .local/economic-market-20261016/final/seed-41.receipt.json
```

This probe is a separate mechanism experiment, not a modification to the four
frozen market runs. Its source, binary and output hashes are recorded in the
companion result.

Prototype experiments `qualification`, `qualification-v2` and
`qualification-funded` were interrupted when cost/risk-exit defects required a
new executable. Their logs, incomplete output files and abort markers remain; they
are excluded from final-code qualification. Short pilots are separate smoke
evidence, not substitutes for the declared final cases.

One-second frozen observation waves, ordered submissions and sampled
after-wave books remain. The test does not qualify intra-wave liquidity,
production-server scheduling/latency, live trading or similarity to a declared
real-market benchmark. A plausible chart is not a realism certificate.

## Final results

**Mechanism contracts passed; market realism is not qualified.** All four declared main cases completed. All 180 execution parents traded; 176 reached their full target.

There were 238,832 actual prints and 3,841,440 two-sided-observation quote cost checks. The 274 stop triggers produced 289 market submissions and 14025 filled lots; 274 were confirmed flat, with 0 pending. Partial/zero-fill submissions numbered 10/5; they were not counted as completed exits.

| Seed | Prints | Completed execution parents | Stop triggers / submissions | Spot makers with absolute loss | 1m return ACF(1), spot / perp |
| --- | ---: | ---: | ---: | ---: | ---: |
| seed-7 | 71,009 | 45/45 | 76/76 | 15/15 | 0.582 / 0.599 |
| seed-19 | 62,842 | 44/45 | 85/100 | 15/15 | 0.841 / 0.861 |
| seed-41 | 51,850 | 43/45 | 71/71 | 15/15 | 0.839 / 0.841 |
| seed-73 | 53,131 | 44/45 | 42/42 | 3/15 | 0.565 / 0.549 |

The returns retain excessive directional persistence. The isolated quote-reference probe uses the unchanged first spot maker configuration and restored seed-41 state. With inventory at target, its mid remained fixed; with inventory 108 lots above target, its mid moved -192.27 bp in 120 seconds with zero new trades. The maker repeatedly applies an inventory displacement relative to a midpoint containing its own previous quotes. This reproduces a structural feedback defect; it does not identify its exact share of the full-market price path. That defect remains in the frozen candidate and is a follow-up mechanism repair, not a reason to tune noise or choose a better-looking seed.

The companion `2026-10-16-economic-market.json` records case receipts/hashes, the matched 20–120 minute historical comparison, stop lifecycle totals, actual intervention results, maker PnL and probe provenance. Raw receipts and `analysis.json` remain in the frozen evidence directory.
