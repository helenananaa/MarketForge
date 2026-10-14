# Pine host performance experiments

Status: experiments and selected-code validation complete. The selected source
optimization has repeatable bounded gains; full long-run capacity is not qualified.
Baseline: `05ce347478f9b870996e74ac66c9afc7a980b27e`.
Worktree: `E:/CodexData/worktrees/pine-host-performance/MarketForge`.
Artifacts: `.local/pine-host-performance-20261013/` in this worktree.

## Experiment contract

Investigate every direction proposed in the external review, including alternatives
when the suggested shortcut would change a correctness or recovery guarantee.
No timing prediction is a measured result. Keep all attempts and restore rejected
changes. Current main-directory native-Pine and strategy-research work is excluded.

| Direction | Experiment | Status |
| --- | --- | --- |
| Source admission | Explicit opt-in snapshot shared within an immutable observation wave; public calls still check fresh | Four-run same-binary ABBA complete; candidate retained |
| Identity | Exact configuration/participant cache; global and per-bot locking both tried | Removed: no stable additional real-load gain |
| Incoming execution seal | Full authenticated envelope; cold/mismatch keeps SHA validation | Removed: no stable additional real-load gain |
| Request construction and sizing | Reuse input Value and exact immutable state bytes | Removed with unproven combination; exact limit tests passed |
| Trigger fingerprint | Full prefix proof plus all supplied candle/private/related inputs | Removed with unproven combination; mutation tests passed |
| Worker concurrency | 16 versus 32/core-count | Removed: higher CPU, no clear tail benefit |
| Pine task batches | Groups 4/8 with per-bot permits | Removed: repeated tail regression |
| Fixed wake offsets | 50/100 ms, delay included in total latency | Removed: exploratory behavior change, no repeatable adopted gain |
| Full scheduler checkpoints | Fragment preencoding and full covering-revision snapshot/recovery prototype | Removed fragment workers; naive late full record rejected |
| Checkpoint trigger policy | Byte/time alternatives retaining 499-delta bound | Removed: worse exploratory tails |
| Typed Pine state | Value adaptation and direct canonical typed encoding | Adaptation rejected; direct encoding remains a prototype |

## Validation

Freeze executable, driver, recipe and plugin hashes before timed runs. Never run
compilation alongside load measurements. First use fixed-work comparisons with exact
response/state equality, then 500/1000-bot cycle pilots, native controls and relevant
PostgreSQL recovery. Capacity qualification requires its unchanged activity,
latency, cutoff-backlog and duration gates; post-pause catch-up is not cutoff success.

The v15 raw artifact directory referenced by the previous report is absent at its
recorded path. Historical report numbers remain historical; this task produces
new baseline evidence rather than reconstructing or relabeling those runs.

## Source-wave experiment

The first controller inherited experiment flags through the environment, but the
lower-level isolated server launcher strips those variables. All four cases in
`wave-abba/` therefore ran with source sharing disabled. The grouping is invalid
and explicitly excluded; its receipts are retained. The corrected driver accepts
an allowlisted `--host-experiment NAME=VALUE` and saves the actual server environment.

Candidate v2 `wave-abba-v2/` runs control / wave / wave / control, 1000 Pine plus
38 native liquidity bots, 100ms cadence, 20s warmup, 120s measurement, 64 genuine
prewarm candles, diverse-short inputs and a cycle feed. All recipes and complete
prewarm histories are identical. Every case has valid sampling, no compiler or
other benchmark server, complete trace shutdown and no cutoff backlog or skipped
bars. Configured >=9.5 step/s gates pass; literal >=10 step/s passes only one case.

| Metric | Control median (2 runs) | Source-wave median (2 runs) |
| --- | ---: | ---: |
| Turn elapsed per complete turn (ms) | 1.741748 | 1.341511 |
| Average server CPU (logical cores) | 1.276923 | 1.122375 |
| Accepted bar-to-durable P95 (ms) | 164.772 | 151.785 |
| All bar-to-decision P95 (ms) | 122.732 | 120.552 |
| Dispatch queue own-scope mean (ms) | 37.782161 | 33.155727 |

Actual source reads are 80,622 / 81,467 in controls and 1,550 / 1,151 in enabled
cases. Turn elapsed falls about 23%, average CPU about 12%, accepted P95 about 8%.
The pre-decision P95 improves only about 2%; this is not the predicted fivefold
burst improvement. Accepted populations differ (582/1427 versus 1713/1016) under
asynchronous trading; report medians of case quantiles, not a pooled causal estimate.
Nested elapsed scopes are not additive or CPU shares. These are short memory-journal
results, not PostgreSQL throughput, long-run stability or universal Pine capacity.

