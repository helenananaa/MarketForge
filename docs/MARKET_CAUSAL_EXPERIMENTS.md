# Causal market-mechanism investigation

This follows the rejected maker and basis experiments in
[MARKET_QUOTE_FEEDBACK_EXPERIMENTS.md](MARKET_QUOTE_FEEDBACK_EXPERIMENTS.md).
The original default remains unchanged. New policies are explicitly selected;
neither a small price range nor a low return standard deviation is qualification.

## Exact trace

The continuous research runner accepts `MF_RESEARCH_TRACE_PATH` and
`MF_RESEARCH_TRACE_WINDOWS=start_ms:end_ms[,start_ms:end_ms...]`. It creates a
new JSONL file, refuses to overwrite an old trace, and records:

- Makers' and basis traders' exact pre-decision accounts, orders, public trades,
  candles, reference touches and visible external liquidity; scalar state before
  and after deciding, including inventory/volatility/flow/reservation memory.
- Every ordered gateway action, actual account and book before/after execution,
  authoritative order events, clearing, margin/wallet updates and feedback.
- Clock and cascade executions, with their explicit liquidation flag, separately
  from gateway requests. Only authoritative executions count toward trade totals.

Only eight public book levels are repeated in gateway records; decision records
also retain all visible own-order-excluded liquidity levels. No trace field is
given back to a strategy. This mode supports continuous runs, not checkpoint
segments. Research traces are optional and remain separate from final receipts.

`scripts/research_causal_trace.py` reproduces the old seed-41 reservation
population. The diagnostic window was chosen retrospectively: minimize the
product of adjacent spot minute returns during hours 2–4, then retain minutes
150–161 around the selected reversal at minute 155. The trace is not a holdout.
On/off smoke receipts must be byte-identical; the archived complete trade and
after-wave book prefix must match. A second trace with the final binary verifies
the complete short receipt, including final accounts and bot states, as well.

`scripts/analyze_causal_trace.py` reconstructs the existing tick-size-1 maker
equations and exposes reconstruction errors. It distinguishes proposals from
filled orders and real residuals from intended hedges. Entry classification
means increasing same-direction perpetual exposure, not a profit attribution.
An entry failing the configured executable two-leg threshold is not automatically
a proof that every completed holding period lost money.
The final trace analyzer is schema 3. It includes cancel-only reconsiderations
using the pre-decision deadline, and uses reservation memory after observation
confirmation but before gateway feedback. Earlier schema-2 outputs remain local;
final numerical claims use the corrected schema and verified reconstruction.

## Policies

### BasisLinkTrader 1.2.0 (`--basis-execution touch-hedge`)

Version 1.0.0 keeps midpoint signals; 1.1.0 keeps its prior executable policy.
Version 1.2.0 retains executable entry/reverse-executable exit, observed two-book
depth and finite hedge budgets, and waiting for a zero actual residual before
another lead. It additionally makes both submitted legs and their funded sizing
use the same observed touch. Spot hedge quantity is limited by its visible touch
and actual budget; it does not sweep to deeper levels. A real residual can be
hedged even when the unused side of either market's book has no quote.

This replaces the midpoint slippage-band behavior for 1.2.0. The numeric
`max_slippage_ppm` remains in the legacy recipe, but 1.2 IOC limits are the observed
touch, not `mid ± max_slippage`. A quote moving away before submission produces
an unfilled IOC. The next decision reads actual positions again. No reservation
is invented across venues, and no new capital or assumed atomic fill is added.

### Public-liquidity flow (`--market-depth-flow`)

AdaptiveLiquidityMaker 1.1.0 gains the optional boolean `market_depth_flow`, with
no descriptor default. Version 1.0.0 rejects it, including explicit false.
The old new-flow lean uses
`signed_qty / (total_qty + max_qty * levels)` times the existing half-spread and
flow coefficient. Thus every maker can interpret the same public trade relative
to only its own tiny proposed quote.

The experimental option instead uses the **visible external resting quantity on
the consumed side**, within the already configured maximum quote reach. It
subtracts actual own resting reservations and adds no numerical coefficient.
Buy flow uses asks; sell flow uses bids. Empty visible liquidity contributes
zero, not invented depth. Duplicate trade IDs still add no fresh impact. Risk
spreads, inventory control, flow widening, matching and capital limits are kept.
When external liquidity is scarcer than the old one-bot denominator, the same
flow can produce a larger lean; this is not a uniform impact-reduction factor.
This is a proposed model for liquidity-sensitive impact, not an unchanged GLFT
equation or a claim that all observed flow is permanent information.

## Frozen comparisons

The new full study is in
`.local/strategy-research-causal-20261025/full-study`. Diagnostics and their
independent metadata remain in the parent directory. A failed attempt to place
a full-study plan in the diagnostic directory was retained: the existing
runtime-identity guard rejected mixing old diagnostic runs with a new manifest.
No full run was launched from that attempted plan.

