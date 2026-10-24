# Pine persistence and source admission

Date: 2026-10-24. Worktree: `MarketForge-tuning`; branch
`feat/endogenous-market-tuning`; starting HEAD
`b494d636fbe45ee3efd0480eec0a560857b2f4a1` plus the previous, preserved dirty
changes. This round is uncommitted and does not modify the primary checkout.
The final audit checks all 4,204 frozen starting files: only the five intended
existing source/test files changed, with no missing files. Three new Rust files
and this report/evidence bundle complete the retained changes.

## Changes and boundaries

1. **Schema-4 incremental journal records.** Pine replay states previously missed
   the schema-2/3 journal optimization. Each changed state carried the entire
   compressed historical archive again. The journal now carries newly appended
   chunks, the archive header and bounded live tails. Complete predecessor
   fingerprints, patch checksums and atomic installation remain required.
   Initialization, corrected/truncated history and identity/format changes use
   complete state. Rollover remains incremental when the archive prefix is exact.
2. **Independent source checks can proceed concurrently.** The observation-wave
   map formerly held its mutex through source-file reads for every script. It now
   holds an `Arc<OnceLock<bool>>` per host/script and releases the directory mutex
   before I/O. The same key still shares exactly one success or rejection within
   one immutable wave. Public Host calls remain fresh, other hosts remain
   independent, and each new wave rechecks its sources.

The research runner also reports optional per-script call time and separates
gateway, audit and feedback time inside submission. These are diagnostics, not
alternative execution paths. `bot_request_clone` is a legacy metric label:
direct calls include route/source admission and its waits, so it is not evidence
of expensive state copying by itself.

The strategy sources, capability admission, inputs, gateway, risk, matching and
order submission rules retain their previous behavior. The existing explicit
in-process switch remains. Forty sources / 503 instances still require Python;
this round does not admit exits, conditional entries or `strategy.order`.

The new reader accepts old Pine journal hints. Older readers reject the added
schema-4 replay field; writer and recovery binaries must therefore be upgraded
together. Rolling back only the binary after writing these records is not
qualified. PostgreSQL performance and database process-crash recovery were not
tested in this round.

## Retained online result

Final nested forward/reverse comparison, two runs per version. Every run has zero
compiler-overlap samples. Each version already executes this admitted cohort in
Rust. P95 means below are averages of the two run percentiles, not a pooled
percentile. RSS is the average sampled whole-process-tree peak.

| Version | Accepted bar close → durable P95, two runs (ms) | Mean P95 (ms) | Mean peak RSS (MiB) | Journal operation, summed 45 s window (s) |
|---|---|---:|---:|---:|
| Before this round | 185.48 / 164.70 | 175.09 | 818.96 | 11.78 |
| Incremental journal only | 122.53 / 132.19 | 127.36 | 603.41 | 5.21 |
| Journal + independent source checks, retained | 118.95 / 129.94 | 124.45 | 628.17 | 5.19 |

The final mean P95 is 28.9% lower and mean peak RSS 23.3% lower than the paired
baseline. The journal step removes most of that end-to-end cost: its own summed
operation time falls 55.8%, serialization 71.3%, and compression 68.1%.

The source-concurrency step lowers summed admission time from 81.93 to 20.26 s
and summed dispatch waiting from 882.38 to 647.15 s. Bar-close-to-decision P95
falls from 60.16 to 45.22 ms. Its extra improvement in durable P95 is only about
2.3%; the compute gain does not transfer proportionally to the ordered writer.
Peak memory rises about 4.1% relative to the journal-only version. This tradeoff
is retained for its reduced decision and queueing latency, without claiming a
sustained-throughput increase.

Generated-action receipts and decision outcomes are complete; all measured
instances initialize, no bot errors or skipped-bar decisions occur, and trace
shutdown completes. Nevertheless, all 201 Pine instances are one closed bar
behind at the retained runs' exact scheduled cutoff. Post-cutoff drain clears
them. **The strict no-backlog-at-cutoff gate still fails.** These 45-second runs
are not sustained-load qualification, and the 501-agent online results are not a
3300-agent online capacity claim.

## Remaining measured bottlenecks

