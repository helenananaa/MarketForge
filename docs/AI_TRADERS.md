# AI 交易员插件（agent.v1）

行情历史、MCP 原生图像、合约止盈止损、风险事件与 Python 策略数据接口见 [统一交易能力](TRADING_CAPABILITIES.md)。

实现入口：`python/marketforge/agents/`；随附插件：`agent-plugins/llm-trader/`。
默认服务已改为外部框架的工具服务，接入 Codex / OpenCode 请先看 [AGENT_FRAMEWORKS.md](AGENT_FRAMEWORKS.md)。本文的模型接入循环作为显式 `--enable-legacy-model-loop` 兼容模式保留；行情、策略、价格保护和警报规则共用。
Web 顶部的“AI 交易员 · 模型与自编策略”可配置模型、创建选手、启动/暂停、查看策略和导出执行记录。
现有 `bot.v1` 和 Rust 撮合接口保持兼容。`agent.v1` 是独立的本地运营服务，不在撮合锁内等待模型。

## 启动

PowerShell（仓库根目录，三个终端）：

```powershell
# 1. 交易所；已有实例时复用，不重复启动。
$env:MARKETFORGE_BIND_ADDR='127.0.0.1:57305'
cargo run -p exchange-server

# 2. 交易员运行服务
$env:PYTHONPATH='python'
.venv/Scripts/python.exe -m marketforge.agents --enable-legacy-model-loop

# 3. Web
npm.cmd --prefix marketforge-web run dev
```

首次启动生成 `.local/agents/operator.token`。在 UI 中填入该令牌，连接本机 57306 服务。
运行服务使用固定的 `--exchange-url`（默认 57305）；改变交易页面 API 地址不会改变它，界面会显示实际交易所地址。
服务只绑定 loopback，所有 API 都要求令牌，浏览器 Origin 默认仅允许本机 57304。需要其他本地 Web 端口时用 `--origin`。
不要将此服务通过无鉴权代理暴露到网络。

插件的模型服务支持兼容 Chat Completions 的 `tools`/`tool_calls` 协议。地址填包含 `/v1` 的基础 URL；不要填完整 `/chat/completions`。
输入模型名和 API Key，点击“测试并连接模型”（会产生一次真实请求）。模型密钥仅保存在服务内存，不写入 SQLite、代码或导出记录；服务重启后必须重新连接。
同一个连接可以供多个选手使用。更换连接前需暂停使用它的交易员。
外部模型地址要求 HTTPS；loopback 推理服务允许 HTTP；禁止跟随重定向传递密钥。

## 准备房间与资金

交易员不能给自己充值、创建账户、改时间或给自己授权。先用运营命令建立一个有限资金、有限种子挂单的双市场示例：

```powershell
.venv/Scripts/python.exe scripts/agent_arena.py --room ai-arena --traders trader-1 trader-2
```

此命令创建 `V-USD-SPOT` 和 `V-USD-PERP`，选手账户从 20 开始，默认现金配置 100000；账户 10 提供有限初始流动性。
它同时授权 `agent-trader-1`、`agent-trader-2` 各自的账户，并启动市场时钟。
在 Web 房间输入 `ai-arena`，点击“载入”；配置选手时填对应账户 ID 和两个品种。
已有房间不要再次执行创建命令，使用原有成员/账户分配 API 即可。

Bearer 模式下，运营脚本使用 `MARKETFORGE_AGENT_ADMIN_TOKEN`；每个选手需要自己的交易所 token。
例如交易所 token 映射的用户为 `agent-trader-1`，在运行服务环境中设置 `MARKETFORGE_TRADER_TOKEN_A`，UI 的身份环境变量填该名称。
该 token 不传入模型或生成代码。未配置 token 时使用现有本地开发模式，身份固定为 `agent-<选手名称>`。
同一运行服务禁止两个 AI 选手占用同一房间账户。交易所依然是资金和权限的最终裁决者。

此示例不是完成的比赛系统：没有自动现货/合约指数联动、自动资金费率、自选初始资产或大规模背景 bot 配方。可继续通过现有 bot 配置入口添加背景交易者。

## 自编策略隔离环境

外部框架现可使用 [持久 Docker 工作区](AGENT_WORKSPACES.md)：容器内 Python、Shell、联网和后台进程，文件持久化，调用自己账户的 API。下文是保留兼容的短周期 decide 容器模式。

```powershell
docker build -t marketforge-strategy:1 scripts/agent-sandbox
```

