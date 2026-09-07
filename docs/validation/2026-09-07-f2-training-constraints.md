# F2 训练约束与配额恢复

步骤 / 日期：F2 / 2026-09-07

基线 commit 与实现 commit/diff：F1 `360b22ed57862ceb7723de3b721929898c7d9dcf`。本步实现见本提交。

前置阶段及证据：`docs/validation/2026-09-07-f1-control-idempotency.md`。F0 将配额标为未实现、冻结为已实现待验证、abort 作用 live rooms、close 未 settle。

本步行为契约：

- 冻结仍由权威 `TrainingRun` 推导（`training_assignment_frozen` / `allows_trainee_action`），无独立布尔。
- 配额维度 `(room_id, authenticated user_id, simulation step)`，上限 `EXTERNAL_ACTIONS_PER_STEP`。鉴权/精度失败不计；幂等重放先于配额检查且不另扣；未 journal 的业务拒绝不计；journal 成功的非 admin 订单在同一事务扣 1。
- 结束路径（scheduler/clock/order/abort/close）调用 `settle_training_residuals`；abort/close 先改 candidate，journal 成功后再安装。

实现位置与迁移：

- 新增 `0014_external_action_quota.sql` / `marketforge_external_action_counts`。
- `JournalExecution::with_quota`；Postgres `INSERT ... ON CONFLICT WHERE count < 8 RETURNING`；内存 map 在 append 成功后更新。
- 删除 `AppState.external_action_counts`。
- close 先 settle 再关市，避免 Closed 拒绝撤单。

执行环境 / 独立数据库 / 端口：与 F0 相同。`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`。

## 实际命令、退出码、测试数

```bash
cargo test -p exchange-server --lib freeze_survives
cargo test -p exchange-server --lib external_action_quota
cargo test -p exchange-server --lib training_settle_failure
cargo test -p exchange-server --lib close_room_settles
cargo test -p exchange-server --lib
cargo fmt --all -- --check
cargo clippy -p exchange-server --all-targets -- -D warnings
```

| 检查 | 退出码 | 测试数 |
| --- | --- | --- |
| freeze_survives | 0 | 1 |
| external_action_quota | 0 | 3（含 postgres 重启） |
| training_settle_failure | 0 | 1 |
| close_room_settles | 0 | 1 |
| exchange-server --lib | 0 | 122 passed |
| fmt / clippy -D warnings | 0 | — |

测试名：

- `tests::training_freeze_survives_restart_and_takeover_for_terminal_states`
- `tests::external_action_quota_survives_restart_and_resets_on_step`
- `tests::external_action_quota_idempotent_retry_does_not_double_count`
- `tests::external_action_quota_postgres_survives_restart`
- `tests::training_settle_failure_does_not_install_live_state`
- `tests::close_room_settles_training_residuals`

PostgreSQL 配额重启实际执行，非 skip。

## 故障点及预期/实际状态

- Running 与 Aborted 后分配冻结；重启与租约接管结果相同。
- 配额用尽 → 重启仍 429 → `clock/advance` 一步后恢复。
- 最后一份额度用幂等 key 占用后重放仍 200，新请求 429。
- `TrainingProgress` 写入失败：live `training_runs` 不出现该 run。
- close 将 trainee 挂单标为 canceled。

## 日志路径与摘要

scratch：`f2-tests.log`、`f2-server-lib.log`、`f2-clippy.log`。

## 覆盖范围、跳过项和原因

- Failed 状态当前无独立 start 入口；用 Aborted 覆盖终态冻结。Completed 仍由既有 horizon/fill 路径覆盖。
- 角色变更本身未冻结（契约只冻账户分配）。
- 并发最后配额由 AppState mutex + DB WHERE count < 8 保护。

## 兼容限制与剩余问题

- 新增迁移 0014，向前兼容空表。
- 私有流与 batch runner 仍待 F3/F6。

## 门槛是否满足

是。任务限制、动作额度、终止撤单跨恢复一致；训练写入失败不把 live map 提前生效。
