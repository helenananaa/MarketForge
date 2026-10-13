# Bot state throughput and recovery qualification, 2026-10-13

Current result: **v15's full 35-minute, 500-bot cycle measurement is valid,
but the sustained active-load capacity contract did not pass**. Its 5336
accepted actions have **164.947 ms P95 / 230.995 ms maximum**; none exceed
250 ms. Overall clock rate is **10.0000150467 steps/s**, all 35 one-minute
clock windows meet the configured >=9.5 threshold, and cutoff/paused backlog,
skipped bars, bot errors and harness errors are zero. Peak server RSS is
**2011.414 MiB**. All frozen-input and trace-closeout checks passed.

The failure is **activity coverage**, not an observed latency, rate, backlog
or memory-limit failure: five minutes have no MA accepted actions, and two
of those also have fewer than 20 accepted actions across all strategies.
Thus the observed requests meet the performance gates, but this run does
not establish uninterrupted sustained trading-load capacity. Memory also
continues to grow; a completed 35-minute run is not indefinite-memory
qualification. The full result and exact failed windows are retained below.

**v15's valid 1000-bot cycle pilot also failed capacity qualification**.
Its private stamp-cache patch has passed focused correctness tests and an isolated cache
microbenchmark, which also exposes a single-worker match-only regression.
**Only the stamp patch remains**: the request-size alternative passed its
correctness checks but was 25.87% slower in its fixed admission microbenchmark
and was exactly reversed. The final v15 release/locked/all-features workspace
suite passed **755 tests, with 23 ignored**; both release Clippy configurations,
format/diff checks and the frozen build passed. All three real PostgreSQL HTTP
recovery tests also passed with durability enabled. The 120-second pilot's
1295 accepted actions have **264.242 ms P95**, above 250 ms; both minute windows
also fail latency, and **299 bots are one bar behind at the scheduled cutoff**.
The paused snapshot has zero backlog, which does not erase that cutoff failure.
Two independent 500-bot pilots passed the configured short-run contract with
**146.392 / 149.572 ms accepted P95** and zero cutoff/paused backlog. The first
also meets literal 10 steps/s; the repeat measures 9.999309739 and does not.
All nine short-run offline analyses pass their input checks. The 500-bot long
attempt passed the two-pilot prerequisite re-audit and completed all 2100
measurement seconds. No 1000-bot long run, deployment or complete sustained
active-load qualification is claimed. Code remains in the isolated worktree;
the unrelated NativePine/main-checkout changes have not been merged into it.

Status: **the latest System candidate shows a substantial short throughput gain
against the original baseline, but 1000-bot sustained qualification is still
unmet**. Valid, separate flat-input ABBA groups measured median clock rate
**10.812683 → 19.501898x at 1000 Pine bots** and **14.403685 → 20.008186x at
500 bots**, with lower HTTP probe latency, CPU and sampled RSS. The two latest
500-bot flat samples have zero skipped bars and paused backlog. The 1000-bot
samples still have paused backlog and one has 21 skipped bar decisions. These
30-second, untraced flat runs do not establish the 250 ms BOT trading-latency
gate or 35-minute capacity; detailed results are retained below.

The separate **v14 same-source System/mimalloc ABBA comparison is valid;
all four 120-second, 1000-Pine cycle runs failed the unchanged latency gate**.
Accepted P95 was **468.734 / 355.965 / 456.424 / 389.058 ms** in System / MI /
MI / System order. No skipped bars or scheduled-cutoff backlog were observed;
MI round 2 and System round 4 had 298 and 301 bots one bar behind in their
paused snapshots. The mimalloc samples used more paired turn CPU and sampled
RSS; the modest difference between allocator medians for accepted latency is
not evidence of faster execution or capacity qualification. The 35-minute
prerequisite check refused to launch, with its failure receipt retained.

The earlier **v13 diagnostic pilot failed** at accepted P95 **399.781 ms**,
with **308 bots one bar behind at cutoff and 201 in the paused snapshot**.
Its new timings show substantial FIFO wait and synchronous turn CPU work,
with additional commit-side tails; they do not establish allocator contention
as the cause.

Candidate **v12 failed its 120-second, 1000-Pine cycle pilot**:
accepted P95 **314.524 ms** and **323 bots one bar behind at cutoff**, despite
passing correctness/recovery checks and reducing measured seal, prefilter and
observation time. Its valid, complete trace has no skipped bars; the later
paused snapshot has zero backlog and does not repair the cutoff result.

Candidate **v11 passed the unchanged latency, accepted-coverage and
zero-backlog gates in a separate 120-second, 500-Pine cycle sample**: accepted
P95 **134.143 ms**, with no skipped bars or cutoff/paused backlog. Its
1000-Pine pilot still **failed**, at P95 **304.178 ms** and with cutoff backlog.
The 500 case reused the frozen v11 inputs and has an unchanged postflight
comparison, but no separate prelaunch input capture. This is a short capacity
point, not repeatability, 1000-bot or 35-minute qualification.

Candidate v10 passed its correctness checks but **failed its first
120-second, 1000-Pine, 10x cycle pilot**, with accepted-action P95 560.775 ms
against the unchanged 250 ms gate. Its event-prefilter mean fell to 0.081089 ms,
but that local improvement did not establish an end-to-end gain. There were no
skipped bars or scheduled-cutoff/after-pause backlog in this pilot. Candidate v9
passed its first short pilot but **failed the subsequent 35-minute attempt**.
The memory guard stopped it after 1644.487916 seconds of observed measurement
clock publications, at 4,301,615,104 bytes. The retained partial trace shows
latency deterioration and 271 skipped bar decisions; the scheduled endpoint
and cutoff are missing. An additional ordinary short repeat produced P95
467.935 ms and cutoff backlog, so the first pilot's P95 200.519 ms is neither
sustained-capacity nor repeatable-qualification evidence. V10's cross-wave
incremental observation cache has been measured in the short pilot and serial
repeats described below. The later v14 allocator ABBA is valid but supplies no
passing 1000-bot or 35-minute qualification. The serial v9/v10/v10/v9 cycle comparison has a truncated
trace in its second run and is not a valid complete ABBA; the other three runs
also missed the latency gate. Earlier failed attempts and partial results retain their original
candidate identities; candidate v2 measurements are not attributed to later
candidates. Accepted-action coverage and actual fill coverage remain separate.

A follow-on cadence experiment restored all tested states correctly but rejected
the proposed **999-delta tail**: pure JSON decode/apply took up to 32.728 seconds,
already beyond the current 25-second readiness budget. A tighter **499-delta**
candidate subsequently took **9.188–11.519 seconds** for pure decode/apply with
exact state equality. This establishes a bounded synthetic recovery result,
not PostgreSQL startup, gateway readiness or throughput qualification.

Candidate **v11 also failed its 120-second, 1000-Pine pilot**: accepted P95 **304.178 ms**,
with 334 bots one bar behind at the scheduled cutoff and 78 at the paused
snapshot. Its measurement and trace closeout are valid, all decisions have
terminal receipts, and no skipped bars were observed. Full-validation frequency
and aggregate writer occupancy fell, but the latency/backlog gates remain unmet.
The frozen candidate passed 731 Rust tests (22 ignored), both Clippy
configurations, 50 Python tests, five isolated shutdown races and four real
HTTP-process closeout cases. PostgreSQL fixture recovery passed all five paths;
the separate HTTP restart group had two passes and one Windows connection-abort
failure. A separate retry of that exact failed method passed in 29.274 seconds;
the original failure's cause remains unconfirmed. These correctness results do
not establish capacity.

## Provenance

- Base: `5a3e5f4b302b1dbac9ac4f71308c1733afb1bb48`.
- Isolated managed worktree: `E:\CodexData\worktrees\bot-state-throughput\MarketForge`.
- Raw receipts: `.local/bot-throughput-20261013/` in that worktree.
- Baseline executable SHA-256:
  `4f27efbe699594a9188891873fa887125185f8048743cd4f30fe20ae264be401`.
- Candidate v2 executable SHA-256:
  `7d98ce70dda543ef58bc32a68c5f44731e92ae328e81367b851f5a0cdf0e96d6`.
- Candidate v3 executable SHA-256:
  `23fc71f06200f956bc599bcf1886dd8d6a9ba9eb68ab4e022fadc840646f1bd8`.
- Candidate v9 executable SHA-256:
  `654891d8aa629321e7c5dd4528145aeaf94af9ba35f36e2cc4942a0e8809179b`.
- Candidate v10 executable SHA-256:
  `a10331a59a09d92b8f84569279220cb76f6ce92571f08319eeac936086c73e0a`.
- Candidate v11 executable SHA-256:
  `9592d4129bc5c27f0be6c104a76dbd298b6a40c004877f00ccb0b870bea7076d`.
- The benchmark binaries use release, locked dependencies and
  `pine-in-process,bench-execution-timing`. Baseline plugin sources are from the
  base commit. They use the same private Python runtime binary (SHA-256
  `8ef5900e5bfeadfbced7edef772e8b0a00188215448d4417f869b29d09c9c427`).
- Main checkout's staged research and NativePine work were not included or edited.
  No commit, push or production deployment is part of this task.

## Implemented behavior

1. Runtime route and evaluation-mode counters record actual direct/process/native
   execution. Optional durable receipts connect each decision to committed action
   outcomes and the successful clock publication that closed its input candle.
   Cutoff progress is separate from subsequent pause/drain.
2. Existing schema-2 Pine states use authenticated append deltas for unchanged
   historical prefixes. Unsupported changes use full state. Validation occurs
   before mutation, and in-memory history stores compressed wire data without
   retaining prior/next state trees.
3. New eligible instances can opt into schema 3: real Pine VM checkpoints every
   32 bars plus a bounded replay tail. Retention analysis specializes immutable
   integer inputs without replacing executable Pine code. Cold restoration
   verifies checkpoint identity, integrity and replayed intents.
4. Qualified initialized instances request a 64-bar observation window, expanded
   for catch-up. The exchange supplies a complete historical-prefix proof;
   changed, unavailable or excessive history fails closed. Private fill-history
   completeness is checked independently.
5. Strategy-only commits retain ordered durable installation while allowing
   ordinary same-room order preparation to proceed. Risk/automation paths keep
   their existing fallback, and venue FIFO remains enforced.
6. V9 reads and verifies current source once per direct admission. A private
   host/script-bound token carries that verification through the same attempt;
   public decision calls verify afresh. This is not a TTL or mtime cache.
   Direct execution errors do not fall back to a process, and schema 3 refuses
   the process path.
7. V10 retains an incremental observation projection across compute waves to
   avoid rescanning unchanged history. Its cold/fallback paths preserve the
   existing history, proof and trading semantics. The pilot below qualifies
   neither overall latency nor long-duration capacity.

Candidate v3 also encodes schema-3 state changes as authenticated append deltas
when the VM checkpoint and old tail are unchanged. The complete predecessor
digest binds the inherited checkpoint; both tail arrays append exactly, and
the remaining metadata replaces its previous value exactly. Checkpoint rollover
or a modified prefix uses a complete state. The reconstructed execution seal is
unchanged. Candidate v2 used compact full schema-3 payloads instead. Periodic
full scheduler checkpoints remain.
Own-fill accounting still retains historical fills and has the existing 4096
private-history limit. This is not arbitrary-script or unlimited-lifetime
qualification.

## Correctness evidence

- Full v3 release workspace/all-features suite: **679 passed, 15 ignored**, no
  failures, using a fresh PostgreSQL database with durability enabled
  (`workspace-v3-tests-2.log`). The initial v3 attempt omitted the repository
  Python directory from PATH and failed process-worker tests with exit 9009;
  its log is retained. The passing rerun corrected the environment only.
- Default and all-features release Clippy, all targets: passed with warnings
  denied. Formatting and patch whitespace checks passed.
- Python receipt/cycle/provenance diagnostics: **41 passed** in the latest
  focused run (28 receipt, 5 cycle and 8 wrapper tests). The earlier 3 duration
  and 2 process-sampling tests passed separately.
- Nested rejection classification tests cover Spot and Perp and are included
  in the 679-test workspace result.
- Python/Rust/HTTP differential suite: 17 existing cases passed. The new schema-3
  PostgreSQL restart case subsequently passed after correcting its cancellation
  fixture and mapping independently allocated cross-room order IDs through real
  creation receipts. Same-room restart identity comparisons remain exact.
- Fixed complete-trade receipt: 59 steps, hash
  `48c26aa5e50e9a8417e8ce7de464eb4cff0a511f65288bb686e0d3a15024ecfa`.
- New PostgreSQL case validates checkpoints at 32/64, bounded-tail recovery,
  cancel/replacement, partial fill and close: two actual fills and final position
  zero. `fsync`, `synchronous_commit` and `full_page_writes` were on. The isolated
  PostgreSQL server was stopped after verification.
- A separate **live/auto** PostgreSQL case passed on frozen v3 in 9.726 s
  (`schema3-live-pg-v3-run1.log`). SQL found 33 actual version-2 schema-3 tail
  append records, including records after the latest full scheduler checkpoint.
  After pause, the scheduler had evaluated 35 bars (checkpoint 32 plus tail 3).
  Restart restored the complete scheduler JSON exactly; a new real gateway bar
  advanced evaluation to 36, wrote a new append and placed Buy 97 x 2. Metrics
  observed checkpoint restoration and no process/blocked route. This test uses
  no manual bot or clock stepping. Receipt:
  `target/pine-http/rust-pine-live-delta-postgres-1791860704499568100.json`.

The initial 100-Pine correctness smoke passed route, schema, complete receipt and
backlog checks. It observed compiler activity and is excluded from performance
evidence. Review then identified nested exchange rejections being classified as
accepted by the diagnostic trace; the correction and a new binary are required
before formal latency qualification. This correction passed its focused tests
and is included in candidate v2; acceptance thresholds are unchanged in
`benchmark-contract-v2.json`. All failed tests and attempts are retained.

## Performance contract

The frozen contract uses the same seeded diverse-short recipe and real gateway
prewarm (64 bars at the longest interval), with 38 native liquidity providers:

- 500 and 1000 Pine instances: serial baseline/candidate/candidate/baseline,
  50 ms clock interval (20x), 20 s warmup and 30 s measurement.
- 1000 Pine instances: 100 ms clock interval (10x), 20 s warmup and 35 minutes
  measured, with durable traces and the 4096 MiB server guard. This crosses the
  old 2049-bar window for the fastest cohort.
- Gates: at least 95% of configured clock cadence; accepted bar-close-to-durable
  P95 and independent HTTP order P95 at most 250 ms; no errors, skipped bar
  decisions or runtime fallback; complete receipts and resource sampling; no
  concurrent compiler or other benchmark server.

Literal target cadence and instantaneous zero cutoff backlog are reported
separately. The original baseline/candidate flat ABBA uses no trace for either
binary; the later allocator cycle ABBA uses the frozen traced pilot contract.
The baseline predates the new
route counters, so missing counters are not presented as measured fallback.
These are in-memory-journal load measurements. PostgreSQL recovery correctness
does not establish PostgreSQL capacity. Asynchronous trade paths may differ;
fixed complete-trade equality is independently validated above.
The cutoff timestamp comes from the scheduled metrics response in the server's
monotonic clock domain; captures must be within 0.5 seconds of their intended
deadline and retain the measured capture offset. Optional host traces use a
different simulation-time cohort and are not enabled or used in these results.

## Candidate v2 results

`abba-1000-20x/summary.json`: all four runs completed the requested measurement,
with identical recipe/history, unchanged binary/plugin/driver inputs, complete
sampling, and no compiler or parallel benchmark server. Numbers below are the
median of the two per-run results, not pooled request percentiles.

| 1000 Pine + 38 native, target 20x | Baseline | Candidate v2 |
| --- | ---: | ---: |
| Simulation steps / wall second | 14.165 | 19.810 |
| HTTP place P95, ms | 81.072 | 28.565 |
| HTTP cancel P95, ms | 101.989 | 29.246 |
| Pine instances behind after pause, per-run range | 528–1000 | 0 |
| Skipped bar decisions, per-run counts | 4876 / 4806 | 0 / 6 |
| Server peak working set, per-run MiB | 3989.5 / 3860.4 | 1433.6 / 1371.7 |

This is a 39.9% improvement in measured clock progression, with substantially
lower independent HTTP order latency. It is **not** full 20x qualification:
one candidate run recorded six skipped bar decisions, both fell just short of
literal 20x, and these untraced runs do not measure Pine accepted-action latency.
The short flat-price feed also does not establish continued strategy trading.

The corrected 100-bot trace smoke (`smoke-02`) had complete receipt relationships,
no errors or backlog and no compiler interference, but zero accepted bar actions
inside its ten-second measurement cohort. The accepted-latency gate correctly
failed instead of passing vacuously. Sustained trading needs a changing input
profile verified through actual exchange fills/candles and continued action
coverage, not merely a varying submitted limit price.

The v2 500-bot ABBA attempt (`abba-500-20x`) was invalidated by another task's
compiler during a candidate run. Its files are retained, but no paired 500-bot
performance claim is made from that attempt.

Candidate v2 saved a full schema-3 checkpoint/tail in each changed-state journal
record. Current state was small, but journal history and transient peaks were
not bounded by that state size. V3's append encoding addresses this repeated
serialization. It does not impose a retention bound on the in-memory journal,
compact own-fill ledgers, or prove PostgreSQL throughput. These remain explicit
long-run boundaries. Existing receipts and binaries remain unchanged.

Additional existing recovery boundary: `bot_history.rs` restores at most 100,000
commands and 100,000 trades. Exceeding that cap marks authoritative history
incomplete, and the checkpoint adapter refuses continuation. A successful short
restart case or long uninterrupted run does not qualify takeover beyond this
cap. No history is silently discarded to force continuation.

## Candidate v4 cycle pilot

V4 adds the history proof and private-history completeness flag to the second
event fingerprint. This prevents an unchanged visible payload from delaying an
integrity error. Binary SHA-256:
`280c306cf6db1cef3ab97b12dd097301ba93726220065f50668c36e4e373cac1`.
The complete release workspace/all-features suite passed **681 tests, 15
ignored**, with both Clippy configurations passing. Driver and plugin hashes
match the v3 freeze.