## Frozen candidate v5 screen

The first `v5-option-screen` controller was interrupted after five completed
cases. The partial combined case has no completed report and is excluded.
`v5-option-screen-interruption.json` retains that disposition. The remaining
directions were run serially by a detached controller in `v5-rest-screen`;
all twelve cases completed. Each has a complete sampling window, quiet compiler,
no concurrent benchmark server, actual option receipts and graceful trace closeout.
Recipes and initial candle histories are identical within the series.

This is a screen, not a randomized causal experiment. Each option has one run,
with controls at both ends. Several cases fail activity/receipt/cutoff gates
despite valid measurements. The two controls themselves fail sustained accepted
receipt coverage, so this series does not establish 1000-bot capacity.

| Option | Turn elapsed ms | CPU cores | Accepted durable P95 ms | All decision P95 ms |
| --- | ---: | ---: | ---: | ---: |
| Control, median of two | 2.081 | 1.587 | 257.253 | 180.787 |
| Five host flags together | 1.592 | 1.380 | 247.091 | 186.333 |
| 32 workers | 3.837 | 1.907 | 240.292 | 197.727 |
| Core-count workers | 3.655 | 1.759 | 291.614 | 185.240 |
| Pine batch 4 | 0.998 | 0.945 | 232.790 | 231.149 |
| Pine batch 8 | 1.477 | 1.366 | 690.213 | 673.372 |
| Background JSON fragments | 2.126 | 1.633 | 236.459 | 192.576 |
| Fixed offsets 50 ms | 1.990 | 1.499 | 222.235 | 175.214 |
| Fixed offsets 100 ms | 1.771 | 1.455 | 236.392 | 151.526 |
| Checkpoint byte budget 32 MiB | 2.278 | 1.727 | 335.001 | 223.596 |
| Checkpoint time budget 10 s | 2.240 | 1.680 | 393.628 | 217.046 |

Five host flags and batch 4 warrant repeated comparisons. Batch 8 has a large
tail regression; raising concurrency also increases CPU without a clear latency
gain. Wake offsets include intentional delay in the end-to-end metric and change
market behavior. Their single-run result is not authorization to enable them by
default. The byte/time checkpoint policies preserve the maximum 499-delta tail,
so they cannot arbitrarily defer full checkpoints to claim lower cost.

`screen-summary.json` verifies report hashes and includes every capacity gate,
actual flag value, initial-history hash and measurement count. It also retains
the first five completed exploratory cases; those are not pooled with this screen.

## Typed-state experiment boundary

The fixed-work prototype loads all 1000 actual saved Pine states, clones them,
increments a counter and encodes exact durable bytes. Three ABBA rounds produce
median elapsed times of 96.507 ms for Value, 113.395 ms for typed state converted
back to Value, and 44.718 ms for direct canonical typed encoding. Every output is
byte-equal. Preparation/decoding is outside timing.

The current host's Value boundary makes the typed adaptation slower. Direct typed
encoding demonstrates a useful structural direction, but is not a typed Pine
broker implementation and excludes state admission, VM, gateway, queue and journal
work. Neither number is an end-to-end improvement. Raw evidence is retained in
`typed-state-direct-micro.json`; the Value adaptation is rejected.

## Correctness evidence so far

Candidate v4 release workspace/all-features tests with required authenticated
PostgreSQL: 768 passed, 24 ignored, no failures. All-feature release Clippy passed;
default-feature Clippy passed after gating two feature-specific helper functions.
Candidate v5: 30 focused Pine tests passed, 2 ignored with all five cache flags.
Two actual PostgreSQL HTTP restart tests passed with explicit candidate flags:
limit restart/cancel, and continuous versus restart schema-3 fill/close.
These establish those recovery paths, not PostgreSQL realtime load capacity.

The additional boundary/recovery audits, repeated comparisons, production cleanup,
native controls and sustained validation are complete, with limits recorded below.

## Additional exact-work and recovery audits

The borrowed request guard test passes at 4 MiB minus one byte, exactly 4 MiB,
and 4 MiB plus one byte, with Unicode and numeric metadata. Only the last case
receives the size-limit error, matching the original serializer.

