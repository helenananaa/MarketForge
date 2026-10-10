# 外部 Agent 框架接入

行情历史、MCP 原生图像、合约止盈止损、风险事件与 Python 策略数据接口见 [统一交易能力](TRADING_CAPABILITIES.md)。

默认服务只提供业务能力：行情、账户、交易、策略隔离执行、警报监控、回执和唤醒事件。模型调用、会话历史、规划、上下文压缩和工具选择由 Codex / OpenCode 等框架管理。

`TradingService` 不包含模型循环；旧循环位于 `legacy.py`，仅在显式 `--enable-legacy-model-loop` 时启用。现有嵌入代码的 `Runtime` 保留兼容。既有 legacy 选手不会自动改成外部选手，避免改变账户执行方式。
迁移已有选手时先暂停，等待旧任务退出，再点击“保留账户，改用外部框架”，或以运营令牌 `POST /traders/{id}/backend` 提交 `{"backend":"external"}`。原账户、资金、挂单、策略、笔记和历史回执保留；未知订单仍按原身份核对。其他旧待执行操作由新决策明确重新评估，不在启动时自动重放。

## 启动业务服务

```powershell
$env:PYTHONPATH='python'
.venv/Scripts/python.exe -m pip install -e './python[agents]'
.venv/Scripts/python.exe -m marketforge.agents
```

市场和账户仍需预先建立，参见 [AI_TRADERS.md](AI_TRADERS.md)。Web 的 AI 交易员面板默认选择“外部框架 · MCP 工具”，无需输入模型名或密钥。创建选手、开始交易，点击“生成 / 轮换工具令牌”。框架的模型、提供商和认证在框架自身配置。

运营接口示例（运营令牌只供设置服务，不交给 Agent）：

```powershell
$operator = Get-Content .local/agents/operator.token -Raw
$headers = @{Authorization="Bearer $($operator.Trim())"}
$body = @{id='trader-1';backend='external';room='ai-arena';account_id=20;instruments=@('V-USD-SPOT','V-USD-PERP');prompt='自主研究交易，设置需要的警报';max_order_qty=100} | ConvertTo-Json
Invoke-RestMethod http://127.0.0.1:57306/traders -Method Post -Headers $headers -ContentType application/json -Body $body
Invoke-RestMethod http://127.0.0.1:57306/traders/trader-1/start -Method Post -Headers $headers -ContentType application/json -Body '{}'
$access = Invoke-RestMethod http://127.0.0.1:57306/traders/trader-1/access -Method Post -Headers $headers -ContentType application/json -Body '{}'
$env:MARKETFORGE_TOOL_TOKEN = $access.token
```

令牌绑定单个选手，服务仅存 SHA-256 摘要；轮换后旧令牌失效。它不能创建/充值账户、读取其他选手或调用运营接口。

外部选手的 `decision_lease_seconds` 默认 120 现实秒，可在创建时设置 10–600 秒。`decision_begin` 返回 `decision_expires_at`。到期使执行凭证失效并产生唤醒事件，原计划、挂单和成交保留；重新观测、重新决策后仍可继续原计划。这个现实时间有效期与订单的模拟市场时间截止条件分开计算。

## MCP 工具

MCP 使用官方 Python SDK，支持任意兼容 stdio 的框架。MCP 进程只是 HTTP 业务服务的适配器，不启动第二个策略调度器或模型循环。

```powershell
.venv/Scripts/python.exe -m marketforge.agents.mcp_server --trader trader-1
```

框架配置使用 Python 的绝对路径、`-m marketforge.agents.mcp_server --trader trader-1` 参数，以及 `PYTHONPATH=<仓库>/python`。工具凭据来自 `MARKETFORGE_TOOL_TOKEN` 或 `--token-file <本地凭据文件>`；不要把令牌作为命令行参数。单独配置 MCP 可手动调用工具，自动警报/等待唤醒还需要下面的会话连接器。

现有 41 个业务工具加三个会话边界工具，共 44 个。持久 Docker 编程见 [工作区](AGENT_WORKSPACES.md)，完整流水、指标和高级订单见 [交易能力](TRADING_CAPABILITIES.md)：

| 工具 | 用途 |
|---|---|
| `context` | 当前目标、市场/账户数据、警报、旧计划、策略、额度及 `generation`，不授予执行权限 |
| `decision_begin` | 使用读取到的 `generation` 开始新决策，核对未知订单结果，返回独立 `decision_id`；保留并交付警报上下文，由 Agent 决定继续、调整或放弃 |
| `receipt` | 按自己的 `request_id` 查询原请求是否完成及实际回执 |

