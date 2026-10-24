# Ecosystem Pine direct execution — 2026-10-23

The first architecture-level migration is implemented in `feat/endogenous-market-tuning`.
It retains the original Pine programs and exchange submission chain. The new exact-hash
cohort contains **201 of 241 scripts, representing 2497 of 3000 Pine instances** in the
unchanged 3300-bot recipe. The remaining **40 scripts / 503 instances** use Python.
Direct execution is opt-in and bounded to 128 visible/committed bars for this new cohort.
This is a qualified short-history migration, not removal of Python from every workload.

Machine-readable measurements, source and executable hashes, admission inventory,
qualification results and recovery digests are in [the evidence JSON](2026-10-23-pine-direct.json).
Frozen original recipes, recovery recipe, benchmark plans and the starting dirty patch are
in [the inputs folder](2026-10-23-pine-direct-inputs/). Full local logs, binaries, traces
and checkpoints remain under `.local/pine-direct-20261023/`.

## Final performance

| Original recipe | Runs per route | Python wall | Candidate wall | Wall change | Peak RSS, Python → candidate |
| --- | ---: | ---: | ---: | ---: | ---: |
| Scale: 3300 bots, 1800 steps | 2 | 53.38 s | 36.12 s | -32.3% | 3271 → 2651 MiB |
| Long: 1500 steps × 3 s | 2 | 51.01 s | 53.40 s | +4.7% | 2014 → 2005 MiB |
| Related-market: 600 steps | 1 | 6.72 s | 5.58 s | -17.0% | 787 → 740 MiB |
| Polling: 600 steps | 1 | 6.71 s | 6.36 s | -5.2% | 815 → 818 MiB |
| No-Pine control: 600 steps | 3 | 4.86 s | 5.20 s | +7.0% | 347 → 360 MiB |

Values are means per route; scale and long use both execution orders. The scale reduction is robust across these pairs. Long-history mean regressed, and enabling the route also added overhead in the no-Pine control. Keep the feature opt-in: this is not a universal speedup. Related and polling each have only one pair.

The baseline uses the retained round-2 Python adapter; the candidate enables the direct
route and the verified-state handoff fix. Both use the same final Rust binary, original
recipes, seeds, starting balances, clock schedules, frozen decision waves and order
submission order. The benchmark includes process startup and teardown. Timed runs have
the per-command research audit disabled and run sequentially, without concurrent builds
or qualification tests. All 18 final/reverse-order runs completed and matched their
original complete output SHA256 exactly. Resource samples reported no compiler activity.
The final scale route counters confirm 2497 cold direct sessions, 50269 direct append
evaluations and 56045 direct calls; 18324 calls remain on the unqualified-source Python
route. The scale mean plugin stage falls from 25.55 to 11.50 s. This is measured routing,
not an estimate based on the feature switch alone.

This started from the already dirty round-2 worktree at
`b494d636fbe45ee3efd0480eec0a560857b2f4a1`, not a clean HEAD baseline. The inherited
incremental-runtime, caller-pool and position-protection changes are present on both
sides and are not attributed to this round. RSS is sampled summed working set; shared
pages count once per process. These finite samples include normal shared-host variation.

## What changed and what each experiment established

| Change / experiment | Before → after | Decision |
| --- | --- | --- |
| Initial unrestricted direct cohort (202 sources) | Scale 59.58 → 53.91 s; long history 55.49 → 134.59 s | Reject unrestricted routing; long history regressed severely. |
| Release already executed VM account/provider passes | Long-history direct peak RSS 2979 → 1747 MiB; wall 134.59 → 135.26 s | Retain the memory reduction; no speed claim. Durable transcript remains complete. |
| Session cache 2048 → 4096 | Scale cold rebuilds 15054 → 2509, append evaluations 37984 → 50529; paired means 43.56 vs 43.62 s | Retain bounded capacity for reuse; no independently established wall-time gain. |
| Append-only cached Rust transcript experiment | Long run remained slow (147.58 s); shorter prefixes did not justify the added path | Reverted, not part of the delivered implementation. |
| Bound the ecosystem route and preserve Python output-state sharing after handoff | Initial bounded handoff long-case mean 60.21 s; verified-fold candidate mean 57.22 s on a later matrix | Retain exact-value sharing; this cross-matrix change alone is not a controlled speedup claim. Isolated Python long means were 52.90 vs 52.77 s. Final combined long results are above. |
| Compile ecosystem programs lazily, once per source and share across instances | New-process Host-load probe, warm runs about 0.338 → 0.042 s | Retain; this is a startup microbenchmark, not end-to-end capacity. Original six sources retain eager validation. |

