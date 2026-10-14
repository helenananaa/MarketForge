# Native ports of bundled Pine strategies

MarketForge includes sixteen independently selectable Rust bots. They execute
directly through the existing native scheduler, shared observation delivery,
gateway risk checks, matching and journal. They do not invoke Pine, Python or an
external strategy worker. They are fixed ports of the bundled scripts, rather
than a compiler for arbitrary Pine source.

| Bot ID | Pine source | Strategy inputs and defaults |
|---|---|---|
| `NativePineMa` | `ma.pine` | `fast=3`, `slow=8`, `qty=2` |
| `NativePineRsi` | `rsi.pine` | `length=7`, `low=35`, `high=65`, `qty=2` |
| `NativePineBreakout` | `breakout.pine` | `length=6`, `qty=2` |

All sixteen use plugin version `1.0.0`, config version `1` and state version `1`.
They are present in the normal built-in bot catalog; no `pine-in-process` feature
or environment switch is required for these bots. Existing `pine.strategy` bots
remain independently available.

The thirteen additional ports below have fixed indicator parameters and `qty=1`
by default. They accept the common host parameters and `qty`; indicator periods
and thresholds are frozen to their installed Pine references. The full formulas,
entry/exit predicates and source hashes are in
[the catalog](native-pine-popular-catalog.json).

Downloaded libraries and generated `scripts/native_*.pine` samples are ignored
by Git. Rust bots do not need these files. Differential tests reconstruct the
exact reference bytes from the catalog and verify the frozen SHA-256 hashes;
HTTP tests install them only into a disposable test plugin. A fresh checkout
therefore requires neither downloaded corpora nor locally generated samples.

| Bot ID | Generated Pine reference | Frozen formula / trading rules |
|---|---|---|
| `NativePineEmaCross` | `native_ema_cross.pine` | EMA 12/26; fast > slow entry, fast < slow exit; level conditions despite the reference name |
| `NativePineMacd` | `native_macd.pine` | MACD 12/26/9; MACD above/below signal |
| `NativePineBollingerReversion` | `native_bb_reversion.pine` | BB 20, 2; close below lower entry, above mid exit |
| `NativePineRoc` | `native_roc.pine` | ROC 20; >2 entry, <0 exit |
| `NativePineStochastic` | `native_stochastic.pine` | Stoch 14/3/3; cross up below 25, cross down above 75 |
| `NativePineCci` | `native_cci.pine` | CCI hlc3, 20; <-100 entry, >50 exit |
| `NativePineWilliams` | `native_williams.pine` | Williams %R 14; <-80 entry, >-20 exit |
| `NativePineAdx` | `native_adx.pine` | DMI/ADX 14/14; DI+ > DI- with ADX >25 entry, DI+ < DI- exit |
| `NativePineSupertrend` | `native_supertrend.pine` | ta.supertrend 3/10; direction negative entry, positive exit |
| `NativePineUtAtr` | `native_ut_atr.pine` | ATR 10, key 1 trail; close/trail cross; ordinary close source |
| `NativePineSsl` | `native_ssl.pine` | SMA high/low 10, persistent side; +1 entry, -1 exit |
| `NativePineWaveTrend` | `native_wavetrend.pine` | hlc3 EMA 10, deviation EMA 10, WT EMA 21, signal SMA 4; crosses with -53/+53 gates |
| `NativePineSqueeze` | `native_squeeze.pine` | BB 20/2, KC TR SMA 20/1.5, linreg 20; positive squeeze release entry, falling momentum exit |