The live PostgreSQL restart rerun passed in 9.768 s. Its first attempt exposed a
test assumption: an evaluated bar may legally use the periodic full scheduler
record rather than an append. The repaired fixture requires SQL evidence of
that full record before feeding another real bar, and still requires an actual
post-restart VM-tail append. Both attempts are retained. The original failure
did not save its final database query, so it does not independently prove that
particular attempt hit revision 100.

`cycle-pilot-v4-1000` is a valid, isolated 120-second test at a 10x target:

- 9.969 simulation steps/s; all 1000 Pine instances initialized; no bot/feed/
  harness errors, skipped bar decisions, cutoff backlog or paused backlog.
- HTTP place/cancel P95: 45.001 / 25.503 ms; peak server working set 1313.7 MiB.
- 1116 accepted bar actions, but their P95 bar-close-to-durable latency was
  **365.797 ms**, exceeding the frozen 250 ms gate. The two minute cohorts had
  P95 376.660 / 345.255 ms. **This pilot did not qualify for the 35-minute run.**
- The two cohorts had 599 / 517 accepted actions and all three sources. A
  conservative offline mapping of actual fill timestamps to enclosing clock
  publication intervals confirms MA/RSI/Breakout fills of 11/5/2 and 13/7/3.
  Both windows' actual feed prices rose and fell across 80–120. Thus the failed
  latency gate is not caused by a dormant or all-rejected workload.
- Decision timing decomposition: bar-close-to-decision P95 259.146 ms and
  decision-to-durable P95 165.617 ms. These percentiles are not additive.
  Dispatch queue mean was 98.351 ms; the existing direct/process shared budget
  was 16 while the measurement host had 32 logical CPUs. A separate bounded
  direct-compute budget is under investigation before a new frozen pilot.

Per-minute fill evidence is in `cycle-pilot-v4-1000/window-fills.json`.
Cross-boundary or unclosed clock intervals remain ambiguous and do not count
toward coverage. This offline audit does not modify the frozen driver gates.

## Rejected v5 concurrency experiment

Frozen v5 SHA-256:
`ba83fa511d8f7571a34d8081fd1e5468f5fec84d4f37cbb43126b6da24ed8559`.
It used a separate hardware-bounded pool of up to 32 initialized schema-3 Pine
calls and retained 16 process-class permits, with independent FIFO queues.
Its workspace tests passed 685/15 ignored. Clippy passed after an equivalent
while-let style correction; the initial warning log remains. Its live PostgreSQL
restart case passed in 10.614 s. Drivers/plugins were identical to v4.

The isolated same-recipe 120-second pilot **regressed** and is not accepted:

| Metric | V4 | V5 experiment |
| --- | ---: | ---: |
| Clock steps / wall second | 9.969 | 9.937 |
| Accepted bar-to-durable P95, ms | 365.797 | 581.461 |
| Accepted samples | 1116 | 1361 |
| Dispatch queue mean, ms | 98.351 | 89.652 |
| Bot turn mean, ms | 4.510 | 7.391 |
| Event prefilter mean, ms | 0.293 | 0.493 |
| Full scheduler encoding/validation mean, ms | 69.977 | 78.260 |
| Peak server working set, MiB | 1313.7 | 1300.2 |

Both had no skipped bar decisions, bot/feed/harness errors, cutoff backlog or
external compiler/server interference. V5's second minute had no confirmed
Breakout fill, despite actual feed prices continuing to cycle. Asynchronous
order outcomes and accepted sample counts differ; these are same-recipe pilots,
not a fixed-trade equivalence comparison or a claim of statistical significance.

Increasing concurrency did not resolve the measured latency. The concurrency
pool/queue expansion is being removed. The separate reduction of duplicate
source-file checks in schema-3 window metadata is retained: actual execution
still checks current source and refuses process fallback. The next candidate
targets full-scheduler JSON encoding stalls with bounded parallel fragment
encoding, without changing checkpoint cadence or recovery validation.

## Candidate v6 encoding pilot and validation

V6 restores the single 16-permit FIFO and retains the installed-source metadata
optimization. Full scheduler checkpoints pre-encode missing Pine fragments with
at most four workers; checkpoint cadence, checked outer serialization and
recovery validation remain unchanged. Frozen executable SHA-256:
`6d44af3058995cbb0a2cc114ec6bb0d75387a2fe401b504d5d04b2331aabb210`.
The isolated `cycle-pilot-v6-1000` ran the same 120-second cycle recipe. Frozen
inputs remained unchanged, and compiler/other-server samples were both zero.

| Metric | V4 | V6 |
| --- | ---: | ---: |
| Clock steps / wall second | 9.9692 | 9.9792 |
| Accepted bar-to-durable P95, ms | 365.797 | 506.616 |
| Accepted bar actions | 1116 | 714 |
| All committed bar decisions at 1000-bot anchors: total P95, ms | 434.469 | 433.679 |
| Full scheduler encoding/validation mean, ms | 69.977 | 42.676 |

The 74 full encoding/validation operations show a local improvement, while the
P95 across all 40,000 committed decisions at 1000-bot anchors is essentially
unchanged. Accepted-action latency still fails the 250 ms gate; its minute
cohorts were 375.529 / 513.284 ms. The asynchronous accepted-action mix changed,
so the higher accepted P95 alone does not establish a causal overall regression
from parallel encoding. Phase durations and latency percentiles are not added.

All 1000 Pine instances initialized, with no skipped bar decisions or bot/feed/
harness errors. The run passed the 95%-of-target cadence gate but did not reach
literal 10x. At the scheduled cutoff, committed receipts showed zero backlog;
after pause, 71 bots were one bar behind. These are separate observations, and
the failed after-pause backlog gate remains visible.

Both windows' actual feed prices rose and fell across 80–120. Confirmed
MA/RSI/Breakout fills were 2/10/0 and 2/11/0, despite 36 / 26 accepted Breakout
actions. Accepted admission therefore does not prove a fill or continued trading
by every source. **V6 did not qualify for the 35-minute run.** Evidence is in
`cycle-pilot-v6-1000/bots-1000/report.json`, `bot-receipts-audit.json`, and the
pilot directory's `latency-attribution.json`, `window-fills.json` and
`provenance-after.json`.

Focused validation passed 49 journal tests (one ignored) and the retained
changed-source qualification test (`journal-v6-tests-2.log`,
`source-qualification-v6-tests.log`). The first journal compilation failed on an
extra `unwrap` in the new test fixture; `journal-v6-tests.log` is retained. The
live/auto PostgreSQL checkpoint/delta restart test passed in 10.831 s
(`schema3-live-pg-v6-run1.log`). These are the v6 focused checks; earlier full
workspace results above retain their original candidate identity.

## Candidate v7 computation pilot and validation

Frozen v7 executable SHA-256:
`b8782f605a6d327fcf565e8506973aeecbdf2977a2f8dc38e55845b9a6348ba0`.
The isolated `cycle-pilot-v7-1000` repeated the 120-second cycle recipe; frozen
inputs remained unchanged, with zero compiler or other-server samples.

| Metric | V6 | V7 |
| --- | ---: | ---: |
| Measurement CPU, average logical cores | 2.691 | 2.319 |
| In-process call mean, ms | 2.191 | 1.487 |
| Bot turn mean, ms | 4.343 | 3.944 |
| Accepted bar-to-durable P95, ms | 506.616 | 506.218 |
| All committed bar decisions at 1000-bot anchors: total P95, ms | 433.679 | 503.184 |

Lower CPU use and individual computation costs did not deliver the required
end-to-end latency. V7 had 972 accepted bar actions; overall P95 remained above
250 ms, with minute-cohort P95s of 448.392 / 506.218 ms. The all-decision wave
metric was also higher in this run. Asynchronous action mixes differ, so these
same-recipe observations are not a fixed-trade comparison or a causal overall
performance claim. Nested phase timings and percentiles are not added.

Clock progression was 9.984584 steps/s: the 95%-of-target gate passed, while
literal 10x failed. All 1000 Pine instances initialized without skipped bar
decisions or bot/feed/harness errors. Scheduled-cutoff committed backlog was
zero; after pause, 214 bots were one bar behind. The after-pause gate failed.

Actual feed prices rose and fell across 80–120 in both windows. Confirmed
MA/RSI/Breakout fills were 0/4/3 and 2/15/3. The first window had 18 accepted MA
actions but no confirmed MA fill; the second had confirmed fills from all three
sources. Admission still does not establish fill coverage. **V7 did not qualify
for the 35-minute run.** Source evidence is the pilot's `bots-1000/report.json`
and `bot-receipts-audit.json`, plus `latency-attribution.json`,
`window-fills.json` and `provenance-after.json` in the pilot directory.

Focused validation passed six Pine encoding/borrowed-seal tests and four
dispatch tests (`pine-v7-tests.log`, `dispatch-v7-tests.log`). Three PostgreSQL
restart cases passed in 42.274 s (`schema3-pg-v7-run1.log`), covering qualified
limit/cancel confirmation, schema-3 continuous-versus-restart fill/close, and
live/auto delta continuation. These focused results do not replace a full
workspace validation of v7. V8 is being prepared with finer phase profiling and
less hint cloning; no v8 performance result is claimed here.

## Candidate v8 measured phases and validation

Frozen v8 executable SHA-256:
`a41b5cf410d05c24abfce0ce48ae4ce5aeea1df67eae1e4a5bda2ef1e5b3b6b6`.
The isolated `cycle-pilot-v8-1000` completed the same 120-second cycle recipe,
with unchanged frozen inputs and zero compiler/other-server samples.

- Clock progression was **10.003423851 steps/s**, passing both the 95%-of-target
  and literal 10x gates. All 1000 Pine instances initialized; skipped bar
  decisions, bot/feed/harness errors, scheduled-cutoff backlog and after-pause
  backlog were all zero.
- The 2224 accepted bar actions had P95 **308.013 ms**, still above 250 ms.
  Minute-cohort P95s were 313.104 / 288.224 ms. The observed overall P95 was lower
  than v7's 506.218 ms; all committed decisions at 1000-bot anchors had P95
  336.550 ms versus v7's 503.184 ms. These single asynchronous pilots have
  different action mixes and do not establish a causal or repeatable speedup.
- Within-window process-tree sampling reported 2.0736 average logical cores
  and 1253.56 MiB peak sampled working set. The report's separate server peak
  was 1300.61 MiB; it is not the same sampling metric.
- Actual feed prices rose and fell across 80–120 in both windows. Confirmed
  MA/RSI/Breakout fills were 20/0/1 and 10/5/1. The first window's 119 accepted
  RSI actions therefore did not establish confirmed RSI fill coverage.

The new diagnostic phases use one common denominator of 82,330 measured turns
(`cycle-pilot-v8-1000/phase-per-turn.json`):

| Stage | Samples | Mean per invocation, ms | Elapsed ms per common turn |
| --- | ---: | ---: | ---: |
| Current-source verification | 164660 | 0.407701 | 0.815401 |
| VM `append_bars` execution | 80040 | 0.047785 | 0.046456 |

Source verification occurred twice per turn. These disjoint subphase durations
are elapsed wall time, not CPU time or shares of the full critical path; queue
and commit waits are outside this comparison. They must not be added to parent
timers or compared by adding per-invocation means with different sample counts.
The measurements motivate v9's proposed single verification per attempt, but
do not yet establish a benefit from that change.

V8 passed the full release workspace/all-features Rust suite: **696 passed,
15 ignored** (`workspace-v8-tests.log`). Python diagnostics passed **46 tests**
(`diagnostics-v8-tests.log`); default and all-features release Clippy passed
(`clippy-v8-default.log`, `clippy-v8-all.log`). Three PostgreSQL HTTP restart
cases passed in **41.680 s** (`schema3-pg-v8-run1.log`).

**V8 did not qualify for the 35-minute run.** The latest-candidate ABBA and
long-run gates remain open. Full receipts, latency attribution, fill-window
evidence and unchanged-input confirmation are retained under
`cycle-pilot-v8-1000`; earlier failures are not replaced by this validation.

## Candidate v9 passing short pilot and validation

`cycle-pilot-v9-1000` passed the frozen 120-second pilot gates on the v9 binary
identified above. The workload and all 14 frozen driver inputs remained
unchanged (`provenance-after.json`); compiler and other-server samples were zero.
This establishes a passing short pilot, not long-duration or 20x capacity.

- Clock progression was **10.004962813 steps/s**, passing literal 10x and the
  configured cadence gate. All 1000 Pine instances initialized, with no bot/feed/
  harness errors, skipped bar decisions, cutoff backlog or after-pause backlog.
- The 1806 accepted bar actions had P95 **200.519 ms** and maximum **330.416 ms**.
  The frozen gate constrains P95, not the maximum. HTTP place/cancel P95s were
  21.376 / 20.874 ms. Maximum accepted-anchor gap, including measurement
  endpoints, was 3.01177 s against the 60 s limit.
- Within-window process-tree sampling reported **1.5458566** average logical
  cores and **1279.77 MiB** peak sampled working set. The separate server peak
  was **1296.87 MiB**; these memory metrics retain their distinct scopes.

| Minute cohort | Accepted actions | Accepted MA/RSI/Breakout | Accepted P95, ms | Confirmed MA/RSI/Breakout fills |
| --- | ---: | --- | ---: | --- |
| First | 837 | 466 / 281 / 90 | 198.555 | 17 / 4 / 0 |
| Second | 969 | 566 / 360 / 43 | 211.635 | 16 / 7 / 1 |

Both cohorts passed the frozen accepted-action count, source and latency gates.
Actual feed prices rose and fell across 80–120 in both. The independent
`all_windows_have_confirmed_pine_fills_from_each_source` diagnostic remained
**false**: 90 accepted Breakout actions in the first minute yielded no confirmed
Breakout fill in that window. Accepted admission is not a fill, and this passing
pilot does not establish every-source, every-minute fill coverage.

Measurement deltas show 81,846 source checks for 81,846 turns: one check per turn,
**0.333333 ms per common turn**, versus v8's two checks and 0.815401 ms per turn.
Bot-turn mean was 2.237761 ms versus v8's 3.352960 ms. The 70 full scheduler
encoding/validation operations averaged 34.893 ms. These elapsed wall timings
are not CPU or additive critical-path shares. The asynchronous accepted mix
also changed; formal ABBA comparison is still pending.

V9 passed **702 Rust tests, 15 ignored** in the full release workspace/all-features
suite (`workspace-v9-tests.log`). Default and all-features release Clippy with
warnings denied and the frozen build passed (`clippy-v9-default.log`,
`clippy-v9-all.log`, `build-v9.log`). Three PostgreSQL HTTP restart cases passed
in **40.838 s** with durability enabled (`schema3-pg-v9-run1.log`). The prior
46-test Python diagnostic result retains its v8 identity; frozen drivers did
not change. Pilot evidence includes `bots-1000/report.json`,
`latency-attribution.json`, `window-fills.json` and `provenance-after.json` under
`cycle-pilot-v9-1000`.

The passing pilot permitted proceeding to the 35-minute attempt documented
below. Its failure does not replace the valid short-pilot result, and the
short-pilot result does not qualify the longer workload.

## Candidate v9 failed long attempt and independent partial audit

`cycle-long-v9-1000` used the same frozen v9 executable and cycle workload for
a requested 2100-second measurement. It stopped at the 4096 MiB memory guard:
the recorded peak was **4,301,615,104 bytes (4102.339844 MiB)**. Compiler and
other-server sample counts were both zero, with no process-sampler errors.
`provenance-after.json` reports no changed frozen inputs and explicitly labels
the attempt guard-terminated. This is a failed duration/capacity attempt, not
a compiler-contaminated run.

The actual measurement-start metric timestamp was **20,007,262 us**. The final
successful clock publication was **1,664,495,178 us**, at market time 18,229,000 ms,
matching the final paused clock. Thus the observable bar-anchor interval was
**1644.487916 seconds (27.408132 minutes)**. The final trace event was another
19.113 ms later. The scheduled measurement-end metrics and cutoff were never
captured. The original attribution/fill-window helpers correctly refused those
missing endpoints; their failure logs and all original reports remain intact.
After-pause backlog zero cannot supply the missing scheduled cutoff or erase
skipped decisions.

The independent task-local `analyze-partial-receipts.py` streams the trace twice
and writes `partial-latency-attribution.json`, explicitly **PARTIAL**, with
`qualified: false` and no full-duration/cutoff claim. It uses actual clock
publications for bar-close anchors, 60-second half-open cohorts and a separate
24.487916-second final cohort. Terminal receipts remain attached to their
original anchor cohort; no accepted or complete bar decision in this run became
durable after the final clock. No drain observation is used to infer cutoff
backlog. Its 14 saved-audit/final-state crosschecks passed, with no malformed,
orphan, duplicate, conflicting or pending receipt errors. Both streaming passes
matched the same 686,278,430-byte trace SHA-256:
`42dbcefa2ffa3c9b579fe331d18f370600ebf451f30259ec0afa1ad107779160`.
The result also fingerprints its helper and source artifacts.

Across the observed interval there were **6299 accepted bar actions**: total
P95 **468.173 ms**, maximum 1571.359 ms. All **1,073,932 committed bar decisions**
had total P95 **489.629 ms**, maximum 1912.221 ms. Every observed cohort had at
least 20 accepted actions and at least one from each source; the largest
accepted-anchor gap was 14.513757 s. These are admissions, not fill receipts.
The acceptance samples therefore remain present while latency deteriorates:

| Observed minute | Accepted actions | Accepted total P95, ms | All committed bar decisions total P95, ms |
| --- | ---: | ---: | ---: |
| 1 | 843 | 220.116 | 215.448 |
| 2 | 480 | 316.024 | 234.161 |
| 3 | 391 | 267.990 | 261.094 |
| 4 | 370 | 227.082 | 240.599 |
| 5 | 356 | 397.808 | 310.747 |
| 6 | 289 | 357.702 | 347.470 |
| 7 | 280 | 302.651 | 310.575 |
| 8 | 308 | 354.977 | 349.517 |
| 9 | 278 | 345.781 | 329.296 |
| 10 | 210 | 358.255 | 358.255 |
| 11 | 195 | 349.396 | 407.326 |
| 12 | 254 | 414.708 | 410.851 |
| 13 | 204 | 489.243 | 432.393 |
| 14 | 211 | 403.157 | 421.501 |
| 15 | 130 | 491.156 | 483.501 |
| 16 | 185 | 542.662 | 467.028 |
| 17 | 131 | 592.543 | 557.615 |
| 18 | 143 | 416.498 | 535.768 |
| 19 | 100 | 425.694 | 531.934 |
| 20 | 125 | 638.332 | 547.541 |
| 21 | 102 | 590.435 | 577.450 |
| 22 | 96 | 659.375 | 536.531 |
| 23 | 185 | 686.590 | 603.915 |
| 24 | 66 | 652.577 | 579.135 |
| 25 | 85 | 879.217 | 696.759 |
| 26 | 187 | 674.870 | 749.455 |
| 27 | 66 | 617.772 | 627.861 |
| Final 24.487916 s | 29 | 686.271 | 744.941 |