Those intermediate experiments used 202 sources. Real recovery later excluded one
unsafe incremental source; the final performance table uses the corrected 201-source
manifest. The earlier two-pair scale matrix improved from 56.46 to 42.92 s (24.0%), but
its timings are supporting evidence, not substituted for final-version measurements.
A comment-only Python edit between two intermediate runs is recorded in the JSON;
the final matrices use immutable adapter files throughout.

The diagnostic long run measured about 7.27 s in VM execution versus 964.88 s in total
direct-call timers. These are **nested cumulative wall timers across concurrent callers**,
not CPU time or additive percentages. JSON input construction (82.30 s), request-length
validation (95.92 s), working-state copies (129.84 s), cached transcript copies (241.40 s)
and response-length validation (86.19 s) dominate that measurement. Removing Python alone
does not remove full-transcript state costs.

## Admission and trading behavior

The Rust broker now matches Python decimal `Fraction(str(value))` conversion, buy-limit
floor / sell-limit ceiling, lot flooring, partial and percentage closes, `close_all`,
`cancel_all`, duplicate pending-close handling and replacement/reservation ordering.
Forty-five shared Python/Rust vectors cover successful results, exact states and rejection
boundaries, including replacement/Amend behavior. The actual gateway recovery run below
did not exercise Amend; vector coverage is not presented as live execution coverage.

New cohort admission requires the exact installed source hash, default input overrides,
explicit `quantize_strategy_orders=true`, broker mode, and no automatic hedge or exit
protection. Thirty-two exit scripts, three `strategy.order` scripts, four conditional-entry
scripts, and one history-dependent script remain on Python. The qualifier also requires
the source to have independent Python incremental qualification. Existing six-source
configuration behavior remains covered by regression tests.

The removed script is `ecosystem/library/9a14a0e2008b2e17.pine`. It depends on
`barstate.islast`; real-market history growth changed previously emitted intents. Its
short synthetic tests passed, but the recovery run failed with `previous Pine intents
changed during reconstruction`. Python's incremental manifest had not approved it either.
Its admission was revoked rather than weakening the historical-intent check. An earlier
all-241-source one-second recovery probe also failed in a separate, never-admitted
`strategy.order` source on unsupported numeric quantity; both failed receipts are retained.

The cohort uses **schema 2 with the complete transcript**. Enabling
`MARKETFORGE_PINE_VM_CHECKPOINT=1` does not migrate it to schema 3. The installed Python
runtime is `0.3.1+mf1`; its checkpoint behavior is not assumed compatible with the vendor
Rust runtime. Existing six-source schema-3 support remains separate.

At more than 128 bars in primary/related/provider observations or committed state,
pre-execution routing hands the same schema-2 state to Python and evicts speculative VM
cache state. No bars are truncated, accounts reset or scripts replaced. Execution errors
never trigger silent fallback. `MARKETFORGE_PINE_ECOSYSTEM_MAX_BARS` accepts 1–8192 for
controlled research; raising it is not a qualified performance setting. Session cache
capacity defaults to 4096 and accepts 1–8192 via `MARKETFORGE_PINE_SESSION_CAPACITY`.

The worker's verified-state fold retains the exact output state only after type-strict
delta equivalence, complete-response size and envelope size checks. Future input changes
remain copy-on-write. Generic mutable decoder consumers keep the original patch fold.
This preserves parsed-history object sharing after cold recovery or a Rust handoff,
without changing wire patches, durable JSON values or size limits.

