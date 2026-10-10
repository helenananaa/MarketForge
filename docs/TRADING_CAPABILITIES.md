# 人类、MCP 和 Python 交易能力

交易、保护单和风险结算都由 MarketForge 的同一交易所执行。MCP 不给予其他账户或运营接口权限。

| 能力 | 人的 CandleScope 仿真页面 | MCP | Python |
|---|---|---|---|
| 行情、盘口、公开成交、自己的账户和挂单 | 实时页面 | `context` / `market_read` | `Client.observe` |
| 历史 K 线 | 图表滚动和周期选择 | `market_history`，每页最多 2000 根 | `Client.candles` |
| 指标 | CandleScope 内置和 Pine/Pyne 指标 | `indicator_catalog` / `indicator_compute` 共用人的引擎；基础指标仍保留 | `Client.indicator_compute`，工作区 `api.call` |
| 图像 | 个人图表导出 | `chart_export` 返回原生 PNG，可指定 builtin 或 Pine/Pyne 指标线 | `Client.chart_export` 返回 PNG/base64 与来源信息 |
| 合约开仓附止盈止损 | 下单面板的开仓止盈/止损字段 | `trade(action="bracket")` | `Client.place_bracket`，策略同一 action |
| 已有合约仓位保护 | 持仓风险面板，设置、替换、取消 | `trade(action="protection")` | `Client.protect_position`，策略同一 action |
| 仓位和风险详情 | 均价、盈亏、资金费、保证金、风险比例、估算强平价、双向仓位 | 账户观察的 `own_account`、`risk`、`position_protections` | 相同观察字段 |
| 保证金预警和强平记录 | 当前状态与风险事件提醒 | 自动风险监控进入现有警报/唤醒通道；`risk_events` 查询历史 | `Client.risk_events`，也可配置风险指标警报 |

| 完整账户流水 | 页面以近期交易为主 | `account_history` / `ledger` 游标分页，成交、手续费、已实现盈亏、资金费和强平结算 | `iter_account_history` / `order_activity`；工作区 `iter_history` / `watch` |
| 老订单精确查询 | 原生 HTTP/API；页面仍为近期记录 | `orders(order_id="十进制编号")` 查询完整订单投影 | HTTP orders 的 order_id 参数；工作区同一工具 |
| 品种和账户元数据 | 页面显示盘口、账户与部分配置 | `market_rules` / `portfolio` | 同名 SDK 方法、工作区工具 |
| FOK、只减仓挂单 | 原生 API；页面尚未增加高级控件 | `fok` / `reduce_only_fok` / `reduce_only_limit` / `reduce_only_post_only` | `submit_action` 或工作区 `trade` |
| 条件开仓、追踪止损、分批止盈、限价退出 | 原生 API；页面尚未增加高级控件 | `conditional`、扩展 bracket/protection 字段、`conditional_orders` | `set_conditional`、扩展保护方法，工作区同一 action |
| 自由编程和长驻策略 | 使用自己部署的程序 | Docker 文件、Shell、Python、联网、依赖和后台进程工具 | `/work/marketforge_program.py` 绑定自己的账户 API |

外部框架 MCP 现有 **44 个工具**。持久 Docker 编程工作区详见 [Agent 工作区](AGENT_WORKSPACES.md)。图像是公共成交 K 线的独立无界面渲染，可叠加共享 CandleScope 引擎计算的指标线；不读取人的个人绘图、私有脚本或图表布局。基础 `market_indicators` 在返回窗口内计算，不宣称与 CandleScope 全历史指标逐点一致。EMA 以首根收盘价初始化，RSI/ATR 使用 Wilder 平滑，VWAP 使用成交额/成交量；预热不足返回 null。数据保留仿真时间和 K 线终结状态，空时间段不补造交易。

## 原生仓位保护语义

