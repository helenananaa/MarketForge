步骤：P3/P4 缺口修复（ skeptic 复核）+ P4.4 persist/replay
基线 commit：29ff9df / 6322960
实际变更：
- `commit_scheduler_step` 与 `clock/advance` 调用 `TrainingRun::on_step` 并持久化 `TrainingProgress`。
- 成交/挂单/撤单写入 run；费用来自 clearing；scheduler 成交计入 trainee maker/taker。
- complete/abort/expiry 按 order_id 顺序撤销 trainee 剩余挂单。
- 报告绑定 order_id、command_seq、fee、下单前 book。
- 训练进行中拒绝 trainee 入金/出金/划转。
- 训练建房在租约模式下走 `create_room_with_writer_lease`，与普通建房同一 fencing。
- P4.4 对照：
  - 连续 vs 共享 journal 崩溃恢复（命令序、账户、训练状态、评分）。
  - 连续 vs 租约接管（node-a 中途释放，node-b fencing_token=2 续跑）。
  - 快 3 步 vs 慢 3×1 步。
  - PostgreSQL `connect_migrated`（不用进程独占 runtime lock）崩溃恢复；避免与 `postgres_journal_persists_and_recovers_room_when_configured` 抢 advisory lock。

24h soak：操作者豁免（「不需要跑24h soak」），见 `docs/validation/2026-09-06-p6.md`。未把短时 soak 当作 24h。

是否满足本步骤门槛：是（P3/P4 所列缺口已在 shipped 路径上覆盖；24h 仍豁免）。