Each of seeds 41, 7, 19 and 73 runs both treatments for 12 hours at one-second
steps with 300 native bots. Primary window: `[2h,12h)`. All numerical inputs,
funding, initial inventory, role allocations, signals and execution tasks are
fixed. The paired control is the **prior failed reservation candidate**. It is
not the original touch default. A full seed-41 control rerun must reproduce the
old receipt byte for byte.
Signal-generation rules and exogenous tasks are fixed; actual price- and
inventory-dependent signals change naturally along the counterfactual paths.

- `hedged`: change only the basis version from 1.0.0 to 1.2.0; retain prior
  reservation innovations for makers.
- `depth`: the same 1.2.0 basis, plus `market_depth_flow=true` for makers.

All nine full runs and all failures are retained. The same selected absolute
diagnostic window is traced under both treatments; endogenous paths may differ.
No parameter adjustment or reselection changes this frozen nine-run study.
The four seeds were already examined in prior studies, not unseen holdouts.

Evaluation includes trade and quote-mid return autocorrelation, candle
range/wick shape, tails, actual role activity, quote availability/depth, finite
capital checks and spot/perp linkage. The old four-calendar-day reference is
reused; its eight disjoint ten-hour windows per market are descriptive, not
confidence intervals, and contain no historical book or quote-mid reference.

### Separately frozen inventory-level hypothesis

The completed seed-41 depth run still had strongly alternating spot returns.
Applying the same early-hours selection rule to that run selected minute 182;
the second trace retains minutes 177–188. In one actual four-second maker path,
inventory moved from 209 toward its target of 216, ending at 213. The differenced
private contribution nevertheless flipped negative; along with the observed
midpoint and flow response, this produced a 620-tick center fall. Other large
jumps still involved flow changes with unchanged inventory. This is evidence
against the earlier differenced-reservation hypothesis, not proof that every
remaining wave has one cause. The largest full-window reversal at minute 710
was recorded separately and was not traced.

`scripts/research_inventory_level_study.py` freezes a fresh four-seed design in
`.local/strategy-research-causal-20261025/inventory-level-study`. The sole recipe
change removes `reservation_innovations=true`, keeping touch hedges and public
depth flow. This restores both the existing level-based inventory reservation
and its raw-plus-flow final-clamp ordering. It adds no numerical parameter or
production code. The same frozen executable is copied, not rebuilt. Each
already completed depth control is verified by executable, recipe, receipt and
metric hashes before the new full plan is created.

All four recipe hashes were frozen before the short pilot and all four full
runs remain in the study regardless of pilot results. The original nine-run
plan and receipts are unchanged. The known isolated quote-only peer-drift
failure of level pricing remains a limitation and must be considered alongside
full-market results. This sequential exploratory study has no new holdout.

The initial parallel seed-7 attempt was explicitly resource-cancelled with last
logged step 22,200 when available system commit approached 2.3 GB. Progress logs
occur every 100 steps; this is a lower bound, not the exact unrecorded stop step.
Its original failure
metadata, partial log and plan are preserved. A fresh
`inventory-level-retry-s7` plan records the identical frozen recipe/executable
and reruns only that seed serially after the original cases finish. This is an
infrastructure retry, not seed selection from market outcomes. Consolidation
retains the cancelled attempt and requires its successful full retry for the
paired statistics. The final design therefore has 14 native attempts and targets
13 successful full runs; actual completion is recorded in the final report.
The remaining batch admission was paused while existing children completed, so
later cases started alone; this changes wall time, not market clock or inputs.

## Reproduction

```powershell
cargo test -p exchange-core --lib
cargo test -p exchange-core --example maker_reference_probe
python -m unittest discover -s python/tests -p 'test_*mechanism*.py' -v
python -m unittest discover -s python/tests -p 'test_*execution*.py' -v
cargo build --release -p exchange-server --features pine-in-process,bench-execution-timing --example strategy_ecosystem_market
# Freeze the executable first; use fresh output/trace paths and preserve old files.
# Analysis uses the recorded Python 3.12 / NumPy 2.5.3 environment; the local
# isolated analysis-deps path and exact rendering versions are in the manifests.
python scripts/research_causal_mechanisms.py --output <fresh-full-study> --diagnostics <diagnostic-directory>
python scripts/research_market_mechanisms.py run --output <fresh-full-study> --workers 1
python scripts/analyze_causal_mechanisms.py --output <fresh-full-study> --charts
# The inventory-level extension requires its frozen depth baselines first:
python scripts/research_inventory_level_study.py design --output <fresh-level-study>
python scripts/research_inventory_level_study.py plan --output <fresh-level-study>
python scripts/research_market_mechanisms.py run --output <fresh-level-study> --workers 1
python scripts/analyze_inventory_level_study.py --output <fresh-level-study>
python -m unittest discover -s python/tests -p 'test_inventory_level_recipe.py' -v
```

The plan records executable, recipe and compiled-source identities. Runtime
inputs are frozen before full runs and rechecked afterward. Optional native
memory scheduling changes wall time, not simulated time; these runs are not a
performance benchmark. Native results do not qualify the previously failing
3,300-bot/Pine cohort.
