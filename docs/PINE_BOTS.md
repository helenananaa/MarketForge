# Pine 背景交易者

`pine.strategy@2.0.0` 是一个通用 `bot.v1` 进程插件。安装到插件包的 Pine
脚本读取本房间的真实成交 K 线和自己的实际账户，输出买卖意图，由 MarketForge
统一撮合、风控和记账。它不使用 Pine 的模拟成交，也不直接调用交易 HTTP API。

附带原创的 `ma.pine`、`rsi.pine`、`breakout.pine`、`limit.pine`、`perp.pine`、
`basis.pine`，提供趋势、回归、突破、持续限价、合约多空与现货/永续基差示例。
默认使用 `execution_mode=broker`。这些脚本是仿真示例，没有盈利保证或实盘校准。

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
不能通过这个本地解释器运行。修改脚本或参数应使用新的实例；
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
    "plugin_version": "2.0.0",
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
宿主传入真实品种元数据，`syminfo.mintick` 使用交易所的 `tick_size`；IOC 的
`slippage_ticks` 按该步长计数。显式订单价格和数量必须对齐 tick/lot，预算裁剪
后的数量也对齐 lot，不会把无效精度伪装成已接受订单。

## 执行和恢复

### 默认 broker 模式

- 现货和永续共用人工交易网关、账户余额和交易所风控。可读取自己的挂单和真实成交，
  可以共用账户、人工下单或划转现金。现货没有持仓成本字段；接入已有初始库存时，
  必须提供 `initial_avg_price_tick`，避免伪造初始成本。
- `strategy.entry` 支持现货多头、永续多空、反手、多 entry ID 和加仓；交易数量仍受
  `max_qty`、`inventory_cap` 和可用资产约束。Pine `pyramiding` 基于真实成交的 entry 计数。
  `strategy.order` 使用独立订单语义，不套用 entry 的反手和 pyramiding 过滤。
- 带 `limit` 的意图产生持续限价单。相同 ID 更新对应订单，部分成交只保留剩余数量。
  原生改单不允许提高主动成交程度；此时先撤单，收到成功回执后再挂剩余数量。
  `strategy.cancel/cancel_all` 只撤本实例跟踪的订单，不撤人工或其他 bot 的订单。
- 无 limit 的意图使用价格保护 IOC；永续平仓使用 `reduce_only` 和实际持仓数量。
  `position_side=Both` 用于单向账户；双向永续账户必须明确选择 `Long` 或 `Short`。
- 永续的 `stop` 入场和 stop-limit 交给宿主条件单，实际提交 ID 由条件单状态反馈。
  `strategy.exit` 的止盈、止损及激活后的追踪保护交给永续持仓保护。入场尚未成交时
  保存 exit 意图，实际持仓出现后安装保护；超越触发价时提交 reduce-only 平仓。
  现货条件单和持仓保护仍按核心网关支持范围拒绝，不能假装交易所已经接受。
  原生持仓保护按账户/品种/持仓侧保存，多个 exit ID 不能形成独立的 OCA 括号；
  共用账户时最后安装的保护生效。撤销 Pine exit 前核对当前保护，保留人工改写的保护。
- 永续均价、权益、已实现收益、资金费和强平结果来自真实账户。现货成本来自完整
  成交回执。历史账户反馈、请求数据的可见前缀、已提交意图和订单映射都存入 state；
  恢复只续跑未完成动作，同一 bar 不重发历史意图。
  重启和写入租约接管都会从 journal 恢复清算反馈及成交的 Long/Short 侧；旧日志
  若缺失双向成交侧，明确拒绝不完整反馈，不把它猜成单向成交。历史现货清算反馈
  只包含现金、库存和费用，不虚构当时的挂单预留资产；交易预算使用当前账户快照。
- 新增 bar 才执行 Pine；撤单后的替换、exit 安装与两腿敞口处理可在没有新 bar 的
  观察中继续。新实例预热历史，只提交最新 bar 的意图，不补发过去的交易。

### 多品种、多周期与双腿

配置 `related_instruments: ["V-BTC-PERP"]` 声明同房间可观察和交易的品种。
宿主提供同一市场时点的公开盘口、真实成交、自己的账户与订单，以及每个品种的
收盘 K 线/成交手续费。其他人的账户不可见。配置 `additional_intervals_ms: [60000]`
可增加周期；这些数据注入 `request.security`/`request.security_lower_tf` 的宿主 provider。
最多 8 个附加品种、8 个附加周期，总数据流最多 16，仍受进程消息大小上限约束。

