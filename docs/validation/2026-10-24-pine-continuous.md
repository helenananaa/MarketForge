# Continuous Rust execution for the admitted Pine cohort

Date: 2026-10-24. Worktree: `MarketForge-tuning`, branch
`feat/endogenous-market-tuning`, starting HEAD
`b494d636fbe45ee3efd0480eec0a560857b2f4a1` plus the preserved previous-round changes.
This change remains uncommitted. It does not change the primary MarketForge checkout.

## Scope and implementation

With `pine-in-process` and `MARKETFORGE_PINE_IN_PROCESS=1`, the existing 201 exact
ecosystem sources now remain in Rust after 128 bars. This covers 2497 of the
3000 Pine instances in the unchanged 3300-bot recipe. The other 40 sources / 503
instances still use Python because their capabilities have not been admitted.
No scripts, input defaults, exchange-grid normalization, order ordering, gateway,
risk, matching, or authoritative receipt rules were replaced.

Schema 4 stores all historical external inputs and historical intents as LZ4
compressed immutable chunks, plus a short tail. Floats preserve their exact bits.
The final candidate archives after at least eight passes; original append-batch
boundaries remain in the log even when a batch exceeds that threshold. A warm
session appends new input only. A cold session replays every original batch and
checks its intents. **This is a replay log, not a constant-size VM snapshot.**
The six original checkpoint-qualified sources retain their existing schema-3
VM snapshot implementation and schema-2 compatibility contract.

Legacy ecosystem schema-2 state is migrated only after its full transcript has
been validated and all historical intents have been reproduced. Economic broker
state and already-produced actions are preserved. Existing schema-4 state cannot
fall back to Python when source admission or direct execution is unavailable.
`MARKETFORGE_PINE_REPLAY_STATE=0` retains the old routing path for new/legacy state
as a comparison mode; it does not downgrade an existing schema-4 instance.

Both the production scheduler and research runner use the existing authoritative
candle-prefix chain to send a suffix instead of the full history. Public candle
windowing is separate from complete private fill/account feedback. The research
runner queries these cursors only for Pine, avoiding snapshots of native bots.
All 201 admitted sources were checked for both `request.*` and legacy `security()`
calls; none contain them. No request-capability admission was added here.

## Performance

| Workload | Before (s) | After (s) | Wall change | Peak tree RSS before → after (MiB) |
|---|---:|---:|---:|---:|
| Long history, 324 bots | 53.12 | 40.76 | -23.3% | 1965 → 1314 |
| Original 3300-bot recipe | 41.66 | 42.57 | +2.2% | 2631 → 2336 |
| Related/timeframe control | 6.27 | 6.31 | +0.6% | 818 → 810 |
| Polling control | 6.67 | 7.13 | +6.9% | 865 → 890 |
| No-Pine control | 4.91 | 4.66 | -5.0% | 329 → 372 |

Negative changes are improvements. All 20 completed runs match their frozen economic receipts. Short-history scale and polling do not show a speed improvement; the opt-in switch remains.

### Bounded HTTP observation

201 admitted Pine strategies plus 300 native agents; 5 s warmup, 45 s measurement, 250 ms ticks. Both runs crossed 128 bars. One run per variant; asynchronous market trajectories and accepted-action counts differ, so this is not an identical-trade latency sample.

| Metric | Before | After |
|---|---:|---:|
| Accepted bar-close → durable P95 (ms) | 304.91 | 203.76 |
| Measured-window peak tree RSS (MiB) | 1636.16 | 786.18 |
| Python fallback calls | 11312.00 | 0.00 |
| Instances behind at cutoff | 201.00 | 121.00 |
| Maximum backlog bars at cutoff | 3.00 | 1.00 |
| Skipped bar decisions | 2661.00 | 99.00 |

Both runs have complete generated-action and decision outcomes, no harness/bot errors, and confirmed trace drain. The new path has zero Python fallbacks in this all-admitted cohort. **It still fails the strict no-backlog/no-skipped-bar criteria**, so this is not sustained-load qualification.


The reference is the frozen previous-round direct binary with its 128-bar routing
threshold. Recipes, seeds, simulation steps, bar intervals and native liquidity
agents are unchanged. Long and scale use two observations per variant in A/B and
B/A order. Controls were repeated in reverse order after initial polling/RSS variance; each now also has two observations per variant. Wall time includes startup and teardown; RSS is
the sampled peak of the whole process tree. These are local bounded measurements,
not statistical capacity guarantees. No builds or validation runs overlap the
final performance runs. Other user applications were not stopped.

## Correctness and recovery

- 201 sources × 32 comparisons = 6432 comparisons through 2048 bars on the initial
  32-pass chunk format, including rejected receipts, cold reconstruction and
  duplicate uncommitted attempts. Probe deadline: 30 seconds.
- 201 sources × 30 comparisons = 6030 comparisons through 256 bars, including
  Python-state migration at bar 129 and cold retries. Probe deadline: 5 seconds.
- Eight-pass replay candidate: 201 sources × 32 = 6432 comparisons through 512 bars,
  including migration, cold recovery and retries, with a 5-second deadline.
