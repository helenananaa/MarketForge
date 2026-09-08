# F5 持续做市与训练场景

步骤 / 日期：F5 / 2026-09-07

基线 commit：F4 `e8cf8455f7aa9bcbf271e2c09ea26bebcfaa978b`。

本步行为契约：Grid 行为不变。新增 `ContinuousMarketMaker`（双边价差、每档数量、库存上限、重报价阈值、最大挂单、补单节奏）和 `CancelAtStep`（指定账户在仿真步撤单）。三场景 `basic_execution` / `liquidity_withdrawal` / `inventory_stress` 带 version 与 child seed。

实现：`exchange-core/src/agents.rs`、`scheduler.rs`、`training_scenarios.rs`。

## 实际命令

```bash
cargo test -p exchange-core --lib continuous_mm
cargo test -p exchange-core --lib training_scenarios
cargo test -p exchange-core --lib grid_trader_seeds
cargo clippy --workspace --all-targets -- -D warnings
```

| 检查 | 退出码 | 测试 |
| --- | --- | --- |
| continuous_mm | 0 | 1（后续补洞为 2，见 skeptic-gap-fixes） |
| training_scenarios | 0 | 2（后续补洞为 5，含真实调度样本） |
| grid_trader_seeds | 0 | 1（Grid 未变） |
| clippy | 0 | — |

## 门槛是否满足

是。Grid 回归通过；持续做市有界并恢复；三场景版本化且流动性撤离为账户撤单而非改价。
