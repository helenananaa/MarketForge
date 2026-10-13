# Bot runtime and durable decision receipts

`GET /metrics` exposes the execution path actually selected by the host:

- `marketforge_bot_runtime_route_total{route,reason}` counts attempted calls to
  native, direct Rust Pine, or process adapters. `reason` records why a Pine
  request used the process adapter, including a disabled direct backend,
  unqualified source, unsupported inputs or state.
- `marketforge_bot_runtime_evaluation_total{route,mode,reason}` distinguishes
  incremental append, session rebuild, full replay, a call with no new bar to
  evaluate, and an unknown mode. Direct modes are recorded at the evaluation
  branch. Process modes are optional worker-reported diagnostics; missing
  metadata stays `unknown` and is never inferred from the source filename.

These counters are bounded, contain no account or script payload, and remain
available in normal builds. Selecting `--pine-in-process` does not prove a
request used that path. The existing build feature, environment switch and
script/configuration eligibility checks still apply.

In v9, each direct admission reads the complete current source and compares it
exactly, after newline normalization, with the qualified installed program.
A private borrowed token binds that verification to this host and script for
the same attempt; it avoids a second read inside execution. It is not a TTL,
mtime or startup-only cache. Public `decide`, `decide_borrowed` and `decide_value`
entry points each obtain fresh verification. A source edit after admission may
leave the current call free to finish using the installed program; the next
attempt re-reads and rejects the changed source. A direct execution error is
returned without process fallback, and schema 3 always refuses the process path.

Set `MARKETFORGE_BOT_TRACE` to a new file path to retain detailed receipts. The
file must not already exist. Trace recording adds overhead and is disabled by
default. `marketforge_bot_trace_errors_total` must remain zero for qualification.
Metrics reads flush the trace and publish a timestamp in the same monotonic
clock domain, allowing the benchmark to exclude warmup.

A decision has a unique diagnostic ID and an action count. Each action gets a
receipt only after its containing transaction was durably installed and
published. `accepted` and `rejected` come from actual exchange execution
results. An accepted action is not necessarily a fill: IOC orders, resting
orders and cancellations preserve their existing exchange semantics. Failed
transaction attempts are separate from the final outcome when a batch retries.
Stale/discarded decisions and failed decisions cannot count as accepted orders.
An empty action list has a separate durable strategy-state receipt.

For decisions that actually advance the strategy's closed-bar cursor, the
trace records the supplied candle's close timestamp. The latency audit joins
that timestamp to the first successful clock publication crossing the close.
It measures from that publication to durable action installation. Account
wakes that do not advance the bar and initial prewarmed bars without a recorded
clock anchor are reported separately. No wall-clock latency is invented for
those events. Simulation milliseconds are never treated as wall milliseconds.

Run the existing load recipe with `--bot-trace` to report these receipts, or
with `--require-bot-receipts --max-bar-close-to-admission-ms 250` to require
complete outcomes, an exercised accepted-order sample and its P95 threshold.
This does not replace the clock, independent HTTP-order P95, input freshness,
backlog, error, compiler and concurrent-load checks. Rejected-action latency is
reported separately. An all-rejected or no-order workload cannot pass the
accepted-action latency gate vacuously.

The report's `runtime_paths` includes observed counts, fallback reasons and the
requested path. `bot_durable_receipts` reports generated action dispositions,
accepted/rejected latency distributions, no-order state latency, missing
receipts and trace errors. The measurement cohort uses bar-close anchors after
the scheduled warmup-end metrics sample through the scheduled measurement-end
sample. Terminal outcomes for that cohort may arrive during drain, but their
original bar-close latency is retained. A separate cutoff progress audit uses
only decisions durably published by the measurement cutoff, so pausing and
draining cannot erase an observed backlog. Whole-run outcomes remain separately
visible. A standalone audit is:

```powershell
python scripts/bot_runtime_receipts.py path/to/bot-receipts.jsonl --output audit.json --max-accepted-p95-ms 250
```

The standalone command returns nonzero for failed qualification. The tool
validates receipt relationships; it does not independently certify that an
arbitrary input file came from a trusted server binary. Preserve the binary,
recipe, trace and process-tree evidence together for performance comparisons.

## Optional orderly trace closeout

For a compatible candidate, add these options to
`scripts/benchmark_strategy_runtime.py`:

```text
--require-bot-receipts --trace-graceful-shutdown --trace-shutdown-timeout 15
```

