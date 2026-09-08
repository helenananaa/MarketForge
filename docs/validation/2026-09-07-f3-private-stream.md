# F3 私有流与恢复游标完整性

步骤 / 日期：F3 / 2026-09-07

基线 commit 与实现 commit/diff：F2 `3bc2a55a8ddc08f499e31cc3017aeabbe1778eb6`。本步实现见本提交。

前置阶段及证据：F0 将纯挂单私有更新与跨连接 cursor 标为未实现。F1/F2 已提交。

本步行为契约：

- 公共流：盘口、成交、房间状态。私有流：访问者自己的订单/账户及 rest-only/撤单/成交。
- Cursor：`{room_id, scope, version: stream.v1, command_seq}`。`stream_seq` 仅连接局部。`after_command_seq` 为持久化位置；过滤空洞不是丢包。
- Snapshot 带 `at` 边界；后续 execution 只应应用 `command_seq` 大于该边界的事件。
- 缓存缺少 `after+1` 时从 journal `query_executions` 补齐；live lag 仍 `resync_required`。scope 不符 400。撤权后私有订阅 403，snapshot 不含他人账户。

实现位置与迁移：无新迁移。`RoomExecutionSummary.submit_account_id`；`user_can_see_private_execution` 按提交账户与成交账户授权；`private_stream_snapshot`；`stream_history_executions`。

执行环境 / 独立数据库 / 端口：与 F0 相同。

## 实际命令、退出码、测试数

```bash
cargo test -p exchange-server --lib private_stream
cargo test -p exchange-server --lib
cargo fmt --all -- --check
cargo clippy -p exchange-server --all-targets -- -D warnings
```

| 检查 | 退出码 | 测试数 |
| --- | --- | --- |
| private_stream | 0 | 3 passed（后续补洞见 `2026-09-07-skeptic-gap-fixes.md`，现为 6） |
| exchange-server --lib | 0 | 125 passed |
| fmt / clippy | 0 | — |

测试名：

- `tests::private_stream_snapshot_includes_own_resting_orders_not_others`
- `tests::private_stream_rest_only_and_cancel_are_visible_to_owner`
- `tests::private_stream_rejects_mismatched_scope_and_omits_others_after_revoke`

## 故障点及预期/实际状态

- 私有 snapshot 仅含账户 20 的挂单与账户列表。
- rest-only 下单后重连 `after_command_seq=0` 可见 OrderRested 或订单字段。
- `scope=public` 打到 private 路径返回 400；撤权后 403。

## 日志路径与摘要

scratch：`f3-tests.log`、`f3-server-lib.log`、`f3-clippy.log`。

## 覆盖范围、跳过项和原因

- 慢消费者 live lag 仍走既有 `resync_required`（broadcast Lagged），未改为 journal 追赶在线流。
- 跨市场保证金/划转出现在 clearing 过滤中，无单独 HTTP 场景测试。

## 兼容限制与剩余问题

- snapshot JSON 新增 `cursor`/`orders`/`accounts`，additive。
- F4 完整训练 e2e 仍待做。

## 门槛是否满足

是。测试客户端用私有 snapshot 重建的订单/账户等于授权范围；含 rest-only 与撤权。