- 本版保护**合约仓位**，按品种、账户、`Both` / `Long` / `Short` 绑定。单向账户用 Both，双向账户显式选 Long 或 Short。
- `Mark` 为默认触发价，也支持 `Last`。Long 止盈大于当前触发价、止损小于当前触发价；Short 相反。至少设置一种，价格必须为有效 tick。
- 开仓与保护在同一交易所事务内提交；无效保护不先开仓。限价未成交时为 `awaiting_fill`，成交后为 `armed`。
- 默认触发后锁定退出意图，以 **不限价 reduce-only IOC** 减仓；设置 `exit_price_tick` 则提交原生只减仓限价单。未成交或部分成交时保持 `triggered`，后续市场命令或时钟步重试剩余仓位，并按交易所数量/名义金额上限分批。不能保证触发价成交；无流动性、市场暂停、交易时段和价格限制可能阻止成交。
- 触发时取消该保护关联的未成交入场余单。止盈和止损共享同一退出状态，仓位归零或反向后完成，不沿用到后续新仓位。强平同样结束旧保护。
- 已有仓位更新是**全量替换**该仓位保护；`protection: null` 取消。替换/取消也撤销旧保护仍在盘口的限价退出单，不修改已成交交易。
- 状态进入快照和命令日志；自动退出也进入同一执行日志，恢复时核对自动执行，不重复开仓/减仓。

高级保护字段：

- `trailing_distance_tick`：按选定的 Mark/Last 更新有利方向最高/最低价，反向移动达到距离时退出，状态随快照恢复。
- `exit_qty`：退出目标数量；缺省对当前仓位全部减仓。只减仓撮合逐笔按真实仓位裁剪，多个挂单不会累积反向开仓。
- `take_profit_steps: [{"price_tick":110,"qty":2},{"price_tick":120,"qty":1}]`：最多 16 个有序目标，当前档完成后重新武装下一档；与单一 take_profit_tick 互斥。止损/追踪止损可以抢占未完成止盈档，撤销其挂单并退出剩余仓位。
- `exit_price_tick`：各触发原因共用的退出限价，不保证成交。市价退出没有价格保证，限价退出没有成交保证。

`trade(action="conditional", conditional_key="entry-1", conditional_spec={...})` 注册原生合约条件入场，字段为 side、qty、trigger_price_tick、above，可选 position_side、Mark/Last、limit_price_tick、protection。满足条件后只提交一次，风险拒绝记录 rejected，不自动重试开仓。读取 `conditional_orders` 查询状态；spec 为 null 取消，替换/取消同时撤销已提交且仍挂着的入场子单。注册时不占用保证金，实际提交时重新执行交易所风控。

可选业务层账户策略不是交易所原生风控：启用它时禁止新注册原生条件入场，已有 armed 条件必须先取消；工作区程序在触发时调用 trade 会经过当前业务策略校验。普通原生风控一直生效。

HTTP 订单接口 `POST /rooms/{room}/instruments/{instrument}/orders` 使用原有身份认证和 `Idempotency-Key`：

```json
{"participant_id":"my-strategy","account_id":20,"action":{"PlaceBracket":{"side":"Buy","position_side":"Both","qty":5,"price_tick":100,"protection":{"take_profit_tick":110,"stop_loss_tick":90,"trigger":"Mark"}}}}
```

`price_tick` 为 null 表示不限价市价入场。已有仓位保护的 action 为 `SetPositionProtection`，含 `position_side` 和 `protection`；`protection: null` 取消。

MCP `trade` 的市价 bracket 必须明确设置 `execution_mode="unbounded"` 并省略 `price_tick`；有限价则是限价入场。写工具仍需当前 `decision_id`、`generation` 和稳定的 `request_id`。`accepted=true` 只表示命令受理，还需检查撮合/风险拒绝事件和真实账户状态。

## 风险数据和提醒

`risk.margin_buffer = equity - portfolio_maintenance_margin`，`margin_ratio_ppm` 是全账户维持保证金 / 权益 × 1,000,000，权益非正时为 null。`liquidation_price_estimate_tick` 是其他品种标记价不变时的估算；保证金舍入、资金费、费用、流动性和组合仓位会改变边界，不能当作确定的成交强平价。

```text
GET /rooms/{room}/instruments/{instrument}/risk-events?account_id=20&from_start=true&limit=100
```

返回本账户的保证金状态变化、强平结算和资金费事件，带 `command_seq` 和 `market_time_ms`。不包含损失分担的其他账户明细。省略 cursor 默认读尾页；下一页使用 `next_after_command_seq`，即使本页 events 为空也要前进，`has_more=true` 时继续取。limit 约束扫描的执行记录数（1–500）。身份授权仍在交易所检查。

