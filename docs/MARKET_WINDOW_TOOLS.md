# 窗口统计、组合提醒与订单流

交易所 HTTP API、选手业务 HTTP API、MCP 和 Python SDK 共用交易所日志统计。新增三个工具：`window_stats`、`order_flow`、`combined_alert_set`。这次没有新增画线接口。

## 窗口统计与订单流

原生读取接口（沿用房间读取权限）：

```http
GET /rooms/{room}/instruments/{instrument}/window-stats?window_ms=30000
GET /rooms/{room}/instruments/{instrument}/order-flow?window_ms=30000&bucket_ms=1000&price_group_tick=10
```

参数：`window_ms` 默认 30000，范围 1–86400000；`end_market_time_ms` 可查询历史窗口，默认当前仿真时间，不能超过当前时间；`scan_limit` 默认 20000，范围 1–100000；`book_levels` 默认 10，范围 1–100；`large_trade_min_notional` 默认 `"1000000"`，按单笔实际成交额筛选大额成交。订单流另支持 `bucket_ms`（最多 512 桶）和正整数 `price_group_tick`（价格分组宽度，最多 512 组）。

窗口为 `[max(0,end-window_ms),end]`，两端包含，使用仿真时间。交易日志固定到返回的 `through_command_seq`；当前盘口也来自同一个已提交房间快照。PostgreSQL 使用只读、可重复读事务和已有时间索引查询窗口，不通过最新 500 条日志近似累计。

`metrics` 包含：

- 成交笔数、数量、成交额、主动买/卖数量与成交额、`delta_qty` / `delta_quote`、精确有理数 `vwap_tick`、窗口首末成交价和 `price_change_ppm`。
- 实际爆仓成交笔数、数量与成交额；`long_liquidation_notional` 是强制 Sell 成交额，`short_liquidation_notional` 是强制 Buy 成交额。爆仓交易计入全部成交一次，同时单独分类；失败尝试和没有成交的挂单不计入。
- 大额实际成交笔数、成交额。
- 资金费结算回执数、成功/跳过次数、实际 `funding_total_transfer` 和最后一份回执的有符号费率 `last_funding_rate_ppm`。价格快照中的资金费状态不重复计作结算。
- 当前前 N 档 `book_bid_qty`、`book_ask_qty` 和 `book_imbalance_ppm=(bid_qty-ask_qty)/(bid_qty+ask_qty)*1000000`。空盘口失衡为 null。

金额为原生 quote 单位，数量为原生 lot，宽整数用十进制字符串传递；VWAP 使用 `{"numerator":"...","denominator":"..."}`，不丢失金额精度。首末价格和价格变化基于窗口内实际成交，不代替标记价或指数价。

`order_flow` 还返回：

- `buckets`：按仿真时间分桶的买卖量、Delta、`cvd_qty`。CVD 从本窗口边界的零开始，不是全历史累计；没有成交的桶省略。
- `volume_profile`：按价格组汇总成交量、成交额和主动方向；每组 `price_from_tick` 是价格左边界，宽度为请求的 `price_group_tick`。超过 512 组时 `profile_truncated=true`；该标志只影响价格分布，完整总量仍按所有扫描成交计算。
- `book`：当前前 N 档的数量/金额、最佳买卖价及快照时刻。历史窗口返回 `book=null`，不提供历史盘口指标。

`complete=false` 表示扫描上限截断，或已提交历史中存在无法确定时间的记录。部分总量可以供诊断，但组合提醒不会用它触发或重新布防。`scan_limit` 限制房间命令数量，不是匹配品种的成交数量；交易所市场回放、所有市场命令都参与窗口完整性判断。

## 组合提醒

`combined_alert_set` 与现有状态/事件提醒共用 32 个命名规则额度，支持替换、`alerts` 查询和 `alert_cancel` 取消。规则持久保存，服务重启后选手明确恢复运行才继续监控。

每个规则包含 `left`、`op` 和 `right`。左值来自以下来源；右值可以是另一个来源，也可以是数字或精确十进制字符串：

