# Native Rust ports of three bundled Pine strategies

This is historical qualification of the October 13 snapshot identified by the
companion JSON's source and binary hashes. Later main integration and sample
ignore changes are validated separately; these performance numbers do not
describe a new run on the current main revision.

The built-in catalog now includes `NativePineMa`, `NativePineRsi` and
`NativePineBreakout`, all at version `1.0.0`. They use incremental indicators,
compact recoverable state, reusable native instances and borrowed shared
observations. Orders still pass through the normal gateway, risk checks,
matching, authoritative fills and journal.

These are fixed ports of the bundled `ma.pine`, `rsi.pine` and `breakout.pine`
scripts. They do not translate arbitrary Pine, migrate existing Pine state or
remove the existing Pine implementation. See [configuration and scope](../NATIVE_PINE_BOTS.md).

## Correctness qualification

The final source passes the workspace tests with every feature enabled:
**649 passed, 15 explicitly ignored, zero failed**. The 13 new native unit tests
cover indicator initialization, actual serialized JSON restoration, hot/shared
instance parity, protected IOC actions, authoritative entry ownership, changed
history/configuration rejection, rolling history and the 4095-fill boundary.
Running and paused manual-step observations produce identical actions and
state; closed markets are rejected without advancing state.

PostgreSQL tests ran with `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1` and an explicitly
configured disposable loopback database. Nine existing PostgreSQL tests passed.
The temporary cluster used `fsync=on`, `synchronous_commit=on` and
`full_page_writes=on`. Database recovery is a separate correctness check from
the memory-journal load comparison.

Five Python differential tests compare these native ports with the pinned Pine
runtime and broker, including:

- Nullable warmup plots, flat and alternating prices, and input overrides.
- Three random seeds × three scripts × 64 sequential bars, with synthetic
  authoritative partial fills and fees, duplicate calls and restoration.
- A 128-bar rolling-history run per strategy compared with complete Pine
  history; altered retained history and cold truncated history are refused.
- Rejected entries followed by a later retry, and pre-existing inventory that
  `strategy.close("L")` must not sell as an owned entry.

Indicator values agree to ten decimal places; normalized order actions agree
exactly. Tests retain raw actions, plot values and state. Each request also
restores the native instance from actual JSON and checks the result. Six existing
Rust-Pine differential tests pass on the same release build.

The live HTTP tests use paired isolated rooms, real gateway orders, two-unit
entries that fill only one unit, nonzero fees, risk rejection, idempotency and
PostgreSQL server restart. Globally allocated order IDs require an explicit
bijective mapping between rooms; raw observations and receipts are retained.
Envelope keys differ by plugin identity. The selected trainee account, own
orders, book, public trades, clock fields and host receipts remain compared in
full after these identity mappings. This does not compare every participant's
entire ledger. On a repeated closed bar, Pine retains its previous `last_actions`
diagnostic while native clears it; both submit no actual orders, verified by
empty current host receipts and unchanged account/book/trades and candle cursor.

All three HTTP tests pass on the final release binary with PostgreSQL forced.

Nine benchmark-recipe tests and five existing market-load tests pass. Clippy
passes for all workspace targets/features with warnings denied, and formatting
passes.

## Performance scope

The benchmark accepts `--population native-pine` and preserves the corresponding
Pine accounts, funds, seeds, parameters, candle periods and 38 liquidity bots.
It exports normalized recipes, initial candles, actual bar progression and
backlog. Native timing excludes liquidity bots through
`bot_native_pine_evaluate`.

The completed comparison uses the same release binary for Pine/native/native/Pine,
1000 strategy instances, the `diverse-short` profile, 64 prewarm bars, 20 seconds
of warmup and 30 seconds of measurement, at a 50 ms scheduler interval. Pine
uses its existing direct Rust interpreter via `--pine-in-process`.

The user authorized coordination with the other local MarketForge optimization
task. Every measured round reports `compiler_quiet=true` and
`no_parallel_benchmark_server=true`. Each population has 1000 strategy bots plus
38 fixed liquidity bots. All 1000 strategies initialized and advanced through
their lookbacks in every round, with no bot or input-feed errors.

| Round | Steps/s | Average logical cores | Sampled peak RSS, MiB | Mean strategy state, bytes | Instances behind / max bars |
|---|---:|---:|---:|---:|---:|
| Pine 1 | 14.384 | 6.972 | 3741.5 | 59745 | 612 / 2 |
| Native 1 | 20.006 | 0.835 | 338.7 | 5560 | 152 / 1 |
| Native 2 | 20.007 | 0.846 | 373.1 | 5561 | 0 / 0 |
| Pine 2 | 13.975 | 6.431 | 3813.6 | 58624 | 631 / 2 |

The two-case medians are 14.180 versus 20.006 steps/s: a **41.1% increase** in
this capped asynchronous workload. Average logical cores decrease from 6.702 to
0.840 (**87.5%**), sampled peak RSS from 3777.5 to 355.9 MiB (**90.6%**), and mean
serialized strategy state from 59184 to 5560 bytes (**90.6%**). CPU/RSS samples
cover the requested measurement window and exclude startup/prewarm/warmup.
State sizes come from final compact JSON, excluding liquidity bots.

Both Pine rounds fail the configured/literal 20-step/s gate and zero-backlog
gate. Native 1 meets the speed gate but fails zero backlog; Native 2 passes all
applicable gates. The native rate is limited by the configured 20-step/s clock;
these results do not establish its maximum capacity or consistent zero backlog.

