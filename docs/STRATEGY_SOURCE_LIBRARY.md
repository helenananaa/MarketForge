# 本地背景策略来源库

采集日期：2026-10-10。此轮目标是广泛保存，解释器支持范围不作为淘汰条件。

已保存 **33 个 GitHub 来源、13,111 个文件、6,689 个代码片段、7 份 TradingView 源码转录和 10 份 PDF**。

原始文件中直接识别的 Pine 文件 2,211 个；代码片段语言分布 `{"python": 846, "cpp": 20, "pine": 5358, "javascript": 372, "pascal": 27, "rust": 66}`。

下面 87 个具名入口包括指标、策略、执行器、风险模块、同公式的不同实现和学习框架，**不是独立可用 bot 数量，也不是盈利排名**。此前热门 TradingView 的页面人气与 223 个 Pine 文件测试见 [前轮报告](<E:/projects/MarketForge/docs/validation/2026-10-10-strategy-research.md>)。

## 入口与保存范围

- [离线检索全部文件与代码片段](<E:/projects/MarketForge/.local/strategy-library-20261010/index.html>)
- [具名候选 JSON](<E:/projects/MarketForge/.local/strategy-library-20261010/candidates.json>)
- [固定提交、许可文件和 ZIP 清单](<E:/projects/MarketForge/.local/strategy-library-20261010/manifest.json>)
- [完整性验证回执](<E:/projects/MarketForge/.local/strategy-library-20261010/verification.json>)

`repos/` 是任务内下载的仓库；4 个前轮来源复用原目录。`snapshots/` 为独立 ZIP，选定策略路径及根目录文件的快照；不是所有来源的完整依赖。`extracts/` 保留 Notebook 代码及 Markdown 源码片段，每个片段有来源与哈希 sidecar。`papers/` 保留论文/作者讲义和部分摘要页。`tradingview/` 为可见源码文本转录（NBSP 归一），非字节精确的上游文件。`previous-probes/` 保存前轮 22 个公式适配和受控测试证据。

仓库 ZIP 对应本地 checkout 内容，可能含 Git 换行规范化；哈希针对保存的本地字节。Windows 非法文件名另保留原名 ZIP、可读副本和映射。不支持、含外部数据依赖或疑似未来偏差的原版仍保留，接入资格单独判定。

## 数据与接入判断

- OHLCV 类可使用这两个商品各自历史；多周期必须因果聚合、避免未收盘高周期泄漏。
- 做市与订单流只需本市场数据，但需要盘口、逐笔成交、订单生命周期和真实账户反馈，K 线不足以重建队列。
- 现货永续基差需要两边可执行报价和成交；资金费率需平台实际费率、结算时点及保证金规则。
- TWAP/VWAP/POV/冰山/Almgren–Chriss 是执行政策，需要给定目标需求，不能自行假定市场方向收益。
- BSE 私有需求限价和 ABIDES 外生价值 oracle 是不同概念。可研究私有偏好与预算约束；只有两种行情时不能照搬真实价值 oracle。
- RL/机器学习保存的是算法、环境与示例。训练权重、输入状态、奖励、时间切分与本市场资格均待验证。
- `lookahead_bias` 原版作隔离反例。静态标记只是搜索线索，尚未逐个因果/重绘审计。
- 保存许可文件不等于已取得统一商用或再分发许可；接入时逐文件核对，保留署名和许可要求。

策略数量增加本身不能保证深度或真实性。后续应按角色、资本、库存目标、时钟/延迟、风险预算分散，并实测价差、可成交深度、冲击、撤单、拥挤和双市场联动。

## 具名入口

