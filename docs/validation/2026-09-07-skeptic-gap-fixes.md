# Skeptic gap fixes (F3 stream boundary, F4 e2e, F5 scenarios)

步骤 / 日期：验收补洞 / 2026-09-07

基线：F7 `47b76732a30d2f153d6280bf2453f0ac386b4e2b`。

本步行为契约：私有/公共流不得把最新 snapshot 与更早 delta 混用；cancel 必须带 `submit_account_id` 才能进私有流。F4 e2e 启动 Grid 机器人，用 `/trades` 独立复算，终态后再下单分数不变。F5 三场景真实跑调度；MM restore 不得重复报价，补单与 twin 步数一致。

## 实际命令

```bash
cargo test -p exchange-server --lib private_stream
cargo test -p exchange-core --lib training_scenarios
cargo test -p exchange-core --lib continuous_mm
MARKETFORGE_DATABASE_URL=$MARKETFORGE_TEST_DATABASE_URL ./scripts/training_recovery_e2e.sh
cargo clippy -p exchange-core -p exchange-server --all-targets -- -D warnings
```

| 检查 | 退出码 | 计数 |
| --- | --- | --- |
| private_stream | 0 | 6 |
| training_scenarios | 0 | 5（inventory_stress 现要求 filled>0；MM 账户预置仓位 8，taker 限价吃卖盘） |
| continuous_mm | 0 | 2 |
| training_recovery_e2e | 0 | 4/4；report_q=journal_q=score_q；score_frozen |
| clippy | 0 | — |

新增测试：`private_stream_rest_only_and_cancel_are_visible_to_owner`（真撤单）、`private_stream_snapshot_plus_deltas_match_authority_orders`、`private_stream_cache_overflow_fills_from_journal_or_resyncs`、`private_stream_reconnect_does_not_double_apply_resting_orders`、`basic_execution_sample_quotes_two_sided_and_stays_bounded`、`liquidity_withdrawal_named_account_cancels_at_sim_step`、`inventory_stress_background_flow_keeps_mm_within_cap`、`continuous_mm_replenishes_after_fill_and_restart_matches_live`。

日志：`{SCRATCH}/s0-f3-stream.log`、`s0-f5-scenarios.log`、`s0-f5-mm.log`、`s0-f4-e2e.log`。

24h soak 未启动。