如果 Windows Docker 不可用，但 WSL 的 Docker 已运行：

```powershell
wsl -d Ubuntu-22.04 -- docker build -t marketforge-strategy:1 /mnt/e/projects/MarketForge/scripts/agent-sandbox
$env:MARKETFORGE_AGENT_DOCKER_WSL='Ubuntu-22.04'
$env:PYTHONPATH='python'
.venv/Scripts/python.exe -m marketforge.agents
```

策略在一次性容器内执行：无宿主目录挂载、无凭据、系统文件只读、非 root、移除 capabilities，提供限额的 `/work` 临时工作区，限制 CPU、内存、进程数、输出大小和墙钟时间。
现在支持多文件工程、PyPI 依赖、HTTPS/Git HTTPS 直接引用和 Debian 系统包。需要库时由模型声明并安装，不采用固定库白名单。
依赖安装使用单独的联网构建容器（1 GiB 内存、2 CPU、128 进程、300 秒期限），可运行 pip 的源码构建和 apt 包安装。它不挂载宿主目录，不持有交易所或模型凭据。
安装完成后保存不可变 Docker image ID、`pip freeze`、pip 安装报告及系统包实际版本，后续策略复用该环境，不在每个 tick 重装。
交易代码容器使用这个环境，仍不直接联网；LLM 通过 `web_search`/`web_read` 获取外部数据，可将需要的数据写入项目文件再供策略或分析使用。此限制只适用于旧 decide 容器；持久工作区允许程序直接联网。
镜像不可用时测试/部署明确失败，绝不改用宿主 Python 执行。第三方包的构建脚本具有构建容器内部权限；构建容器的默认 bridge 出网不等同于公开网页工具的逐目标 IP 校验。
该容器适用于本机实验，并不宣称达到了面向不可信租户的托管服务隔离认证。

## 工具与策略协议

LLM 通过插件发起多轮工具调用：

| 工具 | 行为 |
|---|---|
| `market_read` | 公开 ticker/K 线、自己的账户/挂单/近期公开成交；按允许品种查询 |
| `trade` | 限价、市价/IOC、post-only、reduce-only、撤单；默认价格保护，可显式选择不限价市价执行；可声明决策和挂单有效期；固定当前账户 |
| `web_search` / `web_read` | 公网搜索、网页/JSON/RSS/新闻资料读取；返回来源 URL、获取时间和摘要哈希 |
| `strategy_save` | 保存多文件工程、入口与依赖清单的 SHA-256 版本；运行中版本必须先停止 |
| `strategy_read` / `strategy_patch` | 读取历史版本，修改/新增/删除指定文件，修改依赖并生成新版本 |
| `strategy_install` / `strategy_status` | 后台安装依赖、查询日志/失败原因与实际安装版本 |
| `strategy_analyze` | 在项目环境中运行 Python 分析，读取当前观察和项目数据，返回 stdout 与 JSON result，不下单 |
| `strategy_test` | 容器中试运行并检查动作，无交易副作用 |
| `strategy_start` | 仅启动已测试的当前版本 |
| `strategy_stop` | 停止策略，可同时撤销该策略挂单；保留取消失败的回执 |
| `strategies` | 查询自己的策略、版本、运行状态和上次状态 |
| `note` | 保存跨决策轮次的私人笔记 |
| `announce` | 发布给观众的短声明 |
| `wait` | 2–300 秒后唤醒，可选择在自己的账户/挂单变化时提前唤醒 |
| `alert_set` / `alerts` / `alert_cancel` | AI 自设、查询、取消条件警报；触发后插入新信息，中断当前决策并重新评估旧计划 |

单个选手最多 4 个策略。每笔数量、每分钟请求额度由父选手统一限制，模型直单和子策略共同消耗。创建更多子策略不能增加资金或额度。
市场快照包含其自己的 step/time；多品种查询不是原子快照，跨市场订单也不是原子组合。
每个交易员只有一轮有效决策在运行；子策略独立周期运行，不等待模型响应。中断后的旧请求可能仍在传输，但其回复不能执行工具；每位选手最多保留两个未结束的模型/慢工具任务，满额时可中断地等待空位，不无限创建线程。
模型一轮最多 8 次请求、每次最多 16 个工具调用、回复上限 2048 tokens；另有选手总请求次数预算。该预算不是货币计费上限，实际费用取决于服务商。
安装任务不占交易锁，已有策略可继续运行；网页查询和代码测试也不持有交易锁。安装结束会提前唤醒正在等待的交易员。暂停交易员会取消其安装任务，迟到的结果不会启用。
Web 的策略卡显示安装状态，点击“查看工程与安装日志”可查看文件、实际依赖版本与构建输出。