Only full minutes 1 and 4 met accepted P95 <=250 ms. The all-bar population
also deteriorated, so the finding does not depend solely on the changing
accepted-action mix. For all committed bar decisions, minute 1 had 40,020
samples and minute 26 had 36,833:

| Elapsed stage | Minute 1 mean / P95, ms | Minute 26 mean / P95, ms |
| --- | ---: | ---: |
| Bar publication to decision | 59.163 / 165.709 | 255.104 / 672.656 |
| Decision to durable publication | 31.440 / 65.555 | 52.018 / 232.369 |
| Bar publication to durable publication | 90.603 / 215.448 | 307.122 / 749.455 |

Most mean-time growth occurred before the decision receipt, which includes
scheduling/queueing and computation. Commit-side tail latency also increased.
These trace boundaries do not separate VM execution from input preparation,
source verification or queue wait, and do not establish PostgreSQL saturation
(this benchmark uses the in-memory durable journal). The three P95s are
independent distributions and must not be added.

Committed per-participant `evaluated_bars` increments locate **271 skipped bar
decisions**, exactly matching the final persisted counter. The first cold state
is excluded; later multi-bar increments contribute increment minus one, matching
the schema-3 implementation. Detection times relative to measurement start were:

- **1453.055052–1453.203365 s:** 85 MA skips.
- **1538.987352–1539.682976 s:** 125 MA and 61 RSI skips.

These timestamps locate the completed decision that caught up multiple bars,
not the earlier publication time of each skipped bar. Final observed clock
windows 25 and 26 also fell below 95% of the configured 10x cadence, at 9.477662
and 9.396200 steps/s. No after-pause catch-up can turn these into a passing run.

The separate durable-state inspection does **not** show unbounded growth of the
serialized Pine VM checkpoint. Across the 1000-bot short/long snapshots, total
checkpoint size was **3,501,993 -> 3,515,792 bytes** and serialized series
payload remained **389,175 bytes**. Tail lengths moved from **12–29** to **30–31**
bars near the existing 32-bar rollover. Pine state size rose from 15,713,944 to
22,829,124 bytes; `vm_tail` alone grew from 9,665,237 to 16,403,169 bytes,
accounting for **94.7%** of that increase. Tail intents separately grew from
459,874 to 756,063 bytes. The inputs are the short and long v9 cases'
`bots-1000/scheduler.json`; `.local/bot-throughput-20261013/analyze-state-growth.py`
provides the durable-field comparison. This comparison concerns
serialized durable fields at two different rollover phases. It is not a heap
profile or an attribution of process RSS/memory-guard growth, and does not bound
the journal, accumulated market observations or every other retained structure.

V10 subsequently implemented an incremental observation cache across compute
waves, retaining the existing history/proof and trading semantics. Its first
short pilot, recorded below, still failed latency. The latest-candidate formal
ABBA comparisons and a complete 35-minute run must meet their own unchanged
gates. The v9 failure and partial evidence remain retained.

## Additional v9 repeat and memory-retention diagnostic

The first attempted memory-profile run, `memory-profile-v9-1000`, did **not**
enable profiling: the standard driver's environment cleanup removed the profile
environment setting. It is retained as an additional ordinary v9 short repeat,
not memory-profile evidence. Its 900 accepted bar actions had P95 **467.935 ms**
and maximum 648.407 ms. There were no compiler/other-server samples, bot/feed/
harness errors or skipped bars. Nevertheless, **285 bots were one bar behind at
the scheduled cutoff**; later pause/drain reduced backlog to zero. Its cadence
was 9.997219087 steps/s. Both latency and cutoff gates failed, so the original
200.519 ms pilot result must not be presented as a repeatable qualification.

The task-local diagnostic wrapper `run-memory-profile.py` then enabled the
existing `run_case.write_profile` option without changing the frozen benchmark
driver. `memory-profile-v9-1000-enabled/memory-retention-summary.json` records
65 profile samples. At the final sampled **6500 retained mutations**, the
uncompressed JSON byte total was **1,445,089,869**, while their actual retained
raw/LZ4 payload was **198,316,223 bytes** (about 189.13 MiB). The server peak was
**1266.011719 MiB**, and the sampled process-tree peak was 1229.476563 MiB.

The payload count is an exact retained-byte observation for those mutation
payloads, not the journal's entire heap and not all process RSS. It cannot
directly account for the failed long run's heap. It does establish cumulative
payload retention as a long-duration risk worth measuring. The enabled run is
diagnostic evidence only, not a performance qualification: accepted P95 was
443.073 ms, with 334 bots one bar behind at cutoff and 110 still behind after
pause. Its accepted-action count was 2010; profiling changes its measurement
conditions. All failed attempts and their original reports are retained.

## Offline lossless recompression feasibility

A separate task-local experiment compressed the **complete final scheduler
snapshots** from the passing v9 short pilot and failed v9 long attempt. Input
JSON was compacted with UTF-8, unchanged key order and no optional whitespace.
The source snapshots remained unchanged. All commands, tool executable hashes
and versions, source/compact/compressed hashes, decoder results and uncontrolled
CLI elapsed times are retained in
`journal-recompression-feasibility-v9/attempt-02/manifest.json`, alongside a
reproducible script and compressed outputs. It used LZ4 CLI **1.9.4** and Zstd
CLI **1.5.6**. The initial invocation incorrectly used LZ4's positional output
syntax for Zstd; that failed attempt and its outputs remain separate.

| Snapshot | Compact input bytes | CLI LZ4 default | Zstd `--fast=1` | Zstd `-1` | Zstd `-3` |
| --- | ---: | ---: | ---: | ---: | ---: |
| V9 short | 16,335,974 | 1,884,485 | 698,537 | 447,182 | 393,503 |
| V9 long | 23,476,782 | 3,673,460 | 844,831 | 565,795 | 479,231 |

All **eight roundtrips** reproduced the corresponding compact input's exact byte
count and SHA-256. This supports investigating stronger lossless compression of
older journal payloads. It does not establish a production space or speed gain:
CLI LZ4 uses a frame format, the application stores raw/LZ4 blocks, and complete
scheduler snapshots are not representative of all individual mutation records.
The one-shot CLI timings were not controlled and are not performance evidence.

The next useful feasibility check would use actual canonical mutation payloads
and retain every record, byte-exact content, sequence and recovery semantics.
Background recompression would also need bounded transient memory/CPU, explicit
codec metadata and verified restore behavior before any implementation claim.
This experiment deleted or merged no canonical records and changed neither
product code nor the benchmark contract. It does not replace the still-unmet
35-minute or latest-candidate formal ABBA gates.

## Candidate v10 correctness and first cycle pilot

The v10 release workspace/all-features run passed **710 tests, with 15 ignored**
(`workspace-v10-tests.log`). Default and all-features release Clippy passed with
warnings denied, followed by the release build. The three live PostgreSQL HTTP
recovery tests passed in **49.935 s** (`schema3-pg-v10-run1.log`), with the three
durability settings recorded as `on`. The initial observation-cache compile
attempt failed type inference (`E0283`); its log is retained separately, followed
by seven passing focused tests and the passing full suite. These checks establish
correctness evidence for this candidate, not capacity.

The frozen v10 binary ran `cycle-pilot-v10-1000`: 1000 Pine bots, 10x target,
20-second warmup and 120-second requested measurement. The recipe SHA-256 was
`8e689a741b9672894bc48e8d22215ba8fb24c96a3c07dccea5a9cb323f891379`, unchanged
from both ordinary v9 short runs. `provenance-after.json` records no changed
inputs. Compiler and other benchmark-server samples were zero. The saved
receipt audit has no parse/trace errors, complete action receipts and decision
outcomes; the independent latency attribution agrees with its five cross-checks.

**The pilot failed latency.** Its 1345 accepted bar actions had P95 **560.775 ms**
and maximum **636.564 ms**. Both anchor-cohort minutes had enough accepted actions
and all three sources, but both failed the same 250 ms P95 gate:

| Anchor window | Accepted actions | MA / RSI / Breakout accepted | Accepted P95, ms |
| --- | ---: | --- | ---: |
| First 60 s | 671 | 396 / 130 / 145 | 482.725 |
| Remaining 60.008432 s | 674 | 334 / 308 / 32 | 595.458 |

The maximum accepted-anchor gap was 4.998741 s, below 60 s. Cadence was
**9.975736484 steps/s**: the literal 10x check failed, while the frozen >=95%
cadence check passed. All 1000 bots were initialized; skipped bars, bot/feed/
harness errors, scheduled-cutoff backlog and after-pause backlog were zero.
The cutoff market time was 3,318,000 ms. These separate successes do not override
the failed accepted-latency and sustained-coverage gates.

Measurement-only process-tree CPU averaged **2.381064 logical cores** across 429
samples, with sampled peak RSS **1228.902344 MiB**. The report's server peak was
**1279.742188 MiB**. CPU and RSS scopes must not be interchanged. The 79,706
committed bar decisions had total P95 **450.005 ms**; the 40,000 decisions at
1000-bot anchors had total P95 **490.120 ms**. Accepted-action bar-to-decision,
decision-to-durable and total P95s were respectively **435.887 / 197.806 /
560.775 ms**, independently computed, not additive.

Conservative fill-window attribution counted actual MA/RSI/Breakout fills
**9/5/7**, then **5/11/0**. Actual feed trades traversed 80..120 ticks with rises
and falls in both windows. Two feed trades crossed attribution boundaries and
remain ambiguous; no Pine fills were ambiguous. Accepted admission therefore
still does not establish three-source actual fills in every minute. The sources
are the case's `bots-1000/report.json`, `bots-1000/bot-receipts-audit.json`,
`latency-attribution.json` and `window-fills.json`.

## V9 first/repeat versus v10: bounded attribution

The following comparison uses scheduled measurement-start/end counter deltas.
The v9 repeat is `memory-profile-v9-1000`, whose profiling was not enabled; the
separate enabled diagnostic is excluded. These are asynchronous short runs,
not an ABBA comparison. The changing accepted-action mix is visible in the
receipt counts and must not be treated as a fixed amount of trading work.

| Measurement | V9 first | V9 ordinary repeat | V10 first |
| --- | ---: | ---: | ---: |
| Accepted actions / total P95, ms | 1806 / 200.519 | 900 / 467.935 | 1345 / 560.775 |
| Measurement CPU, logical cores | 1.545857 | 2.047085 | 2.381064 |
| Event-prefilter mean elapsed, ms | 0.207456 | 0.328817 | 0.081089 |
| Bot-turn mean elapsed, ms | 2.237761 | 2.982253 | 2.819626 |
| Full scheduler validate / serialize calls | 70 / 70 | 69 / 69 | 66 / 66 |
| Full validate mean elapsed, ms | 34.893467 | 61.264245 | 66.902229 |
| Full serialize mean elapsed, ms | 5.304679 | 9.860133 | 9.493889 |
| Journal-operation total elapsed, s | 8.073673 | 12.908312 | 14.475258 |
| Scheduler apply calls / mean elapsed, ms | 5502 / 0.405556 | 5244 / 0.774351 | 4829 / 0.939075 |
| Pine VM checkpoint calls | 2335 | 2335 | 2335 |

V10's prefilter is locally faster in this comparison. Full-validation mean is
1.917x the v9 first result, but most of that difference already appears between
two runs of the **same v9 executable**. Full-snapshot and VM-checkpoint invocation
counts did not increase. The large `scheduler_checkpoint` counter counts a
checkpoint-check scope, including no-snapshot paths; it is not the full scheduler
snapshot count. These timers measure elapsed wall time, not writer-thread CPU;
their nested totals must not be added or substituted for a CPU profile.

To test whether final state inflation explains the difference, the three final
`bots-1000/scheduler.json` files were read sequentially at idle process priority,
one agent at a time with a 5 ms pause between agents. Byte counts below are UTF-8
compact JSON reserialization with unchanged key order, not journal wire bytes.
All snapshots contain 1038 total agents, including the same 1000 Pine bots.

| Final paused snapshot field, bytes | V9 first | V9 ordinary repeat | V10 first |
| --- | ---: | ---: | ---: |
| Complete compact scheduler | 16,335,974 | 16,295,655 | 16,101,840 |
| Total Pine state | 15,713,944 | 15,673,448 | 15,479,615 |
| VM checkpoints | 3,501,993 | 3,501,993 | 3,501,993 |
| Checkpoint series | 389,175 | 389,175 | 389,175 |
| VM replay tails | 9,665,237 | 9,697,782 | 9,500,825 |
| Replay tail intents | 459,874 | 464,116 | 434,733 |
| Own-fill ledgers | 366,961 | 356,451 | 358,378 |

The final MA/RSI/Breakout tail lengths were **12/29/14**, **12/29/14** and
**11/29/14**, respectively, across 334/333/333 bots. All three had checkpoint
bar counts **320/192/96**. V10 ended at step 3319, while both v9 snapshots ended
at step 3322; the smaller MA tail reflects that final observation point. The
full compact v10 snapshot is 1.43% smaller than v9 first. This rules out an
observed doubling of the **final** payload or checkpoint size as the explanation.
Equal byte counts do not imply identical checkpoint contents.

Snapshot input SHA-256s, in the same column order, were
`bd7f8d243c8d0798ab7772a8602aace572ff0cf5081d908cf7afa93167f54f52`,
`c103a452943db903682a929195036a1d4a49b1abb4a2729719abd3b026d1ef5a`
and `4d27f6f8d9ae13821af5150516aca67e730752504e809cbaaac932d53404ef07`;
source size and modification time were unchanged across each read.

The remaining attribution gap is material: these ordinary runs do not retain
per-invocation full-snapshot encoded sizes, dirty-state/cache-hit counts or
writer-thread CPU. Final after-pause snapshots cannot establish the average
payload or cache work at the earlier 70/69/66 full writes. Their counts and final
sizes therefore do not explain the elapsed-time increase, nor prove that v10's
cache caused a regression. A controlled repeat with per-write byte/work counters
and CPU attribution is needed to distinguish payload/cache work from contention
and asynchronous batching. No controlled cause or overall speedup is claimed.
Latest-candidate formal ABBA and the complete 35-minute gate remain outstanding.

## Serial v9/v10 cycle comparison: retained, incomplete ABBA

`cycle-abba-v9-v10-1000` ran **v9 / v10 / v10 / v9** serially with the same
1000-Pine cycle recipe, 64-bar prewarm, 20-second warmup, 120-second measurement,
100 ms interval, direct schema-3 route and unchanged receipt/diagnostic gates.
All four benchmark subprocesses exited zero. Manifest timestamps confirm no run
overlap; recipe and prewarm-candle hashes match. All four had zero compiler and
other-server samples, and their full-duration, process-sampling, harness-error
and scheduled-metric-window validity checks passed. Measurement-only process
sample counts were **431 / 430 / 427 / 430**. Capacity failures remain distinct
from these measurement checks.

The task-local wrapper then called `changed_inputs` with the wrong argument
signature during finalization. Its original source and failure are retained.
After all servers stopped, `inputs-after-recovered.json`,
`input-changes-recovered.json` and `wrapper-finalization-receipt.json` recovered
the provenance check: **direct equality true, changed inputs `[]`**. This was a
wrapper finalization error, not four failed benchmark subprocesses; no raw case
was rewritten or benchmark repeated for that recovery.

Fresh `latency-attribution.json` and `window-fills.json`, with their separate
logs, were generated inside every run directory. Both helpers rejected **run 2**
because of its truncated receipt file. The other three runs have parseable
inputs. The group is therefore **not a valid complete ABBA comparison**; removing
run 2 does not reconstruct one. The retained observations are:

| Run | Trace | Accepted / P95, ms | Steps/s | Measurement CPU, cores | Cutoff / paused bots behind |
| --- | --- | ---: | ---: | ---: | ---: |
| 1, v9 | Parseable | 2117 / 365.681 | 9.990478 | 1.962274 | 332 / 0 |
| 2, v10 | Truncated; observed prefix only | 1723 / 496.760 | 10.003355 | 2.207483 | 334 / 206 |
| 3, v10 | Parseable | 2061 / 476.191 | 9.966513 | 2.431268 | 0 / 0 |
| 4, v9 | Parseable | 1774 / 476.467 | 9.987225 | 2.415172 | 0 / 184 |

Every behind bot in the table was one closed bar behind. All four reported zero
skips and initialized all 1000 Pine bots. All passed >=95% cadence; only run 2
passed the literal 10x check. These facts do not erase the failed latency,
backlog or receipt-completeness gates. In particular, run 4's bar cohort contains
1774 accepted actions and 77,932 committed no-order decisions, but the whole trace
still has 136 discarded and **48 pending** no-order decisions outside that bar
cohort. Its `complete_decision_outcomes` is false, despite parseable trace input.

Every retained minute has >=20 accepted actions and >=1 from each source; none
meets the 250 ms P95 gate. The following source triples are **MA / RSI / Breakout**.
Run 2 values are provisional observations from an incomplete trace:

| Run / minute | Accepted source counts | Accepted P95, ms | Confirmed actual fill counts |
| --- | --- | ---: | --- |
| 1 / first | 937 / 102 / 92 | 372.800 | 11 / 5 / 3 |
| 1 / second | 793 / 159 / 34 | 358.351 | 9 / 5 / 2 |
| 2 / first, incomplete trace | 593 / 79 / 18 | 395.025 | 14 / 3 / 2 |
| 2 / second, incomplete trace | 505 / 480 / 48 | 510.876 | 10 / 14 / 1 |
| 3 / first | 799 / 252 / 81 | 489.749 | 22 / 9 / 1 |
| 3 / second | 595 / 284 / 50 | 464.502 | 14 / 9 / 4 |
| 4 / first | 625 / 182 / 103 | 476.467 | 21 / 9 / 3 |
| 4 / second | 390 / 429 / 45 | 492.159 | 12 / 9 / 2 |