The graceful option requires `--require-bot-receipts`. Its cleanup deadline
defaults to 15 seconds and accepts 1–60 seconds. The driver creates fresh paths
and supplies `MARKETFORGE_BOT_TRACE`, `MARKETFORGE_BOT_TRACE_SHUTDOWN=1`, and an
absolute `MARKETFORGE_SHUTDOWN_FILE`. After capturing the paused final state,
it creates the shutdown-request file and waits for process exit. Older frozen
baselines keep their previous cleanup behavior unless explicitly opted in;
do not require an unsupported baseline to produce the new acknowledgement.

A paused room is not a completed background-work barrier. In the optional
closeout path, stopped or closed-market workers cease dispatch and await their
writer tasks and started blocking computations. Work already admitted to a
durable transaction retains its actual outcome. Unsubmitted computed decisions
receive `decision_discarded` with reason `worker_shutdown`. The exact global
shutdown refusal before durable admission also produces a discard, without a
journal mutation. Replaced workers remain covered even after their join handle
leaves the room directory. Contained errors and caught panics prevent a
successful seal, including errors raised before a diagnostic decision ID exists.

After HTTP requests, workers and detached durable writes have drained, the
server appends a final `trace_complete` record, flushes and synchronizes the
trace, then creates and synchronizes `<trace-path>.complete.json`. This sidecar
contains version 1, the trace path, exact byte length and final marker. The
marker includes the process ID, decision sequence and trace-error count.
The driver requires exit code zero, matching identity and byte count, zero
trace errors, and that exact marker plus newline at physical EOF. The full
receipt audit still checks every row; an acknowledgement cannot excuse a
malformed interior/final row, missing outcome, duplicate or conflicting receipt.

`shutdown.json` and the report's `shutdown` field retain cleanup mode, deadline,
elapsed time, exit code, forced-kill status and acknowledgement or error.
Opted-in reports also include `qualified.trace_shutdown_complete`. Timeout
causes a forced kill and invalidates the run. Missing/malformed acknowledgements,
nonzero exits and trace/ack IO failures invalidate it as well. Cleanup does not
extend the measurement window or erase cutoff backlog; terminal receipt
latencies retain their original bar-close anchors. Refreeze the driver and
explicit options before comparing new runs with a different cleanup contract.

This is an orderly-closeout diagnostic, not crash recovery or proof against
power loss. The server does not impose an internal closeout timeout; an open
HTTP request or a blocking computation can outlive the driver's deadline.
Metrics-triggered flushes alone do not prove that all producers have stopped.
The optional path does not change the existing per-event trace writer or flush
frequency. Forced termination remains an invalid trace outcome.

Local validation on 2026-10-13: the `trace_shutdown_` unit filter passed five
tests ([log](../.local/bot-throughput-20261013/trace-shutdown-unit-v11.log)), and
five isolated Rust test processes passed with fresh trace paths
([logs and original traces](../.local/bot-throughput-20261013/trace-races-v11/)).
The isolated cases cover shutdown before durable admission, shutdown during a
gated real append with exact recovery, real worker append failure, a closed
market with a started native job, and a caught native panic after an epoch/stop
change. The ordinary unit tests additionally cover detached-worker registration,
contained errors and buffered-tail/ack integrity. The subsequent
[whole-process HTTP validation](../.local/bot-throughput-20261013/http-closeout-v11/summary.json)
passed all four cases: success, open-request timeout, acknowledgement-output
failure and trace-open failure. The three negative cases correctly remain
invalid benchmark outcomes. These checks validate the closeout/failure protocol;
they establish no throughput or long-duration capacity qualification.

## Pine phase counters for developers

V8 introduced 15 stages under the existing `bench-execution-timing` feature. Each is
exported as `marketforge_bench_<stage>_count_total`, `_nanos_total`, `_max_nanos`
and `_p95_upper_nanos`. Instrumentation is absent without that feature. These
timers measure elapsed wall time, including descheduling or file I/O, **not CPU
time**; concurrent accumulated durations can exceed the measurement duration.
Timers also record calls that return early with errors.