A full 1038-agent scheduler snapshot cloned at revision 8 takes 1.003 ms; encoding
its 16,251,229 bytes on a background thread takes 53.254 ms. Thirty-two synthetic
opaque metadata deltas advance the foreground to revision 40 while the snapshot
remains isolated. Explicit replay from the covered revision equals the final
state. Appending that old snapshot as a later ordinary SchedulerProgress record
makes the current recovery code select revision 8 instead of 40. Therefore the
naive late-record proposal is rejected. Independent covering-revision anchors need
a storage/recovery protocol extension; fragment preencoding is not that extension.
These are correctness/prototype measurements, not concurrent trading throughput.

Fixed cache work over all 1000 saved Pine states uses the actual manifest validator
to insert the same configuration defaults as ProcessBot construction. The initial
attempt used raw persisted configuration and failed the identity equality check;
its failure is retained. The corrected three ABBA rounds produce median 139.148 ms
for original identity plus incoming/outgoing seals and 108.660 ms with caches,
including outgoing seal generation, full proof write and comparison. Identities
and seals equal the durable fixture in every successful case. This excludes the
VM, observations, cold restore and queues; it is not a full single-turn replay.

An additional cache review found signed-zero configuration equality insufficient:
ordinary Value equality can hide a change that canonical Python hashing preserves.
The candidate now compares float bits recursively, with a positive/negative-zero
identity regression test. The original screen workload is retained as bounded
evidence. Its repaired candidate was validated in repeated comparisons and fixed
turn tests, then removed because the extra runtime gain was not stable.

Defender realtime monitoring is enabled and this worktree's plugin path has no
covering path exclusion. This read-only check does not establish that antivirus
causes the filesystem timing; no system exclusions were changed.

## Candidate v6 repeated comparison

The signed-zero repair is included in a new frozen executable, SHA256
`41a113413945b7b8640c67c5817f375703d3b1df9ecdf4eeb16546d28168f745`.
Its four-case source-wave / combined / combined / source-wave block completed.
This comparison removes repeated file reads in both versions and asks whether
the additional identity/seal/request/fingerprint caches help.

| Metric, median of two cases | Source-wave only | Five flags together |
| --- | ---: | ---: |
| Turn elapsed ms | 1.685 | 1.669 |
| CPU cores | 1.503 | 1.510 |
| Accepted durable P95 ms | 317.411 | 312.616 |
| All decision P95 ms | 211.658 | 208.510 |

These small differences do not establish an additional runtime benefit. The
fixed-work cache result does not override this evidence. Tail latency also fails
the existing receipt qualification; this is not 1000-bot capacity evidence.
The second ABBA block comparing combined with combined-plus-batch-4 completed.
Both batch cases report accepted P95 517.858/342.705 ms,
decision P95 310.022/309.859 ms and CPU 1.298/1.287 cores. The latency regression
is a reason to reject this combination despite its lower CPU consumption.

This machine has 32 logical processors. Therefore the explicit 32-worker and
core-count screen cases select the same worker bound; their differing results
are run variation, not evidence about two distinct algorithms.

Expanded related/private-input cursor tests passed, including stream histories,
fills, fees, account events, conditions, related markets, configuration seed and
receipts. The complete borrowed-turn experiments below also passed.

## Complete fixed-turn comparison

The generated fixture contains 1000 unique identities and three qualified scripts,
all sharing one real fixture account, with synthetic 64-to-65-bar histories. It
occupies 4,918,823 bytes, SHA256
`9a130ad556d1cfabfd3f5a318bd2cacc05a78eba89b2f60fb0c8300ce7711ac5`.
This is a smaller, near-checkpoint state than the saved live scheduler. Host/program
preparation and cold initialization are outside timing; source admission sharing
is enabled in both variants. Every complete response is byte-equal.

| Mode | Original 1000 calls ms | Cached 1000 calls ms | Original/cached VM elapsed ms |
| --- | ---: | ---: | ---: |
| Serial, medians of four | 715.256 | 708.788 | 30.648 / 31.640 |
| 16 workers, medians of two | 614.226 | 564.267 | 88.054 / 83.395 |

The cache combination improves serial full calls by less than 1%, and this bounded
parallel fixture by about 8%. Parallel VM elapsed scopes overlap; subtracting them
from wall time would be invalid. Gateway, trigger, queue, journal and real market
feedback are excluded. This does not replace the real-load ABBA result.

Moving identity comparison/hashing outside the shared map lock also passed all
identity and byte-equality tests with the exact same fixture hash. Its serial
medians were 752.888/743.396 ms and parallel medians 624.601/578.246 ms, original versus
cached. The relative improvement did not increase. Global-lock contention was a
hypothesis, not a confirmed explanation. That additional implementation was removed.

