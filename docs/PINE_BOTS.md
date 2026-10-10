# Pine 背景交易者

`pine.strategy@1.0.0` 是一个通用 `bot.v1` 进程插件。安装到插件包的 Pine
脚本读取本房间的真实成交 K 线和自己的实际账户，输出买卖意图，由 MarketForge
统一撮合、风控和记账。它不使用 Pine 的模拟成交，也不直接调用交易 HTTP API。

第一版附带原创的 `ma.pine`、`rsi.pine`、`breakout.pine`，分别提供均线趋势、
RSI 均值回归和前期高低点突破行为。它们是背景订单流示例，没有盈利保证或实盘校准。

## 安装和启动

需要 Python 3.10+ 和 `pine-compat-runtime==0.3.1`。安装清单使用官方发布的
Windows AMD64 / Linux x86_64 ABI3 wheel，解释器仓库不需要依赖 MarketForge。

在仓库根目录的 PowerShell：

```powershell
.venv/Scripts/python.exe -m pip install -r bot-plugins/pine-strategy/requirements.txt
$env:PATH = "$PWD\.venv\Scripts;$env:PATH"
$env:MARKETFORGE_BOT_PLUGIN_DIR = "$PWD\bot-plugins"
cargo run -p exchange-server
```

Linux 可将安装命令改成 `python3 -m pip install -r ...`，设置同样的插件目录。
其他架构需要自行构建同版本 wheel；当前安装清单没有对应预编译包。
运行插件的 Python 必须安装该依赖：服务端直接调用清单中的 `python3`，只继承
必要的启动环境。Windows 的 `.venv/Scripts/python3.exe` 可通过上述 PATH 使用。

服务端启动时加载插件目录，`GET /bots` 应显示“Pine 策略交易者”。前端已有的
bot 参数表单即可选择该插件。已经运行的服务端需要重启加载新插件及后端代码。
重启后存档可恢复；托管服务会恢复已保存的 Auto worker，并保留暂停与机器人启用状态。
嵌入式恢复构造器不启动 worker；检测到未完成手动动作时，托管服务拒绝自动启动。

创建一个含原有 20 个背景 bot 和 6 个 Pine bot 的新市场：

```powershell
.venv/Scripts/python.exe scripts/background_market.py --pine --room pine-background --seed 7
```

`--dry-run` 只打印请求。脚本先检查服务端是否安装插件，再创建新房间；不覆盖
已有房间或充值旧账户。Bearer 身份沿用 `MARKETFORGE_AGENT_ADMIN_TOKEN`。
6 个实例使用独立的 300–305 账户，初始资金各 10000、持仓为零，周期分别是
10 秒和 15 秒仿真时间；同类策略使用不同参数。原有 20 个背景 bot 的配置保留。

## 添加脚本和配置

将 `.pine` 源码放到 `bot-plugins/pine-strategy/scripts/`，设置实例的 `script`
相对文件名。路径必须位于该目录内，单个脚本最大 64 KiB。没有源码的受保护脚本
不能通过这个本地解释器运行。修改脚本或参数应使用新的实例和新的空仓账户；
存档通过源码、配置和账户身份摘要拒绝未经声明的行为变更。

实例示例：

```json
{
  "Plugin": {
    "participant": {
      "participant_id": "pine-ma-fast",
      "kind": "RuleAgent",
      "room_id": "pine-background",
      "account_id": 300,
      "instrument_id": "V-BTC-SPOT"
    },
    "plugin_id": "pine.strategy",
    "plugin_version": "1.0.0",
    "state_version": 1,
    "config_version": 1,
    "seed": 7,
    "config": {
      "script": "ma.pine",
      "inputs": {"Fast": 3, "Slow": 8, "Quantity": 2},
      "bar_interval_ms": 10000,
      "history_limit": 512,
      "max_qty": 2,
      "inventory_cap": 10,
      "slippage_ticks": 2,
      "fee_buffer_ppm": 1000
    }
  }
}
```