| Stage | Timed scope and sample multiplicity |
| --- | --- |
| `bot_pine_input_value` | Borrowed adapter input `json!` construction, excluding committed state; once per direct borrowed call, outside `bot_inprocess_call`. |
| `bot_pine_source_check` | Installed-program lookup, full current file read, newline normalization and equality; normally once per successful direct borrowed attempt in v9, outside `bot_inprocess_call`. Historical v8 made two checks. Other callers also count. |
| `bot_pine_request_length` | Exact serde request-byte count inside the existing deadline; once per call reaching request-size validation. |
| `bot_pine_working_clone` | Schema-3 committed-state deep clone or cold empty object; once per call reaching that point. |
| `bot_pine_audit` | Entire stream audit: proof/candle decoding, hashing, cursor checks and saved audit/last candle; once per supplied stream, not per candle. |
| `bot_pine_seal` | Canonical borrowed-seal serialization and SHA-256; normally warm verification plus export (two), or cold export (one). |
| `bot_pine_broker_feedback` | Broker feedback update; once per schema-3 call reaching feedback. |
| `bot_pine_broker_frame` | Account-frame construction per newly evaluated bar; excludes provider-pass construction. |
| `bot_pine_broker_translate` | Intent translation and action append; once per turn with new bars. |
| `bot_pine_broker_replacements` | Replacement generation and action append; once per schema-3 call reaching replacement processing. |
| `bot_pine_vm_execute` | Only `HistoricalRuntime::append_bars`; excludes pass decoding, feed extension, output extraction/serialization and feedback compaction. Restore can produce separate tail-replay and live-append samples. |
| `bot_pine_vm_checkpoint` | VM checkpoint export at the existing 32-pass tail boundary; excludes installing the new Value and dropping the old one. |
| `bot_pine_stamp_clone` | V15 retains this metric name for clearing the reusable Vec and encoding all three checkpoint/tail/intents fields into the complete private byte cache; once per call with new bars. Includes buffer growth when needed; excludes the CompareWriter warm-match comparison. |
| `bot_pine_response_length` | Python-compatible response-size bound, including the actions clone; excludes response metadata construction. |
| `pine_delta_hint_prepare` | Entire hint preparation, including shape/prefix checks, predecessor digest, payload/checksum construction and unsuccessful full-state-fallback attempts. All callers count, including non-Pine early returns. |

These new scopes do not nest one another. Most schema-3 scopes are children of
`bot_inprocess_call`; broker frame/translation, VM execution/checkpoint and
stamp construction also sit inside `bot_inprocess_append` or
`bot_inprocess_rebuild`. Hint preparation occurs separately from the Pine call
and can also run outside a computation task. V9's admission source check sits
inside the existing `bot_request_clone` timer: on the direct path that name now
measures eligibility and source verification, not a request deep clone. The
process path still includes request cloning there. Source verification and
input construction precede `bot_inprocess_call`. Historical v8 also performed
a second source check before that parent timer; its measured two-check counts
remain historical evidence. Separate calls to public eligibility or decision
APIs perform their own checks, so aggregate source-check counts need not equal
the adapter's turn count. No v9 latency gain is implied by the expected count.

Use the same scheduled start/end metric snapshots for every delta. Choose and
report a common turn count, for example
`N = delta(marketforge_bench_bot_turn_count_total)` for a controlled Pine
external-turn workload. For each stage, report:

```text
wall milliseconds per common turn = delta(stage_nanos_total) / 1_000_000 / N
samples per common turn           = delta(stage_count_total) / N
```

Require positive `N`, present endpoints and no counter resets. Check sample
multiplicity for restore, catch-up, multiple streams, no-new-bar calls and
fallback attempts. Counters are server-wide; extra callers or other languages
must be disclosed when normalizing them to a Pine turn count. The stage's own
`delta(nanos) / delta(count)` is time per stage invocation, not time per turn.
Do not add raw stage means with different denominators, or parent timings to
their children. These stages do not form an exhaustive partition of a turn.

`max_nanos` and `p95_upper_nanos` describe cumulative process-lifetime samples;
subtracting endpoint gauges cannot produce a measurement-window maximum or
P95. Use receipt cohorts for measured end-to-end latency. These diagnostics
explain where time was observed; adding counters or reducing one stage alone
does not establish a performance improvement or qualification.

## V13 lock, screening and thread CPU diagnostics