- Server unit suite: 378 passed, 12 ignored. Final Python regression suite: 89 passed,
  including four replay-specific tests. Corrupt archives/seals, changed prefixes, incomplete private history,
  timeouts and oversized responses must fail without advancing committed state.
- The 600-step real gateway control, continuous Rust, and Rust restarted at steps
  200 and 400 have identical per-command audit bytes and economic receipts. Rust's
  complete final durable state also matches uninterrupted Rust. This uses the
  32-pass candidate. Its redundant fourth migration run was stopped after the
  three completed modes; it is not counted as a completed gate.
- The eight-pass replay candidate completes all four 210-step modes: Python,
  continuous Rust, Rust restarted at 70/140, and Python → Rust → restarted Rust.
  All audit files and economic receipts match; restarted Rust's entire durable
  state equals uninterrupted Rust. This includes an OS restart after 128 bars.

The cohort matrices and OS-recovery matrix preceded the final research-only
native-snapshot correction and duplicate-stream compatibility repair. Their exact
binary hashes are retained. The final binaries reran the complete 378-test Rust
and 89-test Python suites (including the new duplicate/migration case), all 20
paired research/control runs, and the HTTP probe. The replay codec and execution
algorithm did not change between the eight-pass matrix and the final build.

Cross-format comparison excludes only the enumerated execution-storage fields
in `scripts/pine_receipt_projection.py`, and `_host_dispatch`, whose event hash
includes the supplied candle window and proof. Every other broker/host field,
order, fill, cancellation, rejection, account, reservation, book snapshot and
action count remains exact. The independent per-command audit equality and exact
Rust-to-restarted-Rust state equality are required in addition to this projection.
The failed initial comparison is retained: its only remaining differences were
the 19 admitted instances' `_host_dispatch` hashes. This is file/OS-process
recovery evidence, not PostgreSQL recovery qualification.

## Experiments retained as negative evidence

- Raising the old routing ceiling alone did not solve the problem: the prior
  unrestricted-transcript experiment took 94.39 s versus 39.81 s through Python.
- Compressed replay without integrating authenticated candle windows took
  57.60 s versus 40.92 s through Python in the first local long-history comparison.
- The first window-enabled research runner also snapshotted native bots while
  looking for Pine cursors. Native decision phase time increased from roughly
  5 s to 17 s. Both 32-pass and eight-pass variants remained around 54 s overall.
  That unnecessary snapshot work was removed before the final measurements.
- The initial window attempt correctly stopped on a private-history validation
  error. Schema 4 now uses the separate authoritative private-completeness flag,
  as schema 3 already did; candle-prefix validation is still required.
- The related-market control exposed a valid host configuration that also lists
  the primary timeframe among additional streams. Schema 4 now coalesces identical
  public history and rejects conflicting duplicates, including during migration.
  The existing schema-3 rejection contract is unchanged. A regression test covers
  both valid coalescing/migration and conflict rejection; the original frozen
  related-market recipe is rerun without modifications.

## Boundaries and reproduction

No history-length downgrade does not mean unlimited resources. Full replay logs,
retained VM calculation/output state, exchange history and private ledgers still
grow. Cold Rust recovery remains proportional to historical work. State/request
size limits, complete-private-history requirements, decision deadlines and runtime
quotas remain enforced. One expensive binomial-average script made the synthetic
2048-bar differential matrix particularly slow; the matrix does not certify that
every cold restore fits the production deadline. Large research-room checkpoints
also spend substantial time rebuilding the exchange before bot execution begins.

The direct backend remains explicitly enabled by `MARKETFORGE_PINE_IN_PROCESS=1`.
This work does not qualify the 40 excluded scripts, arbitrary input overrides,
new request expressions, PostgreSQL capacity, or unlimited-duration operation.

Local raw evidence is under `.local/pine-continuous-20261024/`: frozen binaries,
the previous dirty-source snapshot, isolated patch, plans, raw receipts, audits,
resource samples, failures and logs. The adjacent JSON report records summaries
and hashes; the compressed input bundle preserves recipes, source qualification,
plans and the isolated patch. To relocate a plan, change only executable/worktree
paths and keep the frozen recipes, runtime versions, seeds and environments.

```powershell
cargo build --release -p exchange-server --features pine-in-process `
  --example strategy_ecosystem_market --example pine_inprocess_probe --bin exchange-server
python scripts/qualify_pine_direct.py --probe <probe.exe> --bars 512 `
  --replay-state --check-migration --timeout-ms 5000 --output <new-directory>
python scripts/validate_pine_direct_recovery.py --exe <research.exe> --cwd <frozen-python-root> `
  --recipe <long24.json> --segment-steps 70 --replay-state --require-direct-segments `
  --output <new-directory>
python scripts/benchmark_research_optimizations.py <final-measurement-plan.json> <new-directory>
```

[Machine-readable evidence](2026-10-24-pine-continuous.json) · [Reproduction inputs and baseline overlay](2026-10-24-pine-continuous-inputs.tar.gz).