Maximum accepted-anchor gaps were 4.499949 / 5.995931 / 3.118707 / 3.087851 s,
all below 60 s. The three parseable runs each have confirmed actual fills from
all three sources in both minutes, and actual feed prices span 80..120 ticks
with rises and falls. These actual ledger observations are separate from order
admission and do not repair any failed gate.

Run 2's original audit already reported the error at **line 297221**. That final
line consists of only two bytes, `{"`, at byte offset **59,148,404**, in a
59,148,406-byte file. Its scheduled cutoff was **140,030,180 us**. The final clock
publication, line 296790, was **140,057,194 us** (+27.014 ms), at market time
3,321,000 ms, matching the final paused clock. The last complete line, 297220,
was a `decision_discarded` receipt with `epoch_changed_in_ready_queue` at
**140,199,790 us** (+169.610 ms). The preceding discard tail spans
140,171,606..140,199,790 us. The malformed line has no event type or timestamp;
its exact write/termination time cannot be recovered.

This tail is consistent with the inspected driver sequence: pause, metrics
flush, final-state reads, then process termination while late compute-result
receipts can still be written. It is not evidence that the scheduled endpoint
was absent. It does mean terminal outcomes are incomplete: the bar cohort has
**11 discarded and 4 pending actions**, plus **167 discarded and 24 pending
no-order decisions**. Both original and fresh audits reject it. No line was
trimmed, repaired or treated as ignorable, and no post-cutoff drain was folded
into cutoff progress.

For descriptive scale only, the two parseable v9 runs' per-run accepted P95
median is **421.074 ms**, range **365.681..476.467**; their all-bar P95 median is
**403.010 ms**, range **355.729..450.291**. The single parseable v10 run is
**476.191 ms** accepted P95 and **475.202 ms** all-bar P95. The sample counts are
unequal, and these are neither pooled percentiles nor a causal version effect.
Compared with the original v9 pilot's 200.519 ms, both new v9 observations fail
to reproduce that passing result. Compared with v10's original 560.775 ms, its
parseable repeat remains well above the gate.

The parseable v10 run's event-prefilter mean is **0.079922 ms**, close to its
first pilot's 0.081089 ms; the two v9 values are **0.296597 / 0.377266 ms**.
That local difference persists without an overall capacity success. Full
validation means are **59.700 / 62.108 / 73.433 / 65.062 ms**, from
**67 / 69 / 66 / 65** calls. The repeated v9 values are already substantially
above its original 34.893 ms. These elapsed timers still do not establish
writer-thread CPU attribution or isolate a version effect.

`summarize-cycle-abba-v9-v10.py` produces the retained `comparison-summary.json`,
including source/derived hashes, all original qualification flags, group
validity checks and per-version medians/ranges. Its all-attempt statistics are
explicitly descriptive and include the invalid run; its parseable-run subsets
do not reconstruct ABBA. The group's `valid_abba_comparison` is **false**.
No new capacity or 35-minute qualification is claimed.

## V11 cadence recovery experiment: 999-delta candidate rejected

The initial cadence patch proposed changing periodic full scheduler state writes
from every 100 revisions to every 1000, bounding the uninterrupted tail at
**999 scheduler deltas**. Both scheduler commit paths use the same boundary;
manual operations, actor checkpoints and roster/configuration changes retain
their full-state behavior. This is a bound on scheduler replay records, not on
all journal rows, retained bytes or startup time. No canonical record is removed
or merged. The candidate was implemented for this experiment and is **not a
qualified throughput improvement or an accepted default**.

Six targeted tests passed: two cadence tests, the existing empty-wave,
failure/retry and actor-tail tests, and one Perp observation-projection test.
The latter checks funding/liquidation suffix equivalence and old-wave stability.
The cadence checks cover both commit paths, the boundary retry, retained actor
checkpoint behavior, old 100-revision checkpoint recovery and rejection of
missing/corrupt/reordered durable deltas. Logs are `cadence-tests-v11.log`,
`cadence-empty-wave-v11.log`, `cadence-failure-retry-v11.log`,
`cadence-actor-tail-v11.log` and `perp-projection-v10.log`. These six focused
passes do not replace a full candidate suite or live PostgreSQL/gateway checks.

`cadence-recovery-v11` then ran the already compiled ignored recovery test,
serially with no build or server load. The frozen `recovery-tests.exe` SHA-256 is
`5c1c655eae1bc14f4347a3349c3546bc6de6c4cdf3c63de54d6c94ee55014cb8`;
this identifies a **test executable**, not a qualified server release. Manifest
exit code is zero, and the test passed in **263.35 s**.

The fixture generates real schema-3 states through the Pine Host for **1000
agents**, distributed across MA/RSI/Breakout, starting at **32 bars**. A synthetic
scheduler sequence then applies **999 batches of 64 changed agents**, including
**62,000 append changes and 1,936 complete agent-state replacements** at actual VM
checkpoint rollovers. It retains one full scheduler base of **4,743,363 bytes**.
Full base plus 99 deltas is **19,631,841 bytes**; full base plus 999 deltas is
**155,302,524 bytes**, all uncompressed JSON record bytes. Fixture preparation
took 172.083547 s and is excluded from the replay timers. The fixture SHA-256 is
`5e3953c1f1ddb12cf817fd8c113c4b940cd02187f6aa6e16ce32d4c6a998df69`.

Every repetition decodes the wire JSON afresh, removing live prepared-state
identity shortcuts, then calls scheduler recovery. Exact final scheduler JSON
equals the expected state in **all six repetitions**. Six representative agents
also passed cold-session next-decision equivalence after recovery, covering all
three source profiles. The latter is six continuation checks, not 1000 separate
live restart/continuation checks.

| Delta tail / repetition | Decode, s | Apply, s | Total, s |
| --- | ---: | ---: | ---: |
| 99 / 1 | 0.264589 | 0.723640 | 0.988229 |
| 99 / 2 | 0.258341 | 0.721708 | 0.980049 |
| 99 / 3 | 0.284392 | 0.746967 | 1.031359 |
| 999 / 1 | 2.179468 | 16.426502 | 18.605970 |
| 999 / 2 | 3.608141 | 24.979547 | 28.587687 |
| 999 / 3 | 3.704739 | 29.023700 | 32.728438 |

The per-run total medians are **0.988229 s** and **28.587687 s**, respectively.
These timers cover uncompressed JSON decoding and scheduler application; they
exclude process startup, database/storage fetch, codec decompression, gateway
readiness, the equality assertion and subsequent Pine continuation calls. The
experiment is a synthetic scheduler/recovery sequence using genuine VM states,
**not PostgreSQL recovery, gateway replay or asynchronous trading throughput**.

Whole-experiment process sampling recorded **949 samples**, peak summed RSS
**2367.046875 MiB**, average **0.993806 logical cores**, zero compiler/other-server
samples and no sampling errors. Its measurement-window subsection is unset;
these resource figures include fixture construction, retained wire/state copies,
all repetitions and verification. They are not isolated replay-stage memory or
production server RSS. The successive 999-tail samples become slower, but the
logs do not attribute that progression to allocator retention, memory pressure,
CPU scheduling or another cause; it is not evidence of a specific leak.

The existing benchmark readiness wait is **25 seconds**
(`scripts/benchmark_market_load.py`). Two of the three 999-tail samples exceed
that entire budget before any startup/database work is included. This is enough
to **reject 999 as the current cadence setting**; it does not claim a measured
PostgreSQL startup failure. The next candidate tightens the record bound to
**499**; its separate measurements are below, rather than inferred by linear
interpolation from the rejected 999-tail result.
Original experiment logs, process samples and the rejected candidate executable
remain in `cadence-recovery-v11`; the failed capacity and long-run evidence above
is unchanged.

## V11 follow-up: 499-delta synthetic recovery and live-hint proof

`cadence-recovery-v11-499` repeats the same actual-Host, 1000-agent synthetic
sequence with **499 batches of 64 changed agents**: **31,000 append changes**
and **936 complete agent-state replacements**. Its frozen test executable
SHA-256 is `35c8c87e1a4bf2927fdef5ca9e1b7f7a4b46428c577033b6af720da8b5c81d32`.
The manifest records exit 0; the ignored recovery test passed in **82.69 s**,
including **46.971548 s** of fixture preparation outside the replay timers.
The two focused cadence tests also passed at the new 499-record limit
(`cadence-499-tests-v11.log`).

| Delta tail / repetition | Decode, s | Apply, s | Total, s |
| --- | ---: | ---: | ---: |
| 99 / 1 | 0.266374 | 0.726202 | 0.992576 |
| 99 / 2 | 0.268889 | 0.730518 | 0.999406 |
| 99 / 3 | 0.272877 | 0.796132 | 1.069009 |
| 499 / 1 | 1.135377 | 10.384042 | 11.519419 |
| 499 / 2 | 1.297803 | 7.890040 | 9.187843 |
| 499 / 3 | 1.358443 | 7.906561 | 9.265004 |

All six reconstructed scheduler states were byte-equal to their expected JSON;
the six representative cold-Host continuation checks also passed. Median total
times were **0.999406 s** for 99 and **9.265004 s** for 499. Every printed replay
sample is below 25 seconds, but the excluded process/database/codec/readiness
work remains unmeasured. This is **not a complete startup-budget pass or a
PostgreSQL/gateway test**. The non-monotonic repetitions do not identify a cause.
Whole-experiment process sampling recorded peak summed RSS **1506.402344 MiB**,
average **0.994164 logical cores**, zero compiler/other-server samples and
`errors: []`. As before, these are whole-fixture-and-verification resources,
not isolated replay-stage or production-server memory.

The validated fixture was exported only after the equality and continuation
checks. `fixture/mutations.jsonl` contains **500 records / 80,100,701 bytes**
(one full base plus 499 deltas, each with an LF separator), alongside the two
expected scheduler snapshots and its manifest. The concatenated JSON records
without separators total **80,100,201 bytes**. The fixture manifest SHA-256 is
`601a7b6621ec9c338ccab180459bdfa30dc10eeb0a8713bd848392b53c7e4810`;
the JSONL SHA-256 is
`1c80b166ba6963da2f24004c8ad5abf09a4cfeb171a6219b62189a7adf34980b`.
These identify this 499-tail export, not the earlier 999 experiment or a later
database import. No imported PostgreSQL result is implied.

The live-hint validation proof patch has also been applied, with **13 focused
tests passing** in `live-hint-proof-tests-v11.log`. They cover exact allocation
and header identity, copy-on-write and rehashed mutations, stale proofs,
unchanged serialized wire, existing empty-identity fallback, rejection before
partial installation, and proof lifetime without a retained history chain.
The production `canonical_number` implementation is unchanged. These tests
establish the targeted validation behavior; they do not measure throughput or
replace the full candidate checks. The current candidate still lacks a passing
repeated latency result, complete formal ABBA and 35-minute qualification.

## V11 frozen correctness and process closeout evidence

The frozen v11 executable is
`9592d4129bc5c27f0be6c104a76dbd298b6a40c004877f00ccb0b870bea7076d`.
`candidate-v11/freeze.json` binds the copied executable to **924 source files,
14 driver files and 20 plugin/runtime files**, the wrapper, Python executable,
Git state, tracked patch and actual build log/command. The build was release,
locked, with `pine-in-process,bench-execution-timing`. The numerical acceptance
object is identical to v10. The candidate adds the measured 499-delta cadence,
private live-hint proof and explicit diagnostic closeout; its new closeout
contract does not retroactively validate the old truncated ABBA trace.

The first full-suite attempt retained a failure in
`schema3_shared_hint_cow_corruption_isolated_and_rejected_before_any_install`:
its old assertion expected a tampered hint still to match. The narrow test
repair now expects rejection by the strengthened proof check. The first
all-feature Clippy attempt also rejected an assertion on the constant tail
limit; that test assertion was moved to a const block. Both original failures
remain in `workspace-v11-tests.log` and `clippy-all-v11.log`; neither required
weakening production validation or changing `canonical_number`.

The subsequent full workspace run passed **731 tests, with 22 ignored**, in
`workspace-v11-tests-repaired.log`. Default and all-feature Clippy with warnings
denied passed (`clippy-default-v11.log`, `clippy-all-v11-repaired.log`), followed
by the release build (`build-v11.log`). The combined Python diagnostic/driver
suite passed **50 tests** (`benchmark-python-v11-tests.log`). The earlier four
dedicated closeout Python passes remain at the originally named
`.local/bot-throughputput-trace-python-v11.log`; they are included evidence,
not four additional unique cases to add to the 50.

Five ordinary Rust closeout tests passed in `trace-shutdown-unit-v11.log`.
Five further ignored race cases were explicitly run one per fresh process and
trace in `trace-races-v11`, and each passed: rejection before the durable guard,
an already guarded append held across shutdown, real append failure, a closed
market with started native work held, and a caught native panic before trace-ID
allocation across stop/epoch change. The two original static-review blockers
were repaired before these runs: Closed and stop share a drain epilogue, and
contained computation/commit errors cannot disappear into successful closeout.
The deliberately caught panic text in passing test logs is the injected fault.

`http-closeout-v11` then exercised the frozen server through actual HTTP in four
separate processes, using a **38-agent native fixture** and cumulative native
call checks. This does not assert per-agent identity coverage or Pine capacity.
Its retained summary has `passed: true`, `complete: true`, all owned children
exited, and all case assertion-error lists are empty:

| Case | Observed shutdown | Protocol result |
| --- | --- | --- |
| Normal pause and shutdown | Exit 0, no forced kill, matching synced ack/EOF; cleanup 0.204130 s | Valid closeout; 76 decisions, all terminal, 27 accepted actions, no audit errors |
| Accepted incomplete HTTP body | HTTP 100 received, 1/65,536 bytes supplied; 2-second wait timed out, forced kill, exit 1, no ack | Expected invalid closeout; `qualified: false` |
| Ack-path collision | Existing sentinel unchanged; exit 1, no forced kill | Expected invalid closeout; `qualified: false`; complete trace still audited |
| Trace path is a directory | Trace-open error observed; exit 1, no forced kill, no ack | Expected invalid closeout; `qualified: false` |

The negative cases pass because the protocol rejects their injected failures.
Their false closeout qualifications remain false, even where a prefix trace
happens to have complete outcomes. These sub-second cleanup observations are
not a latency benchmark. The normal performance pilot explicitly requires
`--trace-graceful-shutdown --trace-shutdown-timeout 15`; cleanup never extends
the scheduled measurement cutoff, and failure must retain an invalid receipt.

The first PostgreSQL wrapper attempt, `pg-validation-v11`, **failed before any
fixture or HTTP test stage ran** (`stages: []`, exit 1). PostgreSQL started, but
the wrapper's captured pipe remained inherited across startup and its command
timed out after 75 seconds. The owned process (PID 38528), task data directory
and port 56713 were verified before a fast stop; the original log and explicit
stop receipt are retained, and `owned_cluster_stopped` is true. This is a
wrapper-startup failure, not a failed scheduler-recovery measurement.
`run-pg-validation-v11-v2.py` uses file-backed command output. Its separate
`pg-validation-v11-attempt02` results and the subsequent pilot follow below;
neither overwrites the original wrapper failure.

## V11 PostgreSQL fixture: recovery passed, HTTP group partially failed

`pg-validation-v11-attempt02/pg-fixture.log` records the real PostgreSQL 18.4
import and recovery of the retained 1000-agent, 500-record fixture. The dedicated
database is `mf_bot_throughput_recovery_v11_499`, with a fresh retained schema
`mf_scheduler_fixture_d83caa227b53bede37f2e733d1b90153`. The wrapper verified the
task data directory/port and **fsync, synchronous_commit, full_page_writes and
data_checksums all on**. It imported through the actual journal API into JSONB,
reconnected before reading, and retained original checksum validation.

| Public recovery path | Loaded mutations | Query/decode, s | Scheduler replay, s | Total, s |
| --- | ---: | ---: | ---: | ---: |
| No actor runtime view | 500 | 3.264100 | 13.057420 | 16.321519 |
| Actor-head runtime view | 500 | 3.275943 | 12.880295 | 16.156238 |
| Missing actor snapshot, full-journal fallback | 500 | 2.842445 | 12.923719 | 15.766164 |
| New full scheduler state with actor-head view | 1 | 0.336985 | 0.005488 | 0.342472 |
| New full state, missing actor snapshot fallback | 501 | 3.220823 | 0.005416 | 3.226239 |

All five paths produced both **raw JSON equality and normalized state equality**;
this fixture had zero numeric-representation changes. Deleting internal mutation
250 inside a transaction was rejected both with and without the actor snapshot,
then rolled back. After the new full state, all **501 canonical rows remain**
while the optimized runtime view exposes one row. These are real journal/query/
replay checks, not application startup or a 1000-bot PostgreSQL throughput test.
The test completed in **92.92 s** (wrapper phase **93.003 s**). Fixture-phase
sampling recorded test-tree RSS **1322.5625 MiB**, PG-tree RSS **326.71875 MiB**,
CPU **0.939326 / 0.128003 logical cores**, no compiler samples and no sampler
errors. These cover the whole import/recovery/verification phase; they are not
isolated replay memory or unique physical memory.

The following real HTTP restart group completed **two passes and one error**
in **16.273 s**. Qualified limit restart/cancel and automatic schema3 delta
restart/continuation passed. `test_schema3_postgres_restart_matches_continuous_limit_fill_and_close`
failed while reading an HTTP pause response with **WinError 10053**. Its cause
is unconfirmed; the original group remains failed and is not relabelled as a
pass. The attempt's manifest has exit 1, and its owned PostgreSQL cluster was
stopped.

The separate `http-pg-v11-schema3-retry` ran **only that exact failed method**
against a fresh dedicated database, using the same frozen v11 binary and test/
plugin inputs. It passed in **29.274 s** (wrapper phase **29.471751 s**), with
exit 0, unchanged inputs, complete resource samples and no compiler samples.
All four durability settings remained on, and its owned PostgreSQL cluster was
stopped. The retry is useful positive recovery evidence; it neither explains
the original WinError 10053 nor converts the earlier three-method group into a
clean pass. No PostgreSQL throughput or startup-capacity claim follows.

## V11 cycle pilot: valid receipts, latency and backlog gates still fail

