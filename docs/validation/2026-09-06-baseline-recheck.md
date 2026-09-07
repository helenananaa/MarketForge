# 当前版本基线复验

验证时间：2026-09-06，完成时间约 06:21 UTC（14:21 Asia/Shanghai）。

基线 commit：`29ff9df612097353669cd8cf4b98972f3754917b`。
开始及完成测试时工作区干净，HEAD 未改变。本次只新增此记录，不修改业务实现，不提交 Git。

## 范围与环境

用户授权执行 Rust 检查、工作区测试、真实 PostgreSQL 恢复测试和两个 HTTP smoke。由于当前 commit 已包含 CLI 和 Python SDK，同时执行仓库 CI 中的 CLI smoke 与 Python 语法检查。

- Rust / Cargo：1.95.0；Linux / WSL 工作区。
- 复用现有 `marketforge-postgres` 容器（`postgres:16-alpine`），端口 55432；未重启容器。
- 新建独立数据库 `marketforge_verify_29ff9df_20260906`，未使用已有业务数据库执行迁移或测试。
- smoke 使用 57305/57306、57307/57308、57311，启动前确认无监听。
- PostgreSQL 工作区测试与两个数据库 smoke 顺序执行，避免 journal 独占锁互相干扰。

## 实际结果

| 检查 | 结果 | 证据 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 通过，退出码 0 | 无格式差异 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过，退出码 0 | `clippy.log` |
| 强制 PostgreSQL 的 `cargo test --workspace` | 283 passed、0 failed、0 ignored | `workspace-tests.log` |
| `postgres_smoke.sh` | 通过，退出码 0 | `postgres-smoke.log` |
| `postgres_multi_active_smoke.sh` | 通过，退出码 0 | `postgres-multi-active.log` |
| `backend_training_smoke.sh` | 通过，退出码 0；内存 journal | `training-smoke.log` |
| Python SDK / 示例 / batch runner compileall | 通过，退出码 0 | 语法检查无错误；不代表 Python 运行时集成已覆盖 |

工作区测试分布：exchange-core 171、exchange-server 101、CLI 2、插件集成 3 + 6，共 283。
`postgres_journal_persists_and_recovers_room_when_configured` 明确显示 `ok`，设置了强制开关，不是缺少数据库导致的静默跳过。

两个数据库 smoke 的成功标识：

```text
PostgreSQL journal smoke passed for pg-smoke-1788675592-744675
PostgreSQL multi-active smoke passed for pg-multi-a-1788675607-745792 and pg-multi-b-1788675607-745792
backend training smoke passed for cli-smoke-1788675632-747564
```

原始日志保存在工作区 `target/validation/2026-09-06-29ff9df/`，该目录为本地忽略产物，不随此 Markdown 提交。原始运行日志另在 `/tmp/marketforge-validation-29ff9df/`。

## 执行命令

以下数据库地址中的凭据是仓库 compose 的本地测试默认值：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

MARKETFORGE_TEST_DATABASE_URL=postgres://marketforge:marketforge@127.0.0.1:55432/marketforge_verify_29ff9df_20260906 MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 cargo test --workspace

MARKETFORGE_DATABASE_URL=postgres://marketforge:marketforge@127.0.0.1:55432/marketforge_verify_29ff9df_20260906 ./scripts/postgres_smoke.sh
MARKETFORGE_DATABASE_URL=postgres://marketforge:marketforge@127.0.0.1:55432/marketforge_verify_29ff9df_20260906 ./scripts/postgres_multi_active_smoke.sh

env -u MARKETFORGE_DATABASE_URL ./scripts/backend_training_smoke.sh
python3 -m compileall -q python scripts/batch_runner.py python/examples/buy_remaining.py
```

## 结论与验证边界

本轮约定的基线验证通过，无测试失败需要修复。它证明当前实现满足现有测试断言，不代表执行计划所有阶段门槛已实现。

对现有记录和相关源码的复核仍发现以下待办，不能被本轮绿色结果消除：

1. `control_idempotency` 使用进程内 map；P1 记录明确说明控制幂等未使用 0013 表。跨重启 manual step 重试仍需要后续修复和测试。
2. P2 记录说明私有流缺少无成交纯挂单更新；本轮未补写该行为测试。
3. P3 记录说明结束时撤销剩余订单尚无完整 settle 路径，做市仍复用 GridTrader。
4. P4 记录说明报告没有逐笔绑定下单前盘口。
5. P5/P6 记录说明配额、训练冻结存在进程内状态限制。当前 `AppState` 中确有相关 map，后续需专门验证持久化与接管语义。

`backend_training_smoke.sh` 实际覆盖房间创建、下单幂等、撤单、ticker/candles 查询不推进时钟、暂停和关闭；它没有完整执行训练 start → 完成 → 评分 → 重启复算，也没有在该脚本中开启 Bearer 或启动机器人。不能仅凭脚本名称宣称这些路径已端到端验证。

本轮未运行 24 小时 soak、磁盘容量故障、性能压力测试，也未将此前短时运行记录作为本轮结果。前端不在范围内。

## 环境收尾

smoke 自行结束所启动的服务进程；保留既有 PostgreSQL 容器。独立验证数据库保留用于复查，其中可能有 smoke 留下的测试房间。后续可在不需要证据时单独删除该数据库，不应删除整个容器或既有数据卷。

建议下一项优先修复跨重启控制幂等，再对训练状态/冻结/配额恢复与结束撤单建立端到端验收。