## 自设警报与决策中断

AI 可主动调用 `alert_set`，自行决定名称、阈值、单次/重复以及是否暂停策略。默认无警报、单次触发、保留策略部署。每位选手最多 32 个命名警报，每个包含 1–8 个条件，`match="all"` 表示全部满足，`"any"` 表示任意满足；同名设置替换旧规则。

```json
{"name":"price-breakout","conditions":[{"instrument":"V-USD-SPOT","metric":"best_ask","op":"gte","value":120}],"reason":"突破后重新评估原来的买入计划","repeat":true,"cooldown_seconds":5,"pause_strategies":false}
```

指标包括 `best_bid`、`best_ask`、`spread`、`last_price`、`bid_qty`、`ask_qty`、`cash_balance`、`available_cash`、`position_qty`、`equity`、`own_order_count`、`market_time_ms`；比较符为 `gt/gte/lt/lte/eq/ne`。只能监控允许品种及自身账户。价格采用 tick，资金采用交易所原生单位，时间阈值采用模拟市场毫秒。现货权益按最近成交价计价，没有成交时使用买一；无可靠价格则未知。空订单簿、无成交、缺失账户字段及非运行市场不会按零值匹配，也不会凭未知数据重新布防。

独立监控线程每 250 ms 轮询一次，不等待 LLM 或策略计算。实际延迟还包括行情 HTTP 读取和正在提交的交易动作；这不是逐成交回调，两个采样之间短暂穿越阈值可能被漏掉。规则设置时已经满足，会在第一次检查触发。重复规则需先明确退出条件，且距上次触发经过 2–3600 秒墙钟冷却（默认 5 秒），避免持续满足时反复打断；同名未确认触发合并为最新证据。

触发后，运行服务停止使用旧模型请求的回复，暂缓旧轮次尚未发送的动作；将触发条件、实际值、step/市场时间、最新行情与 `interrupted_plan` 交给下一轮。原来的目标、笔记、简短计划、最近公开对话及待执行动作保留，AI 可以继续原方案、调整或放弃，不强制推翻目标。保留的参数预览超过 4000 字符时标记截断，完整原始请求仍在回执中。策略的状态/代码保留，待执行动作以 `held_actions` 及原调用 ID/回执供核对；这些提案不会自动重放。默认保持部署，策略在新信息交给 AI 后可按最新行情重新计算；`pause_strategies=true` 则需 AI 显式 `strategy_start` 恢复。

已经提交的订单与成交不会回滚，已有挂单不会自动撤销；未知下单结果保留原幂等键，重新决策前先核对结果，避免重复下单。随附 HTTP 插件在中断时尝试断开本地连接；底层读取和不支持取消的旧插件可能仍等待，服务商是否停止推理/计费无法保证。迟到回复始终不能触发交易。警报无匹配时不消耗模型请求，重新决策仍受原有总预算限制。

警报规则、触发计数与未处理上下文持久化；暂停交易员会暂停监控，重启服务不会自行启动交易，显式恢复后继续检查。触发事件、监控失败/恢复都写入执行记录；读取失败期间不能保证检测到阈值。Web 展示 AI 自设的警报、条件、状态及触发次数；`GET /traders/{id}/alerts` 提供同样的只读列表。

## 延迟决策与订单保护

LLM 直单和 Python 策略使用相同的 `execution_mode`：省略或指定 `"bounded"` 时，新订单必须提供 `price_tick`（买单最高可接受成交价，卖单最低可接受成交价）。此模式的 `market` 和 `reduce_only` 转换为带该价格的 IOC，允许部分成交，未成交部分立即过期。

显式指定 `execution_mode="unbounded"` 时，`market` 和 `reduce_only` 可以按实际订单簿不限价扫盘，必须省略 `price_tick`。此模式不能用于 `limit`、`post_only` 或 `ioc`，不能同时声明价格边界，未成交余量不挂单。缺少价格且未明确选择不限价模式仍会报错，不会自动关闭保护。交易所原有普通市价接口保持兼容。

价格保护属于策略选择；账户权限、资金/持仓/保证金、单笔数量、共享订单额度、市场规则及 reduce-only 禁止反向开仓仍是强制规则。不限价执行不保证全额成交或市场价格变动幅度。