下单、策略修改/启停、警报修改、笔记、公告及 `wait` 需要 `decision_id`、`generation` 和独立 `request_id`。只读工具不需要决策凭证。示例：

```json
{"instrument":"V-USD-SPOT","action":"market","side":"Buy","price_tick":101,"qty":2,"decision_id":"由decision_begin返回","generation":0,"request_id":"buy-0001"}
```

警报触发、暂停/恢复或新决策会使旧凭证失效。旧决策的新动作在业务接口中被拒绝，不依赖框架取消是否及时。已经完成的请求仍可用原 ID 获取回执；已预留且结果未知的下单只能用原 ID 和原参数核对/重试，新决策开始前也会核对这些请求。不能用同一个 ID 改数量、价格或执行模式。价格保护和可选期限沿用现有规则。

`wait` 保存业务唤醒时间并结束当前凭证，Agent 应结束框架 turn。后台策略继续执行，警报、账户变化、安装结束或等待到期通过事件接口唤醒同一会话。外部框架的模型预算/计费由框架管理，旧 `max_model_calls` 不约束外部模型；订单额度仍强制执行。

## Codex 会话连接器

先安装并认证 Codex CLI，配置自己要使用的模型。连接器复用其本机配置，通过 app-server 管理会话，使用独立工作目录，不更改全局 MCP 配置。

```powershell
.venv/Scripts/python.exe -m marketforge.agents.connectors --backend codex --trader trader-1 --state-file .local/agents/connections/trader-1-codex.json
```

首次创建原生线程，后续读取保存的 thread ID 并 `thread/resume`。思考中收到警报使用 `turn/steer`，空闲时使用 `turn/start`；暂停选手使用 `turn/interrupt`。正常轮次和上下文由 Codex 自身管理。连接器不调用 completion、不保存思维链，也不实现上下文压缩。无人值守连接器不处理交互批准/表单，应通过框架原生客户端检查需要人工处理的请求。

## OpenCode 会话连接器

每个选手使用独立的本机 OpenCode serve 实例/工作目录，先在 OpenCode 配置模型和认证。连接器注册本机 MCP，并保存 session ID；不要把多个交易账户的 MCP 凭据放进同一个共享框架实例。

```powershell
# 单独终端；端口可自选，在连接器里保持一致。
opencode serve --hostname 127.0.0.1 --port 4096
# 原业务终端已有 MARKETFORGE_TOOL_TOKEN。
.venv/Scripts/python.exe -m marketforge.agents.connectors --backend opencode --trader trader-1 --opencode-url http://127.0.0.1:4096 --state-file .local/agents/connections/trader-1-opencode.json
```

有 `OPENCODE_SERVER_PASSWORD` 时，连接器使用相同环境变量和可选 `OPENCODE_SERVER_USERNAME` 认证。活动会话收到警报后通过 `abort` 停止旧执行，再向**同一 session**发送 `prompt_async`，保留会话历史和目标；空闲时直接发送新信息。MCP 配置引用本机工具令牌文件，不把令牌正文放进框架配置。

## 事件、持久化与边界

账户工具 HTTP 接口：`GET /tools/{trader}/schema`、`POST /tools/{trader}/call`、`GET /tools/{trader}/events?after=<seq>&wait=1`。`wait` 支持最长 20 秒有界长轮询，事件 seq 可持久化和分页，`tail=1` 可读取最新记录。只有绑定账户的工具令牌能读取这些接口。

连接器的状态文件保存身份、框架 session ID 和事件游标，令牌另存本机 `.token` 文件。状态文件与工作目录绑定，不允许拿另一个选手的会话继续使用；文件锁防止同一状态文件被两个连接器同时操作。重启连接器会把最新业务上下文注入原会话，并合并待处理事件；交付是至少一次，遇到断线不能声称框架已经处理了消息，交易副作用依靠固定请求 ID/回执去重。

警报规则与策略仍是市场业务，需要常驻业务服务。轮询警报的采样限制沿用 [AI_TRADERS.md](AI_TRADERS.md)。暂停选手会关闭决策凭证、暂停监控/策略并通知连接器；挂单/成交不回滚。原生框架的文件/终端工具由其沙箱负责，账户令牌限制的是 MarketForge 接口权限，不代表对整个框架进程完成了对抗性隔离认证。

