# Background market participants — 2026-10-08

Adds five `bot.v1` builtins alongside the unchanged legacy five: dynamic market
making, value/information, trend/exit, adaptive noise and cumulative TWAP.
The web and Python provisioning script consume the same Rust-exported finite
spot recipe, with 20 exclusive background accounts and two human accounts.
The web creates and starts this recipe; existing rooms retain their configuration.

## Validation

- `cargo test --workspace`: 352 reported passes, zero failures (199 core,
  142 server, 11 adapter/contract tests). Database tests can return early without
  a configured PostgreSQL test URL; this is not a fresh database integration pass.
  Windows commands prepend `.venv/Scripts` to PATH. One final rerun without that
  setting failed three existing process-plugin tests with Python alias exit code
  9009; that log is retained separately and the configured run passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`, `git diff --check`: passed.
- `.venv/Scripts/python.exe -m unittest discover -s python/tests -v`:
  47 tests reported, 12 skipped, zero failures. Missing database and explicitly
  enabled external/container dependencies account for the skips.
- `npm.cmd --prefix marketforge-web run build`: passed (TypeScript and Vite).
- New local HTTP test: a separate loopback server with in-memory storage and
  a test bearer identity created the shared recipe and ran automatic mode to at
  least 80 simulation steps. All 20 bots had saved state, `bot_errors` was empty,
  public trades were observed, and those recent trades had distinct maker/taker
  accounts. The test pauses the room and terminates its server afterwards.
  This verifies the real API and worker, not a browser click or a deployment.

Fifteen new core tests cover:

- catalog bounds, cross-parameter validation, and invalid saved state;
- target-dependent inventory quote skew, resting exposure, conservative cash
  reservation, volatility-dependent spread/size and withdrawal;
- order age, delayed private valuation changes, trend warmup and neutral exit;
- buy/sell execution progress after actual fills, retry after no fill, bounded
  quantities, price protection, start delay and hard decision-time deadline;
- margin-call reduce-only behavior, paused/duplicate observations, and own-order
  crossing avoidance without assuming a cancellation succeeded;
- RNG/history/plan state roundtrips for all five strategies;
- shared-fixture parity and exclusive funded account assignment;
- seeds 7, 19 and 41: actual flow from all five strategy types, no self trades,
  bounded positions, nonnegative available spot balances, conservation of
  inventory and cash plus cumulative fees, and exact execution progress;
- full-room snapshot recovery at saved-decision, before-submit and after-submit
  crash points. Order IDs, scheduler state and all execution receipts match the
  uninterrupted run. Only the serialized `seen_order_ids` HashSet is sorted for
  snapshot comparison; price queues, commands and events retain their order.

## Controlled synthetic experiment

`background_market` example, 300 manual steps (300000 ms simulation time):

| Seed | Trades | Cancellations | Rejections | Trade-price range | Last price |
|---|---:|---:|---:|---|---:|
| 7 | 538 | 551 | 1 | 97–129 | 114 |
| 19 | 444 | 376 | 0 | 86–111 | 87 |
| 41 | 508 | 575 | 0 | 97–120 | 113 |

Seed 7's one rejection was `PostOnlyWouldTakeLiquidity`; the receipt is retained,
and no risk check was loosened to force acceptance. Its buy and sell execution
tasks each filled 40/40. Seed 19's buy task filled 38/40 and stopped at its deadline,
while its sell task filled 40/40. These are different legitimate outcomes with
finite liquidity; an unfilled remainder is not reported as completion.

The derived seeds are masked to 53 bits so JSON number handling in the browser
does not silently round the shared recipe's initial seeds. Full RNG state remains
server-owned and versioned. `initial_position` is stored as a decimal string to
preserve the engine's i128 account range through bot-state JSON.

Commands and JSON receipts are retained under `.local/background-*`, including
per-bot fill quantities, taker quantities, rejection reasons and final states.
Earlier failure receipts are retained: the first TWAP schedule exposed fractional
slice rounding, repaired with ceiling entitlement without extending the deadline;
raw snapshot comparison exposed unordered HashSet serialization, verified to be
the sole difference before canonicalizing that set.

## Limits

This establishes functioning synthetic participants and bounded recovery,
not calibration to a real market. The private valuation shift is a preconfigured
scenario, not a public news feed. There is no new network-latency model,
cross-market index/mark feed, funding-rate process, spot/perp arbitrage or
large-population performance qualification. The price-change EWMA is measured
in integer ticks per observation, not an annualized volatility estimate.
TTL and execution deadlines are checked at observation/decision time; there is
no matching-engine expiry or `valid_until` revalidation of late live decisions.
All acceptance servers and simulations are isolated; existing user rooms were
not replaced, funded or restarted.