These are MarketForge formula adaptations. The common indicator group covers
EMA, MACD, BB, ROC, Stochastic, CCI, Williams and ADX. The popular-family selection
draws on the public [SuperTrend / KivancOzbilgic](https://www.tradingview.com/script/r6dAP7yi/),
[UT Bot / QuantNomad](https://www.tradingview.com/script/n8ss8BID-UT-Bot-Alerts/),
[SSL Channel / ErwinBeckers](https://www.tradingview.com/script/xzIoaIJC-SSL-channel/),
[WaveTrend / LazyBear](https://www.tradingview.com/script/2KE8wTuF-Indicator-WaveTrend-Oscillator-WT/)
and [Squeeze Momentum / LazyBear](https://www.tradingview.com/script/nqQ1DT5a-Squeeze-Momentum-Indicator-LazyBear/)
families, with provenance retained in the
[source research](validation/2026-10-10-strategy-research.md).
Popularity is a selection signal, not a return or correctness ranking.

Published originals remain separate. These ports do not reproduce their full
short/reversal behavior, optional filters, risk exits, date/session windows,
Heikin Ashi sources, or multiple timeframes. Supertrend uses the pinned runtime's
`ta.supertrend`, not every optional ATR mode of the community indicator. Squeeze
uses BB multiplier 2.0; LazyBear's original page describes an older 1.5 multiplier
typo. The frozen Pine formulas are the numerical and trading oracle for this
batch; direct TradingView numeric equivalence has not been established.

For example, use this agent template in a room or manual training run:

```json
{
  "Plugin": {
    "participant": {
      "participant_id": "native-ma-1",
      "room_id": "my-room",
      "account_id": 300,
      "kind": "RuleAgent",
      "instrument_id": "V-BTC-SPOT"
    },
    "plugin_id": "NativePineMa",
    "plugin_version": "1.0.0",
    "state_version": 1,
    "config_version": 1,
    "seed": 7,
    "config": {
      "bar_interval_ms": 10000,
      "fast": 3,
      "slow": 8,
      "qty": 2
    }
  }
}
```

Common defaults are `bar_interval_ms=10000`, `history_limit=512`, `max_qty=2`, `inventory_cap=10`,
`slippage_ticks=2`, `fee_buffer_ppm=1000`, and `initial_avg_price_tick=0`.
For nonzero pre-existing spot inventory, provide its known initial average price,
as required by the existing Pine broker. Inputs use flat Rust names rather than
Pine's `inputs` object. Unknown inputs and incompatible saved configurations are
rejected.

The candle interval must be a multiple of 1000 ms, from 1000 through 86400000 ms.
`history_limit` ranges from 16 through 4095. Cold initialization requires complete
history within this limit; a committed instance can continue beyond it using
rolling input and its restored indicator state.

## Preserved behavior

These ports support the bundled spot, long-only, closed-bar behavior. MA tests
whether the fast average is above or below the slow average on every new bar;
it does not replace those conditions with crossover events. RSI uses the pinned
runtime's Wilder initialization and smoothing, including its flat-series result.
Breakout compares the current close with the previous bar's highest/lowest
window, preserving `[1]`.

The current authoritative account position determines whether the account is
flat. Entry tracking is based on accepted order references and actual own fills;
a rejected order creates no position, and a partial IOC creates only the filled
quantity. Closing `L` sells only its tracked entry quantity, subject to available
inventory and normal gateway rules. Existing inventory and reservations remain
accounted for. Fees, limits and protected IOC prices use the existing broker
defaults.

Cold initialization warms indicators over the supplied closed bars, then may
trade only the latest bar. If several new bars arrive together, their indicators
are all updated but orders are generated only for the latest bar, matching the
existing broker's catch-up behavior. A repeated observation of the same bar
does not submit another order.

## State and scope

Durable state contains indicator windows or accumulators, the bar cursor,
bounded audit data, tracked entries/orders, and own-fill audit information.
There is no growing Pine transcript. Snapshots bind to the normalized template
and cannot be silently reused with changed parameters or participant identity.
Failed audits leave the previous strategy state intact.

The JSON parser enables `serde_json/float_roundtrip` so MA/RSI floating-point
state survives actual durable JSON bytes exactly. Candle/fill prices and the
initial average price are restricted to `2^41-1` ticks. Authoritative cash,
position, fees and protected order prices are limited to `2^53-1`, retaining the pinned
Pine runtime's exact integer domain for these ports. Instrument precision and
the monotonic observation clock are checked on resumed calls.

Before history rolls, complete supplied candle prefixes are checked. Complete
own-fill prefixes remain checked. Once the host rolls the candle window, a
restored bot requires its committed cursor and saved audit overlap to match.
The rolling audit covers only the intersection with the saved 64-bar overlap;
corrections before that boundary cannot be checked even if older candles still
appear in the supplied window. The saved `rolling_audit_start_ms` records the
last rolling check's boundary.
Cold truncated history, missing cursors, changed retained history, and unavailable
complete own-fill history are refused. The current host has a bounded own-fill
stream, so this version refuses 4096 or more own fills instead of claiming
unbounded lifetime recovery. Rolling candle support is not an unlimited-history
qualification.

Perpetuals, short entries, related instruments, arbitrary edited scripts,
conditional orders, automatic hedging and strategy exits/protection are outside
these sixteen ports. Continue using the corresponding Pine path for those cases.
Running these bot IDs does not migrate an existing Pine bot's saved state.

## Reproducible comparison

`scripts/benchmark_strategy_runtime.py --population native-pine` creates the same
three-strategy workload as `--population pine`, with the same account identities,
funds, input parameters, candle periods and 38 liquidity providers. It changes
the implementation IDs and input representation. Both modes export a normalized
recipe for identity comparison and audit actual initialized strategies, evaluated
bars and backlog. Native timing uses `bot_native_pine_evaluate`, excluding the
liquidity bots.

Use `--pine-in-process` for a comparison with the optional direct Rust Pine
interpreter. Preserve the same server executable, profile, prewarm bars, clock
cadence and observation window across versions. The load benchmark uses the
memory journal; PostgreSQL restart tests validate durable recovery separately.
Fixed-input equivalence and asynchronous performance are different claims.

See [the original three-port validation](validation/2026-10-13-native-pine.md)
for that workload's measured performance. The additional thirteen ports have
[separate correctness validation](validation/2026-10-13-native-pine-popular.md);
their thousand-instance performance has not been measured.

[Main integration checks](validation/2026-10-14-native-pine-main.md) verify that
ignored samples are unnecessary for a fresh checkout and record the latest
targeted recovery and regression results.