| 入口 | 角色 | 数据/适配依赖 | 备注 | 本地原版 |
|---|---|---|---|---|
| Pine Turtle | 行情 | OHLCV | 海龟趋势候选；需审计通道引用和仓位 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hasnocool/Turtle Trend Trading System [racer8].pine>) |
| Pine 三重 SuperTrend | 行情 | OHLCV | 多个 ATR 趋势条件，保留原版 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hasnocool/3x SuperTrend Strategy (Mel0nTek) V1.pine>) |
| Pine Bollinger breakout | 行情 | OHLCV | 突破候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hasnocool/Bollinger Band Breakout.pine>) |
| Pine Squeeze strategy | 行情 | OHLCV | 完整策略候选，区别于原始指标 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hasnocool/Squeeze Momentum Strategy.pine>) |
| Pine Ichimoku pyramiding | 行情 | OHLCV | 云图与加仓候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hasnocool/2Tier Ichimoku Pyramiding [Takazudo].pine>) |
| Pine VWAP mean reversion | 行情 | OHLCV | 成交量加权均值回归候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hasnocool/HYE Mean Reversion VWAP [Strategy].pine>) |
| Pine Grid | 做市网格 | OHLCV+限价成交 | 网格候选；库存、订单生命周期必须适配 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hasnocool/(IK) Grid Script.pine>) |
| James MA crossover | 行情 | OHLCV | 前一轮原脚本运行产生订单 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/jamesbachini/crossover.ps>) |
| James price channels | 行情 | OHLCV | 前一轮原脚本运行产生订单 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/jamesbachini/price_channels.ps>) |
| HalfTrend indicator | 行情 | OHLCV | 指标构件；需定义交易规则 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/everget/trailing_stops/halftrend.pine>) |
| ALMA indicator | 行情 | OHLCV | 平滑构件；需定义交易规则 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/everget/movings/arnaud_legoux_moving_average.pine>) |
| Bandtastic | 行情 | OHLCV | 长预热、多指标组合 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/Bandtastic.py>) |
| CombinedBinHAndCluc | 行情 | OHLCV | 布林偏离与量价条件 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/berlinguyinca/CombinedBinHAndCluc.py>) |
| ADXMomentum | 行情 | OHLCV | ADX/DI/MOM/SAR 组合 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/berlinguyinca/ADXMomentum.py>) |
| MultiRSI | 行情 | 同商品多周期 | 多周期收盘对齐 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/berlinguyinca/MultiRSI.py>) |
| Freqtrade Supertrend | 行情 | OHLCV | 多个 SuperTrend 组合 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/Supertrend.py>) |
| Freqtrade MACD | 行情 | OHLCV | MACD 条件候选 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/berlinguyinca/MACDStrategy.py>) |
| PSAR stoploss | 风险退出 | OHLCV+持仓成交 | 退出模块 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/CustomStoplossWithPSAR.py>) |
| BreakEven | 风险退出 | 持仓成交 | 保本退出模块 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/BreakEven.py>) |
| Fixed risk reward | 风险退出 | OHLCV+持仓成交 | 风险收益退出模块 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/FixedRiskRewardLoss.py>) |
| NostalgiaForInfinity X7 | 组合策略 | 原版含外部多资产 | 保留完整原版；单标的改写须另命名 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/NostalgiaForInfinity/NostalgiaForInfinityX7.py>) |
| Freqtrade biased examples | 隔离研究 | 未来偏差示例 | 保存作反例，排除候选生产池 | [源码](<E:/projects/MarketForge/.local/strategy-research-20261010/freqtrade-strategies/user_data/strategies/lookahead_bias/Zeus.py>) |
| Jesse Dual Thrust | 行情 | OHLCV | 区间突破候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/jesse-examples/DUAL_THRUST/__init__.py>) |
| Jesse Donchian | 行情 | OHLCV | 通道候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/jesse-examples/Donchian/__init__.py>) |
| Jesse RSI2 | 行情 | OHLCV | 短周期 RSI 候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/jesse-examples/RSI2/__init__.py>) |
| Jesse KDJ | 行情 | OHLCV | 摆动指标候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/jesse-examples/KDJstrategy/__init__.py>) |
| Jesse MACD EMA | 行情 | OHLCV | 均线与动量候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/jesse-examples/MACD_EMA/__init__.py>) |
| VeighNa ATR RSI | 行情 | OHLCV | ATR 与 RSI 候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-cta/vnpy_ctastrategy/strategies/atr_rsi_strategy.py>) |
| VeighNa Boll channel | 行情 | OHLCV | 布林通道候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-cta/vnpy_ctastrategy/strategies/boll_channel_strategy.py>) |
| VeighNa Dual Thrust | 行情 | OHLCV | 区间突破候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-cta/vnpy_ctastrategy/strategies/dual_thrust_strategy.py>) |
| VeighNa King Keltner | 行情 | OHLCV | Keltner 候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-cta/vnpy_ctastrategy/strategies/king_keltner_strategy.py>) |
| VeighNa Turtle signals | 行情 | OHLCV | 海龟信号组合 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-cta/vnpy_ctastrategy/strategies/turtle_signal_strategy.py>) |
| VeighNa multi timeframe | 行情 | 同商品多周期 | 同商品聚合与收盘对齐 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-cta/vnpy_ctastrategy/strategies/multi_timeframe_strategy.py>) |
| Hummingbot PMM simple | 做市网格 | 盘口+账户成交 | 基础报价控制器 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/controllers/market_making/pmm_simple.py>) |
| Hummingbot PMM dynamic | 做市网格 | 盘口+OHLCV+账户成交 | 动态报价控制器 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/controllers/market_making/pmm_dynamic.py>) |
| Hummingbot Avellaneda | 做市网格 | 盘口+库存+到达率 | 库存风险做市；需本市场参数估计 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/hummingbot/strategy/avellaneda_market_making/avellaneda_market_making.pyx>) |
| Hummingbot perpetual maker | 做市网格 | 永续盘口+保证金成交 | 永续做市 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/hummingbot/strategy/perpetual_market_making/perpetual_market_making.py>) |
| Hummingbot Bollinger | 行情 | OHLCV | 方向控制器 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/controllers/directional_trading/bollinger_v1.py>) |
| Hummingbot MACD BB | 行情 | OHLCV | 动量与布林控制器 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/controllers/directional_trading/macd_bb_v1.py>) |
| Hummingbot spot perpetual arb | 双商品套利 | 现货永续报价+两边成交 | 基差回归与双腿风险 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/hummingbot/strategy/spot_perpetual_arbitrage/spot_perpetual_arbitrage.py>) |
| Hummingbot hedge | 双商品套利 | 两边账户+可执行报价 | 库存对冲；不能假设免费对冲成交 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/hummingbot/strategy/hedge/hedge.py>) |
| Hummingbot funding arbitrage | 双商品套利 | 原版跨市场+实际资金费率 | 原版不是单现货单永续即插即用 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/scripts/v2_funding_rate_arb.py>) |
| HftBacktest GLFT grid | 做市网格 | 盘口逐笔+延迟队列 | 模型、参数估计、网格报价笔记本 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hftbacktest/examples/GLFT Market Making Model and Grid Trading.ipynb>) |
| HftBacktest order book alpha | 订单流 | 盘口逐笔+库存成交 | 盘口不平衡信号 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hftbacktest/examples/Market Making with Alpha - Order Book Imbalance.ipynb>) |
| HftBacktest basis alpha | 双商品套利 | 原版基差上下文待核查 | 单现货永续可研究的机制；未做等价适配 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hftbacktest/examples/Market Making with Alpha - Basis.ipynb>) |
| HftBacktest queue maker | 做市网格 | 队列+逐笔+延迟 | 排队与大 tick 报价 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hftbacktest/examples/Queue-Based Market Making in Large Tick Size Assets.ipynb>) |
| HftBacktest Rust grid | 做市网格 | 盘口逐笔+库存成交 | 原生 Rust 算法入口 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hftbacktest/hftbacktest/examples/algo.rs>) |
| Nautilus Rust grid | 做市网格 | 盘口+账户成交 | 原生 Rust 网格做市候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/nautilus/crates/trading/src/examples/strategies/grid_mm/strategy.rs>) |
| Nautilus Rust composite maker | 做市网格 | 盘口逐笔+账户成交 | 组合做市候选，适配模型输入 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/nautilus/crates/trading/src/examples/strategies/composite_market_maker/strategy.rs>) |
| Nautilus Rust Hurst VPIN | 订单流 | 逐笔成交+成交方向 | Hurst/VPIN 方向候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/nautilus/crates/trading/src/examples/strategies/hurst_vpin_directional/strategy.rs>) |
| Nautilus Rust EMA | 行情 | OHLCV | 原生 Rust 均线候选 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/nautilus/crates/trading/src/examples/strategies/ema_cross/strategy.rs>) |
| VeighNa spread arb | 双商品套利 | 两腿盘口+合约单位+成交 | 价差机制可研究；须改现货永续腿定义 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-spread/vnpy_spreadtrading/strategies/statistical_arbitrage_strategy.py>) |
| VeighNa TWAP | 拆单执行 | 目标需求+时间+成交 | 时间拆单；需要真实目标需求 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-algo/vnpy_algotrading/algos/twap_algo.py>) |
| VeighNa iceberg | 拆单执行 | 目标需求+盘口成交 | 隐藏大单分批执行 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-algo/vnpy_algotrading/algos/iceberg_algo.py>) |
| VeighNa best limit | 拆单执行 | 目标需求+最优报价成交 | 追踪最佳限价 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-algo/vnpy_algotrading/algos/best_limit_algo.py>) |
| VeighNa sniper | 拆单执行 | 目标需求+盘口成交 | 达到目标价格时执行 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/vnpy-algo/vnpy_algotrading/algos/sniper_algo.py>) |
| ABIDES TWAP | 拆单执行 | 目标需求+时间+成交 | 独立需求执行者 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/execution/TWAPExecutionAgent.py>) |
| ABIDES POV | 拆单执行 | 目标需求+逐笔成交量 | 参与率执行者 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/execution/POVExecutionAgent.py>) |
| ABIDES VWAP | 拆单执行 | 目标需求+历史成交量 | 量曲线执行者 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/execution/VWAPExecutionAgent.py>) |
| Hummingbot DCA executor | 拆单执行 | 目标需求+账户成交 | 分批买卖执行器 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/hummingbot/strategy_v2/executors/dca_executor/dca_executor.py>) |
| Hummingbot TWAP executor | 拆单执行 | 目标需求+账户成交 | 时间执行器 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/hummingbot/hummingbot/strategy_v2/executors/twap_executor/twap_executor.py>) |
| Almgren Chriss | 拆单执行 | 目标库存+波动率冲击参数 | 最优执行模型；不是方向 alpha | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/almgren-chriss/almgren_chriss/model.py>) |
| LEAN C# VWAP | 拆单执行 | 目标需求+成交量+成交 | C# 执行模型 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/lean/Algorithm.Framework/Execution/VolumeWeightedAveragePriceExecutionModel.cs>) |
| LEAN C# std execution | 拆单执行 | 目标需求+OHLCV成交 | 波动条件执行 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/lean/Algorithm.Framework/Execution/StandardDeviationExecutionModel.cs>) |
| LEAN C# trailing risk | 风险退出 | 实际仓位成交 | 追踪止损风险模块 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/lean/Algorithm.Framework/Risk/TrailingStopRiskManagementModel.cs>) |
| LEAN C# EMA alpha | 行情 | OHLCV | C# 信号模块 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/lean/Algorithm.Framework/Alphas/EmaCrossAlphaModel.cs>) |
| LEAN C# RSI alpha | 行情 | OHLCV | C# 信号模块 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/lean/Algorithm.Framework/Alphas/RsiAlphaModel.cs>) |
| ABIDES Noise | 交易者决策 | 盘口+账户+私有需求 | 噪声参与者，保留资金与库存边界 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/NoiseAgent.py>) |
| ABIDES Momentum | 交易者决策 | 同商品历史报价 | 动量参与者 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/examples/MomentumAgent.py>) |
| ABIDES order imbalance | 订单流 | 盘口+账户成交 | 订单簿不平衡参与者 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/OrderBookImbalanceAgent.py>) |
| ABIDES HBL | 交易者决策 | 原版 oracle+近期订单流 | 学习成交信念；原版价值 oracle 需替换且另命名 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/HeuristicBeliefLearningAgent.py>) |
| ABIDES Value | 隔离研究 | 原版外生价值 oracle | 只有两个行情源不能获得原版真实价值 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/ValueAgent.py>) |
| ABIDES Zero Intelligence | 交易者决策 | 原版 oracle+私有价值 | 可研究私有偏好与报价机制；不能伪造 oracle | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides/agent/ZeroIntelligenceAgent.py>) |
| ABIDES adaptive maker | 做市网格 | 盘口+库存成交 | 自适应做市参与者 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/abides-jpmc/abides-markets/abides_markets/agents/market_makers/adaptive_market_maker_agent.py>) |
| BSE ZIC | 交易者决策 | 私有需求限价+盘口 | 类 TraderZIC；有约束随机报价 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/bse/BSE.py>) |
| BSE ZIP | 交易者决策 | 私有需求限价+报价成交 | 类 TraderZIP；自适应利润报价 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/bse/BSE.py>) |
| BSE PRZI PRSH PRDE | 交易者决策 | 私有需求限价+成交反馈 | 类 TraderPRZI；参数化随机、自适应搜索变体 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/bse/BSE.py>) |
| BSE AA | 交易者决策 | 私有需求限价+报价成交 | 自适应激进程度；需求供给不可照搬外部 oracle | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/bse/Trader_AA.py>) |
| BSE shaver sniper | 交易者决策 | 私有需求限价+盘口 | 类 TraderShaver/TraderSniper；报价/时机机制 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/bse/BSE.py>) |
| FMZ Python grid | 做市网格 | 同商品行情+账户成交 | 保留 FMZ 平台 API 依赖 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/fmz/grid.md>) |
| FMZ Javascript iceberg | 拆单执行 | 目标需求+盘口成交 | JavaScript 执行源码已提取 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/fmz/Iceberg-Buy-Order.md>) |
| FMZ Pine Dual Thrust | 行情 | OHLCV | Pine 文档源码已提取 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/fmz/Dual-Thrust-Trading-Algorithm-ps4.md>) |
| FMZ MyLanguage turtle | 行情 | OHLCV | 麦语言原版已保留 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/fmz/M-Language-Turtle-Trading-strategy-implementationsV-10.md>) |
| FinRL SB3 agents | 学习模型 | 训练环境+权重+原版多资产 | 候选算法框架，尚无本市场训练/策略资格 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/finrl/finrl/agents/stablebaselines3/models.py>) |
| TensorTrade DQN | 学习模型 | 训练环境+权重 | DQN 决策框架，需构造因果状态与奖励 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/tensortrade/tensortrade/agents/dqn_agent.py>) |
| Qlib RL execution | 学习模型 | 目标需求+训练权重+市场适配 | 学习执行政策框架 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/qlib/qlib/rl/order_execution/policy.py>) |
| C++ orderbook agent | 学习模型 | 盘口成交+学习机制待核查 | 名字含 RL 不代表已经验证训练能力 | [源码](<E:/projects/MarketForge/.local/strategy-library-20261010/repos/cpp-mm/agent/rl_agent.cpp>) |

