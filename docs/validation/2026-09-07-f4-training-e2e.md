# F4 完整训练端到端验证（批次 A）

步骤 / 日期：F4 / 2026-09-07

基线 commit 与实现 commit/diff：F3 `9318dc01f3bef46188075f01dbc95eb95bbab66c`。本步实现见本提交。

前置阶段及证据：F1–F3 已提交并通过门槛。

本步行为契约：独立 PostgreSQL + Bearer（admin-token/trainee-token）HTTP。四条路径：连续、进程重启、room-leased 接管、快/慢 `clock/advance`。终点无 trainee 可成交残单；fills 绑定 `order_id` 与 `book_before`；费用 0 可手算。

实现位置与迁移：`scripts/training_recovery_e2e.sh`。无新迁移。进程杀用 SIGTERM 后同库重启；接管用 `MARKETFORGE_RUNTIME_MODE=room-leased` 第二实例。速度路径 1×3 步 vs 3×1 步。

执行环境 / 独立数据库 / 端口：`marketforge_f0_a653ff2_20260907`；57421/57422。显式 step，无 sleep 猜测提交点。

## 实际命令、退出码、测试数

```bash
MARKETFORGE_DATABASE_URL="$MARKETFORGE_TEST_DATABASE_URL" ./scripts/training_recovery_e2e.sh
# 连续执行两次
```

| 检查 | 退出码 | 结果 |
| --- | --- | --- |
| e2e 第 1 次 | 0 | 4/4 paths |
| e2e 第 2 次 | 0 | 4/4 paths |
| `tests::training_postgres_crash_recovery_matches_live_run` | 0 | ok |

机器可读断言（每次路径）：status Completed、open_trainee_orders 0、book_before_bound true、fees_paid 0、filled_qty 1、score_q 1。速度路径 `speed_match: true`。后续补洞（`2026-09-07-skeptic-gap-fixes.md`）增加 Grid 机器人、`/trades` 复算与终态分数冻结。

## 故障点及预期/实际状态

- 连续：trainee 挂单 101 成交 1 手 + 90 残单，到期 settle 撤残单。
- 重启：成交后 SIGTERM，同库重启再 advance 至终态。
- 接管：A 成交后退出，B `room-leased` 接管并完成。
- 速度：fast 一次 3 步 vs slow 三次 1 步，规范化终点一致。

进程覆盖：重启 SIGTERM、接管换实例。单元注入覆盖：F2 `training_settle_failure_does_not_install_live_state`。

## 日志路径与摘要

scratch：`f4-e2e.log`、`f4-e2e-2.log`。服务日志 `target/training-recovery-e2e/`（本地忽略）。

## 覆盖范围、跳过项和原因

- 脚本未再启动 Grid worker（Bearer 内部 scheduler 已由 F1 单步/既有 agent 测试覆盖）；本脚本用显式 trainee 订单 + clock/advance。
- 私有流重建等式在 F3 HTTP 测试中覆盖，e2e 以权威 GET orders/result 为准。

## 兼容限制与剩余问题

- 训练开始即冻结账户分配，故 e2e 由 trainee 建房（owner）再添加 admin 成员。
- 批次 A 功能门槛已满足；24h soak 仍未跑。

## 门槛是否满足

是。F1–F4 均有行为证据。批次 A 候选 commit 为本步提交。