原生 Pine 订单没有目标品种参数。MarketForge 明确扩展订单 ID 的前缀路由：
`strategy.entry("V-BTC-PERP::S", strategy.short, qty=1)` 提交到声明的永续品种。
不带前缀时提交到实例主品种；`strategy.position_size` 仍表示主品种/所选持仓侧。
这是 MarketForge 宿主约定，不能当作 TradingView 原生多品种执行语法。

需要自动协调现货/永续双腿时，给主腿设置：

```json
{"script":"basis.pine", "execution_mode":"broker", "related_instruments":["V-BTC-PERP"],
 "hedge_instrument_id":"V-BTC-PERP", "hedge_timeout_ms":5000}
```

`basis.pine` 绑定 `V-BTC-SPOT`，对冲器根据**真实主腿持仓**建立反向永续仓位。
只成交 1 单位就对冲 1 单位；主腿平仓后减仓对冲腿。未完成对冲时阻止新增主腿交易，
超时或保证金异常时撤销本实例挂单并尝试平掉两腿。流动性不足时继续保留敞口和
`unwinding` 状态，不能把一次尝试当作已经平仓。两腿没有原子成交保证。
对冲器使用同一账户、同一基准数量单位的现货/永续组合；配置自动对冲时，Pine
只能向主腿发交易意图，避免两个控制器同时操作对冲腿。

### 旧实例兼容

清单显式兼容 `1.0.0` 和 state_version=1。已持久化的旧 `identity` 状态自动继续
legacy 执行，保留原来的源码/配置摘要；不会把旧账户强制切换到新成交语义。
新 broker 状态用 `broker_schema=2` 区分。需要将旧策略升级为 broker 时创建新实例，
根据实际账户持仓配置初始成本。下面的旧执行规则只适用于 `execution_mode=legacy`：

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

## legacy 模式与解释器边界

legacy 模式不支持现货做空、合约、加仓、多 entry ID、限价挂单、止损/止盈括号、追踪止损，
以及成交后或每 tick 重算。`request.security`、其他市场/周期数据、外部库和其他
宿主数据尚未提供；调用这些能力会被解释器或适配器拒绝。复杂 TradingView
策略需要按具体脚本验收，不能承诺整个社区策略库兼容。

两个模式仍受外部 Pine runtime 的语义边界约束：仅整数固定数量、bar-close 执行；
不支持用户要求的每 tick/成交后重算、原生成交设置或整个社区策略库的全部语义。
broker 的宿主 pass 用于冻结请求数据，不制造成交后的模拟计算。

默认清单使用 4 个常驻 worker，按收盘或账户/回执变化及未完成工作 deadline 唤醒，避免每次观察创建
Python 进程。编译 Program 缓存最多 64 项；每个 worker 的增量 session 最多 512 项。
清单启用 `instance_affinity`，按完整 participant 身份的稳定摘要固定到 worker 槽位；
槽位忙碌时在原 deadline 内等待，不迁移到其他进程。槽位失败、跨插件淘汰或进程重启后仍需重建。
Pine 的 worker 最多处理 500000 次请求，避免默认 10000 次回收使大量实例反复冷启动。
session 必须与宿主已提交的完整 bar/账户/pass 前缀和设置
一致；重启、淘汰、未提交计算或前缀不符时从 durable transcript 重建。

暖会话只构造新增 bar 的 external pass，缓存保存 provider 与各历史时点可见长度，
不再保存各历史 pass 的完整行情前缀。完整重建仍使用冻结前缀，历史改写继续拒绝。
`delta_requests` 在同一 worker 中缓存最多 512 份此前宿主输入，仅传字段变更和数组追加；
源码、配置、账户和完整展开请求仍受原协议约束，首次或淘汰后发全量。worker 解码按写时复制应用补丁，
从不原地修改已解码对象；因此 broker 对未变的 K 线和成交前缀按对象身份复用此前的解析，只解析、
校验新增部分。清单另启用 `resident_state`（需 `delta_requests`）：worker 只回复相对请求 state 的
补丁。宿主要求补丁路径互不包含，并逐个对照未修改的 state 检查后才应用，被拒绝的补丁不改变 state；
宿主接受回复后，双方把同一补丁折叠进各自缓存的请求，下一次请求只携带宿主自己的修改。没有精确补丁
时（首次决策、非 JSON 规范值）回复完整 state。durable state 仍是宿主持有的完整结构，worker 重启或
淘汰后从中重建；完整响应的 1 MiB 上限仍按完整编码长度检查。宿主编码请求时，观察中的 K 线、自己的
成交、成交明细和账户事件按类型数据比较，只把新增项转换为 JSON；state 直接与缓存比较，折叠后若只有
宿主写入的顶层键（事件游标、观察时刻）变化，则只比较这些键，任何整体替换 state 的路径都回到完整比较。
当前没有完整计算 checkpoint。