## 来源固定版本

| 来源 | 类型 | 文件 | 固定提交 | 许可文件 |
|---|---|---:|---|---|
| [abides-sim/abides](https://github.com/abides-sim/abides) | agent_behaviors | 99 | `c4bf15767892` | 1 |
| [jpmorganchase/abides-jpmc-public](https://github.com/jpmorganchase/abides-jpmc-public) | agent_behaviors | 52 | `f9cbe51342b7` | 2 |
| [joshuapjacob/almgren-chriss-optimal-execution](https://github.com/joshuapjacob/almgren-chriss-optimal-execution) | optimal_execution_notebook | 9 | `3a71a1c37daf` | 1 |
| [sauloduttra/almgren-chriss](https://github.com/sauloduttra/almgren-chriss) | optimal_execution | 13 | `2bffe8630c8d` | 1 |
| [fedecaccia/avellaneda-stoikov](https://github.com/fedecaccia/avellaneda-stoikov) | optimal_market_making | 20 | `1e84656689e5` | 0 |
| [kernc/backtesting.py](https://github.com/kernc/backtesting.py) | python_examples | 32 | `ca2e2611621e` | 1 |
| [mementum/backtrader](https://github.com/mementum/backtrader) | python_examples | 127 | `b853d7c90b67` | 1 |
| [davecliff/BristolStockExchange](https://github.com/davecliff/BristolStockExchange) | adaptive_auction_agents | 131 | `6ebc4155440b` | 1 |
| [abhishektang/RL-HFT-OrderBook](https://github.com/abhishektang/RL-HFT-OrderBook) | cpp_market_making | 29 | `d7cf1f081bdd` | 0 |
| [dearvn/tradingview-pinscript-futures-binance](https://github.com/dearvn/tradingview-pinscript-futures-binance) | pine_perpetual | 31 | `68868417b771` | 0 |
| [dearvn/trading-futures-tradingview-script](https://github.com/dearvn/trading-futures-tradingview-script) | pine_futures | 89 | `15033d66146b` | 0 |
| [everget/tradingview-pinescript-indicators](https://github.com/everget/tradingview-pinescript-indicators) | pine_indicators | 213 | `7037c4bcc318` | 1 |
| [f13end/tradingview-custom-indicators](https://github.com/f13end/tradingview-custom-indicators) | pine_indicators_strategies | 155 | `be0ba0d612cb` | 1 |
| [AI4Finance-Foundation/FinRL](https://github.com/AI4Finance-Foundation/FinRL) | reinforcement_learning | 125 | `00f3596facd0` | 1 |
| [fmzquant/strategies](https://github.com/fmzquant/strategies) | multilanguage_strategies | 5807 | `7853bb2bf262` | 0 |
| [freqtrade/freqtrade-strategies](https://github.com/freqtrade/freqtrade-strategies) | python_strategies | 78 | `f3340ce11f5b` | 1 |
| [hasnocool/tradingview-pine-scripts](https://github.com/hasnocool/tradingview-pine-scripts) | pine_strategies | 1809 | `69969aeaf271` | 1 |
| [nkaz001/hftbacktest](https://github.com/nkaz001/hftbacktest) | microstructure_python_rust | 105 | `5f3ec40b2afb` | 1 |
| [hummingbot/hummingbot](https://github.com/hummingbot/hummingbot) | market_making_arbitrage | 232 | `9af100d6822d` | 1 |
| [hummingbot/hummingbot-site](https://github.com/hummingbot/hummingbot-site) | decision_docs | 840 | `31c98bf5358d` | 1 |
| [jamesbachini/Pine-Script-Examples](https://github.com/jamesbachini/Pine-Script-Examples) | pine_strategies | 9 | `6d14308f8ce4` | 1 |
| [jesse-ai/example-strategies](https://github.com/jesse-ai/example-strategies) | python_strategies | 58 | `7c91e0a37bf6` | 2 |
| [QuantConnect/Lean](https://github.com/QuantConnect/Lean) | python_csharp_strategies | 1477 | `80e7843f6456` | 1 |
| [nautechsystems/nautilus_trader](https://github.com/nautechsystems/nautilus_trader) | python_rust_strategies | 457 | `8bb7bab01020` | 6 |
| [iterativv/NostalgiaForInfinity](https://github.com/iterativv/NostalgiaForInfinity) | python_composite | 308 | `918b34e37304` | 1 |
| [Ahmed-GoCode/tradingview-strategy-collection](https://github.com/Ahmed-GoCode/tradingview-strategy-collection) | pine_strategies | 4 | `1d57c3c86e6d` | 0 |
| [LePotoRico2105/pine-script-strategies](https://github.com/LePotoRico2105/pine-script-strategies) | pine_strategies | 7 | `e8bb2935b98c` | 1 |
| [microsoft/qlib](https://github.com/microsoft/qlib) | learned_decision_models | 285 | `543552324638` | 1 |
| [tensortrade-org/tensortrade](https://github.com/tensortrade-org/tensortrade) | reinforcement_learning | 280 | `d58afba23deb` | 1 |
| [polakowo/vectorbt](https://github.com/polakowo/vectorbt) | python_notebooks | 138 | `f54802a5982c` | 3 |
| [vnpy/vnpy_algotrading](https://github.com/vnpy/vnpy_algotrading) | execution_algorithms | 27 | `bee959dc4647` | 1 |
| [vnpy/vnpy_ctastrategy](https://github.com/vnpy/vnpy_ctastrategy) | python_cta | 38 | `7a8768de9784` | 1 |
| [vnpy/vnpy_spreadtrading](https://github.com/vnpy/vnpy_spreadtrading) | spread_arbitrage | 27 | `51c9fdfb5061` | 1 |

## 决策模型与论文

| 文献/讲义 | 本地 PDF | 原始来源 |
|---|---|---|
| High-frequency trading in a limit order book | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/avellaneda-stoikov.pdf>) | [作者/论文](https://people.orie.cornell.edu/sfs33/LimitOrderBook.pdf) |
| Dealing with the Inventory Risk | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/glft.pdf>) | [作者/论文](https://arxiv.org/pdf/1105.3115) |
| Simulating and analyzing order book data: The queue-reactive model | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/queue-reactive.pdf>) | [作者/论文](https://arxiv.org/pdf/1312.0563) |
| The Impact of Heterogeneous Trading Rules on the Limit Order Book and Order Flows | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/heterogeneous-rules.pdf>) | [作者/论文](https://arxiv.org/pdf/0711.3581) |
| ABIDES: Towards High-Fidelity Market Simulation for AI Research | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/abides.pdf>) | [作者/论文](https://arxiv.org/pdf/1904.12066) |
| BSE: A Minimal Simulation of a Limit-Order-Book Stock Exchange | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/bse.pdf>) | [作者/论文](https://arxiv.org/pdf/1809.06027) |
| Fundamentals of Perpetual Futures | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/perpetual-futures.pdf>) | [作者/论文](https://arxiv.org/pdf/2212.06888) |
| High-frequency market-making with inventory constraints and directional bets | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/directional-market-making.pdf>) | [作者/论文](https://arxiv.org/pdf/1206.4810) |
| Market Making lecture | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/market-making-lecture.pdf>) | [作者/论文](https://web.stanford.edu/~ashlearn/RLForFinanceBook/MarketMaking.pdf) |
| Optimal Trade Execution lecture | [PDF](<E:/projects/MarketForge/.local/strategy-library-20261010/papers/optimal-execution-lecture.pdf>) | [作者/论文](https://web.stanford.edu/~ashlearn/RLForFinanceBook/chapter9.pdf) |

## 验证边界

完整性校验：33 个 ZIP、13,111 个原始文件和 6,689 个片段哈希，错误 0 个。PDF 校验文件头和 SHA256；未声称完整通读全部论文。

本轮未安装或运行这些第三方框架，未将候选接入默认市场，也未修改解释器。前轮仅受控运行/适配验证，不能延伸到本轮新增样本或真实市场拟合。

复现：先运行 `scripts/archive_strategy_sources.py`，再运行 `scripts/archive_strategy_documents.py` 和 `scripts/build_strategy_catalog.py`。均使用仓库 `.venv/Scripts/python.exe`。`--only source-id` 可定向重采并保留其他清单；普通重采重新生成快照，历史提交以现有 manifest 为准。