业务服务在运行中的选手上轮询持久风险事件（250 ms 加网络读取延迟），保证金进入 `margin_call` / `liquidatable` 或发生强平时，中断旧决策凭证并记录 `alert_triggered`。已提交订单不会撤销；策略通过原有中断机制保留计划并重新评估。主动通知外部 LLM 需要已连接的会话连接器，单独运行 stdio MCP 并不会主动唤醒框架；服务暂停期间也不产生即时通知，历史可查询。首次连接读取尾页，完整历史需显式分页。

`alert_set` 还支持 `mark_price`、`maintenance_margin`、`margin_buffer`、`margin_ratio_ppm`、`liquidatable`（0/1）。阈值警报为抽样检查，原生保护则由交易所执行，两者时序不同。

## Python 和隔离策略

```python
from marketforge import Client
client = Client("http://127.0.0.1:57305", bearer="YOUR_EXCHANGE_TOKEN")
bars = client.candles("room", "V-BTC-PERP", interval_ms=1000, limit=500)
analysis = client.market_indicators("room", "V-BTC-PERP", period=14)
image = client.chart_export("room", "V-BTC-PERP")  # install marketforge[agents]
receipt = client.place_bracket("room", "V-BTC-PERP", 20, "Buy", 5,
    price_tick=100, take_profit_tick=110, stop_loss_tick=90, idempotency_key="entry-001")
client.protect_position("room", "V-BTC-PERP", 20,
    take_profit_tick=115, stop_loss_tick=95, idempotency_key="replace-001")
events = client.risk_events("room", "V-BTC-PERP", 20, from_start=True)
```

旧 `decide(observations,state)` 短周期策略容器继续不联网、无凭据。新的持久工作区开放容器内互联网和任意 Python/Shell，通过账户绑定的文件队列访问 API，无交易所密钥；详见 [完整用法](AGENT_WORKSPACES.md)。`strategy_save` 可添加：

```json
{"market_data":{"interval_ms":1000,"limit":500}}
```

服务向 `decide(observations, state)` 的每个品种观察注入 `analysis_data.candles`（带来源的 HTTP 结果）和 `analysis_data.indicators`。账户观察同时有风险和保护状态；返回 `bracket` / `protection` actions 由服务验证后提交交易所。`strategy_patch` 保留该数据设置。跨品种及账户/历史读取不原子，策略应核对各自时间戳。容器内部不直接调用带凭据的 HTTP API，不改用宿主 Python 执行。

## 验收复现

使用独立测试服务/数据库，执行：

```powershell
$env:PYTHONPATH='python'
.venv/Scripts/python.exe scripts/validate-trading-capabilities.py --base-url http://127.0.0.1:57315 --room capability-qa-new
```

脚本创建 QA 房间，验证真实 HTTP、官方 MCP stdio 会话原生图片、私有风险历史和强平唤醒，不调用模型提供商。结果在 `output/trading-capabilities/`，测试工具令牌在结束时删除；不要将它指向已有交易房间。服务/数据库重启恢复及人类页面验收见 [验证记录](validation/2026-10-10-trading-capabilities.md)。

使用现有 WSL Docker 环境进行真实策略验收时，设置 `MARKETFORGE_AGENT_DOCKER_WSL=Ubuntu-22.04`，并给脚本添加 `--strategy`。未提供该参数时，不启动生成策略。

## 仍与现实 API 量化不同的部分

- 这是虚拟市场。普通选手/脚本不能任意推进时钟、改价格、充值、修改规则或访问其他账户；这些是运营权限。没有真实交易所资金出入金或跨交易所原生路由。
- 完整流水可恢复和对账，工作区事件消费目前是游标轮询，没有专业低延迟 WebSocket、L2 增量序号/盘口重建、队列位置或高频延迟保证。
- 合约高级订单已补充；选手动态调整杠杆/保证金模式、现货原生条件单、冰山单、跨品种原子多腿单、组合 OCO 尚未提供。Python 可以协调多个动作，但不具备交易所原子语义。
- 共享指标支持内置/Pine/Pyne，需对应运行时。PNG 当前仅叠加指标线，不完整渲染脚本标注、填充、所有副图或人的私有布局；结构化结果保留原引擎输出。
- 工作区保留文件/任务记录；服务正常退出停止容器，恢复后选手默认暂停，需要明确 resume、恢复 workspace 并重新运行程序。脚本须保存状态/游标，使用稳定交易 ID。
- 自由研究和编程不代表策略已验证盈利；本轮没有调用模型提供商进行成熟框架真实模型回合验收。
