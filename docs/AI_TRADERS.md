# AI 交易员插件（agent.v1）

实现入口：`python/marketforge/agents/`；随附插件：`agent-plugins/llm-trader/`。
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
.venv/Scripts/python.exe -m marketforge.agents

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
交易代码容器使用这个环境，仍不直接联网；LLM 通过 `web_search`/`web_read` 获取外部数据，可将需要的数据写入项目文件再供策略或分析使用。代码中的直接 `requests.get`/浏览器访问不在本版开放。
镜像不可用时测试/部署明确失败，绝不改用宿主 Python 执行。第三方包的构建脚本具有构建容器内部权限；构建容器的默认 bridge 出网不等同于公开网页工具的逐目标 IP 校验。
该容器适用于本机实验，并不宣称达到了面向不可信租户的托管服务隔离认证。

## 工具与策略协议

LLM 通过插件发起多轮工具调用：

| 工具 | 行为 |
|---|---|
| `market_read` | 公开 ticker/K 线、自己的账户/挂单/近期公开成交；按允许品种查询 |
| `trade` | 限价、市价、IOC、post-only、reduce-only 市价、撤单；固定当前账户 |
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

单个选手最多 4 个策略。每笔数量、每分钟请求额度由父选手统一限制，模型直单和子策略共同消耗。创建更多子策略不能增加资金或额度。
市场快照包含其自己的 step/time；多品种查询不是原子快照，跨市场订单也不是原子组合。
每个交易员只有一轮模型请求在运行；子策略独立周期运行，不等待模型响应。
模型一轮最多 8 次请求、每次最多 16 个工具调用、回复上限 2048 tokens；另有选手总请求次数预算。该预算不是货币计费上限，实际费用取决于服务商。
安装任务不占交易锁，已有策略可继续运行；网页查询和代码测试也不持有交易锁。安装结束会提前唤醒正在等待的交易员。暂停交易员会取消其安装任务，迟到的结果不会启用。
Web 的策略卡显示安装状态，点击“查看工程与安装日志”可查看文件、实际依赖版本与构建输出。

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