`inputs` 使用脚本 `input.*` 的唯一标题作为键，适配器转换成解释器的 callSiteId；
没有标题的 input 可继续使用脚本默认值。
未知标题、非法值和不支持的 Pine 功能会报错。支持的周期选择见 `GET /bots`，
包括 Pine 支持的 1/5/10/15/30/45 秒及所列分钟周期，不能把任意秒数伪装成标准周期。
价格单位为引擎的 price_tick，数量单位为引擎 qty；没有额外的币价或合约手数换算。

## 执行和恢复

- 宿主通过可选 `market_data` 清单声明，为 bot 提供完整、已经收盘的非空 K 线，
  以及自己的成交、仿真时间和实际手续费。不使用最近 32 笔成交自行拼接行情。
- 首次运行需要新的独立现货账户：零持仓、零历史成交、零已付手续费。历史预热
  使用这个空仓账户；只提交最新 K 线的意图，历史预热订单不会补发。
- 实际成交决定持仓和平均成本。账户现金、持仓、手续费必须与成交回执吻合；
  共用账户、外部下单、资金划转或遗留挂单会破坏这项约束并使该 bot 停止。
- 只支持单个 entry ID、固定整数数量的多头市价 entry、close 和 close_all，
  每个收盘 bar 最多一个意图。意图转换成带价格保护的 IOC，受单笔数量、库存、
  可用现金、手续费预算和交易所风控限制。部分成交和未成交不会记成完整成交。
- 同一 bar 不重复下单。慢决策或暂停导致遗漏多个 bar 时，通过带时间和手续费的
  真实成交重建遗漏 bar 的账户反馈，推进 Pine 状态，但只提交最新 bar 的意图。
  `skipped_bar_decisions` 记录没有实际提交交易的中间 bar。
- 源码、历史 bar、账户反馈和历史意图被冻结；重建若改写此前意图会拒绝执行。
  所需 transcript、成交成本和游标全部存入版本化 bot state，进程重启不依赖内存。
- 数据库恢复时，宿主从 durable execution receipts 单独恢复 bot 的行情和成交，
  不重新提交订单、不改变恢复后的撮合状态；避免引擎 checkpoint 只保留尾部历史。
  房间写入租约接管使用同一恢复流程。

`GET /rooms/{id}/bots` 的 state 可查看最近意图、提交动作、已评估 bar 数、实际持仓、
现金和遗漏决策数量。这些是策略活动记录；最终成交和盈亏仍以市场账户与日志为准。

## 第一版边界

不支持现货做空、合约、加仓、多 entry ID、限价挂单、止损/止盈括号、追踪止损，
以及成交后或每 tick 重算。`request.security`、其他市场/周期数据、外部库和其他
宿主数据尚未提供；调用这些能力会被解释器或适配器拒绝。复杂 TradingView
策略需要按具体脚本验收，不能承诺整个社区策略库兼容。

每次观察启动一个进程，新增 bar 时按冻结 transcript 重建。`history_limit` 默认
512、最大 2048 个非空收盘 bar；超过限制明确暂停，不截短历史后偷偷重置 Pine
变量。启动前按实验时长设置周期与 horizon。请求和响应沿用 bot.v1 的大小限制，
一次观察最多提供 2049 个 bar 和 4096 笔自己的成交；缺失或截断历史会拒绝执行。
恢复最多读取 100000 个 journal command 和 100000 笔 public trade。归档导致的
command 缺口、缺失时间或手续费、恢复越界都会使输入不完整，不能当作恢复成功。
长期常驻执行、无限期运行、归档历史接续和大规模策略负载尚未完成资源资格验证。

## 验收

```powershell
$env:PATH = "$PWD\.venv\Scripts;$env:PATH"
$env:PYTHONPATH = 'python'
$env:MARKETFORGE_REQUIRE_PINE_TESTS = '1'
.venv/Scripts/python.exe -m unittest python.tests.test_pine_bot python.tests.test_pine_http -v
```

HTTP 验收需要先构建当前 server；可用 `MARKETFORGE_TEST_SERVER` 指定独立构建的
可执行文件。PostgreSQL 重启验收需要独立的 `MARKETFORGE_TEST_DATABASE_URL`；设置
`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1` 可禁止静默跳过。验收启动和清理自己的
loopback 服务端，证据写入 `target/pine-http/`。