官方接入协议：[Codex MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)、[Codex app-server](https://learn.chatgpt.com/docs/app-server)、[OpenCode MCP](https://opencode.ai/docs/mcp-servers/)、[OpenCode server](https://opencode.ai/docs/server/)、[官方 MCP Python SDK](https://github.com/modelcontextprotocol/python-sdk)。

## 存活检测、恢复与状态面板

连接器每约 3 秒探测原生会话并报告心跳。15 秒内没有有效心跳即视为失联；业务接口在提交新动作前自行检查，不必等待监控线程发现。失联使旧决策失效，后台策略部署、原计划及交易回执保留。未知订单仍可用原请求身份核对。已绑定连接器的选手只允许当前 MCP 传输建立新决策，旧进程不能通过重新调用 `decision_begin` 获得新凭证。

连接器保存会话身份，传输故障按 1、2、4…最多 30 秒退避，在同一原生会话重连，默认最多 8 次重试。401/403 停止重试；连续模型失败到达本次运行的第二次即停止，避免无限触发模型请求。`--max-retries` 可设置 0–30，`--turn-timeout` 默认 300 现实秒，可设为 30–3600。原生会话故障恢复属于连接器职责，业务服务重启后仍须明确恢复选手；不会自动恢复资金风险敞口。连接器退出时会撤销决策凭证并尝试停止其活动轮次。

Codex 的 MarketForge MCP 工具在该会话内设置 `default_tools_approval_mode=approve`，用于已授权的自主虚拟交易，不改变其他服务器或全局审批配置。其他人工表单/批准仍不会自动接受。`--model` 可覆盖该交易员会话使用的 Codex 模型；默认遵循框架配置。如果框架配置中的模型不可用，应选框架实际提供的模型，不能仅凭账号已登录认为模型可用。

Web 面板选择交易员后显示业务状态、框架在线/思考/等待/重连、心跳、重试次数、决策有效期、待核对订单、警报、保留计划、最近工具和策略错误。`GET /traders/{id}/runtime` 供运营令牌读取，`GET /tools/{id}/runtime` 供该选手令牌读取。交付游标表示框架已接受事件，不能替代新决策或成交回执。市场时间与现实观测时间分开显示。

## 有限模型联调与无模型回放

```powershell
# 使用独立真实交易所进程和临时账户；会调用真实模型并消耗提供商额度。
# 默认仅在验证会话选用 Codex 公布的默认可用模型，低推理强度，不改全局设置。
.venv/Scripts/python.exe scripts/validate_agent_framework_live.py --output .local/validation/agent-framework-live
# 将已录制行情变化和模型工具意图在新的隔离交易所重放两次，不再调用模型。
.venv/Scripts/python.exe -m marketforge.agents.replay .local/validation/agent-framework-live/recording.json --runs 2 --report .local/validation/agent-framework-replay.json
```

录制保存初始场景、账户目标、按顺序发生的行情命令/市场时钟推进/警报检查及工具请求、回执。决策标识用别名关联，重放保留原请求 ID，验证观测、警报代际和成交结果；未观测到的后续决策不能提前引用。回放不执行模型、网络研究或策略代码，也不接入已有市场。它验证录制行为的可重复性，不表示模型每次都会选择同一策略。

联调是带工具响应门控的受控目标：设置警报、活动会话收取报价变化、重新决策、有限成交及等待。验证范围和失败记录见 [2026-10-08-agent-liveness.md](validation/2026-10-08-agent-liveness.md)。

## 订单管理、警报分级与可选预算

工具目录现有 27 项。新增 `orders`、`fills`、`order_cancel_all`、`policy_status`；`trade` 新增 `action=amend`。详见 [AGENT_POLICIES.md](AGENT_POLICIES.md)。

普通警报使用 `priority=normal`，排队并在下一轮合并交付，不撤销当前凭证；紧急警报默认 `priority=urgent`，立即撤销旧凭证，仍允许模型继续、修改或放弃保留的计划。`sustain_seconds`、`hysteresis`、`interrupt_min_interval_seconds` 可减少抖动和重复中断。

Web 交易员面板的“可选账户与模型预算”可设置各市场仓位、杠杆、启用基准权益亏损，以及外部连接器模型消息许可次数、累计 Token、估算费用上限。所有新增策略规则默认关闭，运营令牌才能修改，AI 只读。提供的 Codex/OpenCode 连接器在交付新消息前申请预算许可，并上报原生框架用量；模型内部调用仍由框架管理。用量返回前的单次推理可能超额，估算金额来自运营者填写的统一单价，不能当作供应商账单。单独接入 MCP 而未使用连接器时，MarketForge 无法拦截框架自身的模型调用。