两个可选字段使用行情快照的**市场时间**（`market_time_ms`），不是 Unix 时间或宿主机时间：

| 字段 | 含义 |
|---|---|
| `valid_until_market_time_ms` | 决策提交的绝对截止时间。交易所在处理订单时检查，当前时间大于或等于截止时间则拒绝；不会自动延长或重新下单 |
| `expires_at_market_time_ms` | 仅限 `limit`/`post_only` 的挂单绝对到期时间。在第一个到达或越过期限的市场时钟 step，移除剩余挂单、释放冻结现金/持仓/保证金，生成 `OrderExpired` 回执 |

例：观察时间为 2000 ms，最多在接下来 10 秒内以 101 买入 2 手：

```json
{"instrument":"V-USD-SPOT","action":"market","side":"Buy","price_tick":101,"qty":2,"valid_until_market_time_ms":12000}
```

短期挂单可使用 `action="limit"` 并额外指定 `expires_at_market_time_ms=32000`。提交有效期只决定是否接受新订单，不会撤销已经接受的挂单；挂单期限独立生效。长期挂单可以省略时间字段，不统一套用短期过期规则。

不限价扫盘示例（买入 2 手；卖出使用 `side="Sell"`）：

```json
{"instrument":"V-USD-SPOT","action":"market","side":"Buy","qty":2,"execution_mode":"unbounded"}
```

不限价模式仍可独立设置 `valid_until_market_time_ms`，交易所在处理订单时检查该期限；省略则没有决策到期限制。`reduce_only` 使用相同模式选择，但始终只能减少当前合约仓位。

期限、价格边界及到期执行进入交易所持久化与恢复链路。相同 idempotency key 的重试返回原始回执，即使期限已经过去；请求结果不确定时不得改期限或换 key 重发。收到拒绝或部分成交后，先核对回执、最新行情与仓位，再决定是否创建新的交易意图。历史订单的 `remaining_qty` 保留过期时未成交数量，`status="expired"` 表示它已不在实时订单簿中。

升级前编写的无价格边界 `market`/`reduce_only` 策略需补充 `price_tick`，或对新的交易意图显式声明 `execution_mode="unbounded"`。旧版遗留的无价格边界 pending 请求会保留并阻止恢复，需要先核对交易所回执；不会自动把未确定结果的旧订单改价或改执行模式重发，也不会标成已完成。新版显式不限价请求可按原参数和原 key 恢复。

这保护执行价格和意图寿命，不保证策略收益，也不提供跨品种原子成交、自动止损或强制平仓保证。减仓价格边界可能导致无法成交，模型应检查回执后处理。

## 多文件和依赖示例

模型可以通过 `strategy_save` 提交下面的项目，然后调用 `strategy_install`；看到 `strategy_status.installation.status=ready` 后测试、启动：

```json
{
  "name": "researcher",
  "interval_seconds": 5,
  "entrypoint": "strategy.py",
  "files": {
    "strategy.py": "from signals import mean\ndef decide(observations, state):\n    return {'actions': [], 'state': {'mean': mean()}}",
    "signals.py": "import numpy as np\nimport pandas as pd\ndef mean(): return float(pd.Series(np.array([1, 2, 3])).mean())"
  },
  "requirements": ["numpy==2.1.3", "pandas==2.2.3"],
  "system_packages": ["libgomp1"]
}
```

遇到 `ModuleNotFoundError` 可以修改 requirements；遇到缺编译器/原生头文件可声明 `build-essential`、所需的 `*-dev` 系统包，再安装重试。包本身不存在、版本冲突、源站不可达或超出资源限额会给出失败日志，不会静默降级。
工程最多 64 个文本文件、合计 1 MiB；相对目录和项目内导入均支持。`strategy_patch.files` 中值为 `null` 表示删除文件。文件路径不能逃出自己的项目。
分析脚本可直接 `import` 项目文件和已安装的库，打印诊断，并把结构化结果赋给变量 `result`。工作区内临时文件只在一次执行内存在；跨轮次数据通过版本化文件和策略 state 保存。
环境镜像保留供复用和恢复，不自动删除历史镜像；服务重启把未完成的安装标记为 interrupted，模型可以显式重试。
只修改代码或数据文件、依赖声明不变时，新版本复用原有安装环境。环境丢失或损坏时，可调用 `strategy_install(name=..., force=true)` 重建；运行中的策略应先停止。