All decisions still enter the existing deterministic `OrderGateway → risk → matching →
authoritative receipts/persistence` chain. This change does not bypass risk, transaction
ordering, account isolation or order quotas, and does not replace the ecosystem with a
few handwritten native strategies.

## Equivalence and recovery evidence

- **201 scripts × 22 comparisons = 4422 passed** against the installed Python broker:
  complete responses and schema-2 states, two price histories through 512 bars, flat and
  held positions, rejected order feedback, duplicate uncommitted calls and cold reconstruction.
  118 sources emitted actions on these synthetic paths; 83 did not. This is finite-path
  qualification, not proof of every branch or every input parameter.
- A separate **501-bot / 300-step** real gateway recipe uses one instance of each of the
  201 sources plus the original 300 native agents, with one-second bars. It preserves
  native liquidity, seeds and funded accounts. It is separate from the unchanged scale recipe.
- Continuous Python, direct/hybrid, Python→Rust→Python, and Rust→Python→Rust runs all
  produced the same complete final receipt hash
  `ba709e7c3e7fc2a472eae8ec957d6d446440a745d74f7e074db3bb807a3b66cc`.
  Restart boundaries at steps 100 and 200 use fresh OS processes and actual saved checkpoints.
  The last Rust-enabled segment exceeds the history bound and correctly routes to Python.
- All four **entire per-command audit files** are byte-equal, SHA256
  `c15ad0f5eb8bd0904309ae4d1ebf81e38aee06edd8625d5abdfbc6fdf285331a`:
  52562 gateway actions, 52815 authoritative execution records, **13184 trades**, 18142
  cancel actions and 119 gateway receipts containing rejection. All typed execution
  fields and clock-triggered authoritative suffixes are retained. This includes 2282
  Pine protected-place actions and six Pine cancels. Audit records are research Debug
  evidence compared with one binary, not a public persistence format.

The 300-step recovery and the 4422 comparisons used the frozen `approved` binaries. Final release
binaries differ only in source normalization removing one leading BOM, matching Python
UTF-8-sig, instead of all leading BOMs. All 201 admitted source files have zero leading
BOMs. The new normalization test and the full final Rust/Python suites passed; executable
hashes for both stages are retained. No recovery result is represented as a PostgreSQL
restart, crash-consistency or production durability qualification.

An additional **120-step recovery matrix on the final release binary** keeps all segments
within the direct bound, restarting at steps 40 and 80. All four flows again match their
complete receipt and entire audit hashes, covering 5097 trades. Route counters explicitly
prove 8026 direct calls in the middle Python→Rust→Python segment, and 8040 direct calls
after the final Rust→Python→Rust restart. Thus the reverse-resume proof does not rely on
an enabled switch after the history bound has already routed execution back to Python.
Final receipt SHA256 is `09952f0cae95d34edb955623c8a3b271d6f4006dc6774b31db07f2e09d5a2b2d`;
audit SHA256 is `a9e3d5a7dec1bbf96a4f090ef687773d6617d7c365d43debb4311323e6aa5ed0`.

## HTTP latency, resources and backlog

The HTTP probe runs **201 Pine + 300 native bots**, one-second bars, 250 ms scheduler
interval, 5 s warmup and 20 s measurement. It retains the real source/configuration
diversity and uses a separate empty instrument for HTTP probe orders. It uses the final
server and an in-memory durable journal. Two sequential pilot runs used legacy termination;
the reverse-order final pair enabled the existing graceful trace-shutdown protocol.
All measurements and unsuccessful qualification fields are retained.

| Final graceful pair | Python | Direct |
| --- | ---: | ---: |
| Process-tree peak RSS | 1091.60 MiB | 614.23 MiB |
| Average logical cores in measurement window | 4.66 | 3.17 |
| Accepted Pine bar-close → durable P95 | 136.59 ms (412 actions) | 174.35 ms (723 actions) |
| Independent HTTP place P95 | 35.51 ms | 24.56 ms |
| Required runtime route exercised, no direct fallback | Yes | Yes |
| Complete generated-action and decision terminal receipts | Yes | Yes |
| Graceful trace completion / exit zero | Yes | Yes |
| Instances behind at scheduled cutoff | 201, one bar | 0 |
| Instances behind after pause snapshot | 120, one bar | 170, one bar |