The final frozen offline run completes the original 3300-bot recipe in 36.69 s,
with 11.2 s in the plugin stage and 9.1 s inside the trading gateway. The long
324-bot recipe takes 38.91 s, with 19.2 s in plugins and 9.5 s in the gateway.
These single diagnostic runs are not speedup comparisons: the research runner
does not exercise the production incremental journal or observation-wave source
cache. Its five final receipt files match the unchanged frozen references exactly.

- **Remaining Python capability coverage:** 40 scripts account for 89.02 of
  160.55 summed plugin-call seconds in the 3300-bot run, about 55.4%, from 24.6%
  of calls. The top three are `fcbb24e7080c73f4`, `d4e09690084cd781`, and
  `660e65aadf0a9b74`, all excluded for `strategy.exit`. In the long recipe,
  `3964e98f6646fd00` and `179a641fbb9d2f08` lead the ranking, also exits. An exit
  adapter is therefore a better migration target than blindly admitting all
  remaining sources. These seconds include Rust Pine execution and worker waits;
  they cannot all be counted as removable Python overhead.
- **Typed Rust input boundary:** even direct calls build a JSON `Value` from the
  typed observation before invoking the Rust broker. One final 45-second run
  spends 13.99 summed seconds there, versus 7.46 in VM execution and 2.74 in
  working-state cloning. A typed borrowed input path deserves evaluation before
  another small archive-encoding cache. These timers overlap concurrent work and
  do not imply a 13.99-second end-to-end saving.
- **Gateway and ordered persistence:** 9.1 of 9.5 submission seconds in the scale
  diagnostic are inside `OrderGateway`; feedback is below the report's 0.1-second
  precision. Per-command audit output is disabled in these timing runs, so its
  near-zero counter says nothing about production audit cost. The gateway needs
  finer matching/risk/account-state attribution before changing its algorithms.
  Online, the source speedup's small durable-latency effect keeps ordered state
  validation, journal work and publication on the critical path.

## Rejected experiment: cold archive encoding cache

A separate candidate shared immutable archive JSON encoding between private VM
stamps, public seals and response-size checks, with complete byte comparisons
before reuse. It preserved complete receipt bytes and passed its correctness
tests. Nevertheless, the repeated polling control regressed, and the final
working tree **does not contain this cache**. Its source snapshot, binaries and
measurements are retained as negative evidence.

| Fixed offline recipe | Journal-only before (s) | With cache (s) | Wall change | Runs per version |
|---|---:|---:|---:|---:|
| Long history, 324 bots | 41.67 | 40.29 | -3.31% | 3 |
| Original 3300 bots | 45.88 | 46.05 | +0.36% | 2 |
| Related/timeframe | 5.64 | 5.49 | -2.55% | 2 |
| Polling | 5.88 | 6.54 | +11.15% | 3 |
| No Pine | 4.51 | 4.20 | -6.91% | 2 |

These are means of compiler-free runs. Polling was slower in all three adjacent
pairs; the first pair was particularly noisy. Some excess time appeared in the
unchanged gateway/native phases, so this does not prove all of the regression
was caused by cache logic. It does mean the cache failed the retention gate;
lower encoding counters alone were not sufficient. All 24 before/after receipts
in this experiment match their frozen full-file SHA-256 values.

Working-state copying, full seal hashing, growing VM history and linear cold
replay remain in the retained implementation. This round does not claim a
completed typed hot/cold state redesign.

## Verification

- Retained server library suite: **391 passed, 12 ignored**. The ignored cases are not
  counted as executed qualification.
- Python adapter/runtime suite with the frozen private runtime: **89 passed**.
- **201 exact sources × 32 comparisons = 6,432**, through 512 bars, including
  rejected receipts, cold execution, speculative retries and Python-state
  migration at bar 129; all pass with a five-second probe deadline.
- Real gateway matrix: continuous Python, continuous Rust, Rust restarted at
  steps 70/140, and Python → Rust → restarted Rust all finish 210 steps. Every
  mode has identical per-command audit bytes and economic receipts. Rust's
  complete durable final state also matches after OS restart. Audit SHA-256:
  `76bbcf3a57efc25aa509055991226de8e2fd6959f8602c6be84aef395de23a54`.
