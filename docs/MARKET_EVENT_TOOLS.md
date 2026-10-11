# LLM 市场事件查询和提醒

MCP、业务 HTTP API 和 Python 共用同一套交易所数据。`market_events` 读取全市场的公共事件；`risk_events` / `account_history` 保留本账户的资金费收付、保证金、强平、ADL 和损失分担记录。公共接口不会返回其他账户的身份、订单或资金明细。

## 历史查询

交易所原生接口：

```text
GET /rooms/{room}/instruments/{instrument}/market-events
```

| 参数 | 语义 |
|---|---|
| `event_type` | `liquidation`、`funding`、`price_update`、`trade`；省略则全部 |
| `from_start=true` | 从历史起点扫描；省略游标默认尾页 |
| `after_command_seq` | 排他的持久日志游标；不能与 from_start=true 同时使用 |
| `limit` | 扫描房间命令数，1–500，默认 100；不是匹配事件数 |
| `start_market_time_ms` / `end_market_time_ms` | 含端点的仿真时间范围；仍需从起点或已知游标翻页 |
| `min_notional` | 仅 liquidation；单笔实际强平成交的最小金额，含阈值，十进制整数 |
| `side` | 仅 liquidation，强平订单方向 `Buy` / `Sell`；Sell 通常平多仓，Buy 通常平空仓 |
| `min_rate_ppm` / `max_rate_ppm` | 仅 funding；有符号结算费率的含端点范围 |

`LiquidationTrade` 来自实际撮合，每笔部分强平成交独立记录。未成交的强平尝试不计入。`notional` 是 `price_tick * qty` 的原生 quote 金额，十进制字符串保证大整数精度；不是自动换算的 USD，也不是整次强平尝试的累计金额。tick_size 是价格增量，不再乘一次。`FundingSettlement` 保留 settled/no_positions/skipped_prices/unbalanced_positions 回执，不能把跳过周期当作实际支付。`total_transfer` 同样是十进制字符串。

返回 `events: [{event_id, command_seq, market_time_ms, event}]`、`next_after_command_seq`、`latest_command_seq`、`has_more`。`event_id` 在本房间、品种的相同日志投影中稳定。过滤后为空的页也必须推进游标；`has_more=true` 继续请求。日志恢复后可沿用游标。房间权限和品种存在性由交易所检查。

MCP 示例：

```json
{"instrument":"V-BTC-PERP","event_type":"liquidation","from_start":true,"min_notional":"1000000","side":"Sell","limit":500}
```

工具名是 `market_events`。资金费历史使用 `event_type="funding"`，例如 `max_rate_ppm=-100` 查询费率不高于 -0.01% 的结算回执；1 ppm = 0.0001%。

```python
from marketforge import Client
client = Client("http://127.0.0.1:57305", bearer="YOUR_EXCHANGE_TOKEN")
for page in client.iter_market_events("room", "V-BTC-PERP",
        event_type="liquidation", min_notional="1000000"):
    for row in page["events"]:
        print(row)
```

Docker 工作区的 `marketforge_program.Client` 也有 `market_events(instrument, **query)` 和 `iter_market_events(instrument, **query)`；权限仍绑定本选手允许的品种。

## 按新事件设置提醒

`event_alert_set` 注册或替换同名规则，与 `alert_set` 共用 32 条规则额度、`alerts` 列表和 `alert_cancel`。注册时读取日志当前 head，从该位置之后的事件开始，不重播历史。提醒游标、触发证据和待交付通知一同持久化；恢复服务后选手需要明确恢复运行。

大额爆仓示例：

```json
{
  "name":"large-liquidation",
  "instrument":"V-BTC-PERP",
  "event_type":"liquidation",
  "min_notional":"1000000",
  "repeat":true,
  "cooldown_seconds":10,
  "priority":"urgent",
  "reason":"出现大额实际强平成交，重新评估仓位和盘口",
  "decision_id":"decision_begin 返回的凭证",
  "generation":0,
  "request_id":"large-liquidation-rule-001"
}
```

资金费结算提醒使用 `event_type="funding"`，可设置 `min_rate_ppm=500` 或 `max_rate_ppm=-500`；正负各用一条规则。不带费率过滤时会收到跳过结算的公共回执，证据内的 status 说明是否实际结算。

默认一次触发，repeat=true 持续消费新事件。每次轮询每条规则扫描至多 500 条命令，一页匹配事件合并为一个提醒，证据保留最多 20 行并附 `event_count` 和 `evidence_truncated`。冷却期间不推进游标，结束后按日志顺序消费积压，事件提醒不要求先出现“不匹配”采样。大量积压会延迟交付；它不是撮合线程同步推送。轮询周期 250 ms 加网络/读取延迟，持续运行时不会因为事件在两次采样之间发生而漏掉持久日志记录。

urgent 默认使旧决策失效、保留计划并唤醒；normal 只排队，在下一次决策交付，不中断当前工作。pause_strategies=true 只允许 urgent。已提交订单和已完成成交保留。主动唤醒 Codex/OpenCode 仍需 [会话连接器](AGENT_FRAMEWORKS.md)，stdio MCP 本身只响应调用。

## 业务 HTTP API

提醒注册和历史查询均可通过绑定选手的 Bearer 工具令牌调用：

```text
POST http://127.0.0.1:57306/tools/{trader}/call
Authorization: Bearer YOUR_SCOPED_TOOL_TOKEN
Content-Type: application/json
```

历史请求正文：

```json
{"name":"market_events","arguments":{"instrument":"V-BTC-PERP","event_type":"funding","from_start":true,"limit":500}}
```

提醒正文使用 `{"name":"event_alert_set","arguments":{...上面的规则...}}`。写操作须先 context、decision_begin，再带当前 decision_id/generation 和固定 request_id。查询不需要决策凭证。取消使用 `alert_cancel`；读取规则使用 `alerts`；提醒证据通过 `GET /tools/{trader}/events?after=<seq>&wait=1`、context 的 interrupts/notifications 或现有连接器交付。令牌不能访问其他选手或运营 API。

## 当前费率和账户指标

`market_read(kind="observe")` / context 的 `perp_price.funding` 包含预估费率、覆盖情况、下次结算时间和最近结算回执。状态型 `alert_set` 新增 `funding_rate_ppm`（预估有符号费率）、`next_funding_time_ms`、`index_price` 和 `funding_pnl`（本账户累计实际资金费现金流）。缺失估算/未启用资金费时不触发。状态阈值仍有采样限制；历史结算提醒使用 event_alert_set。
