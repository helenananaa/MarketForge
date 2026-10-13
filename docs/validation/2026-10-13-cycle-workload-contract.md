# Cycle input and sustained action coverage

This is a new workload qualification. Existing flat-input ABBA results keep their
original recipes and interpretation. The default `--input-profile flat` still
submits one sell and one buy at tick 100 per observed simulation step.

`--input-profile cycle --sustained-input` changes only the submitted limit price:

```python
phase = (market_time_ms // 5_000) % 80
price_tick = 80 + min(phase, 80 - phase)
```

The limits move from 80 to 120 and back every 400 simulation seconds, about 40
wall seconds at the 10x target. The phase follows market time, not HTTP request
count. Each side still submits one lot, uses the same finite funded accounts,
and cancels any remainder through the gateway. Prewarm behavior is unchanged.
Better resting orders can execute at a different price. A limit-price cycle is
not proof of a traded-price cycle.

For cycle runs with `--require-bot-receipts` and a requested measurement of at
least 60 seconds, every 60-second bar-close-anchor cohort must contain at least
20 accepted durable actions, including at least one each from `ma.pine`,
`rsi.pine`, and `breakout.pine`. Each cohort's accepted P95 must meet the configured
limit, which is 250 ms in this contract. Rejected actions do not count. Recipe
participant identities determine the source; non-bar account wakes do not count.
The maximum gap between accepted bar anchors, including the measurement start
and end, must be at most 60 seconds. A final remainder under five seconds merges
into the preceding cohort. Terminal outcomes after a cohort remain attached to
their original bar anchor, with their full latency. Cutoff progress still accepts
only commits published at or before the cutoff.

These checks supplement complete outcomes, no skipped decisions, runtime route,
clock/resource coverage, compiler isolation and other existing checks. They do
not require every individual bot to trade every minute. Ten-second correctness
smokes retain coverage diagnostics without applying the minute-sample gate.
`--long-run` requires receipts and qualifies only with cycle input. A standalone
reporting run can still exit zero while recording failed qualification fields;
inspect the report rather than treating process exit as capacity acceptance.

The report's `feed_execution` summarizes actual `TradePrinted` events separately
from requested limits. `pine_ledger_fills` summarizes actual final Pine own-fill
ledgers, deduplicating market trades shared by two Pine counterparties. These
final fill summaries include warmup and drain; they are not exact wall-clock
cutoff or per-minute fill evidence. Accepted IOC admission alone does not prove a
fill. Pilot qualification and actual traded-price behavior must be reviewed
before freezing a long run.

Run the independent 120-second pilot in a fresh directory with no concurrent
compiler or server. Use the intended frozen candidate binary and plugin inputs:

```powershell
python scripts/benchmark_strategy_runtime.py --server <candidate-server.exe> `
  --plugin-dir bot-plugins --output <fresh-pilot-directory> `
  --population pine --pine-profile diverse-short --counts 1000 `
  --pine-in-process --pine-vm-checkpoint --sustained-input --input-profile cycle `
  --prewarm-bars 64 --warmup 20 --seconds 120 --interval-ms 100 `
  --require-diagnostics --require-bot-receipts
```

The 35-minute candidate run uses `--counts 1000 --seconds 2100 --long-run` with
the same cycle and receipt flags, a separately frozen contract, and the 4096 MiB
server memory guard. At 10x its finite feed budget is 23,320 iterations; the
23,832-lot sell allocation covers the 384 prewarm iterations plus that budget,
and the 10,000,000 cash allocation exceeds maximum buy notional at tick 120.
This remains memory-journal throughput; PostgreSQL capacity is separate evidence.