## Selected code and final verification

Only source admission sharing remains in the production path. It is enabled by
default within the already opt-in in-process realtime route; set
`MARKETFORGE_PINE_SOURCE_WAVE=0` to compare or revert its admission granularity.
Public `decide`, `decide_value` and `decide_borrowed` calls still read the current
file independently. Realtime tasks share checks only for the exact immutable
wave, Host instance and script. Rejection is cached for that wave too. File edits
are observed by the next wave. The scope guard cannot move between threads;
unwinding restores the prior thread-local scope. Native batches skip this cache.

Identity/seal checks, original exact request/response bounds, raw trigger cursor,
16 external permits, one bot per external task, normal wake timing and the original
ordered checkpoint/journal policy are restored. The full prototypes remain in
the checksummed [archive patch](experiments/2026-10-14-pine-host-prototypes.patch),
which was verified against a temporary baseline Git index; the real index was
unchanged. See [reproduction notes](experiments/README.md).

Selected code workspace/all-feature release tests: 758 passed, 25 ignored, zero
failures with the dedicated authenticated PostgreSQL instance. After adding the
thread-bound guard, Pine tests: 26 passed, two ignored. All-target release Clippy
passes with and without the in-process feature. Final executable SHA256:
`1c732313fe2f052e5d88e1b9a2a6d1e5a192e22d17aaa90b5abcdf2b3fb6f3bf`.
Both actual PostgreSQL HTTP restart/cancel and continuous-versus-restart fill/close
tests pass against this executable, 43.367 seconds total, no skips.

The first workspace-test invocation had an incorrect credential-file path and was
interrupted before qualification. The first final build lost its executor handle
with no compiler process or completion record; it is excluded. A detached rebuild
produced the successful result above. No missing handle was treated as success.

Final frozen qualification runs native baseline/selected/selected/baseline, then
500 and 1000 Pine control/wave/wave/control pilots and a 35-minute selected run.
Controls explicitly set source sharing to zero. All existing latency, activity,
duration, cutoff-backlog and receipt gates remain unchanged. All seven stages
completed with frozen inputs unchanged. The
[JSON companion](2026-10-13-pine-host-performance.json) exports report hashes,
completed receipts, exclusions and explicit capacity failures. Its completion
marker means the validation stages finished, not that every gate passed.

Two initial final-qualification matrices were stopped by sampling validation:
the first monitor treated its own descendant server as an external concurrent
server; the second did not recognize the versioned selected executable name.
Both receipts are retained and excluded from comparisons. The corrected sampler
follows the driver ancestry, requires exactly one owned benchmark server, and
still rejects compilers and any benchmark server outside that tree. A regression
test covers original, candidate and selected executable names. The third matrix
is the authoritative final qualification.

Earlier prototype measurements used the older executable-name predicate. Their
concurrent-server detection covered fewer renamed binaries; controllers were
serial and explicit process checks found no concurrent runner, but that older
negative sampling gate is weaker than the corrected final gate. Do not broaden
its scope beyond the documented measurements.

Final native ABBA completed with identical recipe hashes and all four samples
valid: zero compilers, zero external benchmark servers and exactly one owned
server. Baseline step rates were 40.010/39.703 per second; selected rates
40.014/39.960. Every case passed the original cadence and order-latency gates.
Selected place P95 was 19.420/18.902 ms and cancel P95 17.426/18.084 ms, versus
baseline place 19.301/32.571 ms and cancel 16.675/26.978 ms. Peak server memory
was 750/813 MiB selected and 708/772 MiB baseline, all below the unchanged guard.
These short controls show no material native-path regression; variation here
is not a Pine speedup claim.

The final 500-Pine-plus-38-native ABBA completes with identical recipe/prewarm
hashes, valid measurement/trace shutdown, no cutoff backlog or skipped bars and
successful activity/receipt gates. Each case passes the configured >=9.5 step/s
cadence; every case fails literal >=10 step/s. That failure remains explicit.

| 500-bot metric, medians of two cases | Source sharing off | Source sharing on |
| --- | ---: | ---: |
| Turn elapsed ms | 1.844 | 1.394 |
| CPU cores | 0.740 | 0.654 |
| Accepted durable P95 ms | 123.783 | 124.113 |
| All decision P95 ms | 56.892 | 45.811 |

Turn time falls about 24%, CPU about 12% and decision P95 about 19%. The accepted
durable tail is effectively unchanged; this is not a uniform latency speedup.
Asynchronous accepted populations differ.