`cycle-pilot-v11-1000` used the frozen server and the unchanged 1000-Pine plus
38-native, 10x, diverse-short cycle workload. The wrapper records
`measurement_valid: true`, all strict validity checks true and **no changed
source/driver/plugin/binary inputs**. Scheduled metric endpoints and process
sampling are complete, with zero compiler/other-server samples and no harness,
feed or bot errors. All 1000 Pine instances initialized; direct evaluation was
used for **81,427 calls**, with zero process fallback and zero skipped bars.

Accepted bar-close-to-durable latency is **P95 304.178 ms**, maximum **356.810 ms**,
from **1450 actions**, failing the fixed 250 ms gate. Both 60-second anchor
cohorts have enough accepted activity but fail latency:

| Anchor window | Accepted actions | MA / RSI / Breakout accepted | P95, ms |
| --- | ---: | --- | ---: |
| First minute | 711 | 515 / 163 / 33 | 273.181 |
| Second minute | 739 | 521 / 178 / 40 | 318.581 |

The maximum accepted-anchor gap is **3.113478 s**. The measured bar cohorts also
contain **8 discarded actions and 70 discarded no-order decisions**; they have
terminal receipts but do not satisfy the business-outcome qualification. The
scheduled cutoff observes all 1000 participants, with **334 one bar behind**;
the separate paused snapshot still has **78 one bar behind**. Later shutdown
does not erase either snapshot. The observed rate is **9.998134562 steps/s**:
the literal 10x comparison is false and the existing 95%-of-target gate is true.

Server CPU averaged **1.828361 logical cores** and its sampled peak was
**1190.996094 MiB**. The process-tree measurement reports **1.831246 cores** and
**1197.929688 MiB** summed RSS (429 samples), which includes the sampler helper.
These short-window figures do not resolve the prior long-run memory failure.
Order latency passed its existing gate. The shutdown completed in **1.224662 s**
with exit 0, no forced kill and an exact EOF/sidecar acknowledgement covering
**59,826,021 trace bytes**. The full audit reports all terminal outcomes and
no parse errors; the negative capacity result is not a truncated-trace artifact.

Whole-feed actual trades span **80..123**, while submitted limits span **80..120**;
there are 1115 actual feed trades with 186 rises and 151 falls. Final Pine ledgers
contain **592 / 143 / 6** MA/RSI/Breakout fill receipts. These include warmup and
the paused snapshot, and are not per-minute fill proof. Admission counts are
not fill counts.

### Remaining tail and scheduled phase comparison

The retained `latency-attribution.json` has valid inputs and all five saved-audit
cross-checks true. All **144 accepted samples above 250 ms** belong to anchors
where **1000 Pine decisions** become due together. The 333- and 334-decision
anchors have no accepted sample over 250 ms. For those 144 tail samples,
bar-close-to-decision averages **221.783 ms**, followed by decision-to-durable
**77.863 ms**; the first segment is larger in every sample. Across all accepted
actions, the independent P95 values are **224.471 / 91.208 / 304.178 ms** for
pre-decision, post-decision and total latency. These quantiles must not be added.
Pre-decision includes dispatch, queueing, computation and hint preparation;
post-decision includes ready waiting and ordered commit. The receipts cannot
assign individual stalls to checkpoint work without additional timed evidence.

All committed bar decisions, including no-order decisions, have total P95
**247.101 ms**. Their 1000-fanout subgroup is **271.771 ms**, versus
**116.023 / 109.078 ms** for 333/334 fanout. In the slow accepted anchor at
**66.859584 s** after measurement start, 1000 decisions finish between
**11.130 and 255.603 ms** after bar publication; the last complete durable
decision reaches **356.810 ms**. This points to the synchronized thousand-bot
decision burst as the main remaining tail dimension, while commit waiting still
adds material latency. It is not evidence that the Pine VM itself dominates.

`phase-comparison.json` retains seven historical case reports and their hashes.
The compact comparison below uses **each case's common completed `bot_turn`
count N**, including all invocations of each named server-wide stage:

| Metric | V9 first | V9 ordinary repeat | V10 first | V11 first |
| --- | ---: | ---: | ---: | ---: |
| N | 81,846 | 80,683 | 81,051 | 81,427 |
| `bot_turn`, elapsed ms/N | 2.237761 | 2.982253 | 2.819626 | 2.302746 |
| `bot_inprocess_call`, ms/N | 1.047551 | 1.299154 | 1.563072 | 1.219467 |
| Request-byte count, ms/N | 0.113487 | 0.152761 | 0.210624 | 0.138925 |
| Source verification, ms/N | 0.333333 | 0.470486 | 0.320784 | 0.329051 |
| Hint preparation, ms/N | 0.121419 | 0.138331 | 0.152021 | 0.142787 |
| Full validation calls | 70 | 69 | 66 | 24 |
| Full validation total, s | 2.442543 | 4.227233 | 4.415547 | 1.475531 |
| Full validation mean/call, ms | 34.893467 | 61.264245 | 66.902229 | 61.480458 |
| Journal operation total, s | 8.073673 | 12.908312 | 14.475258 | 6.887793 |
| Room guard held total, s | 16.306166 | 26.744696 | 30.005300 | 13.343626 |

V11's lower full-validation frequency and aggregate journal/guard durations are
local evidence. Its individual full validations still average 61.480 ms; hint
preparation still has **134,752 invocations** for 81,427 turns. The parent
`bot_request_clone` name measures eligibility/source verification on this route
(**0.331317 ms/N**) and contains the source-check child above. Other V11 child
costs are input Value construction **0.206186**, working-state clone **0.124779**,
two seals **0.268661**, cache-stamp clone **0.132751**, and actual VM execution
**0.041455 ms/N**. Request-byte counting still serializes the borrowed full
request through a counting writer. These are elapsed wall aggregates, not CPU;
parent/child scopes and concurrent tasks overlap. No parent and child totals,
stage means with different denominators, or lifetime maxima are combined.

The historical runs have different asynchronous action mixes and are not a valid
complete ABBA, so the table does not establish a causal version speedup. The
next measured target is reducing repeated state/request traversal and allocation
around the synchronized decision burst while preserving exact limits, hashing,
source checks and durable ordering. Canonical numeric formatting is a candidate
to profile, not a measured root cause: it has no isolated timer here. No further
production change or 35-minute run is qualified by this pilot; latency must pass
before another sustained attempt.

## V11 500-Pine cycle sample: short latency and backlog gates pass

`cycle-pilot-v11-500` ran the same frozen v11 executable
`9592d4129bc5c27f0be6c104a76dbd298b6a40c004877f00ccb0b870bea7076d`
with **500 Pine plus 38 native bots**, the existing diverse-short cycle profile,
10x target and 120-second measurement. Its recipe SHA-256 is
`d44639f59bc3bfb61e348fddcdcaa820225c004937613df7c6f7bc871868d481`.
`provenance-after.json` freshly compares all source, driver, plugin and binary
inputs with `candidate-v11/freeze.json` and records **no changes**. There was
**no separate prelaunch capture for this 500 case**: it reused the prior v11
freeze. It is a separate capacity point; the frozen 1000-count contract was not
changed, and this provenance boundary is retained.

All 500 Pine instances initialized, with **40,566 direct calls**, no fallback,
no skipped bars and no harness/feed/bot errors. Scheduled metric endpoints and
process sampling are complete; compiler and other-server samples are zero.
The scheduled cutoff observes all 500 participants with **zero backlog**;
the separate paused snapshot also has zero backlog. The measured rate is
**9.997056681 steps/s**: the existing 95%-of-target gate passes, while the
literal 10x comparison remains **false**. Long-run-only flags remain unset.

Accepted bar-close-to-durable latency is **P95 134.143 ms**, maximum
**145.623 ms**, across **526 actions**. Both anchor windows pass the unchanged
requirements of at least 20 accepted actions, at least one from each source
and P95 at most 250 ms:

| Anchor window | Accepted actions | MA / RSI / Breakout accepted | P95, ms |
| --- | ---: | --- | ---: |
| First minute | 137 | 90 / 38 / 9 | 94.611 |
| Second minute | 389 | 275 / 107 / 7 | 134.143 |

The maximum accepted-anchor gap is **13.018174 s**, below 60 seconds. The bar
cohorts contain 526 accepted actions and **39,514 committed no-order decisions**,
with no failed, discarded or pending outcomes. All generated actions and
decisions have terminal receipts. Graceful trace shutdown took **0.540940 s**,
with exit 0, no forced kill and an exact EOF/sidecar acknowledgement covering
**40,546,940 bytes**, with zero trace errors.

Server CPU averaged **0.796502 logical cores**, with sampled peak RSS
**715.851563 MiB**. The separately sampled process tree reports **0.796302
cores** and **703.058594 MiB** in the measurement interval (454 samples).
The two samplers have distinct sample instants; their peaks are not substituted
for one another. These figures describe this short run, not long-run memory.

Fresh `latency-attribution.json` reports valid inputs and all five audit
cross-checks true. Independent accepted P95 values are **70.646 ms** before
the decision, **60.911 ms** from decision to durable publication and
**134.143 ms** overall; these quantiles are not additive. Across all **40,040**
committed bar decisions, total P95 is **91.235 ms**. The 500-decision anchors
have all-decision P95 **103.860 ms**, versus **36.364 ms** for 167-decision
anchors; neither subgroup has an accepted sample above 250 ms. This is
consistent with the larger synchronized decision bursts having the longer
tail, without proving a causal version speedup or a linear scaling law.

Fresh `window-fills.json` uses conservative clock-publication bounds for actual
trade timestamps. Confirmed Pine fill receipts are **9 / 2 / 0** for
MA/RSI/Breakout in the first window and **7 / 8 / 2** in the second, with no
ambiguous Pine fills. Consequently, actual three-source fill coverage in every
minute is **false**, although the frozen accepted-action coverage gate passes.
Actual feed trades span **80..120** in each window, with **56 rises / 64 falls**
and **63 rises / 56 falls** respectively. Two feed trades cross window
boundaries and remain ambiguous; they are not used to fill coverage gaps.
The final ledgers contain **193 / 29 / 2** fill receipts including warmup and
later activity, so those whole-run counts are not substituted for window counts.
Accepted admission remains distinct from an actual fill.

This provides one passing **500-Pine short-run** capacity point. The failed
1000-Pine pilot, uncompleted 35-minute gate and absence of a valid latest-candidate
ABBA comparison remain unchanged; repeatability is not established by one run.

## V12 preparation: exact-byte encoder microbenchmark, no capacity result yet

Two narrow candidate changes are applied: safe ASCII strings bypass temporary
JSON string encoding while retaining the existing escaped/Unicode path, and
observation preparation captures persistent roots under the history guard before
materializing vectors outside that guard. `ascii-v12-tests.log` records **three
passing byte-equivalence tests** (ASCII pairs/control characters, every valid
Unicode scalar, and complete seal fields including missing/null/numeric cases).
`observation-lock-v12-tests.log` records **two passing tests** for materialization
after unlock and concurrent observations matching a cold result while retaining
an earlier wave. These five focused tests do not establish throughput.

`ascii-v12-microbenchmark.log` records a separate passing ignored Release test
over the **1000 actual schema3 states** saved by `cycle-pilot-v11-1000`. Its
scheduler SHA-256 was independently rechecked as
`fd526d6798edfd8cfde55edd1e388ac8c3c5fbab4a8c0c5d4a32e92c6debea74`.
Before timing, the test compares old/new complete seal bytes and verifies each
existing saved execution seal. Three ABBA rounds then encode every state and
compute SHA-256, using the same final-output allocation policy on both paths:

| Encoder | Six block times, ms | Median, ms |
| --- | --- | ---: |
| Legacy | 151.213, 160.690, 167.216, 158.092, 154.713, 164.841 | 159.391 |
| Safe ASCII | 82.219, 90.476, 90.743, 97.478, 83.540, 83.434 | 87.008 |

Every block encoded **14,033,549 bytes** and produced aggregate SHA-256
`79ba788bf8b1c4e2c418997476857e198f1d548746263f383b75c46091337eee`.
There are **1,020,821 eligible ASCII string sites** among 1,021,821 string sites;
these are encoding opportunities, **not measured allocator calls**. The
independent `ascii-v12-microbenchmark-audit.json` preserves log/source hashes,
round-order checks and exact statistics. The old oracle retains the previous
sorting, number spelling and escaping; the production seal fields and both
seal checks remain intact.

This is a repeated, warmed **fixed snapshot** comparison. It does not sample
the complete 32-bar checkpoint/tail cycle, including rollover, or establish the
distribution of state sizes seen throughout a live run. File read/JSON decode,
source verification, state cloning, dispatch, VM, commit and HTTP work are
outside the timed blocks. No per-block process-sampling receipt establishes
independent host quietness. The V11 phase evidence remains **162,854 seal calls
in 81,427 turns**, totalling **21,876.253 elapsed ms**, or **0.268661 ms/turn**;
it is an observed optimization target, not an amount that may be subtracted
from accepted latency. Parent/child scopes and parallel elapsed time must not
be added or treated as CPU time. V12's full build/checks, frozen HTTP validation
and end-to-end pilot remain separate pending evidence at this update; the
microbenchmark establishes no 1000-bot or long-run qualification. The subsequent
build, HTTP validation and pilot results are recorded below.

## V12 frozen validation and 1000-Pine pilot: local gains, capacity still fails

The frozen v12 executable SHA-256 is
`0f5f91ed89c6820be41153a37c067f677fc6f7576a2efc3f070d98becf5f7a2a`.
The complete workspace run passed **736 tests with 23 ignored**, both release
Clippy configurations passed, and the release build succeeded. The new
`http-pg-v12-three-tests` group passed all **three real PostgreSQL HTTP restart
tests in 42.035 s** (wrapper phase **42.297633 s**), with all four durability
settings on, unchanged inputs, no compiler or sampler errors, and its owned
PostgreSQL cluster stopped. These are correctness/recovery checks, not PG
capacity evidence; the earlier v11 Windows failure remains retained.

`candidate-v12/freeze.json` confirms the acceptance, pilot and trace-closeout
objects are identical to v11. `cycle-pilot-v12-1000` uses the same recipe hash
`8e689a741b9672894bc48e8d22215ba8fb24c96a3c07dccea5a9cb323f891379`,
with fresh before/after checks recording no changed inputs. Its wrapper reports
**measurement_valid true** and all strict validity checks true. Sampling and
scheduled endpoints are complete; compiler/other-server samples and
harness/feed/bot errors are zero. All 1000 Pine instances initialized, with no
runtime fallback or skipped bars.

The **890 accepted actions** have bar-close-to-durable **P95 314.524 ms**, maximum
**444.415 ms**, failing 250 ms. Both minute cohorts have sufficient accepted
activity from each source but fail latency:

| Anchor window | Accepted actions | MA / RSI / Breakout accepted | P95, ms |
| --- | ---: | --- | ---: |
| First minute | 301 | 42 / 152 / 107 | 289.427 |
| Second minute | 589 | 317 / 241 / 31 | 314.825 |

The maximum accepted-anchor gap is **6.007995 s**. All 890 actions and 79,150
no-order bar decisions have committed terminal outcomes, with no discarded,
failed or pending bar cohorts. At the scheduled cutoff, **323 participants are
one bar behind**; the paused snapshot has zero backlog. These are different
endpoints. The rate is **9.991712850 steps/s**: the 95%-of-target gate passes,
but literal 10x is false. Graceful shutdown completed in **1.540430 s**, exit 0,
without forced kill; EOF/sidecar acknowledgement covers **59,428,426 bytes**
with zero trace errors. Server CPU averages **2.016875 logical cores**, with
sampled peak **1145.710938 MiB**. The separate process-tree measurement has
445 samples, **2.016688 cores** and **1152.460938 MiB** peak summed RSS.

The independent latency helper reports valid inputs and all five audit checks
true. Accepted P95 values are **241.412 / 105.979 / 314.524 ms** for
bar-to-decision, decision-to-durable and total, respectively; do not add these
quantiles. All **128 accepted samples above 250 ms** occur at 1000-decision
anchors, and the pre-decision segment is larger in each. All 80,040 committed
bar decisions have total P95 **282.427 ms**; the 1000-fanout subgroup is
**327.600 ms**, versus **120.383 / 117.868 ms** for 333/334 fanout. The trace
still locates the longest tails around synchronized thousand-bot work, without
identifying a specific wait or CPU cause.

Whole-feed actual trade prices span **80..120**, with 1163 trades, 147 rises and
140 falls. Final Pine ledgers contain **343 / 55 / 3** MA/RSI/Breakout fills;
these include warmup and later activity and are not per-minute fill proof.
Accepted coverage cannot substitute for actual fill coverage.

### Work counts and state shape do not yet explain the other slower stages

`v11-v12-pilot-work-shape.json`, produced by
`analyze-pilot-work-shape.py`, retains source hashes, final-state distributions
and scheduled counter deltas. The table divides elapsed totals by the same
completed `bot_turn` count within each case, **81,427 for v11** and **80,583 for
v12**:

| Elapsed ms per common turn | V11 | V12 |
| --- | ---: | ---: |
| Whole bot turn | 2.302746 | 2.687967 |
| Seal computation | 0.268661 | 0.150842 |
| Event prefilter | 0.208779 | 0.115324 |
| Observation | 0.084883 | 0.064318 |
| Source verification | 0.329051 | 0.397374 |
| Input Value construction | 0.206186 | 0.284214 |
| Request-byte counting | 0.138925 | 0.216434 |
| Cache-stamp clone | 0.132751 | 0.184442 |
| Working-state clone | 0.124779 | 0.176025 |
| Actual VM execution | 0.041455 | 0.060145 |

The changes' corresponding local scopes improve, while several other scopes
increase. VM calls are **79,977 / 79,779**, checkpoint exports are **2335 in both
runs**, and seal calls are **162,854 / 161,175**, approximately two per turn.
Independent counter snapshots and scopes crossing the measurement endpoints
cause small count differences; the measurements are not an exact common event
cohort. They do not show a 40% increase in invocation count. Full validations
remain **24 in both runs**, with mean **61.480 / 50.411 ms**; aggregate room-guard
held time is **13.344 / 12.767 s**. These elapsed scopes include scheduling and
may overlap parents or concurrent work; they are not additive CPU costs.

