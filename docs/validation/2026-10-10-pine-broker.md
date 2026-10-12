# Pine broker 2.0.0 验收

日期：2026-10-10。基线：`71b58e9`。本次代码尚未提交。

## 已实现

Pine 默认使用完整 broker 适配器。订单和人工交易进入同一个 MarketForge 网关，
执行相同市场、账户、风控和训练策略检查；实际成交、费用和收益由核心账本决定。
插件不模拟成交，不自行分配订单 ID。

| 能力 | 实现与证据 |
| --- | --- |
| 永续多空、反手、平仓 | 实际永续账户；平仓使用 reduce-only。HTTP 测试验证部分空头成交与真实归零。 |
| 双向永续账户 | 明确 Long/Short 侧；持久化成交侧。PostgreSQL 重启后读到实际 Short 成交，仅关闭该侧。 |
| 持续限价单 | 同 Pine ID 跟踪真实 order_id；允许的改单走原生 amend，提高主动性时撤单后等待回执，再挂剩余量。 |
| 撤单和替换 | 撤本实例跟踪的订单；新 bar 的撤单优先于待替换订单。人工修改过的持仓保护不会被当成原保护撤掉。 |
| 条件单和退出 | 永续 stop/stop-limit 使用原生条件单；止盈止损及激活后的追踪退出使用持仓保护。HTTP 验证条件入场、止盈触发和持仓归零；Pine 执行测试验证追踪激活与 tick 换算。 |
| 多市场与多周期 | 清单声明交易品种和附加周期；同一观察波提供各市场公开数据、自己的账户/订单/成交，以及已收盘 K 线。Pine request provider 冻结每次可见前缀。 |
| 双腿协调 | 同基准单位的现货/永续组合，根据实际成交敞口对冲，部分成交不算满额。超时、拒绝或保证金异常后撤单并持续尝试平两腿。 |
| 订单回报和恢复 | 实际接受/拒绝、原因、命令序号及订单 ID 和 bot state 一起提交。动作中途恢复测试覆盖两腿的各个提交断点。 |
| 数据完整性 | 重启和写入租约接管恢复 journal 清算反馈；只提供自己的历史账户数据。历史现货反馈不虚构预留资产。缺失、截断、未知双向成交侧时拒绝执行。 |
| 精度和预算 | syminfo.mintick 使用真实 tick_size；订单按 tick/lot 检查。同次决策累计预留库存及现金预算。 |
| 旧实例 | 显式兼容 1.0.0；旧 identity 状态继续 legacy 规则。新 broker state 使用 broker_schema=2。 |

新增原创示例：`limit.pine`、`perp.pine`、`basis.pine`。原三种策略及 20+6 混合市场保留。

## 验证环境与日志

使用仓库 `.venv` 的 `pine-compat-runtime==0.3.1`，独立构建目录
`target/pine-validation`，独立 loopback 服务端和 PostgreSQL 55439。
Rust、Python 和最后的 Pine 专项使用不同测试数据库。强制设置 Pine/PostgreSQL
验收变量，恢复测试没有以缺少依赖为由跳过。测试只清理自己启动的服务端进程树。

- 最终 Rust：535 通过、0 失败、12 项原有 opt-in/基准测试 ignored。
- Python 全套：203 项，196 通过、7 项外部 Codex/OpenCode/provider/Docker/实盘环境测试按原规则跳过。
- 最终历史反馈类型收紧后，Pine 全套专项重新运行：35 通过、0 失败、0 跳过，包含 3 个 PostgreSQL 恢复测试。
- Clippy `--workspace --all-targets -- -D warnings`、Cargo 格式检查、Python 编译与 `git diff --check` 通过。

可执行文件 SHA256：`E6BE16775B79F9A10D491ECFA9C145441DD340D28A0052291D0E49CA317498B0`。

- Rust 全工作区日志：`target/pine-broker-rust-tests.log`。
- Python 全套日志：`target/pine-broker-python-tests.log`。
- 最终 Pine 专项日志：`target/pine-broker-pine-tests.log`。
- Clippy：`target/pine-broker-clippy.log`；构建：`target/pine-broker-build.log`。
- 真实 HTTP 回执与服务日志：`target/pine-http/`。

## 能力边界

这组证据来自本地 MarketForge 实际撮合器，不是外部交易所实盘。
Pine 按 bar-close 和固定整数数量执行，history_limit 默认 512、最大 2048。
不支持用户指定的每 tick/成交后重算；两腿不保证原子成交；资源与历史越界会明确停止。
原生持仓保护按账户/品种/持仓侧保存，多个 exit ID 不构成独立 OCA 括号。
现货 stop 和持仓保护仍受核心人工交易接口的能力限制。
复杂 TradingView 脚本、无限期运行和大规模性能需要各自的资格验证。

## 加载

独立可执行文件：`target/pine-validation/debug/exchange-server.exe`。
将 `.venv/Scripts` 加入 PATH，设置 `MARKETFORGE_BOT_PLUGIN_DIR` 为仓库 `bot-plugins`。
重启目标服务后，`GET /bots` 应显示 `pine.strategy@2.0.0`，新实例默认 broker。
具体实例和双腿配置见 `docs/PINE_BOTS.md`。当前用户运行中的服务未被替换或重启。
