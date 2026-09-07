# F1 控制操作持久化幂等

步骤 / 日期：F1 / 2026-09-07

基线 commit 与实现 commit/diff：F0 `10b2752c86a018b1e8a1114660fef646172857a4`。本步实现见本提交。

前置阶段及证据：`docs/validation/2026-09-07-f0-baseline.md`。F0 将控制幂等标为未实现（内存 map，0013 表未读写）。

本步行为契约：`control.v1`。控制 key 作用域为 `(已认证主体, room_id, Idempotency-Key)`，与订单 key 空间分离。指纹为规范化 JSON `{operation, params, protocol}`，不是原始 URL 或 body。成功结果与 mutation 同一 journal 事务写入 `marketforge_control_idempotency`。业务拒绝（无 mutation）与基础设施失败不落盘。key 不过期。重放前必须通过当前权限检查。内存 journal 同进程等价，不承诺跨进程。

实现位置与迁移：

- 复用 `0013_scheduler_and_control_idempotency.sql` / `marketforge_control_idempotency`；未改已应用迁移，无 0014。
- `PendingJournalMutation.control_idempotency` 随 `append_room_mutation` / `_fenced` 写入同一事务；唯一约束冲突回滚 mutation。
- 删除 `AppState.control_idempotency`。pause/resume/close/clock/step/clock/advance 走 journal 查找。
- 单步把控制结果绑在 `SchedulerProgress`（第一 durable 子步骤），不在 TrainingProgress 之后另写。
- 恢复时对暂停房间中已接受的调度成交临时恢复 Running 再回放，避免外部 pause 门禁把合法 scheduler 成交打成拒绝。

执行环境 / 独立数据库 / 端口：与 F0 相同。独立库 `marketforge_f0_a653ff2_20260907`。`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`。

## 实际命令、退出码、测试数

```bash
export MARKETFORGE_TEST_DATABASE_URL='postgres://marketforge:marketforge@127.0.0.1:55432/marketforge_f0_a653ff2_20260907'
export MARKETFORGE_REQUIRE_POSTGRES_TESTS=1
unset MARKETFORGE_DATABASE_URL
cargo test -p exchange-server --lib control_idempotency
cargo test -p exchange-server --lib
cargo fmt --all -- --check
cargo clippy -p exchange-server --all-targets -- -D warnings
```

| 检查 | 退出码 | 测试数 |
| --- | --- | --- |
| `cargo test -p exchange-server --lib control_idempotency` | 0 | 9 passed, 0 failed, 0 ignored（过滤后 > 0） |
| `cargo test -p exchange-server --lib` | 0 | 116 passed（原 107 + 本步 9） |
| fmt / clippy -D warnings | 0 | — |

新增/覆盖测试名：

- `journal::tests::in_memory_control_idempotency_is_unique_and_replayable`
- `tests::control_idempotency_serial_replay_returns_original_body`
- `tests::control_idempotency_concurrent_same_key_steps_once`
- `tests::control_idempotency_same_key_different_operation_conflicts`
- `tests::control_idempotency_pre_commit_failure_leaves_state_and_retry_succeeds`
- `tests::control_idempotency_survives_restart_without_second_step`
- `tests::control_idempotency_takeover_replays_and_old_fence_cannot_append`
- `tests::control_idempotency_revoked_permission_does_not_leak_body`
- `tests::control_idempotency_postgres_restart_replays_step`

PostgreSQL 重启路径实际执行：`control_idempotency_postgres_restart_replays_step` 为 `ok`，不是 skip。

## 故障点及预期/实际状态

- 同键并发单步：只推进一次逻辑时钟。
- 同键不同操作 / 不同 `steps`：409，无额外 mutation。
- 提交前注入 journal 失败：房间仍 Running，重试 pause 成功并之后可重放。
- 内存 journal 崩溃恢复与 PostgreSQL `connect_migrated` 重启：重放原 body，时钟不二次推进。
- 租约接管：B 重放原结果；旧 fencing token `append_room_mutation_fenced` 为 `RoomLeaseLost`。
- 撤权后同键重放：HTTP 403，body 不含缓存 pause 结果。

## 日志路径与摘要

会话 scratch：`f1-tests.log`、`f1-server-lib.log`、`f1-clippy.log`。不随仓库提交。

## 覆盖范围、跳过项和原因

- 覆盖 pause/resume/close/step/advance 的控制空间；pause-only 不是唯一证据。
- `HttpTradingClient::{pause,resume,close,advance}_room` 默认仍不发送 key；CLI 仅在 `--idempotency-key` 时发送。契约已写明。
- 进程 kill 用 drop + 新 AppState 恢复代替 OS SIGKILL；PostgreSQL 路径使用独立 journal 连接而非独占 runtime lock，避免与第二实例抢锁。

## 兼容限制与剩余问题

- 订单幂等仍走 0010 / executions，不受影响。
- 配额、私有流、batch runner 仍按 F0 事实表未实现，留给 F2+。
- 恢复对暂停房间的已接受成交临时解冻回放，仅用于 journal 重建，不改变在线 pause 门禁。

## 门槛是否满足

是。真实 PostgreSQL 与重启/接管路径验证了单步严格一次；不仅是重复 pause。
