# 盘口反馈与永续动机

沿用同一撮合、账户、公共观察、风控和恢复链路。网页在“绑定永续”和“增强行为”
开启时，可勾选“盘口反馈与杠杆”创建 38 bot：原 33 个参与者加两个资金费交易者、
三个杠杆趋势交易者。既有 33 bot 配方仍为默认；新房间的资金和库存全部明确、有限。

```powershell
$env:PYTHONPATH = "$PWD\python"
.venv/Scripts/python.exe scripts/microstructure_market.py --room microstructure-market --seed 7
```

`--export` 保存创建请求；`--dry-run` 打印请求。网页读取相同的
`scripts/fixtures/microstructure_market.json`，Python 测试校验配方一致性。

## 做市反馈

`DynamicMarketMaker` 的 `book_pressure_ticks` 控制盘口压力对报价中心的影响。
前 `book_pressure_levels` 档买卖深度先减掉自己的挂单，再计算不平衡程度：外部
买盘偏厚时上移中心、增加买侧数量并减少卖侧数量，反向亦然。仍叠加库存偏移；
`index_basis_ticks` 可配置永续相对指数的固定偏移。启用压力反馈时，报价限制在
对手最优价之外，实际 post-only 检查仍由交易所执行。

`requote_threshold_ticks` 提供价格调整滞回，数量变化也会触发换单。撤单和重挂分开
完成，等待下一次真实观察确认撤单，继续按实际剩余现金、库存和挂单预留预算报价。

`toxic_flow_threshold_ppm` 和 `toxic_flow_min_qty` 检查新增公开成交中属于自己
maker 的单向成交。触发后撤单，等待 `toxic_cooldown_ms`，再在 `recovery_ramp_ms`
内逐渐恢复数量。成交 ID、防重复触发的状态、冷却截止时间和报价状态均保存。
这是对最近 32 条公开成交的有界启发式，可能漏掉高流量下较早的成交；不是完整
订单流毒性估计。默认压力和毒性阈值为零，旧配置保持原行为。

## 资金费交易者

`FundingRateTrader` 只接受净持仓永续账户。距离下一次结算在
`funding_entry_window_ms` 内且估算费率超过 `funding_entry_rate_ppm` 时建立仓位：
正费率做空，负费率做多。仓位受 `position_size`、库存上限、目标杠杆、可用资金、
最大单笔量、滑点和共用风险阈值约束。

估算费率缺失、降到 `funding_exit_rate_ppm` 以下、反向或结算周期推进后进入退出。
退出按实际仓位提交 reduce-only；旧方向没有真实平仓前不反向开仓。支付和收取资金费
来自真实永续清算，没有模拟收益入账。此策略承担方向风险，并未配套现货对冲；
手续费、价差和价格损失可能超过资金费收入。

## 杠杆偏好和主动减仓

`LeveragedTrendTrader` 从已有价格序列决定方向，以 `target_leverage` 配置目标
名义敞口／权益比例。配方分别使用 1、2、5 倍目标，场所实际杠杆上限是 5 倍；
目标参数不改变交易所保证金制度。其目标数量按真实权益、标记价格、`position_size`
和 `inventory_cap` 截断，开仓预算包含保证金和费用。

权益下降导致现有仓位超出目标时，撤掉旧订单后主动减仓，不等待常规决策间隔。
指数失效或共用保证金／回撤风控触发时也会退出。撤单和平仓可以部分成功；没有
对手深度时保留真实剩余仓位。`deleverage_decisions` 统计决策意图，不能当作
实际平仓次数。账户没有自动充值、重新生成本金或无限借币。

## 联合校准

`scripts/joint_market_calibration.py` 在固定历史样本拟合段上同时搜索订单流人数、
报价宽度和有限做市深度，同时按历史成交数量中位数配置下单尺度。十个候选还
覆盖 0.6、0.8、1.0 的活动比例和 0.7、0.8、0.9 的市价比例；是有界粗搜索，
没有穷举这些维度的笛卡尔积。
新数量单位是 0.0001 BTC。
报价宽度属于模型参数，成交归档不提供盘口；手续费仍按场所实际规则结算。
噪声策略“延续，否则随机”的概率换算使用 `2 * P(同方向) - 1`，低于 0.5 的历史
延续比例只能截断到随机基线，当前模型不拟合反持续性。

先用种子 7 筛选十个参数组合，再用种子 19 复核前三名；固定配方后用种子
7、19、41 与旧配方比较，最后读取留出段。原始真实撮合回执一直保存，现货合并
同一吃单在同时间、同价格的连续成交；永续按相同 100ms 桶、价格、方向合并连续
成交。这分别参考 [现货接口](https://developers.binance.com/en/docs/catalog/core-trading-spot-trading/api/rest-api/market)
与 [永续接口](https://developers.binance.com/en/docs/catalog/core-trading-derivatives-trading-usd-s-m-futures/api/rest-api/market-data)
的聚合定义，但仿真时钟仍以秒推进且仅合并相邻记录，所以永续统计是明确标注的
代理口径，不代表真实 100ms 到达过程。旧回执缺少吃单 ID 时保持原始粒度；联合校准
遇到缺少 ID 的回执直接失败。

```powershell
cargo build -p exchange-core --release --example behavior_market
.venv/Scripts/python.exe scripts/joint_market_calibration.py
```

默认每个拟合运行 300 秒、评估 600 秒，可通过 `--fit-steps` 和 `--evaluation-steps`
设置 120–1800 秒范围内的长度。每次实际运行有 300 秒墙钟超时。配方、单位、
源数据／二进制／配方哈希、候选结果、独立种子结果和留出残差保存到输出目录。
`--reuse-existing` 仅从已完成且来源／二进制哈希一致的报告复用回执，还会核对
配方、种子、长度、运行模式和重新计算的统计结果；这些检查不一致就重新实际运行。
`continuous` 研究运行器复用策略对象，每单仍经过真实撮合网关；与耐久调度器在
33／38 bot 配方下逐笔订单、资金费、账户和最终状态完全一致。研究模式不提供
每步数据库检查点，数据库恢复另用服务进程验证。

## 到达时间与报价补充

38 bot 可选配方现在为噪声交易员启用 `arrival_mode="Poisson"`：各自种子生成
指数等待时间，首次下单也独立延后；剩余等待和 RNG 沿用已有持久状态。默认原生
配置仍是 `Periodic`，旧 33 bot 配方保持原有节奏。当前房间时钟一格为 1000ms，
等待到期只能在时钟格点执行，不能据此声称有真实毫秒级 Poisson 订单流。

做市商配置 `replenish_depth_ppm=800000`，同方向／价格的自有剩余深度低于目标
80% 时追加缺口；旧订单不撤掉，原队列位置保留。只使用当前账户可用预算，
不重置库存或补充本金。默认值 0 保持旧的等待撤单／到期行为。
`size_volatility_ticks` 是数量收缩的波动尺度，默认 1 保留旧计算；38 bot 配方
使用 4 ticks，参考校准使用拟合段每秒收益标准差换算的 tick 尺度。

联合校准可加 `--feedback` 独立搜索这些机制，留出段仍只在参数冻结后读取。
不能把多一项机制当作统计相似性已改善；要看独立种子的残差。

服务负载复测与页面证据见 [负载验证记录](validation/2026-10-09-market-load.md)。
之前的证据见 [微观结构验证记录](validation/2026-10-09-market-microstructure.md)。更多日期和
行情状态、真实盘口／撤单／队列／网络延迟仍需另行校准；目前没有真实市场相似性资格。