The final 1000-Pine-plus-38-native ABBA also completes with matching recipe and
prewarm hashes, valid measurement/closeout and successful cutoff, activity,
receipt and no-skipped-bar gates. All four cases pass configured >=9.5 step/s
and fail literal >=10 step/s, with neither threshold changed.

| 1000-bot metric, medians of two cases | Source sharing off | Source sharing on |
| --- | ---: | ---: |
| Turn elapsed ms | 1.764 | 1.395 |
| CPU cores | 1.320 | 1.143 |
| Accepted durable P95 ms | 164.239 | 154.533 |
| All decision P95 ms | 131.816 | 126.532 |

This final executable shows about 21% lower turn time, 13% lower CPU, 6% lower
accepted durable P95 and 4% lower decision P95. These are medians of run-level
quantiles, not pooled event percentiles. The host cost reduction is useful but
does not establish the review's predicted fivefold burst acceleration. The
35-minute result below bounds the long-duration conclusion.

A selected-code source audit also confirms baseline-identical request encoding/
size checks and the complete decision-parts method, checkpoint identity/seal/
response checks, broker logic, JSON compatibility, raw trigger, process plugin
and journal/writer files (after normalizing line endings). This is direct change
scope evidence, complementary to the actual runtime/recovery tests; it is not
a substitute for those tests. Hashes are exported in the JSON companion.

## Sustained 35-minute result and qualification limits

The selected 1000-Pine-plus-38-native run observed the full 2100-second measurement
with valid metric windows, quiet compiler, no concurrent benchmark server and
complete graceful shutdown. The completion marker contains zero trace errors;
908,655,759 trace bytes were closed without forced termination. Generated actions
and decision outcomes have complete receipts. No input/bot errors, skipped bars,
cutoff backlog or multi-second clock stalls were observed. All 35 one-minute
clock windows pass the configured 95% cadence gate, with overall 9.986 step/s.
Literal >=10 step/s still fails.

| Sustained measurement | Result |
| --- | ---: |
| Average server CPU cores | 1.632 |
| Accepted bar-to-durable P95, 24,867 anchored actions | 254.929 ms |
| Accepted bar-to-durable maximum | 463.963 ms |
| All bar-to-decision P95, 1,398,699 outcomes | 189.972 ms |
| Peak server memory | 3425 MiB |
| RSS least-squares growth | 75.859 MiB/min |
| One-minute accepted windows failing combined coverage/latency gate | 24 / 35 |
| Worst one-minute accepted durable P95 | 361.522 ms |

Every minute has at least 270 accepted actions, versus the required 20; each
required source is represented in every minute. Minimum accepted source counts
are MA 172, RSI 21 and breakout 13, all above the required one. Maximum accepted
anchor gap is 9.171 seconds, below the unchanged 60-second limit. The combined
`sustained_accepted_coverage` gate fails because 24 windows exceed 250 ms, not
because the population went idle. The whole-window accepted P95 also exceeds
250 ms by 4.929 ms, so `bot_durable_receipts` fails its combined qualification
despite structurally complete receipts. The ordinary HTTP order-probe gate passes;
it does not substitute for bar-to-durable latency.

This is a failed full latency qualification. Neither cadence nor delay thresholds
were raised, and post-pause catch-up was not substituted for cutoff progress.
Memory grows throughout the run; its slope includes retained in-memory durable
journal growth and cannot be attributed to Pine state or a cache leak alone.
There is no matched 35-minute source-off control, so this result does not establish
a causal long-run regression or improvement. It does not establish bounded memory,
PostgreSQL saturation/throughput, universal community-script capacity or a
production 1000-bot SLO.

## Delivery and scope

All review directions were attempted and assessed, including failed or partial
structural prototypes. The production selection keeps only per-wave source
admission; the larger typed broker and independent covering-revision checkpoint
protocol were not deployed. Their exact boundaries and negative evidence remain
available in the archive and JSON. This completes the requested optimization
experiment, not every possible future runtime optimization.

Code, report, JSON and benchmark/recovery tools are delivered on the isolated
`codex/pine-host-performance` branch. Existing staged
strategy research and uncommitted native-Pine work in the primary checkout were
excluded. No task edit/restore command targeted that primary source checkout.
The dedicated target directory and PostgreSQL cluster were used only for this
task. Large raw fixtures/traces remain local; clone and check out the delivery
branch to read the committed code, report/JSON and prototype archive. The report/JSON and prototype
patch preserve reviewable results without pretending that absent v14/v15 raw
fixtures were recovered.