自动调度先取得外部任务名额，再复制实际派发 bot 的状态；待处理队列只保存身份和索引。
任务完成后继续派发本轮待处理实例，不要求等到下一次市场 tick。每个 bot 最多有一个在途决策，
停止、暂停和 epoch 变化仍校验；新 tick 保留未派发队列并补充尚未排队的候选。

已安装的公开 `pine-compat-runtime==0.3.1` 仍能完整重放。要启用真正增量追加，构建
仓库提供的私有 `0.3.1+mf1` adapter；只安装到插件 `.runtime`，不替换全局依赖：

```powershell
.venv/Scripts/python.exe scripts/build_pine_incremental_runtime.py --source-repo E:/projects/pine-interpreter --install
```

构建从固定 v0.3.1 commit `8838ed004c7475ce2877189bcd0e43a23bcf365f` 归档、应用
`runtime-patches/pine-compat-0.3.1-external-session.patch`，然后 `cargo build --locked`。
不会修改上游工作区。需要本地该 Git 对象、Rust 和对应 Python 开发环境；Windows
当前产物绑定构建所用的 Python。`.runtime/provenance.json` 记录源码、补丁和产物摘要。
它是本地 adapter，没有发布成上游版本。

增量执行只对 `incremental-qualified.json` 列出的精确源码 SHA256 启用：6 个内置脚本，以及
`scripts/qualify_library_incremental.py` 验证过的策略库脚本（`library/...`）。该工具让每个已安装
脚本分别以完整重放和增量 session 走同一组合成生命周期（永续账户、逐根收盘 bar、订单立即成交），
全部响应和持久状态逐次相同才算合格；这只证明两条执行路径等价，不是交易或市场资格。修改源码、
其他社区脚本以及 legacy 模式继续完整重放。构建脚本另外应用
`runtime-patches/pine-compat-0.3.1-session-output.patch`：增量追加可用 `include_output=False`
跳过逐次转换全部 plot 输出（broker 只读意图），并可用 `intents_from` 只返回此前未取的意图及
总数，broker 以滚动摘要校验已提交的意图前缀；请求数据流可只携带相对上一 pass 的新增 bar
（`stream_tails`，由 adapter 还原完整流后交给运行时），已执行 pass 不再保留各自的请求数据副本，
session 内存随历史线性而非平方增长。旧 adapter 自动回退到完整输出、完整意图和完整流。外部 session 只追加账户反馈和已经确认的
bar/pass，不模拟交易所成交；历史意图仍受 broker 前缀一致性校验。未安装 adapter 时
完整重放回退可用，性能结论应说明实际加载的版本。对冲 residual/unwinding 保存下一次
仿真唤醒时间，不依赖新 K 线才补偿。

`history_limit` 默认
512、最大 2048 个非空收盘 bar；超过限制明确暂停，不截短历史后偷偷重置 Pine
变量。启动前按实验时长设置周期与 horizon。请求和响应沿用 bot.v1 的大小限制，
一次观察最多提供 2049 个 bar、4096 笔自己的成交和 4096 个自己的清算快照；
缺失或截断历史会拒绝执行。
恢复最多读取 100000 个 journal command、100000 笔 public trade 和 200000 个清算账户反馈。归档导致的
command 缺口、缺失时间或手续费、恢复越界都会使输入不完整，不能当作恢复成功。
无限期运行、归档历史接续、整个社区库和 PostgreSQL 混合大规模负载尚未完成资格验证。
常驻与增量缓存不会解除上述 transcript/horizon 上限；短时阶梯压测见
[策略运行时验收](validation/2026-10-11-strategy-runtime.md)。实际 100/500/1000 个 Pine
的持续真实成交负载、阶段负结果与接入修复见
[Pine 大规模接入复测](validation/2026-10-11-pine-scale-continuation.md)；实例初始化成功不能代替容量验收。