Direct execution reduces resource use here, but **does not demonstrate lower strategy
submission latency**. The accepted cohorts differ because these are wall-clock runs;
their trades are not claimed byte-identical. The deterministic gateway runs provide that
separate equivalence evidence. Both paths had all 201 Pine instances initialized and no
bot errors. The direct graceful run passed the receipt audit, while the Python run had
three discarded actions and 63 discarded no-order decisions in its measured bar cohort.

Both pilot runs had 201 instances one bar behind at cutoff; the pilot direct trace also
had 52 pending no-order outcomes when the old termination method stopped it. Graceful
shutdown completed all outcomes in the final pair. A zero-backlog cutoff in one direct
sample does not prove a permanent zero-backlog service: both final after-pause snapshots
still had one-bar backlog, and arbitrary strategy lookbacks / all-source sustained accepted
order coverage are deliberately left unqualified. This is not a production capacity claim.

## Validation and reproduction

- Final Rust server library: **377 passed, 12 ignored**, `--test-threads=4`.
- Final Python adapter/host/receipt suites: **85 passed** using the frozen release probe.
- Server feature-on Clippy and core research-example Clippy passed with warnings denied;
  feature-off server check passed. Those checks preceded the last normalization-only fix;
  full feature-on release build and full suites followed it.
- An earlier unrestricted-parallel Rust test invocation had five child-process deadline
  failures. The bounded four-thread full suites passed without changing timeouts.
- Source-hash revocation, configuration exclusions, legacy false-quantization identity,
  schema-3 exclusion, 128/129 routing boundary, exact-size/patch reconstruction and old-input
  immutability are covered. `git diff --check` passed.

With the installed runtime and plugin sources, the qualification/recovery entry points are:

```powershell
$env:PYTHONPATH = 'bot-plugins/pine-strategy/.runtime/strategy-ecosystem;python/tests'
E:/projects/MarketForge/.venv/Scripts/python.exe scripts/qualify_pine_direct.py `
  --probe .local/pine-direct-20261023/release-probe.exe --bars 512 --output <new-output>
E:/projects/MarketForge/.venv/Scripts/python.exe scripts/validate_pine_direct_recovery.py `
  --exe .local/pine-direct-20261023/release.exe `
  --cwd .local/pine-direct-20261023/python-canonical `
  --recipe .local/pine-direct-20261023/recovery-approved-recipe.json --output <new-output>
E:/projects/MarketForge/.venv/Scripts/python.exe scripts/benchmark_research_optimizations.py `
  .local/pine-direct-20261023/release-plan.json <new-output>
```

Add `--segment-steps 40 --require-direct-segments` to the recovery command for the
120-step matrix that asserts actual direct execution in every enabled segment.

The plans contain the exact run order, environments, expected receipt hashes and paths
to frozen Python installations. To relocate them, decompress the durable inputs and
replace only workspace/executable paths, preserving recipes, adapter/runtime versions,
seeds and switches. Build with `cargo build --release -p exchange-server --features
pine-in-process --example strategy_ecosystem_market --example pine_inprocess_probe --bin
exchange-server`; keep compilation and test processes out of timed runs. The HTTP wrapper
is `scripts/benchmark_pine_ecosystem.py`; final pair flags are `--population pine --counts
201 --seconds 20 --warmup 5 --interval-ms 250 --bot-trace --require-bot-receipts
--trace-graceful-shutdown --event-prefilter`, adding `--pine-in-process` only for direct.

The next architecture priority is a typed/shared immutable history and compatible bounded
checkpoint protocol that removes full JSON transcript work while preserving reconstruction
and receipt checks. New exit/conditional-order/hedge capability groups need their own
qualification before admitting the remaining scripts. Fixed internal strategies can stay
native Rust; this round does not rewrite ecosystem strategies into native templates.

Changes are local and uncommitted. No deployment, live-room migration or push was performed.