The final paused snapshots likewise show no comparable increase in gross
durable-state size. Compact reserialized Pine state is **15,637,854 /
15,705,448 bytes** (+0.43%); checkpoint fields total **3,506,586 / 3,501,993**,
tail fields **9,626,752 / 9,694,033**, and tail intents **425,035 / 467,610**.
Series storage remains **389,175 bytes**. The tail-length populations change
from `{11:78, 12:256, 14:333, 29:333}` to `{12:334, 14:333, 29:333}`: the 78
MA instances still behind in v11's paused snapshot account for the additional
tail frames in v12. Checkpoint bar positions are identical. Object, array,
string and integer node counts each differ by less than 1%, and both snapshots have the same source/
interval populations. These are structural compact JSON counts, not exact
journal wire bytes or heap/RSS measurements.

A final snapshot is not a time-weighted sample of requests, account-event
history, transient allocation or the whole checkpoint cycle. The asynchronous
trade mix also differs. Consequently this comparison does **not** establish
that hardware, larger requests or either patch caused the remaining slowdown.
The next diagnostic target is separately measuring shared history-cell wait/
hold time and the screening-to-FIFO interval, while retaining the same gates.
V12 has no passing 1000-bot, repeatability or 35-minute qualification.

## V13 diagnostic pilot: valid measurement, no capacity pass

`candidate-v13/freeze.json` records executable SHA-256
`6ba77ec75a46bac77bb4fa9b8404afa6f94999cf002167a877627840bbdb5a9a`.
The acceptance, pilot and closeout objects match v12 exactly; the driver and
global 16-worker dispatch budget remain unchanged. V13 adds feature-gated
history-cell wait/held, screening-to-FIFO and paired synchronous thread CPU
measurements. Their observer overhead belongs to this diagnostic binary;
cross-version latency differences are not an isolated performance comparison.

Correctness coverage has an explicit revision boundary. The complete workspace
run passed **743 tests with 23 ignored before the final metric-name change**.
The final targeted timing run then passed **seven tests**, including one new
metric-contract test, giving **744 distinct tested cases across the two runs**.
This is not a 744-test complete-workspace rerun on the final source. Default and
all-feature Clippy passed, the final all-feature Clippy and release build
passed after the rename, and formatting passed. Relevant retained logs are
`workspace-v13-tests.log`, `timing-final-v13-tests.log`, `clippy-default-v13.log`,
`clippy-all-v13.log`, `clippy-all-final-v13.log` and `build-final-v13.log`.
The preceding PostgreSQL HTTP result remains evidence for its v12 binary.

The initial unpublished custom names ended in `count_total`, which would have
made the unchanged generic driver parser expect nonexistent nanos/max fields.
Before freezing, the new history sample counts changed to `samples_total`, and
the paired CPU/health counts received separate sample/failure names. Raw
measurements remain complete; neither zero maxima nor synthetic percentiles
were added. The final Rust contract test and a lightweight test executing the
actual Python `diagnostic_metrics` function verify compatibility with existing
stages. The driver file was not modified.

`cycle-pilot-v13-1000` reports **measurement_valid true**, all strict validity
checks true, no input changes, complete scheduled endpoints and process samples,
zero compiler/other-server samples, and no harness/feed/bot errors. All 1000
Pine instances initialized, with 80,638 direct route calls, no fallback and no
skipped bars. The fixed diverse-short cycle recipe remains
`8e689a741b9672894bc48e8d22215ba8fb24c96a3c07dccea5a9cb323f891379`.

There are **857 accepted actions**, with total **P95 399.781 ms** and maximum
**482.378 ms**. The two anchor windows both meet activity counts but fail the
unchanged 250 ms latency gate:

| Anchor window | Accepted actions | MA / RSI / Breakout accepted | P95, ms |
| --- | ---: | --- | ---: |
| First minute | 405 | 99 / 232 / 74 | 328.612 |
| Second minute | 452 | 95 / 335 / 22 | 410.126 |

The maximum accepted-anchor gap is **8.995843 s**. Bar cohorts include **78,982
committed and 129 discarded no-order decisions**; all have terminal outcomes,
but the discarded cohort does not meet the business-outcome gate. At cutoff,
**308 bots are one bar behind**, and the separate paused snapshot still has
**201 one bar behind**. Rate is **9.987569860 steps/s**: the 95%-of-target gate
passes and literal 10x is false. Graceful shutdown took **1.452873 s**, exited
0 without forced kill, and acknowledged exact EOF/length **59,311,546 bytes**,
with zero trace errors.

Server CPU averages **2.170916 logical cores**, with sampled peak RSS
**1236.050781 MiB**. The separate process-tree measurement has 428 samples,
**2.170670 cores** and **1185.558594 MiB** peak summed RSS. Sampling instants
differ; one peak must not replace the other. Whole-feed actual trades span
**80..120**, with 1121 trades, 147 rises and 137 falls. Final Pine ledgers contain
**337 / 17 / 4** MA/RSI/Breakout fill receipts including warmup and later activity;
these do not prove per-minute actual fill coverage.

### What the new diagnostics show

`timing-analysis.json` uses raw scheduled end-minus-start counters and verifies
their existing-stage deltas against the report. It has valid inputs, no errors
or warnings, and **80,625 completed bot turns**. Successful paired thread CPU
samples also number **80,625**, with **228.597146 s paired wall**, **176.687500 s
user CPU**, **20.937500 s kernel CPU**, or **197.625000 s combined CPU**, and
**482,530,815,448 cycles**. Read failures, nonmonotonic samples and overflow
rejections are all zero. Paired means are **2.835313 ms wall** and **2.451163 ms
CPU**. These are synchronous turn-scope aggregates, not accepted-action or
process-wide CPU cohorts. Sequential probe edges, accounting granularity and
instrumentation cost remain; cycles are not converted to frequency/time, and
no per-turn wall-minus-CPU, off-CPU percentile or CPU P95 is inferred.

The queue scopes have different populations and remain separate: screening
batch submission to current non-skipped FIFO offer averages **2.281684 ms**
over **80,040 offers**; existing FIFO insertion to permit acquisition averages
**74.389083 ms** over **80,641 pops**; actual compute submission to blocking
closure entry averages **0.035205 ms** over **80,641 starts**. The first includes
batch/pool/computation/parent-loop time, not just waiting. These means cannot be
summed into a decision's latency, but the observed FIFO wait is substantially
larger than the other two aggregate means.

| History-cell acquisition site | Completed guards | Mean wait, ms | Mean held, ms |
| --- | ---: | ---: | ---: |
| Event prefilter | 236,347 | 0.033917 | 0.003348 |
| Checkpoint window | 80,641 | 0.006516 | 0.000501 |
| Market data | 82,939 | 0.006623 | 0.005641 |
| Account events | 80,641 | 0.002673 | 0.000306 |
| Prior-wave root clone | 1,404 | 0.002849 | 0.010600 |

Prefilter lock wait totals **8.016111 s**; each site's wait and held sums share
its completed-guard denominator. These are nested, process-wide completed
scopes, excluding the separate history/public-view/event-stamp directory locks.
Small means do not rule out a particular long-tail lock wait or identify which
holder caused it. Atomic scrapes are noncoherent, scopes may cross window
boundaries, and elapsed time includes descheduling. No parent/child totals or
means from different cohorts are combined.

The independent receipt helper is valid with all five audit cross-checks true.
Accepted independent P95 values are **270.044 / 186.763 / 399.781 ms** for
bar-to-decision, decision-to-durable and total; these are not additive. Of
**204 accepted samples above 250 ms**, 157 come from 1000-decision anchors and
**47 from 333-decision anchors**. The latter have accepted P95 **78.963 ms
before decision and 195.270 ms after decision**, total **263.962 ms**, making
commit-side waiting a material second target. All **79,839 committed bar
decisions** have total P95 **355.170 ms**; the 1000-fanout subgroup is
**400.547 ms**. In v13, full validations number **30** at mean **63.585 ms**
(total **1.908 s**), versus v12's **24** at **50.411 ms** (total **1.210 s**).
Aggregate journal operations take **9.797 / 7.050 s** and room-guard held time
**17.917 / 12.767 s**, respectively. Asynchronous commit/action mixes differ;
these comparisons locate additional work and delay without proving causality.

The first timing-helper invocation selected the aggregate `report.json` beside
`bots-1000` and then looked for raw metrics in the wrong directory. Its failed
output and log are preserved as `timing-analysis-attempt01.*`, with original
hashes in `timing-analysis-repair-receipt.json`. The path resolver was narrowly
fixed and ten lightweight offline helper tests passed, including all four
supported directory/report input forms. The successful reanalysis is separate;
raw benchmark artifacts were not repaired or replaced. This was an offline
analysis error, **not a failed or incomplete pressure run**. A duplicate
latency-helper output attempt was refused by exclusive creation; its collision
log is retained and the existing successful JSON was left intact.

V13 establishes useful diagnostic evidence, not a capacity improvement or a
heap-lock diagnosis. Substantial synchronous turn CPU and FIFO wait support
investigating host work alongside ready/commit delay; they do not distinguish
allocation, spinning, source verification, scheduling or power effects by
themselves. The planned next experiment is **same-source System versus opt-in
mimalloc on Windows in serial ABBA order**, retaining the exact workload,
receipts, recovery checks and acceptance gates. Its implementation and results
must be recorded separately. The 1000-bot, repeatability, latest-candidate ABBA
and 35-minute qualification remain unmet.

## V14 preparation and mimalloc PostgreSQL checks

`candidate-v14-allocators/freeze.json` records two release builds from **928
identical source-manifest entries**. The source-manifest files are byte-equal,
both with SHA-256
`a7a72bec227284fe27a4b225432cdd754a60fbd3790e9dd69599cc170b0ed017`.
The recorded feature sets are `pine-in-process,bench-execution-timing` for
System and those same features plus `bench-mimalloc` for the opt-in alternative.
Both retain default features. Frozen executable hashes are:

| Allocator build | Executable SHA-256 |
| --- | --- |
| System | `f5812c97a72380865ea808ad9792f8125116e2898211b9927731b83dbe8eb043` |
| mimalloc | `3e8bda6cdc8a656f84983b971977e43ef0ce66b44757ba42730307d4a24c3b39` |

Both release builds and the all-feature Clippy check completed successfully;
logs are retained in `allocator-v14-build`. The mimalloc build records crate
`mimalloc 0.1.52` and `libmimalloc-sys 0.1.49`. This freeze associates operator
build logs and matching source manifests with the copied binaries; it is not
cryptographic compiler-input or allocator-behavior attestation. The freeze
confirms that driver/plugin inputs and the parent acceptance, pilot and
closeout contract are preserved. Current source has moved beyond v13, so the
historical v13 binary/receipts remain separate and are not rerun or relabelled
as a same-source control; the newly built v14 System executable is the control.

`http-pg-v14-mimalloc` ran the **actual mimalloc executable hash above** through
the same three PostgreSQL HTTP restart tests, once each, on the fresh dedicated
database `mf_bot_throughput_http_v14_mimalloc`. All three passed in **48.081 s**
(wrapper phase **48.334375 s**). The manifest records exit 0, unchanged inputs,
complete resource samples without compiler/sampler errors, all of **fsync,
synchronous_commit, full_page_writes and data_checksums on**, and the owned
PostgreSQL cluster stopped. The wrapper verifies executable identity; allocator
selection comes from the separately recorded build configuration, not a
runtime allocator self-test. These results validate the three tested recovery
paths, not PostgreSQL throughput or 1000-bot capacity.

The exclusive serial **System / mimalloc / mimalloc / System**, 1000-Pine,
120-second cycle comparison subsequently completed in `allocator-abba-v14-1000`.
Its measured results and retained capacity failures follow below.

## V14 allocator ABBA: valid comparison, no 1000-bot qualification

All four rounds independently passed measurement validity: freshly captured
before/after input differences are empty, compiler and other-server sample
counts are zero, scheduled metric windows and process samples are complete,
and graceful shutdown/trace EOF and all terminal receipts are valid. There are
no harness, feed, bot or trace errors, no runtime fallback, and no skipped bar
decisions. The wrapper retains `all_measurements_valid=true` and
`no_reported_false_gates=false`; exit 0 is not a capacity pass. Each round has
79,706 committed Pine bar decisions, with 40 anchors each dispatching all 1000
Pine bots, 40 dispatching 333, and 79 dispatching 334. All 1000 Pine bots are
initialized. The 38 native liquidity bots remain part of the recipe.
Whole-run dispositions are also retained: rounds 2 / 3 / 4 contain 36 / 36 /
35 discarded no-order decisions, and round 3 has two discarded actions. These
fall outside the measured bar cohort; every in-cohort no-order decision is
committed and every in-cohort action is accepted. Their terminal outcomes are
not silently omitted from the raw audit.

| Round | Accepted actions | Accepted P95 / max (ms) | Measured rate (x) | Measurement process CPU (cores) | Measurement sampled RSS peak (MiB) | Cutoff / paused bots behind |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 01 System | 1018 | 468.734 / 615.397 | 9.984189 | 2.200222 | 1159.188 | 0 / 0 |
| 02 mimalloc | 1218 | 355.965 / 454.414 | 9.988112 | 2.469283 | 1228.875 | 0 / 298 |
| 03 mimalloc | 646 | 456.424 / 630.015 | 9.971231 | 2.494258 | 1206.617 | 0 / 0 |
| 04 System | 1854 | 389.058 / 550.083 | 9.981405 | 2.339435 | 1184.465 | 0 / 301 |

The rate gate accepts at least 95% of configured 10x; all four pass it and all
four have literal `>=10x=false`. Paused backlog is one bar per affected bot.
It refers to the later paused market clock and does not contradict zero
scheduled-cutoff backlog. Neither pause nor final graceful drain replaces the
scheduled-cutoff test. Process measurement sample counts are 429 / 427 / 426 /
427, with no sampling errors.

Every minute contains at least 20 accepted actions and at least one from every
Pine source, and maximum accepted-anchor gaps are 6.099 / 5.998 / 12.002 /
3.016 seconds. Nevertheless every minute fails the 250 ms P95 component of
the frozen accepted-coverage contract:

| Round | Minute 1 accepted MA / RSI / Breakout; P95 (ms) | Minute 2 accepted MA / RSI / Breakout; P95 (ms) |
| --- | --- | --- |
| 01 System | 89 / 185 / 43; 365.293 | 350 / 314 / 37; 505.947 |
| 02 mimalloc | 194 / 136 / 124; 355.436 | 278 / 444 / 42; 355.965 |
| 03 mimalloc | 39 / 103 / 155; 415.934 | 106 / 205 / 38; 480.184 |
| 04 System | 859 / 79 / 71; 360.951 | 612 / 203 / 30; 407.067 |

The two-run System median accepted P95 is 428.896 ms (range 389.058–468.734),
versus mimalloc 406.195 ms (355.965–456.424), a descriptive 5.29% difference
with overlapping ranges. These are medians of per-run P95 values, not a pooled
P95 or confidence interval. Accepted action count, source mix, fanout mix and
actual trading paths differ substantially despite the frozen input recipe.
This comparison does not establish fixed-trade equivalence or a repeatable
allocator improvement.

Execution computation did not become cheaper in the mimalloc samples. The
successful paired synchronous-turn counters have zero read failures,
nonmonotonic samples or overflow in every round:

| Round | Paired turns | Paired CPU total (s) | Paired wall total (s) | Paired CPU / wall per turn (ms) | In-process call wall per common turn (ms) |
| --- | ---: | ---: | ---: | ---: | ---: |
| 01 System | 80724 | 202.937500 | 231.478815 | 2.513967 / 2.867534 | 1.536464 |
| 02 mimalloc | 80924 | 236.218750 | 260.804780 | 2.919020 / 3.222836 | 1.784447 |
| 03 mimalloc | 80352 | 237.843750 | 262.090029 | 2.960023 / 3.261774 | 1.816690 |
| 04 System | 81560 | 212.078125 | 239.961304 | 2.600271 / 2.942144 | 1.569640 |

Median paired CPU per turn is 14.95% higher and paired wall per turn 11.62%
higher for mimalloc. Measurement process CPU median is 2.481770 versus
2.269829 cores (+9.34%); sampled RSS median is 1217.746 versus 1171.826 MiB
(+3.92%). These CPU aggregates cover successful synchronous turn pairs, not
accepted decisions or the complete process. Cycles remain raw cycles; no
cycle-frequency conversion, per-event CPU P95, or wall-minus-CPU attribution
is made. In particular, the result does not prove a heap-lock cause.

Stage elapsed milliseconds below use each run's common completed-turn count;
parents and children overlap and must not be added. Source qualification is
still once per turn. New-bar append, stamp and VM counts are exactly 79,706
in each run; total turn differences include account/no-evaluation work.

| Stage (wall ms / common turn) | System 01 | MI 02 | MI 03 | System 04 |
| --- | ---: | ---: | ---: | ---: |
| Source check | 0.481447 | 0.475065 | 0.479629 | 0.493651 |
| Input Value construction | 0.293745 | 0.386052 | 0.395309 | 0.300736 |
| Request length encoding | 0.229769 | 0.306216 | 0.312711 | 0.235737 |
| Stamp clone | 0.184748 | 0.236309 | 0.243449 | 0.187282 |
| Working-state clone | 0.180296 | 0.236479 | 0.241901 | 0.185957 |
| Seal | 0.162507 | 0.195161 | 0.196342 | 0.166208 |
| VM execution | 0.060556 | 0.063835 | 0.064605 | 0.061265 |
| Delta-hint preparation | 0.158056 | 0.152567 | 0.150464 | 0.165057 |
| Observation preparation | 0.071883 | 0.100040 | 0.100459 | 0.079052 |

Final paused compact Pine JSON is 15,700,675 / 15,557,804 / 15,465,949 /
15,548,062 bytes. Every run's checkpoints total exactly 3,501,993 bytes and
series total 389,175 bytes. The tail distributions are respectively
`{12:334,14:333,29:333}`, `{11:298,12:36,14:333,29:333}`,
`{11:334,14:333,29:333}`, and `{11:301,12:33,14:333,29:333}`. Thus the larger
mimalloc turn cost is not accompanied by a larger final checkpoint/state
snapshot. This does not establish equal time-weighted request payloads,
allocation traffic, journal bytes or heap retention during execution.

Full journal validation occurs 24 / 32 / 31 / 31 times, taking respectively
1.836 / 2.436 / 2.313 / 2.143 seconds total, with own-call means 76.507 /
76.129 / 74.603 / 69.115 ms. Journal-operation totals are 8.822 / 11.448 /
11.234 / 10.628 seconds; room-held totals are 16.034 / 19.216 / 18.620 /
19.594 seconds. These aggregates show different commit work and do not locate
a particular accepted tail or establish writer CPU from wall time.