事件型插件可选择先比较冻结行情批次中的内容摘要，再构造完整观察。
Pine 插件现在在 manifest 声明 `host_event_prefilter: true`，默认启用；其他插件仍需
显式选择。压测器的 `--event-prefilter` 或宿主的
`MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER=1` 是全局实验开关。账户历史共享索引与插件
JSON 状态移交默认生效。Pine 开关与后续共享批次优化已降低实际订单延迟和落后实例，
但千 bot 的严格推进速度、零积压仍未达标，不能作为容量承诺。
摘要覆盖完整供应的历史前缀、完整性标记、配置的额外周期和关联市场，以及自己的
订单、成交手续费、账户清算反馈、条件单与风险状态。同一批次共享 K 线摘要和账户
事件扫描；有变化才走原来的完整触发检查。盘口型与每 tick 插件保持原来的调度。
缓存只在提交状态、配置、factory 和调度 epoch 完全匹配时复用；恢复、拒单或
未提交结果使匹配失效，`_next_wake_ms` 到期也绕过跳过检查。检查仍占用调度任务，
因此不能把少构造观察直接等同于消除了所有宿主轮询或队列成本。
进程插件在决策结束后把 JSON 状态移交给调度器，闲置缓存不再另存一份插件状态；
下次实际调用从已提交状态恢复。调度器、持久化与传输路径仍有完整状态和复制成本，
这项改动不能代替计算 checkpoint 或长期历史接续。
此前预热负载、事件过滤的负结果与当时默认开关选择见
[事件过滤与真实历史验收](validation/2026-10-11-pine-event-prefilter.md)。

日志提交现在可把调用者已拥有的 scheduler mutation 移交给日志线程，持久化
确认后交回安装，避免该线程入口再克隆完整 Pine 历史状态。空动作 delta 和
完整 checkpoint 都使用这条路径；失败不安装，调用取消后的 durable task
继续负责已接收的提交。完整 checkpoint 的频率、worker 数量和历史上限不变。
同初始 K 线与千 Pine 配方的分段诊断、A/B/B/A 结果和恢复边界见
[日志状态移交验收](validation/2026-10-12-pine-state-handoff.md)。

插件持久化 JSON 现在由不可变共享句柄持有，全量 scheduler checkpoint、delta
和 bot snapshot 复制句柄，避免复制所有 bot 的历史。新决策替换为独立状态，
执行回执使用写时复制，保持旧快照与失败提交的状态隔离。磁盘和 HTTP 的 JSON
格式、检查点频率及持久化确认后安装顺序保持不变；完整 JSON 编码与 worker
传输仍保留。相同历史、千 Pine 配方及订单/恢复验证见
[共享检查点验收](validation/2026-10-12-pine-shared-checkpoint.md)。

外部 bot 的待执行队列现在跨行情 tick 保留并按实例去重，调度 epoch 改变时清空，
派发时读取最新提交状态。相同已提交行情快照可复用不可变观察批次，避免 worker
完成后补派任务时重复扫描历史；行情或账户写入发布新快照，使共享缓存失效。
日志把恢复 JSON 校验和 payload 编码合为一次遍历，复用已校验字节，保留完整检查点、
恢复检查、磁盘格式及确认后安装顺序。Pine worker 只读解码、增量缓存只复制追加尾部；
broker 仍隔离复制可变状态，通用插件解码仍返回独立副本。
相同真实历史和千 Pine 的分段 trace、负结果、复测与订单恢复边界见
[收线排队与完整日志验收](validation/2026-10-12-pine-dispatch-journal.md)。

压测可用 `--prewarm-bars 64 --warmup 20` 先通过真实 gateway 成交生成历史，再启动
bot。按最长周期生成 64 根，其他周期拥有更多历史；所有周期的历史必须仍在配置
上限内。参数不同不会改变共同收线边界。`process_tree.measurement` 只统计排除
预热后的资源采样，原始进程树统计仍包含启动与预热；阶段计时的 P95 仍是全程
直方图上界。真实历史预热不等于历史期间已有 Pine 交易，也不等于长期恢复验收。

## 验收

```powershell
$env:PATH = "$PWD\.venv\Scripts;$env:PATH"
$env:PYTHONPATH = 'python'
$env:MARKETFORGE_REQUIRE_PINE_TESTS = '1'
.venv/Scripts/python.exe -m unittest discover -s python/tests -p 'test_pine*.py' -v
```

HTTP 验收需要先构建当前 server；可用 `MARKETFORGE_TEST_SERVER` 指定独立构建的
可执行文件。PostgreSQL 重启验收需要独立的 `MARKETFORGE_TEST_DATABASE_URL`；设置
`MARKETFORGE_REQUIRE_POSTGRES_TESTS=1` 可禁止静默跳过。验收启动和清理自己的
loopback 服务端，证据写入 `target/pine-http/`。

私有 adapter 的逐段一致性与缓存恢复验收：

```powershell
$env:PYTHONPATH = 'bot-plugins/pine-strategy/.runtime;python;python/tests'
$env:MARKETFORGE_REQUIRE_INCREMENTAL_TESTS = '1'
.venv/Scripts/python.exe -m unittest discover -s python/tests -p test_strategy_runtime.py -v
```
