# Common and popular Pine formulas as native Rust bots

This report records the October 13 snapshot and its exact hashes. Subsequent
main integration keeps generated Pine samples local: tests now reconstruct
the same byte-exact references from the catalog and use a temporary HTTP plugin.
The results below remain historical qualification, rather than a new run of
every fixture after that test setup change.

MarketForge adds **13 native strategy adapters**, bringing the NativePine group
to **16** and the complete built-in catalog to **31**. Three GPT-6 Luna groups
implemented trend/reversion, oscillators and popular indicator families; the
root integrated the shared host and corrected discrepancies found by the Pine
oracle and real trading tests. The work was uncommitted when this snapshot was recorded.

The eight common formulas are EMA 12/26, MACD 12/26/9, Bollinger reversion,
ROC, Stochastic, CCI, Williams %R and ADX. The five popular families are
Supertrend, UT ATR trailing stop, SSL Channel, WaveTrend and Squeeze Momentum.
Exact bot IDs, formulas, entry/exit rules, reference SHA-256 hashes and installed
Pine scripts are in [the catalog](../native-pine-popular-catalog.json) and
[the configuration guide](../NATIVE_PINE_BOTS.md).

The popular-family references retain credits to
[KivancOzbilgic / SuperTrend](https://www.tradingview.com/script/r6dAP7yi/),
[QuantNomad / UT Bot Alerts](https://www.tradingview.com/script/n8ss8BID-UT-Bot-Alerts/),
[ErwinBeckers / SSL Channel](https://www.tradingview.com/script/xzIoaIJC-SSL-channel/),
[LazyBear / WaveTrend](https://www.tradingview.com/script/2KE8wTuF-Indicator-WaveTrend-Oscillator-WT/)
and [LazyBear / Squeeze Momentum](https://www.tradingview.com/script/nqQ1DT5a-Squeeze-Momentum-Indicator-LazyBear/).
Selection used the existing October 10 research snapshot and primary pages;
it is not a fresh popularity ranking or a profitability recommendation.

## Implemented contract

These are fixed **formula adaptations for spot, long/flat, closed-bar execution**.
The numerical and trading oracle is the corresponding tracked Pine reference
executed with `pine-compat-runtime==0.3.1`. This does not qualify the full published
community strategy or direct TradingView equivalence. Published optional
short/reversal rules, risk exits, filters, dates/sessions, Heikin Ashi and related
timeframes are not ported. Squeeze uses BB multiplier 2.0 and KC multiplier 1.5;
the original author's page identifies an older 1.5 BB multiplier typo.

The new indicator settings are frozen; host settings and `qty` are configurable,
with quantity defaulting to one. `NativePineEmaCross` retains the reference's
level predicates `fast > slow` / `fast < slow`, despite its name. These bots use
incremental Rust indicators, compact serialized windows, native batching,
shared observations and reusable instances. They run without Pine/Python,
while orders pass through the existing gateway, risk checks, matching, fills,
fees and journal. Select their bot IDs explicitly; existing Pine state is not
migrated.

## Qualification

| Check | Result |
|---|---|
| Final workspace, every feature enabled | 674 passed, 15 explicitly ignored, zero failed |
| Native strategy tests | 38 passed, including original three-port host coverage |
| PostgreSQL gates | Nine tests actually ran with required mode; fsync, full-page writes and synchronous commit on |
| Clippy | Whole workspace, all targets/features, warnings denied |
| Formula matrix | 13 strategies × 6 regimes × 128 bars = 9,984 paired observations |
| Large-price cold/rolling restore | 13 × 65 comparisons = 845; 512-bar cold history through bar 576, retained rolling input of 64 bars |
| Live gateway and durable restart | 13 paired strategies, 26 rooms, 128 bars each = 1,664 paired observations; restart after bar 64 |
| Existing regression suites | Five original-port differential tests, six Rust-Pine tests and nine benchmark recipe tests passed |

The formula matrix includes flat, trend reversal, range, squeeze release, jumps
and deterministic random walk inputs. It compares nullable numeric plots with
absolute and relative tolerances of `1e-9`, and requires exact predicates and
normalized order actions. Every bar checks a hot native instance against a
factory recreated from actual JSON, as well as duplicate-call behavior. Accepted
quantity-one fills and fees are supplied as authoritative feedback; rejected
orders must not create phantom entry ownership. Every strategy triggered both
buy and sell orders across the matrix, although individual flat/quiet regimes
do not necessarily trade.

The additional large-price fixture starts with 128 volatile bars followed by a
flat series near `10^12` ticks. It checks full-history Pine against restored
native state and a rolling 64-bar snapshot. Its account has zero cash, so it
qualifies numerical/signal and restoration behavior rather than execution.

The final live test uses actual protected IOC orders and the gateway in isolated
paired rooms. All 13 strategies bought and sold. It compares accounts, books,
public trades, own orders and receipts; globally allocated order IDs use a
verified bijection. All 26 rooms recover persisted bot responses after the
PostgreSQL-backed server restarts at bar 64; repeating that step returns the
saved response and does not duplicate trading. Trading then continues to bar
128. The Pine server flag was enabled, but these new reference scripts use the
external Pine worker because they are outside the existing exact-source Rust
Pine qualification list. The native side uses the built-in Rust bots.

The preceding release also passed the original three-port HTTP checks for
partial fills, fees, rejected-entry retries and recovery after partial fills.
Those tests preceded the final extended-formula comparison correction; the
original three ports were unchanged. Their final-binary differential checks
passed separately. This new live fixture uses one-unit full fills.

## Discrepancies corrected during validation

Pine expression comparisons in the pinned runtime treat absolute differences
up to `1e-10` as equality. A long flat MACD series had identical plotted values
but generated a spurious native exit under raw f64 comparison. The adapters now
preserve that boundary. TA built-in crossings, Supertrend internal comparisons
and ADX directional movement retain their own exact comparisons.

The ordinary rolling-sum path and Pine's `for_bar` SMA path use different
subtraction/addition and Sterbenz behavior. The implementation keeps these
paths separate, preserves their qualified tail recovery and exact zero resets,
and records bounded-window rounding metadata for reliable JSON restoration.
This fixed legitimate state rejection around a near-zero WaveTrend window.
Squeeze's center now uses rolling high/low extrema and its release tests the
immediately preceding bar's squeeze state. Regression fixtures exercise these
specific cases. Earlier failure receipts remain preserved in the local run
directory and are superseded by the successful final checks.

## Reproduction and evidence

Build the final server and probes with:

```powershell
$env:CARGO_TARGET_DIR = 'E:\projects\MarketForge\target\native-pine-20261013'
cargo build -p exchange-server --release --features 'pine-in-process,bench-execution-timing' --bin exchange-server --example native_pine_probe --example pine_inprocess_probe
$env:MARKETFORGE_NATIVE_PINE_PROBE = "$env:CARGO_TARGET_DIR\release\examples\native_pine_probe.exe"
$env:MARKETFORGE_REQUIRE_NATIVE_PINE_TESTS = '1'
.venv/Scripts/python.exe -m unittest discover -s python/tests -p test_native_pine_popular.py -v
```

For the HTTP gate, supply a disposable PostgreSQL database through
`MARKETFORGE_TEST_DATABASE_URL`, set `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`,
`MARKETFORGE_REQUIRE_PINE_TESTS=1` and `MARKETFORGE_TEST_PINE_IN_PROCESS=1`, point
`MARKETFORGE_TEST_SERVER` at the exact built executable, then run unittest
discovery for `test_native_pine_popular_http.py`. Tests write full JSON receipts
under `MARKETFORGE_NATIVE_PINE_RECEIPTS` (this run used
`.local/native-pine-popular-20261013`). The manifest test checks installed
references without relying on a local research archive.

[The machine-readable report](2026-10-13-native-pine-popular.json) records source
and binary hashes, per-strategy order/fill coverage, test logs and raw receipt
hashes. Runtime code and Pine references were frozen before the successful
final build and comparison runs. Pre-existing staged source-library files were
preserved.

## Remaining limits

No thousand-instance performance comparison or long-duration/recovery capacity
test has been run for these 13 strategies. The
[earlier three-strategy benchmark](2026-10-13-native-pine.md) applies only to its
original workload; its speedup must not be extrapolated to this batch. State
byte counts in the JSON evidence are descriptive, not a throughput measurement.
No profitability or full community-script compatibility is claimed.

The shared host retains its existing bounded contract: complete cold history
up to 4095 bars, audits of the saved 64-bar rolling overlap and complete own-fill
prefixes below 4096 fills. Older corrections outside that overlap cannot be
detected. These are finite correctness fixtures, not unlimited-lifetime
qualification.