Measured decision wall means are 6.891 and 6.345 ms for Pine, versus 0.212 and
0.215 ms for native. Invocation counts differ (25997/24774 versus 40354/42617).
Counters exclude warmup but include final quiescence, and event/account wakes can
include different proportions of no-new-bar calls. Their ratio is not a pure
indicator-compute speedup. The independent empty-book place HTTP probe has P95
62.2/131.5 ms for Pine and 18.3/18.2 ms for native. These are per-round probe
percentiles, not bot intent-to-fill latency or pooled percentiles.

The outer runner's initial byte-hash guard failed because equivalent normalized
JSON objects had different key insertion orders. All four parsed recipes compare
equal, and sorted compact JSON hashes match; initial candle files also match.
The original failed guard receipt remains in `abba-20x/comparison.json` and
`abba-20x.log`; `comparison-canonical.json` records the semantic equality and
both raw/canonical hashes. A subsequent metadata-only repair makes future
normalized recipe exports deterministic, with serialized-equality tests. Actual
workloads and measured binaries were unchanged. The JSON report retains the
measurement source manifest separately from the final metadata repair.

This short run qualifies dispatch/state load for this recipe.
The sustained input trades at a fixed price of 100; strategy action activity can
be limited after warmup. Subsequent asynchronous
trajectories and work counts can differ even when initial inputs match.

## Repairs and remaining boundaries

Validation caught and repaired two implementation issues:

1. Default JSON float parsing changed a serialized MA value on restoration.
   Enabling `serde_json/float_roundtrip` preserves indicator state exactly;
   the strict JSON roundtrip assertion remains in the unit suite.
2. The native status guard rejected paused manual clock steps. It now explicitly
   accepts `Running` and `Paused`, with a regression for all three strategies.

Initial harness attempts and failed logs are retained under
`.local/native-pine-20261013/`. They also include an external-worker PATH setup
failure, an incorrectly named PostgreSQL test environment variable, formatting
and clippy findings, and paired-room clock/order-identity assertion corrections.
Only the final successful runs count as qualification evidence.

The current ports are spot-only and long-only. Historical audit checks become
bounded once the host rolls the candle window: only the saved 64-bar overlap's
intersection is checked, so older corrections can escape audit even when older
candles still appear in the supplied window. A restored bot requires its saved
cursor and retained overlap. Cold history must be complete and within
`history_limit`; resumed state can continue beyond it. Own-fill history must
remain complete, and 4096 or more own fills are
refused. Tick values and numeric magnitudes are restricted to the declared
exact integer domain. Arbitrary scripts, perpetuals, short entries and unlimited
lifetime recovery remain outside this qualification. Thirty-to-sixty-minute
stability, PostgreSQL load capacity and market-quality acceptance remain open.

## Reproduction and receipts

Base checkout: `5a3e5f4b302b1dbac9ac4f71308c1733afb1bb48` plus this uncommitted
change. The pre-existing staged strategy-library files were preserved.

```powershell
$env:PATH = "$PWD\.venv\Scripts;" + $env:PATH
$env:CARGO_TARGET_DIR = "$PWD\target\native-pine-20261013"
$env:MARKETFORGE_TEST_DATABASE_URL = '<disposable test PostgreSQL DSN>'
$env:MARKETFORGE_REQUIRE_POSTGRES_TESTS = '1'
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
cargo build --release -p exchange-server --features pine-in-process,bench-execution-timing --bin exchange-server --example native_pine_probe --example pine_inprocess_probe --locked

$env:MARKETFORGE_NATIVE_PINE_PROBE = "$env:CARGO_TARGET_DIR\release\examples\native_pine_probe.exe"
$env:MARKETFORGE_PINE_PROBE = "$env:CARGO_TARGET_DIR\release\examples\pine_inprocess_probe.exe"
$env:MARKETFORGE_TEST_SERVER = "$env:CARGO_TARGET_DIR\release\exchange-server.exe"
$env:MARKETFORGE_REQUIRE_NATIVE_PINE_TESTS = '1'
$env:MARKETFORGE_REQUIRE_INPROCESS_TESTS = '1'
$env:MARKETFORGE_REQUIRE_PINE_TESTS = '1'
$env:MARKETFORGE_TEST_PINE_IN_PROCESS = '1'
$env:MARKETFORGE_REQUIRE_POSTGRES_TESTS = '1'
.venv\Scripts\python.exe -m unittest discover -s python/tests -p test_native_pine.py -v
.venv\Scripts\python.exe -m unittest discover -s python/tests -p test_pine_inprocess.py -v
.venv\Scripts\python.exe -m unittest discover -s python/tests -p test_native_pine_http.py -v
.venv\Scripts\python.exe -m unittest discover -s python/tests -p test_native_pine_benchmark.py -v

# Run each population serially on a quiet machine, in Pine/native/native/Pine order.
.venv\Scripts\python.exe scripts/benchmark_strategy_runtime.py --server "$env:MARKETFORGE_TEST_SERVER" --output .local/native-pine-comparison/pine-1 --counts 1000 --population pine --pine-profile diverse-short --sustained-input --prewarm-bars 64 --warmup 20 --seconds 30 --interval-ms 50 --require-diagnostics --pine-in-process
.venv\Scripts\python.exe scripts/benchmark_strategy_runtime.py --server "$env:MARKETFORGE_TEST_SERVER" --output .local/native-pine-comparison/native-1 --counts 1000 --population native-pine --pine-profile diverse-short --sustained-input --prewarm-bars 64 --warmup 20 --seconds 30 --interval-ms 50 --require-diagnostics --pine-in-process
```

The adjacent [machine-readable receipt](2026-10-13-native-pine.json) records exact
source/binary hashes, test evidence and compact-state sizes. Raw local artifacts
remain under `.local/native-pine-20261013/`.
