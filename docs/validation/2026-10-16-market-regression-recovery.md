# Rejected market candidate and recovery control

The economic profile is rejected. The default experiment entry restores the exact earlier population; market-stop execution remains in the current runtime. No new realism claim is made.

Both historical seed-7 and seed-19 recipes compare structurally equal to regenerated legacy recipes, including funds, accounts, inventory and every bot configuration (room IDs normalized by generating the historical room). The rejected full-depth, maker 1.2, altered-arrival and compressed-demand settings are no longer selected by default.

The rejected four-seed run is preserved under `.local/economic-market-20261016/final`, with an additive `REJECTED.json` marker. Reproduction requires `--profile rejected-economic`. The original frozen plan and receipts are unchanged.

## Same historical 20–120 minute window

All two-hour historical seeds available before this rollback are included. These are diagnostic comparisons of combined profiles, not separate causal estimates for costs, depth or demand. ACF is minute log-return lag-one autocorrelation.

| Seed | Market | Population | Return ACF(1) | Longest one-way run (min) | Max minute move (bp) | Drawdown (%) |
|---|---|---|---:|---:|---:|---:|
| 7 | V-BTC-SPOT | previous-population | -0.015 | 8 | 6.7 | 0.21 |
| 7 | V-BTC-PERP | previous-population | 0.349 | 9 | 4.9 | 0.19 |
| 7 | V-BTC-SPOT | rejected-economic | 0.617 | 28 | 326.8 | 5.96 |
| 7 | V-BTC-PERP | rejected-economic | 0.663 | 31 | 309.8 | 6.09 |
| 19 | V-BTC-SPOT | previous-population | -0.130 | 7 | 5.2 | 0.12 |
| 19 | V-BTC-PERP | previous-population | 0.029 | 6 | 4.3 | 0.08 |
| 19 | V-BTC-SPOT | rejected-economic | 0.882 | 27 | 1050.9 | 51.58 |
| 19 | V-BTC-PERP | rejected-economic | 0.898 | 23 | 1229.7 | 51.27 |

## Frozen paired recovery runs

Seeds 7 and 19 each run for 30 minutes at one-second observation intervals, with the old frozen executable and the stop-fix executable. The recipe bytes are identical between the executable variants. Plans and executable hashes were frozen before launching. Every run completed at 1,800,000 ms and its receipt hash was verified.

- Seed 7: 26030 identical initial prints; first divergent print at 1389000 ms. Current market stops: 2 triggers, 2 submissions, 7 actual filled lots, 2 confirmed completions, 0 pending instances at the cutoff.
- Seed 19: 15802 identical initial prints; first divergent print at 840000 ms. Current market stops: 4 triggers, 4 submissions, 15 actual filled lots, 4 confirmed completions, 0 pending instances at the cutoff.

## Mechanism still requiring repair

A restored maker with other agents idle and zero new trades stays stationary at neutral inventory, but a +108-lot inventory offset moves its quote midpoint down 192.27 bp over 120 seconds. Reusing its own changed book as a fresh reference repeatedly reapplies inventory skew. This confirms a self-feedback mechanism; its share of full-market degradation has not been isolated. The rollback does not repair this inherited mechanism.

The earlier tight quote cap can conceal this behavior; restoring it is a rollback control, not an economic solution. The old population also restores fee-unaware quotes and reactive noise. The 30-minute runs are bounded restoration/stop-execution checks and do not qualify multi-hour market realism.

No exchange-core runtime source changed during this rollback. The previous complete library/Clippy validation still applies to the reused frozen stop-fix executable. The three population tests and diff whitespace checks passed after the entry-point change.

Machine-readable evidence: `2026-10-16-market-regression-recovery.json`; frozen paired plan: `.local/economic-market-20261016/recovery-control/comparison-plan.json`.