- New memory-journal tests reconstruct exact schema-4 state through tail updates,
  rollovers and multi-chunk appends, reject tampering/stale predecessors without
  partial installation, and verify that serialized records retain no old state
  owners. An actual admitted Pine program continues byte-identically in a new
  Host after journal recovery through 257 bars, including discarded speculative
  responses. This complements the OS gateway test, whose research checkpoint
  format does not itself exercise the production scheduler journal.
- Concurrency tests prove that two different script reads can both enter before
  either finishes, while same-key reads, cached rejection, host isolation, source
  edits and scope restoration keep their existing contracts.

Cross-runtime comparisons exclude only the execution-storage fields already
enumerated by `pine_receipt_projection.py`; all economic fields and the separate
per-command audit remain exact. Same-format before/after research comparisons
require the complete receipt file's SHA-256, with no storage projection.

## Measurement method and retained failures

Each implementation step has a frozen release executable with identical
`pine-in-process,bench-execution-timing` features. The offline recipes, seeds,
step counts and intervals are unchanged. HTTP measurements use 201 admitted Pine
scripts plus 300 native agents, five seconds of warmup and a 45-second window at
250 ms ticks. The clean online sequence is baseline → journal → archive cache →
source concurrency → source concurrency → archive cache → journal → baseline
for the exploratory series. After rejecting the cache, the retained sequence is
baseline → journal → journal plus source concurrency → the same → journal → baseline.
Timings of overlapping phases are not added as if they were disjoint CPU time.

An unrelated Pine project compiled during the first online repeat sequence and
the first final diagnostic sweep. Those runs retain correctness evidence but
are excluded from formal performance comparisons. Their compiler-presence
counts remain in the JSON. Clean repeats check for active compilers before each
run and reject any run with overlapping compiler samples. Other user applications
were not stopped. Two observations per version are bounded local measurements,
not statistical or sustained-capacity guarantees. Online asynchronous markets
produce different accepted-action counts, so latency samples are not identical
trades; exact trading consistency comes from the frozen offline/recovery gates.

The first new journal/Pine integration test omitted the ecosystem quantization
setting and correctly failed admission; the fixture was repaired. The first
Python differential launch omitted the private-runtime path and loaded public
`0.3.1`, which rejected unsupported external-order arguments. It was rerun with
the unchanged private `0.3.1+mf1` binary, SHA-256
`4e12faa2992f5934f8c7fdc875fe6858ea56bc469d89e2b2fc203286d6b54075`.
Both initial failure logs are retained, separately from passing results.

## Reproduction and evidence

Set `RESEARCH_SCRIPT_PROFILE=1` together with `RESEARCH_PROFILE=1` to emit the
per-script diagnostic rows. They sum call wall time, including waits and Rust
Pine execution; they are not Python CPU time or removable Python overhead.

Local raw receipts, resource samples, traces, failed runs, frozen executables and
the starting source overlay are under `.local/macro-20261024/`. The companion
JSON records full results and hashes. The input bundle contains the isolated
patch, before/after changed sources, recipes, plans, scripts and compact evidence.

```powershell
cargo test -p exchange-server --features pine-in-process,bench-execution-timing --lib
cargo build --release -p exchange-server --features pine-in-process,bench-execution-timing `
  --bin exchange-server --example strategy_ecosystem_market --example pine_inprocess_probe
$env:PYTHONPATH = 'bot-plugins/pine-strategy/.runtime/strategy-ecosystem;python;python/tests'
python scripts/qualify_pine_direct.py --probe <probe.exe> --bars 512 `
  --replay-state --check-migration --timeout-ms 5000 --output <new-directory>
python scripts/validate_pine_direct_recovery.py --exe <research.exe> --cwd <frozen-python-root> `
  --recipe <long24.json> --segment-steps 70 --replay-state --require-direct-segments `
  --output <new-directory>
python scripts/benchmark_research_optimizations.py <plan.json> <new-directory>
```

[Machine-readable evidence](2026-10-24-macro-bottlenecks.json) ·
[Reproduction inputs and isolated patch](2026-10-24-macro-bottlenecks-inputs.tar.gz).