- `window`：上述 `metrics`；可指定 `window_ms`、`scan_limit`、`book_levels` 和 `large_trade_min_notional`。
- `state`：现有状态提醒指标，包含价格、盘口、本账户权益/仓位/保证金、资金费预估等。
- `indicator`：`sma`、`ema`、`rsi`、`atr`、`vwap`，指定 `period`（默认 14）、`interval_ms`（默认 1000）、`limit`（默认 500）。沿用 `market_indicators` 的窗口内计算与浮点指标语义；未预热完成返回未知。

下面是“30 秒累计多头爆仓成交额达到 600，同时标记价低于 80”的规则。600 和 80 都是原生单位示例：

```json
{
  "name": "long-liquidations-under-support",
  "rules": [
    {"left": {"source": "window", "instrument": "V-BTC-PERP", "metric": "long_liquidation_notional", "window_ms": 30000}, "op": "gte", "right": "600"},
    {"left": {"source": "state", "instrument": "V-BTC-PERP", "metric": "mark_price"}, "op": "lt", "right": 80}
  ],
  "match": "all",
  "priority": "urgent",
  "repeat": false,
  "evaluation_interval_seconds": 1
}
```

也支持价格与移动均线直接比较：

```json
{"left":{"source":"state","instrument":"V-BTC-PERP","metric":"last_price"},"op":"lt","right":{"source":"indicator","instrument":"V-BTC-PERP","metric":"sma","period":20,"interval_ms":1000,"limit":500}}
```

`match=all/any`，每个提醒 1–8 个比较，`op=gt/gte/lt/lte/eq/ne`。所有来源先通过完整性与时间检查；即便 `any` 中部分条件为真，只要其他输入未知，也暂不触发。来源时间默认必须相等；跨多个读取并不原子，持续推进的仿真可能需要显式设置 `max_skew_market_time_ms`（0–60000），允许最多指定的时间差。不同房间不可混合。

组合提醒是采样检查：`evaluation_interval_seconds` 为 1–300 个墙钟秒，另加网络读取延迟。短于采样间隔的穿越可能被遗漏；需要每个新事件的提醒时使用 `event_alert_set`。相同查询在一次检查中复用，网络读取不持有交易锁。

支持现有 `sustain_seconds`、`hysteresis`、`cooldown_seconds`、`interrupt_min_interval_seconds`、`priority`、`pause_strategies` 和 `reason`。持续条件、冷却与中断间隔都用墙钟时间。默认一次性；重复提醒需要条件明确变为假并越过回差，再次满足且冷却结束。缺失、不完整或时间错位的数据打断持续计时，不能重新布防；网络失败记录在规则 `evaluation_error` 中，不阻断其他组合规则的检查。

`urgent` 保存比较证据并使旧决策失效、唤醒重新评估；`normal` 排队至下一轮决策，不能暂停策略。普通统计查询不需要决策凭证；注册/取消仍需当前 `decision_id`、`generation` 和稳定 `request_id`。已取消/替换的规则不接受旧网络读取结果。

## MCP、业务 HTTP 与 SDK

MCP 直接调用上述工具。选手 HTTP API（绑定本选手令牌）使用 `POST /tools/{trader}/call`：

```json
{"name":"order_flow","arguments":{"instrument":"V-BTC-PERP","window_ms":30000,"bucket_ms":1000}}
```

注册正文使用 `{"name":"combined_alert_set","arguments":{...规则与决策凭证...}}`。提醒证据仍通过 context、事件流及现有连接器交付，全部来源品种必须属于本选手允许列表。

原生 Python SDK：

```python
stats = client.window_stats("room", "V-BTC-PERP", window_ms=30000)
flow = client.order_flow("room", "V-BTC-PERP", window_ms=30000, bucket_ms=1000)
```

Docker 的 `marketforge_program.Client` 提供 `window_stats(instrument, **query)`、`order_flow(instrument, **query)`，经同一绑定账户的业务 HTTP API 读取。

真实无模型验证见 [窗口工具验收](validation/2026-10-11-agent-window-tools.md)。