Receipts independently separate the two sides of decision completion:

| Round | Accepted bar → decision P95 (ms) | Accepted decision → durable P95 (ms) | All-bar total P95 (ms) | All-bar total P95 at fanout 1000 (ms) |
| --- | ---: | ---: | ---: | ---: |
| 01 System | 378.105 | 176.451 | 362.737 | 415.585 |
| 02 mimalloc | 252.046 | 209.274 | 322.039 | 364.986 |
| 03 mimalloc | 344.475 | 183.238 | 337.557 | 391.211 |
| 04 System | 273.656 | 166.055 | 364.906 | 407.067 |

Each 1000-fanout row covers 40,000 committed bar decisions. The all-bar cohort
is broader and more comparable than accepted-only counts, but its ordering,
orders and commit grouping remain asynchronous. The mimalloc all-bar median
P95 is lower (329.798 versus 363.822 ms), while accepted post-decision P95
median is higher (196.256 versus 171.253 ms). Neither can be substituted for
the paired execution CPU result, and these independent quantiles cannot be
added. Round 2 is a concrete example: 72 of 240 accepted actions over 250 ms
come from 333-bot anchors, whose accepted pre/post P95 is 75.946 / 228.446 ms;
the commit side matters there. In round 3 all 164 late accepted actions come
from 1000-bot anchors. All four still show expensive 1000-bot bursts.

Own-scope mean FIFO waits remain 76.081 / 75.013 / 77.610 / 77.267 ms;
pre-FIFO screened-offer means are 2.232 / 2.339 / 2.259 / 2.349 ms and blocking
pool waits 0.0366 / 0.0367 / 0.0374 / 0.0363 ms. Prefilter history-cell mean
waits are 0.03495 / 0.03286 / 0.03128 / 0.03481 ms, with held means
0.00331 / 0.00369 / 0.00365 / 0.00361 ms. These populations and nested elapsed
scopes differ; they do not yield a critical-path sum or a thread-lock diagnosis.
The evidence supports retaining System as the control and investigating the
remaining request/state preparation work with fixed-work validation, rather
than claiming that allocator replacement solved the latency problem.

Actual trade checks remain separate: all eight minute windows have feed
execution prices spanning 80–120 and observed rises/falls. Conservatively
anchored Pine ledger fills (MA / RSI / Breakout) are **3/4/2 then 1/12/3**,
**5/6/4 then 7/12/2**, **1/2/2 then 2/4/0**, and **14/5/3 then 10/8/0**.
The last two rounds have no definite Breakout fill in minute 2 despite accepted
Breakout actions. Accepted admission is not fill proof; ambiguous cross-window
ledger timestamps do not repair a gap.

Derived evidence is retained alongside each round as `timing-analysis-v14.json`,
`latency-attribution-v14.json` and `window-fills-v14.json`; all validate, and all
five receipt-audit cross-checks pass per round. `work-shape-v14.json`,
`comparison-v14.json` and `offline-analysis-receipt-v14.json` retain hashes,
commands, normalization and scope. `analyze-allocator-abba.py` reproduces the
group comparison from these inputs without changing raw reports. The earlier
timing helper's compatibility comparison field is historically named `v12`;
these runs explicitly pass System round 1 as its baseline, whose actual path
and hash are recorded, and the group analysis uses each `candidate` directly.

`allocator-long-v14-prerequisite-check/long-manifest.json` records
**`status=refused`, `attempted=false`, `check_only=true`**. Both mimalloc short
rounds fail accepted latency/coverage; round 2 additionally fails the paused
backlog gate. No 2100-second workload was started, no threshold was loosened,
and no 35-minute or memory-retention qualification follows from this ABBA.

## Latest System versus original baseline: complete 500/1000 flat ABBA groups

`flat-abba-v14-system-1000/runs/summary.json` and
`flat-abba-v14-system-500/runs/summary.json` retain two separate serial
baseline / candidate / candidate / baseline groups. Each uses the original
**flat input, 50 ms clock interval (20x), 20 s warmup, 30 s measurement,
64-longest-bar prewarm and 38 native liquidity bots**, without durable traces.
All eight child runs exit 0. In each group all nine measurement invariants are
true: reports, recipe/complete initial history, compiler/other-server quietness,
full duration, process sampling, harness errors and metric-window capture.
Both summaries retain `final_input_changes=[]`.

The baseline is `4f27efbe699594a9188891873fa887125185f8048743cd4f30fe20ae264be401`
with its baseline plugins; the candidate is v14 **System**
`f5812c97a72380865ea808ad9792f8125116e2898211b9927731b83dbe8eb043`
with its candidate plugins. The private Python runtime and Pine strategy sources
are unchanged; intended plugin implementation differences remain captured in
the summaries. Before/after source manifests for **both groups** have SHA-256
`a7a72bec227284fe27a4b225432cdd754a60fbd3790e9dd69599cc170b0ed017`, matching the
v14 build freeze. Recipe hashes are `16dda3d26595820d47a057d00348c09a7b9d4001c9b1827fe05ca4d27df38099`
for 1000 and `39768f71fe46c30b5a0fe130ead890840e2ee0c4ed56b81f53876a6ddc10a37d`
for 500. The common prewarm candles hash is
`11b84c02f79a67dca989f0370cd45c26c42074be7e10a51df1bc5f959d4f11a2`.
The 1000/500 summary hashes are respectively
`d4c75ed1bbb1a8b5c7f4aba82f2fda322345e770c926bfd88c9bcf2b253c146c` and
`75f4e4c112ed25b995ffc57b10a8383cabdef6f20b8bc1a7c8772d29e6af9f3b`.

| Pine count | Metric: two-run median | Baseline | Latest System | Observed change |
| --- | --- | ---: | ---: | ---: |
| 1000 | Clock rate (x) | 10.812683 | 19.501898 | +80.36% |
| 1000 | HTTP place P95 (ms) | 100.339 | 28.912 | −71.19% |
| 1000 | HTTP cancel P95 (ms) | 87.296 | 24.093 | −72.40% |
| 1000 | Measurement process CPU (cores) | 5.269649 | 4.224882 | −19.83% |
| 1000 | Measurement sampled RSS peak (MiB) | 3349.074 | 1212.695 | −63.79% |
| 500 | Clock rate (x) | 14.403685 | 20.008186 | +38.91% |
| 500 | HTTP place P95 (ms) | 71.272 | 25.936 | −63.61% |
| 500 | HTTP cancel P95 (ms) | 59.624 | 22.546 | −62.19% |
| 500 | Measurement process CPU (cores) | 5.007341 | 2.193578 | −56.19% |
| 500 | Measurement sampled RSS peak (MiB) | 2090.451 | 719.686 | −65.57% |

These are medians of each run's rate, P95 or sampled peak, not pooled HTTP
quantiles, unique physical memory or confidence intervals. The workload is
matched by configuration and initial history; faster candidates execute more
market steps during the same wall window, so completed trading work is not
held constant. The observations support a throughput improvement for the full
candidate bundle, not attribution to one patch or equivalence of asynchronous
trade outcomes.

| Pine count / round | Rate (x) | HTTP place / cancel P95 (ms) | Paused bots behind | Skipped bar decisions |
| --- | ---: | ---: | ---: | ---: |
| 1000 / baseline 1 | 12.184132 | 102.899 / 75.145 | 422 | 5145 |
| 1000 / System 2 | 19.715394 | 19.149 / 25.385 | 909 | 0 |
| 1000 / System 3 | 19.288402 | 38.675 / 22.801 | 162 | 21 |
| 1000 / baseline 4 | 9.441233 | 97.779 / 99.447 | 951 | 4767 |
| 500 / baseline 1 | 14.751945 | 60.643 / 55.469 | 440 | 0 |
| 500 / System 2 | 19.978575 | 31.616 / 25.824 | 0 | 0 |
| 500 / System 3 | 20.037797 | 20.256 / 19.269 | 0 | 0 |
| 500 / baseline 4 | 14.055425 | 81.902 / 63.778 | 129 | 0 |

Both candidate runs at each count pass the configured-rate threshold of 19x
(95% of 20x). Literal `>=20x` remains false for both 1000 runs and the first
500 candidate, and true only for the second 500 candidate. No skipped bars or
paused backlog were observed in either candidate 500 sample. The 1000 samples
remain unsuitable for a zero-skip/zero-backlog capacity claim, even with the
large improvement. All 1000 measurements have 107 process samples each; the
500 baseline round 1 has 105, and the remaining rounds have 107. Sampling
completeness passed for every round.

The baseline's `requested_runtime_route=false` reflects missing new route
counters; it is not measured process fallback. The candidate's current direct
route is observed. Scheduled-cutoff durable progress and accepted-action
bar-close-to-durable latency are **unavailable in these untraced groups**.
Independent HTTP probes being fast cannot supply those missing measurements,
and paused snapshots cannot substitute for cutoff receipts. The failed
1000-bot cycle latency results and the refused 35-minute prerequisite remain
in force. The newer 500 flat result is also separate from the older v11
500 cycle sample; neither establishes a latest-candidate long-duration pass.

The first command attempt is retained at `flat-abba-v14-system/driver.log`:
the wrapper rejected `--counts 1000,500` because that option accepts one integer.
Argument parsing failed before workload startup; that directory contains the
failure log and unchanged source manifests, with no case reports. The two
subsequent groups use separate valid integer arguments and outputs. This was
a CLI invocation failure, not a failed or omitted performance sample.

The allocator alternative was rejected and its optional source patch was
actually reversed. `allocator-optin/reverted.json` records reversal of exact
patch `f0cc1baa93b140785b08d2fcb153736e4536b402c62477b4d808efab0ad0fdee`
and restoration of the pre-experiment lockfile with verified SHA-256
`7147ac6f2b0e8efae4ce7b91a07eff77688129993b4f0db05d88238465b82c21`.
Its frozen executables, source manifests and measured failures remain historical
artifacts, without relabelling or replacement. The latest results show
that the broader runtime work has produced a real short throughput gain, while
1000-bot trading response time and sustained resource/backlog behavior still
need a passing candidate. No production deployment is implied by these
isolated-worktree measurements.

## V15: stamp-only build and recovery validated, 1000-bot cycle still fails

The applied stamp change replaces the schema-3 session's owned three-field
`Value` stamp with a reusable complete byte buffer for checkpoint, tail and
frozen intents. Match checks compare the complete serialized bytes and length;
this is not a digest, omission of fields or a new durable format. Settings,
source admission, public execution seals and speculative-session publication
boundaries remain in place. A missing or mismatching cache reconstructs the
committed checkpoint/tail; schema 2 retains its original transcript path.

`stamp-cache-micro/summary-01.json` validates all **72 samples** and their
three-round ABBA ordering: three modes, one or sixteen workers, and old/new
variants. Each variant has six retained samples for each mode/worker count.
The standalone crate uses System allocation and serde_json 1.0.150 with exactly
the captured `default`, `raw_value`, `std` features. Its fixture is the 1000
actual saved schema-3 states from v14 System round 1, scheduler SHA-256
`62139c4cb7f59a7cdd28ddaffb2707db1bdf28388585a3333fcd62d69ee2c5c1`.
All states have equal old/new complete stamp bytes and match outcomes, and
mutations to each stamp field are rejected. The fixture has no unfrozen numeric
floats; arbitrary float signed-zero equality can conservatively miss in the
byte cache, rather than produce a false match.

| Workers | Isolated cache mode | Old median (ms) | Byte-cache median (ms) | Change |
| --- | --- | ---: | ---: | ---: |
| 1 | Build | 117.45110 | 29.81385 | −74.62% |
| 1 | Match | 59.04900 | 92.63785 | **+56.88% regression** |
| 1 | FullCycle | 452.73920 | 171.09890 | −62.21% |
| 16 | Build | 28.60980 | 11.11060 | −61.17% |
| 16 | Match | 12.83365 | 10.76090 | −16.15% |
| 16 | FullCycle | 58.32635 | 20.19810 | −65.37% |

`FullCycle` means constructing, matching and destroying one cache for each of
these fixed states; it does **not** mean a complete runtime turn or a 32-bar VM
checkpoint cycle. Buffers were provisioned outside timing and already fit the
unchanged snapshot. Parsing, preparation and thread creation are excluded;
worker release/result communication is included, and concurrent worker times
must not be added to wall time. The actual fixture tails are 12 / 14 / 29 bars
for 334 / 333 / 333 bots. Growth, rollover reallocation, public seal work,
source I/O, VM execution, queues and durable commits are outside this experiment.
The single-worker Match samples and their regression remain included; no
best-run selection or causal explanation for their variation is substituted.

Complete stamp bytes total 13,684,184, with length-framed hash
`b3af6e17887f1472ec81b95ef7d830b2f36193dd5565adc700767c501c4eaf5b`.
Seeded byte-buffer capacity totals 21,839,872 bytes and retains sufficient
capacity across reuse. This is neither measured allocation-call savings nor
an RSS comparison; the experiment installs no allocator-call counter. Its raw
JSONL SHA-256 is `42cad7e983f4103c20326b6bdaf659171cf6270282a27a75fa12a860e843db69`.

The first standalone test build failed with **E0282** because the cleanup
channel's sender type could not be inferred; `stamp-cache-micro/tests.log`
retains that failure. After the explicit type correction,
`tests-attempt02.log` records **8 micro tests passed**, and `build.log` records
the successful release build. The applied server integration subsequently
passed **11 stamp tests** in `stamp-cache-v15-tests.log`, including actual Host
cold/warm continuation, 32/64 rollover, cache eviction, speculative late-response
rejection and retry from the old committed state, public-seal rejection,
missing-cache/seed isolation and schema-2 compatibility. These focused results
do not substitute for final-candidate workspace/recovery or capacity validation.

The separate request-size upper-bound candidate passed **six targeted tests**
(`request-size-v15-tests.log`), followed by the explicit ignored admission
microbenchmark. All **12 ABBA sample rows** are retained in
`request-size-upper-bound-proposal/micro-attempt01.jsonl` and its summary.
For 1000 saved states combined with a real RoomManager 64-bar observation,
the old exact counter's median is **44.93640 ms** versus **56.56155 ms** for
the proposed upper-bound path, a **25.87% regression**. Every candidate sample
is slower than every old sample. All 1000 requests fast-accept in each candidate
sample with zero exact fallbacks, so fallback work does not explain this result.

This is single-thread size admission only, excluding input Value construction,
source I/O, VM, queueing and persistence; the combined fixture is not a coherent
VM restart or a real trading run. CPU stacks and allocation counts were not
measured. Raw JSONL SHA-256 is
`2cf166feac74260c953c6a27d8b3103c4e9b2a44eae8ad4f36c7622314c703dd`.
`request-size-upper-bound-proposal/reverted.json` confirms exact reversal of
patch `126bd0cd3335b31d9accc686822e3de559f649f638d4aeb7349dfa06c7405ca4`;
production retains its exact serde JSON length check. The request-size change
is not part of final v15, and its failed performance result remains preserved.

The final stamp-only candidate passed the release/locked/all-features workspace
suite: **755 passed, zero failed, 23 ignored**, retained in
`workspace-v15-tests.log`. Default-feature and all-feature release Clippy with
`-D warnings`, formatting and diff checks passed. These results cover the final
System-allocator source after the request-size alternative was removed; they
do not treat the ignored tests as executed. The feature-selected release server
build completed in **1 minute 18 seconds** (`candidate-v15-build/build.log`).
The before/after build source manifests have the same SHA-256
`61cd7bb7f94178a8e29ca0efee9c3223bdbfc54301224097c4c597ac3d9625e8`.

`candidate-v15/freeze.json` records status `frozen`, the unchanged parent
acceptance/pilot/closeout contract, and all 14 driver dependencies and plugins
identical to v13. The candidate uses `pine-in-process,bench-execution-timing`,
System allocation and the complete private stamp cache; it excludes the
request-size upper-bound proposal. Its executable SHA-256 is
`0c37d6549702f4bd6789af71b65542087fd7942ff2400f0b19cfd5a8048b669f`;
the associated PDB SHA-256 is
`3ebaa3a0be1c05d6790d7a1b80bd52e9e6d05e10ceaae3b0aa2c45f369c156fc`.
This is captured build/source provenance, not a compiler-input attestation.

`http-pg-v15/manifest.json` and `http-pg-three-tests.log` record all **three real
HTTP PostgreSQL recovery tests passed** in **47.590 seconds** (wrapper stage
47.8215396 seconds): qualified-limit restart/cancel, schema-3 continuous versus
restart fill/close, and automatic schema-3 delta restart/continuation. The exact
frozen executable was checked. `fsync`, `synchronous_commit`, `full_page_writes`
and data checksums remained enabled on the dedicated database. Exit code is
zero, input changes are empty, compiler-contaminated samples are zero, resource
sampling errors are empty, and the owned PostgreSQL cluster stopped successfully.
These checks establish recovery behavior, not PostgreSQL throughput or BOT
capacity.

`cycle-pilot-v15-1000/wrapper-manifest.json` records a valid completed
120-second cycle measurement: exit zero, all measurement-validity checks true,
input changes empty, complete terminal receipts and acknowledged graceful trace
closeout. Its report SHA-256 is
`b1e61f3748e8bc5956eac5e665923447c5812fb63077c3a3b40a3b86fcd2187d`;
the recipe SHA-256 is
`8e689a741b9672894bc48e8d22215ba8fb24c96a3c07dccea5a9cb323f891379`.
The frozen System executable, 1000 Pine plus 38 native bots, cycle feed,
20-second warmup, 64 prewarm bars and 100 ms interval are unchanged. Compiler
and other-server sample counts are zero. This uses the in-memory durable
journal, not PostgreSQL.

Capacity nevertheless **fails**. Accepted bar-close-to-durable latency is
**264.242 ms P95**, 107.116 ms median and 445.994 ms maximum across **1295**
accepted actions. Both anchor windows fail the unchanged 250 ms P95 gate:

| Anchor window | Accepted | MA / RSI / Breakout accepted | P95 (ms) | Window gate |
| --- | ---: | --- | ---: | --- |
| 0–60 s | 542 | 290 / 154 / 98 | 257.151 | Fail |
| 60–119.989144 s | 753 | 419 / 307 / 27 | 322.790 | Fail |