## 联网研究

`web_search` 默认使用公开 RSS 搜索接口，可通过 `MARKETFORGE_AGENT_SEARCH_URL` 配置含 `{query}` 的 RSS 或 SearxNG JSON 搜索 URL。
`web_read` 支持公开 HTML、文本、JSON 和 RSS，不执行网页 JavaScript，不绕过登录/付费访问限制。
直接请求对 DNS 结果和每次重定向验证公网 IP，并连接已验证的 IP；拒绝本机、内网、云元数据地址、URL 内凭据和非标准 Web 端口。网页正文最多返回 24000 字符，响应最大 1 MiB。
外部资料按实际获取时间标注；虚拟市场的价格与真实新闻没有预设因果关系。外部文本始终作为数据，不作为修改交易规则的指令。

策略示例：

```python
def decide(observations, state):
    instrument = "V-USD-SPOT"
    asks = observations[instrument]["book"]["asks"]
    if state.get("submitted") or not asks:
        return {"actions": [], "state": state}
    return {
        "actions": [{"instrument": instrument, "action": "ioc", "side": "Buy",
                     "price_tick": asks[0]["price_tick"], "qty": 1}],
        "state": {"submitted": True},
        "summary": "Submit one small buy; submission does not guarantee a fill."
    }
```

每次最多 8 个动作，状态最多 64 KiB。策略不得依靠进程内存跨周期保存状态。
模型生成的程序从不装入可信插件目录。它只能返回受校验的订单动作，不能调用交易所管理接口。
策略只能撤自己的订单；父交易员可撤本账户订单。暂停父交易员会停止后续模型动作及子策略调度，保留已有挂单。
暂停时已进入受控提交的操作可能完成；返回较晚的模型响应会被记录但不执行。正在停止的请求结束前不能重复启动。

## 持久化、恢复与视频素材

SQLite 保存选手配置、笔记、代码版本、策略状态、模型实际输入/输出、工具回执和成交引用，默认目录 `.local/agents/` 已忽略 Git。
每次下单先写入持久化请求标识，再以相同 idempotency key 发给交易所。响应丢失会保留 pending；恢复先重试同键请求，再开始新决策。
自编策略先保存本周期输出，再提交动作；崩溃恢复不重新运行代码来猜测旧动作。
交易所跨重启去重仍依赖其持久化配置；内存 journal 不承诺跨交易所进程重启。
服务重启默认暂停全部选手，不自动花费模型额度或继续交易；重新连接模型并显式恢复。
一个数据目录只允许一个服务进程持有锁；不同数据目录/其他客户端的账户配置仍由运营者负责隔离。

Web 可分页查看最新事件、导出截至点击时的完整 JSON。每条模型请求带 round/step/plugin/model，工具请求带 call ID 和来源策略，策略执行带代码版本，交易所响应带 command_seq/events。
无需重新调用模型即可检查当时的输入和输出。本期提供记录导出，不包含可拖动的观战播放器或视频渲染器。公开交易声明与原始回执分开；不是隐藏思维链展示。

## 安装其他交易员插件

运营者在 `agent-plugins/<包名>/agent.json` 声明 `id/name/version/protocol_version=agent.v1/entrypoint`。
入口导出 `complete(connection, messages, tools) -> (assistant_message, usage)`；随附的 LLM 插件是参考实现。
安装的 Python 插件属于可信运营代码，不是模型生成代码；新增后重启服务。模型品牌适配和提示词会话在该层扩展，撮合核心不依赖供应商。

## 验证

```powershell
$env:PYTHONPATH='python'
cargo build -p exchange-server
$env:MARKETFORGE_AGENT_DOCKER_WSL='Ubuntu-22.04' # 仅需要 WSL Docker 时
$env:MARKETFORGE_AGENT_SANDBOX_TEST='1'
$env:MARKETFORGE_AGENT_LIVE_TEST='1'
$env:MARKETFORGE_AGENT_PROJECT_TEST='1'
.venv/Scripts/python.exe -m unittest python.tests.test_agent_runtime python.tests.test_agent_projects python.tests.test_agent_live -v
npm.cmd --prefix marketforge-web run build
```

`test_agent_live` 使用真实交易所 HTTP、真实模型协议 HTTP 和真实 Docker，但模型返回的是明确标注的可控夹具。
它证明插件与交易执行链路可用，不证明真实 LLM 的策略质量或视频观赏性。真实服务商验收需配置用户选定的模型与凭据。
