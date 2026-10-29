# Branch inventory — 2026-10-29

This inventory records the local development work preserved for GitHub. These
branches are research checkpoints; publication does not mean they have passed
the full integration suite or are approved for merging into `main`.

| Branch | Purpose | Starting point before this cleanup |
| --- | --- | --- |
| `main` | Published integration baseline; unchanged by this cleanup | `ac8b5df` |
| `codex/strategy-ecosystem-snapshot` | Exact copy of the former main working-directory changes, reusing the existing commit | `9d9cb4c` |
| `feat/endogenous-market` | Endogenous market development, economic-market research and regression/recovery evidence | `e306311` plus the saved working-directory changes |
| `feat/endogenous-market-tuning` | Pine host optimization, market mechanism research, memory/output improvements and research evidence | `3167ded` plus the saved working-directory changes |
| `codex/archive-alloc-profiling` | Allocation/actor experiments and Windows sampling profiler from `mf-alloc` | `093d907` plus the saved working-directory changes |
| `codex/archive-nanos-profiling` | Fine-grained order-path timing and allocation instrumentation from `mf-nanos` | `0a723de` plus the saved working-directory changes |
| `codex/archive-staged-profiling` | Windows sampling profiler for the staged implementation from `mf-staged` | `093d907` plus the saved working-directory changes |

Before the cleanup, the tuning branch contained all 12 commits on the endogenous
branch and another 30 commits. Each branch's previously uncommitted work is now
preserved independently. The new endogenous checkpoint is therefore not
automatically included in the new tuning checkpoint; compare those changes
before a future merge.

The three archive branches preserve diagnostic experiments for reference. In
particular, the sampling profiler calls Windows APIs and is not a portable
production implementation. Their changes have not been merged into tuning or
main by this cleanup.

## Research materials

The tuning branch retains the existing reports, figures, CSVs, seven ZIP evidence
packages and available SHA-256 sidecars under `docs/validation/`. All seven ZIPs
passed CRC checks during cleanup. Downloaded strategy libraries, runtime build
outputs, and raw local research directories remain governed by `.gitignore`.
Some report links point into those local directories; the committed ZIP packages
preserve the evidence that was already packaged, not every ignored local run.

The parent-liquidity report still says that the complete four-seed 12-hour
verification is pending. Saving these files does not change that conclusion or
enable the candidate mechanism by default.

## Verification during cleanup

- Parsed every changed or newly added Python source file in all six working
  directories without syntax errors.
- Passed 54 targeted Python tests covering research recipes, analysis,
  streaming JSON, memory controls, receipts, and relay behavior in tuning.
- Passed three economic-market tests in the endogenous working directory.
- Tuning's `cargo test -p exchange-core --lib --quiet` completed with 487 passed,
  zero failed and 12 ignored tests.
- The ecosystem snapshot's Pine broker test run discovered 17 tests and
  reported 16 errors because the active Python environment lacked the required
  qualified `pine-compat-runtime==0.3.1`; that suite is not recorded as passing.
- Full PostgreSQL/HTTP/Pine integration and long-duration market validation were
  not rerun as part of this repository cleanup.