Action counts and every-source presence exceed the required minima, and the
maximum accepted-anchor gap is 4.004881 seconds, below 60 seconds. Their
latency failures still make sustained accepted coverage false. These are
anchor cohorts; a durable outcome may complete after its anchor's window.
Admissions are not fills. The final paused ledgers contain 416 / 45 / 3 fill
receipts for MA / RSI / Breakout, including warmup and drain; these are not
per-minute or scheduled-cutoff fill measurements. Actual feed trades span
80–120 ticks with both rises and falls; requested limit prices are separate.

At the scheduled cutoff, all 1000 participants have committed progress evidence,
but **299 are one bar behind** (maximum backlog one). After pause, all 1000 are
initialized and caught up; skipped-bar decisions, BOT errors and receipt errors
are zero. The zero paused backlog cannot replace the failed cutoff. Clock rate
is **9.990830994 steps/s**, passing the configured 9.5 threshold while literal
10 steps/s remains false. Both observed clock windows pass 95% of target and
full-duration/process-sampling checks pass.

The report's server CPU average is **1.615219810 logical cores** and peak memory
is **921.136719 MiB**. Separately, the sampled process tree reports measurement
CPU **1.615673339 cores** and **889.25 MiB** peak summed RSS from 430 samples;
these differently sampled fields are not interchangeable. The short-run memory
guard passes, without establishing 35-minute retention behavior. The 1000-bot
long-run prerequisite remains failed.
No deployment or sustained qualification is claimed, and this
single asynchronous run does not establish a causal end-to-end improvement
from the stamp microbenchmark.

The first **500-Pine-bot v15 pilot passes all applicable short-run gates**.
`cycle-pilot-v15-500/wrapper-manifest.json` records measurement validity true,
exit zero, no wrapper/report errors, no input changes and the same v15 freeze.
Its explicit count override is recorded; the recipe still adds the same 38
native liquidity bots, with all other numerical settings unchanged. Report
SHA-256 is `1cd6dd2c3132ffc6ddd028403044ac9471ef95745fb68b6204abf22dbb5ee200`;
recipe SHA-256 is `d44639f59bc3bfb61e348fddcdcaa820225c004937613df7c6f7bc871868d481`.

Across **342 accepted actions**, bar-close-to-durable P95 is **146.392 ms**,
median 79.281 ms and maximum 168.962 ms. Window 0–60 s has 89 accepted
(MA / RSI / Breakout **31 / 46 / 12**) at **109.741 ms P95**; window
60–119.991018 s has 253 (**186 / 60 / 7**) at **157.601 ms P95**. Both windows
pass the count, source and 250 ms latency gates. Maximum accepted-anchor gap
is 10.497356 seconds. All 500 participants have cutoff evidence, cutoff and
paused backlog are zero, and skipped bars, BOT errors and receipt errors are
zero. Graceful trace closeout and complete measurement sampling also pass.

Clock rate is **10.006460243 steps/s**, so both the configured 9.5 threshold
and literal 10 target pass in this run. Report server CPU is **0.786231151
logical cores** and peak memory is **600.269531 MiB**. The separately sampled
process tree has 429 measurement samples, **0.787641581 cores** and **577.132813
MiB** peak summed RSS; compiler and other-server sample counts are zero. The
independent repeat is reported below. This first 120-second sample alone did
not establish repeatability or 35-minute qualification; the failed 1000-bot
prerequisite is not substituted or weakened.

The independent `cycle-pilot-v15-500-repeat` also passes the **configured**
short-run contract with the same recipe, frozen executable and no input changes.
Its report SHA-256 is
`e0533cd9c4ef51504a4d8c1ae8c1bb6ca09b145c539e41bf5d7cdce8ecc0e5f9`.
There are **551 accepted actions**, with **149.572 ms P95**, 62.581 ms median
and 172.632 ms maximum. Window 0–60 s has 277 accepted (**196 / 76 / 5** by
MA / RSI / Breakout) at **149.572 ms P95**; window 60–120.009853 s has 274
(**98 / 156 / 20**) at **140.768 ms P95**. Both windows pass, maximum accepted
anchor gap is 9.993082 seconds, and cutoff/paused backlog, skipped bars and
errors are zero. Measurement validity, complete trace closeout and sampling
pass; compiler and other-server sample counts are zero.

Repeat clock rate **9.999309739 steps/s** passes the configured 9.5 threshold
but literal 10 remains false; it is not rounded up. Server CPU is **0.785250495
cores** and peak memory **602.214844 MiB**. Its 429 process-tree measurement
samples give **0.785249575 cores** and **581.5625 MiB** peak summed RSS. Two
passing short samples permit checking the matching-count long prerequisite;
they do not establish 35-minute stability. The long wrapper must re-audit the
complete short traces, bind their hashes and recheck the frozen inputs before
an explicit 500-bot attempt. It does not enable a 1000-bot long attempt.

### V15 offline attribution and actual-fill boundaries

After all three servers exited, `offline-analysis-v15-plan.json` was executed
serially by `run-offline-v15-plan.py`. Its receipt records all **nine commands
exited zero with `valid_inputs=true`**, retaining helper/output hashes and new
per-case logs; no prior output or raw evidence was overwritten. Each case now
has `timing-analysis-v15.json`, `latency-attribution-v15.json` and
`window-fills-v15.json`. All five latency-to-saved-audit cross-checks pass for
each case (15 checks), and all raw timing-to-report counter cross-checks pass.
Timing uses scheduled start/end scrapes, not paused metrics. V15 1000 is compared
descriptively with v14's first System sample; first v15 500 with historical
v11 500; repeat 500 with first v15 500. Each comparison has an equal recipe
hash. The timing helper's historical `v12` field label does not change these
explicit baseline paths; missing old CPU/history counters remain null.

| Measured quantity | v15 1000 | v15 500 first | v15 500 repeat |
| --- | ---: | ---: | ---: |
| Completed turn scopes | 81,079 | 40,382 | 40,591 |
| Turn elapsed mean (ms) | 2.061870 | 1.914974 | 1.923659 |
| Paired turn CPU mean (ms) | 1.759087 | 1.568613 | 1.564771 |
| Dispatch queue own-scope elapsed mean (ms) | 53.216803 | 19.521677 | 19.498115 |
| Accepted bar-to-decision P95 (ms) | 195.436 | 61.850 | 63.363 |
| Accepted decision-to-durable P95 (ms) | 152.389 | 103.346 | 90.966 |
| Accepted total P95 (ms) | 264.242 | 146.392 | 149.572 |
| All committed bar-decision total P95 (ms) | 257.151 | 107.812 | 106.352 |

These columns have different populations: all committed bar decisions number
80,040 / 40,040 / 40,040; accepted populations number 1295 / 342 / 551. The
three latency quantiles are independently calculated and must not be added.
Turn CPU uses its own successful-pair cohort: 81,079 / 40,382 / 40,591 pairs,
with zero read failures, nonmonotonic or overflow samples. Paired aggregate
CPU is 142.625 / 63.343750 / 63.515625 seconds, while paired elapsed totals are
167.154970 / 77.322199 / 78.075006 seconds. These aggregates provide no per-event
off-CPU time, CPU P95, processor frequency or heap-lock attribution.

The 1000-bot breach is concentrated in **synchronized 1000-decision anchors**:
all **91 accepted actions over 250 ms** come from those 40 anchors. Of those
91, **88 have more pre-decision than post-decision elapsed time**; 36 exceed
250 ms before the decision, and none exceed 250 ms in the post segment alone.
The 333/334-decision anchors have no accepted breaches. For the 40 full-fanout
anchors, accepted total P95 is **297.125 ms**, and the 40,000 committed
bar-decision total P95 is **293.975 ms**. At 500 bots, the 40 full-fanout anchors
have committed total P95 **124.393 / 118.622 ms**, and no accepted breaches.
This locates the remaining capacity problem at synchronized fanout and its
queueing/commit tail; it does not identify one lock or imply missing VM work.

Against the first v14 System sample, v15's 1000-bot completed-turn mean changes
from **2.867868 to 2.061870 ms**, paired CPU from **2.513967 to 1.759087 ms**,
and stamp-stage elapsed per common turn from **0.184748 to 0.027373 ms**.
In-process call elapsed per turn is **1.536464 → 0.876110 ms**; dispatch own-mean
is **76.081199 → 53.216803 ms**. The source-read/check stage remains relatively
large (**0.500506 ms per turn**, versus **0.041924 ms** for VM execution), with
input Value construction **0.212425**, exact request sizing **0.140097**, and
two seal checks together **0.143089 ms per turn**. These are scope-specific
elapsed values, with nested parents and differing completion cohorts; adding
them or calling them CPU shares is invalid. The two 500-bot turn/paired-CPU
and dispatch means are close despite different accepted action mixes. This is
supporting runtime evidence for the narrow cache change, not a same-source
ABBA causal estimate or proof of 1000-bot qualification.

Post-decision work also remains visible. The v15 1000 sample has **33 full
validations averaging 52.104203 ms**, versus 24 averaging 76.507292 ms in that
v14 System sample; differing counts and payloads prevent an equal-work claim.
Room-held own-mean is **1.716335 ms** and journal-operation own-mean **0.387638
ms**. One 64-decision publication at 106.207028 seconds has post delays ranging
from **191.452 to 200.238 ms**. A trace publication bounds the complete
ready/transaction/publication interval but cannot split it into a particular
writer wait or full-validation event. Neither a queue average nor this sampled
batch justifies another unmeasured commit optimization or a PostgreSQL
saturation claim.

Conservative clock-publication mapping gives the following **actual** Pine
fill-receipt counts per anchor-window time range, separately from admissions:

| Case | First minute MA / RSI / Breakout | Second minute MA / RSI / Breakout |
| --- | --- | --- |
| v15 1000 | 10 / 4 / 2 | 7 / 5 / 1 |
| v15 500 first | 3 / 4 / 1 | 2 / 4 / 1 |
| v15 500 repeat | 12 / 5 / **0** | 5 / 1 / 1 |

Every feed window has confirmed actual prices spanning **80–120 ticks** with
both rises and falls. No Pine fill is ambiguous at these boundaries. Ambiguous
feed-trade listings are 1 / 0, 2 / 2 and 2 / 2 for the three cases; a boundary
trade may be listed in both adjacent windows, so those listings are not a
global unique total. The repeat's first-minute Breakout has **five accepted
actions but zero confirmed fills**. It passes the frozen admission contract,
while the stronger diagnostic claim that every strategy fills every minute is
false. These lower-bound fill mappings do not convert simulation time by a
nominal rate, infer fills from limit prices or replace the frozen gates.

## v15 500-bot 35-minute result: valid measurement, insufficient sustained activity

The explicit run used the two independently passing 500-bot pilots after a
complete prerequisite re-audit. The check receipt is
`.local/bot-throughput-20261013/v15-long-check-500-01/long-manifest.json`;
the final run and raw evidence are in `cycle-long-v15-500/` under the same
artifact root. The frozen v15 binary, plugin, source and driver inputs were
unchanged. No compiler or other benchmark server was sampled. The service
completed all **2100 requested measurement seconds** and shut down normally;
postflight and the full receipt re-audit passed. The outer wrapper returns
**exit 1** because capacity did not qualify; its child driver and server both
returned **exit 0**. `measurement_valid=true` and
`configured_contract_passed=false` are both intentional.

| Observed quantity | Full run result |
| --- | ---: |
| Accepted bar-close-to-durable actions | 5336 |
| Accepted P50 / P95 / maximum | 75.845 / 164.947 / 230.995 ms |
| Accepted actions above 250 ms | 0 |
| Overall clock steps per wall second | 10.0000150467 |
| Minute clock windows meeting >=9.5 steps/s | 35 / 35 |
| Cutoff backlog / paused backlog / skipped decisions | 0 / 0 / 0 |
| BOT / harness / trace errors | 0 / 0 / 0 |
| Peak server RSS | 2011.414 MiB |
| Server average logical cores | 0.992515 |
| Process-tree measured sampled RSS peak | 1999.391 MiB |
| Process-tree measured samples | 7739 |
| Maximum accepted-anchor gap | 42.002744 s |
| Minute accepted-coverage windows passing | 30 / 35 |

The server peak and process-tree sampled peak use different sampling scopes;
they are reported separately. The overall literal 10-step target is met, but
not every minute individually reaches literal 10; the existing per-minute
configured threshold is 9.5. All reported actual accepted latency samples meet
250 ms, including every failed-coverage window. This is not proof of full-load
latency in periods where too little trading work was generated.

The unchanged contract requires each minute to have at least 20 accepted
actions and at least one from each of MA, RSI and Breakout. Exact failures are:

| Measurement seconds | Accepted total | MA / RSI / Breakout | Accepted P95 (ms) | Failure |
| --- | ---: | --- | ---: | --- |
| 600-660 | 54 | 0 / 38 / 16 | 120.095 | MA absent |
| 1500-1560 | 53 | 0 / 50 / 3 | 147.128 | MA absent |
| 1860-1920 | 4 | 0 / 4 / 0 | 137.646 | Total below 20; MA and Breakout absent |
| 1920-1980 | 9 | 0 / 3 / 6 | 179.693 | Total below 20; MA absent |
| 1980-2040 | 29 | 0 / 25 / 4 | 157.561 | MA absent |

The failed `bot_durable_receipts` and `sustained_accepted_coverage` flags inherit
these coverage failures; they do **not** mean a truncated trace, missing
terminal receipt, or accepted-latency breach. Every generated decision has a
terminal outcome. The shutdown acknowledgement matches the final marker and
all **614,366,599 trace bytes**, with zero trace errors and no forced kill.
Raw trace SHA-256 is
`63ce18ae5f7b16b225640eaf6ec45e9937aa6a302f1c348bd11d53848c1c17c1`;
per-case report SHA-256 is
`5ffe0fa5bead0a5bad8c8799a1e871cc6058b9edacce722402d107528a524b56`.
No failed window is discarded or reclassified to obtain a pass.

This remains an **in-memory journal** result. The separate PostgreSQL tests
establish their tested recovery behavior, not PostgreSQL throughput under
this load. The memory series continues to rise, so staying below 4 GiB for
35 minutes does not establish a bounded indefinite working set. The
1000-bot pilot still fails latency/backlog and supplies no sustained-capacity
qualification.

Post-run analysis is retained as `offline-analysis-plan.json`,
`offline-analysis-receipt.json`, `latency-attribution-long-v15.json`,
`window-fills-long-v15.json` and `activity-analysis-long-v15.json` in the long
run directory. The latency and fill helpers pass. The timing helper's
long-versus-short comparison correctly **refuses recipe equality**: the long
feed account has `position_qty=23833`, while the short repeat has 10000, at
`/scenario/accounts/541/Spot/position_qty`. The helper's failure JSON/log are
retained unchanged. Its candidate-only raw counter cross-checks pass, but this
does not turn the rejected comparison into a controlled equal-recipe result.

The activity audit confirms all five missing-MA windows each contain **10,020
MA bar decisions**, all `no_order` with committed state and **zero generated
exchange actions**. Generated RSI/Breakout actions in these windows are all
accepted, with no rejection, pending or failed outcomes. Therefore the coverage
gap is not a missing scheduler evaluation or an exchange rejection. Final MA
cash is 9770-9973, with no pending/reserved amount; this does not support a
claim of exhausted cash. Per-window traces do not include enough internal
intent/book information to assign every suppressed action a unique cause;
final state must not be retroactively treated as each window's state.

`final-ma-close-state-check.json` checks all **100** final MA instances with a
long position, a last close intent and empty last actions. In every case,
account/available/observed position, owned `L` quantity, ledger position,
opening position plus net retained fills, and the last VM external-account
frame position agree. There are 62 one-unit and 38 two-unit positions, no
pending/replacement/reserved state, and cash is at least 9780. Each fill cursor
equals the maximum retained fill ID and the ledger's last account matches.
This finds no obvious final ownership/cursor mismatch. There is no matching
historical order-book snapshot, so it cannot prove that a missing bid caused
any particular failed window.

Candidate-only counters cover 706,036 turns: turn elapsed mean **2.063754 ms**,
paired CPU mean **1.788661 ms**, dispatch own-scope mean **21.210098 ms**,
stamp **0.027390 ms per turn**, source check **0.426138 ms per turn**, and
391 full validations averaging **25.405790 ms**. CPU probe error counters are
zero. These nested counters are not additive CPU shares. Accepted pre-decision
P95 is 71.122 ms and post-decision P95 is 95.234 ms; the independently measured
total P95 is 164.947 ms. For all 700,700 committed bar decisions, total P95 is
128.240 ms and maximum is 321.746 ms; the zero-over-250 statement applies to
accepted actions, not every no-order state publication.

The fixed driver's measured process-tree RSS grows from **320.688 to
1997.754 MiB**, with its reported slope **42.882071 MiB/min**. This documents
continuing growth without assigning it to a particular heap object. The
actual-fill audit conservatively maps **239 Pine fill receipts** inside the
measurement window; all three sources have confirmed fills in only **19/35**
minutes. All feed windows show actual rises and falls, but a moving trade price
does not establish standing two-sided liquidity for Pine IOC orders.

Remaining work is explicitly unqualified: a separately versioned active-load
recipe needs funded standing two-sided providers, full-horizon cash/inventory
budgets, and verified actual candle/intent/accepted/fill continuity. Narrow
no-action reason and decision-time book/account context are needed to distinguish
no signal, position/pending limits, zero quantity/cash and missing counterparty.
The current frozen recipe and its failed gates must remain unchanged. For 1000
bots, the measured synchronized dispatch burst and commit tail remain the
performance target; these five coverage failures do not justify another guessed
runtime optimization. No revised recipe, additional long run or production
merge is claimed here.

The isolated trial launcher also passed an actual post-measurement API smoke:
frozen executable and 20 plugin files matched; readiness returned `ok=true`,
the catalog contained one `pine.strategy`, and a fresh process went from zero
rooms to one explicit empty room. The owned process exited zero through the
shutdown-file path, without a forced kill. Receipts are in
`.local/bot-throughput-20261013/candidate-v15-trials/trial-v15-20261013T103958088Z-4aa93a2dd33442ce8495a7636ee05ae7/`.
The launcher/usage guide are `candidate-v15-trial.ps1` and
`CANDIDATE-V15-TRIAL.md` in the artifact root. This smoke used no BOT load,
market feed, UI or PostgreSQL and supplies no additional capacity claim.
