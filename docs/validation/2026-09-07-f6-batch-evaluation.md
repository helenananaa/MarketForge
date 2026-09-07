# F6 批量策略评估终态与 seed 注入

步骤 / 日期：F6 / 2026-09-07

基线 commit：F5 `cd019613bc79bb30e9d6c8c4aeac8fa4a02226e8`。

前置阶段及证据：F0–F5 已提交；独立库 `marketforge_f0_a653ff2_20260907` @ `127.0.0.1:55432`。

本步行为契约：`start_training` 成功只表示 Running。runner 驱动策略与时钟直到 Completed/Failed/Aborted 再保存服务端成绩。seed 经 `child_seed`（与 `exchange_core::training_scenarios::child_seed` 相同的 wrapping u64）写入带 `seed` 字段的 agent；仅改 run/room 名不是不同实验。run 身份含场景摘要、策略版本、参数、seed、评分版本。丢失 start 按 run_id 查询。运行中恢复对账服务端；本地 JSON 不是成绩真相。状态文件 temp+`os.replace`，单写者 flock。有界并发、超时、取消；失败保留；零成交不是低成本胜利。

实现位置与迁移：`python/marketforge/batch.py`、`python/marketforge/__init__.py`、`scripts/batch_runner.py`、`python/tests/test_batch_runner.py`、`scripts/fixtures/f6_batch_spec.json`。无新 SQL。

## 执行环境 / 独立数据库 / 端口

- Python 3.10.12；`exchange-server` debug binary。
- PostgreSQL 独立库，DSN 仅经环境变量；`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`。
- 测试服务器 `127.0.0.1:57531`（unittest）；代表性调用 `127.0.0.1:57532`。Bearer `admin-token`。
- 日志不入库：`/tmp/grok-goal-3ed636a168d9/implementer/f6-tests.log`、`f6-batch.out`。

## 实际命令、退出码、测试数

```bash
MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 python3 -m unittest python.tests.test_batch_runner -v
python3 scripts/batch_runner.py http://127.0.0.1:57532 scripts/fixtures/f6_batch_spec.json 1 2 \
  --state ... --bearer admin-token --run-prefix f6-rep-... --concurrency 2 --timeout-seconds 30
python3 -m compileall -q python scripts/batch_runner.py python/examples/buy_remaining.py
```

| 检查 | 退出码 | 测试数 |
| --- | --- | --- |
| `python3 -m unittest python.tests.test_batch_runner -v` | 0 | 17（非 compileall） |
| 代表性 `scripts/batch_runner.py` seeds 1,2 | 0 | 2 行均为 Completed，均可 GET `/training/runs/{id}` 复放 |
| compileall | 0 | — |

Live 行为测试（真实 HTTP + PostgreSQL，count > 0）：

- `test_start_is_not_treated_as_completed_score`
- `test_seed_changes_agent_rng_on_server`
- `test_crash_mid_run_resumes_same_server_run`
- `test_same_run_retry_returns_original`
- `test_bounded_concurrency`
- `test_cancel_one_run_leaves_others`
- `test_failed_runs_remain_and_zero_fills_are_not_wins`
- `test_cli_batch_runner_emits_terminal_rows`

## 故障点及预期/实际状态

- start 后 status=Running、`score.finished=false`；驱动后才写终态。
- 中断：`max_clock_advances=1` 留下 phase=running；恢复同一 `run_id`，成绩来自 GET result。
- `--fail-seeds` 不再改 `target_qty`；start 后 abort，失败行留在汇总。
- 代表性调用曾因 `--run-prefix` 未进入 `room_id` 与 unittest CLI 房间碰撞 409；已改为 run_id 与 room_id 共用前缀。

## 覆盖范围、跳过项和原因

无跳过。17 项全部 ok。未启动 24h soak（F7）。

## 兼容限制与剩余问题

- child-seed 只写入 agent 配置里已有的 `seed` 字段（NoiseTrader / ContinuousMarketMaker）。Grid/DCA/CancelAtStep 无 RNG seed。
- 批量并发是进程内线程池 + 单状态文件写者，不是跨进程 worker 池。
- F7 再更新 CI，使 workflow 不只跑 compileall。

## 门槛是否满足

是。批量输出为已完成或明确失败；每条结果可定位到可复放的服务端 run。
