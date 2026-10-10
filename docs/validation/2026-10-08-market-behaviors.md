# 增强市场行为验证（2026-10-08）

本次交付包含基差套利、公共风控退出、POV 执行和共同消息。网页新房间默认使用
33 bot 配方，包含两条同账户套利腿、四个异质消息交易者、两个 POV 执行者，以及
两个提供主动永续订单流的噪声交易者。使用说明见 [MARKET_BEHAVIORS](../MARKET_BEHAVIORS.md)。

## 自动检查

| 检查 | 本次结果 | 本地回执 |
| --- | --- | --- |
| `cargo test --workspace --quiet` | 269 core、166 server、其余 11，共 446 个报告通过；零失败 | `.local/behavior-workspace-tests.log` |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 | `.local/behavior-clippy.log` |
| `cargo fmt --all -- --check`、`git diff --check` | 通过 | 终端检查 |
| 增强配方及隔离 HTTP Python 测试 | 2 个通过 | `.local/behavior-python-tests.log` |
| 原始 20 bot 配方及 HTTP 兼容检查 | 2 个通过 | `.local/behavior-background-compat.log` |
| Pine 运行时兼容检查 | 9 个通过 | `.local/behavior-pine-compat.log` |
| `marketforge-web` 的 `npm run build` | 通过 | 终端构建 |

Rust 总数包含同工作区其他改动的测试，不能将所有通过项归因于本次行为实现。
条件式 PostgreSQL 测试在未提供数据库时可以提前返回；上述结果不证明真实
PostgreSQL 重启恢复。本地 `.local` 回执不进入版本库。

新增行为检查覆盖：风险触发后的持续减仓和冷却；健康保证金下的提前风险退出；
消息发布前不可见、不同接收延迟与过期；完整外部成交量排除自身 maker/taker
成交；POV 部分成交和硬截止；套利实际双腿成交、价差收敛退出、对冲深度消失后的
未对冲超时退出。恢复检查覆盖保存决策、现货提交后、永续提交后的三个中断点。
还验证了原生策略拒绝 Hedge 模式，以及联动价格失效时仍允许必要的 reduce-only
风险退出。actor 过期拒绝可写入日志并恢复，worker 不因此进入失败状态。

## 完整配方的可复现实验

`cargo run -p exchange-core --example behavior_market -- <seed>` 使用网页导出的
33 bot 配方，按确定性调度推进 300 步（300000 仿真毫秒）。本次运行种子 7、19、41。
成交数来自实际撮合回执，数量来自各账户、品种的实际成交，拒绝原因保留原文。

| 种子 | 现货成交笔数 | 永续成交笔数 | 套利两腿各自成交总量 | POV 买 / 卖完成量（各目标 25） | 拒绝 |
| --- | ---: | ---: | --- | --- | --- |
| 7 | 717 | 150 | 3 / 3 | 25 / 25 | 10 个 post-only |
| 19 | 683 | 54 | 0 / 0 | 25 / 25 | 15 个 post-only |
| 41 | 254 | 56 | 2 / 2 | 24 / 25 | 45 个 post-only、12 个联动价格未就绪 |

套利成交总量包含入场和退出，不能解释为最终持仓。种子 19 没有满足条件的套利
成交，策略保持空闲。三次运行均未出现自成交，现货基础资产总量运行前后均为
1025，四个消息交易者均记录了两条消息。种子 41 的买入 POV 截止时未完成目标，
状态如实保留 24，未补造成交。回执为 `.local/behavior-seed7.json`、
`.local/behavior-seed19.json`、`.local/behavior-seed41.json`，包含账户资产与状态。

## 实际 HTTP 自动运行

使用独立编译的 `.local/behavior-server.exe`、临时工作目录和随机端口启动隔离
服务，未停止原有正在运行的服务。测试保留默认 500ms 墙钟调度间隔，仅压缩
消息和 POV 的仿真时间表：消息在 5000/20000ms 发布、15000/40000ms 过期；
POV 从 15000ms 开始、时长 30000ms、最后 5000ms 允许追赶。

运行到第 60 步时，33 个 bot 均有保存状态，worker 仍正常运行，`bot_errors` 为空。
四个消息交易者均收到两条消息，公开成交无自成交。POV 买入实际完成 10/25，
卖出完成 17/25，均记录截止且停止继续下单。验收要求真实成交和诚实的截止状态，
不要求忽略市场约束完成目标。回执：`.local/behavior-market-http.json`、
`.local/behavior-http-last-state.json`、`.local/behavior-market-http.log`。

## 已修复的问题和未验证边界

首次 HTTP 实验发现：actor 层拒绝过期订单后缺少原始命令，导致调度执行不能
写入日志并停止 worker。现已保存拒绝命令并加入写入、恢复回归检查；失败回执
保留在 `.local/behavior-http-default-first-failure.log` 和
`.local/behavior-http-default-second-failure.log`。

25ms 墙钟间隔的高速实验中，worker 已不再因过期拒绝停止，但观察与实际提交
积压约 20–30 个仿真步骤，超过默认 6000ms 子单有效期，POV 两侧均无成交。
该速度未通过行为运行资格验证，未用放宽价格或取消时效保护掩盖积压。失败状态
保留在 `.local/behavior-http-fast-cadence-state.json` 和
`.local/behavior-http-fast-cadence-failure.log`。默认完整时间表由确定性实验覆盖；
本次 HTTP 验收只覆盖上述压缩时间表与默认墙钟节奏。

这是合成市场验证，尚未用真实市场的成交量、价差、撤单率、波动聚集或响应延迟
进行校准。套利仅支持同交易所净持仓模式下买现货、卖永续；无借币、现货做空、
跨交易所延迟或资金费率收益优化。无深度时可能留下未完成退出。真实数据库重启、
长期性能及真实网络延迟仍需单独验证。
