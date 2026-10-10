# F7 记录、CI 与发布门槛

步骤 / 日期：F7 / 2026-09-07

基线 commit：F6 `deccc0f0f12cbc4035422defe82c8ede35eff557`。本步实现见本提交。

前置阶段及证据：F0–F6 验收记录均保留日期与当时结论，未改写为“全部完成”。

本步行为契约：契约与事实表与 F1–F6 源码一致；CI 含 F1–F4 PostgreSQL/HTTP 回归和 Python 行为测试（非仅 compileall）；固定候选 commit 与声明负载；短时探针有上限；不自动启动 86400s soak。24h 门槛保持未满足。

实现位置：`docs/API_CONTRACT.md`、`docs/RUNTIME_CONTRACT.md`、`docs/BACKEND_STORAGE.md`、`.github/workflows/backend.yml`、`scripts/backend_soak.sh`（`MARKETFORGE_SOAK_ROOM_PREFIX`）、`scripts/training_recovery_e2e.sh`（停进程后等待 advisory lock）。无新 SQL。

## 执行环境 / 独立数据库 / 端口

- 独立库 `marketforge_f0_a653ff2_20260907` @ `127.0.0.1:55432`；`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`。
- 短探针：`127.0.0.1:57541`，20s，2 rooms，1s tick，1 order/tick/room。
- CI 本地复跑日志：`/tmp/grok-goal-3ed636a168d9/implementer/f7-ci-local.log`（不入库）。
- 探针工件：`.../f7-probe/{declared-load.txt,resources.csv,metrics.prom,commands.jsonl,exceptions.log}`。

## 能力事实表（相对 F0 更新，不改写 F0 原文）

| 能力 | 状态 | 证据 |
| --- | --- | --- |
| 控制幂等跨重启 | 当前版本验证通过 | F1 `control_idempotency` 9 tests；0013 |
| 动作配额跨重启 | 当前版本验证通过 | F2 `external_action_quota` 3；0014 |
| 训练冻结 / 结算 | 当前版本验证通过 | F2 freeze/settle/close；`settle_training_residuals` |
| 私有流 / cursor | 当前版本验证通过 | F3 `private_stream` 3 |
| 训练 HTTP e2e | 当前版本验证通过 | F4 `training_recovery_e2e.sh` 4/4（本步复跑） |
| 持续做市 + 三场景 | 当前版本验证通过 | F5 `continuous_mm` / `training_scenarios` |
| 批量终态与 child seed | 当前版本验证通过 | F6 unittest 17 + 代表性 runner |
| 24h 持续运行 | **未实现** | 未执行 `MARKETFORGE_SOAK_SECONDS=86400` |

## 候选 commit 与声明负载

- 功能候选：F6 `deccc0f`（批量评估）。本 F7 提交只加契约/CI/探针记录。
- 短探针声明（`declared_load_version=1`）：`duration_s=20`，`rooms=2`，`agents_per_room=0`，`tick_s=1`，`orders_per_tick=1`，journal=PostgreSQL 独立库，bind `127.0.0.1:57541`。
- 实测：ticks=17，errors=0，ready=1，queue_depth=0，RSS 32–35 MB，exceptions.log 空，metrics.prom 已采集。

未启动默认 86400 秒脚本。

## 实际命令、退出码、测试数

本地按 workflow 步骤复跑（`f7-ci-local.log`）：

| 步骤 | 退出码 | 计数 |
| --- | --- | --- |
| cargo fmt --check | 0 | — |
| cargo clippy -D warnings | 0 | — |
| cargo test --workspace | 0 | core 175；server 125；plugin 6；cli 2+3 |
| control_idempotency | 0 | 9 |
| freeze_survives | 0 | 1 |
| external_action_quota | 0 | 3 |
| training_settle_failure | 0 | 1 |
| close_room_settles | 0 | 1 |
| private_stream | 0 | 3 |
| training_postgres_crash_recovery | 0 | 1 |
| training_recovery_e2e.sh | 0 | 4/4（lock-wait 修复后） |
| python.tests.test_batch_runner | 0 | 17 |
| compileall | 0 | — |
| backend_training_smoke.sh | 0 | — |
| 短探针 20s | 0 | 17 ticks / 0 errors |

首次把 e2e 紧接 workspace/探针时，single-active 切换撞到 PostgreSQL advisory lock。已在 e2e `reap_server` 等待会话锁释放，single-active 启动最多等 3s。复跑 4/4。

## 长日志 / 慢消费者 / 库不可用 / 关停（已有阶段证据，非 24h）

| 项 | 证据 |
| --- | --- |
| 长日志 / cache 溢出 | F3：超过 1024 条从 journal 补齐或 `resync_required` |
| 慢消费者 | F3：有界缓冲；落后发 resync |
| 数据库短暂不可用 | F2 `training_settle_failure_does_not_install_live_state`；F1 提交前失败不安装 |
| 正常关停 | F4 SIGTERM 重启路径；探针结束 pause+close+trap kill |

短探针确认 `/metrics` 与 `resources.csv` 采集正常。这些不是 24h 稳定性声明。

## 发布结论（分开写）

- 功能验收：F0–F6 门槛已在各自记录中满足。
- 故障恢复验收：F1 控制幂等重启/接管、F2 配额/冻结/结算、F4 四路径 e2e。
- 容量范围：短探针 2 rooms × 1 order/s，约 20s，errors=0。未测量更大负载。
- 持续运行时长：**未做 24h**。

## 覆盖范围、跳过项和原因

未执行 `scripts/backend_soak.sh` 默认 86400s。GitHub Actions 因本目标禁止 push，无远程 check-run；门槛是已提交 workflow 文本 + 上述本地复跑。

## 门槛是否满足

功能与 CI/短探针项满足。24h 发布门槛 **不满足**，保持未勾选。

功能阶段已完成，长期运行验收待安排