These additions require `bench-execution-timing` (which enables the core
`bench-observation-timing` feature). Without the corresponding features, the
diagnostic guards, timestamps and counters are compiled out. They do not change
the workload or acceptance thresholds. The frozen v13 pilot exercised these
counters successfully: scheduled raw deltas matched the report, all five
history wait/held sample counts reconciled, and 80,625 turn pairs recorded no
read failures, nonmonotonic samples or overflow. The pilot still failed its
latency/backlog limits. This validates collection and interpretation for that
run, not diagnostic overhead: no same-source instrumentation-on/off experiment
was performed. See the [v13 validation results](validation/2026-10-13-bot-state-throughput.md#v13-diagnostic-pilot-valid-measurement-no-capacity-pass).

History-cell lock scopes export
`marketforge_bench_history_<site>_<phase>_samples_total` and `_nanos_total`.
The five sites are `prefilter`, `checkpoint_window`, `market_data`,
`account_events` and `wave_clone`; phases are `wait` and `held`. Wait covers
lock acquisition, while held covers the acquired critical section up to the
unlock boundary. Both samples are recorded after unlocking and count completed
guards, including unwinding after successful acquisition. These scopes exclude
the separate history-directory, public-view and event-stamp locks. Use
`samples_total`, not the deprecated diagnostic `count_total` spelling: the new
name avoids accidental interpretation by the legacy stage decoder.

`marketforge_bench_bot_screen_prefifo_*` retains the ordinary stage suffixes
(`count_total`, `nanos_total`, `max_nanos`, `p95_upper_nanos`). Its sample begins
when a screening batch is submitted and ends when a successful, non-skipped
candidate enters the external-worker FIFO. It includes blocking-task scheduling,
the batch's screening work and the parent's delay handling completed results;
it is not pure queue wait. Candidates in one batch share the starting timestamp,
and skipped, failed or stale candidates do not produce this sample.

Windows builds additionally export these cumulative counters under
`marketforge_bench_bot_turn_cpu_`:

- `paired_samples_total`, `paired_wall_nanos_total`, `paired_cycles_total`,
  `paired_user_100ns_total` and `paired_kernel_100ns_total` for successful pairs.
- `read_failures_total`, `nonmonotonic_samples_total` and
  `overflow_samples_total` for rejected pairs.

The CPU guard surrounds the synchronous `BOT_TURN` scope on its blocking OS
thread and is `!Send`; it must not cross an async suspension or move threads.
Both API readings must succeed, all CPU deltas must be nonnegative, and the wall
duration must be representable before a pair contributes to the totals. A
failed reading never becomes a successful zero sample; a real zero delta is
valid. The existing wall timer also includes probe overhead. FILETIME's 100ns
unit does not imply 100ns measurement precision. Do not convert cycles to time
or frequency, or treat wall minus CPU as exact waiting time: the API and wall
sampling boundaries differ.

These snapshots read independent atomics and are not transactional. Use common
measurement endpoints and disclose sample counts, failures and resets; a single
scrape can observe a partially recorded sample. Lock/screen/CPU scopes overlap
existing parent timers, so do not sum them or other nested scopes as disjoint
turn costs. None of these diagnostics replaces the durable-receipt latency,
backlog, recovery or long-duration acceptance gates.

## Decision-to-durable receipt scope

The `decision` event is emitted after bot execution, snapshot/action validation
and prepared Pine state-hint construction. Its interval to the durable event
therefore includes result collection, ready-queue waiting, the single ordered
writer's clock/decision scheduling, room coordination and commit-lock admission,
market/state preparation, journal work, installation and immutable publication.
It is not a pure journal or PostgreSQL write measurement. Ready waves contain
at most 64 decisions; they do not wait to fill 64, and the 25 ms stop poll is not
a deliberate batching delay. A wave containing any action uses the market
transaction path; state-only work still takes the ordered commit mutex.

The durable timestamp is captured at the published-snapshot swap and emitted
after unlocking. Later trace-buffer writes do not shift that endpoint. Full
scheduler progress can be required by an actor snapshot as well as periodic
scheduler cadence. `journal_full_validate` includes its scheduler JSON encoder,
and the room commit guard remains held through the append/install operation.

Current receipts do not identify ready enqueue, writer start, lock acquisition
or append boundaries per decision/batch. Aggregate room/journal means cannot
split a particular receipt tail among those stages, and independent phase or
receipt quantiles cannot be added. Existing v14 System data contains publication
groups whose latest completed member still preceded publication by over 200 ms;
that establishes post-compute delay, not which lock or checkpoint caused it.
The measured comparison and limits are in the [v14 validation section](validation/2026-10-13-bot-state-throughput.md#v14-allocator-abba-valid-comparison-no-1000-bot-qualification).
