步骤：P3/P4 缺口修复（ skeptic 复核）
基线 commit：29ff9df
实际变更：
- `commit_scheduler_step` 与 `clock/advance` 调用 `TrainingRun::on_step` 并持久化 `TrainingProgress`。
- 成交/挂单/撤单写入 run；费用来自 clearing；scheduler 成交计入 trainee maker/taker。
- complete/abort/expiry 按 order_id 顺序撤销 trainee 剩余挂单。
- 报告绑定 order_id、command_seq、fee、下单前 book。
- 训练进行中拒绝 trainee 入金/出金/划转。
- 测试：horizon 到期、abort 撤残、deposit 409、report 证据、快/慢步进分数一致。

24h soak：操作者豁免，见 `docs/validation/2026-09-06-p6.md`。未把短时 soak 当作 24h。

是否满足本步骤门槛：是（P3/P4 所列缺口已在 shipped 路径上覆盖；24h 仍豁免）。
