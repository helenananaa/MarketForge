# 单现货＋对应永续：背景策略来源与 Pine 实测

日期：2026-10-10。宿主：MarketForge `main`，HEAD `71b58e9`。
解释器：本机 `.venv` 中已安装的 `pine-compat-runtime==0.3.1`；不是以另一个
解释器仓库的未提交代码或最新版本代替宿主使用的版本。

## 结论

可以明显扩大行为来源。已找到并实际运行 223 个公开 Pine 文件，其中 165 个运行
通过、58 个拒绝执行；样本包含指标、研究和工具文件，且不同来源可能重复同一公式，
**不能解释为 165 个独立、可直接上场的交易策略，也不是社区总体兼容率**。

另外构造了 22 个公式级 long/flat 策略适配版，全部通过 6 类合成行情运行、抽样前缀
一致性和现有 Pine bot 的进程内受控账户反馈测试。没有启动模拟房间，没有验证
真实 MarketForge 撮合、盈利能力、TradingView 数值一致性或市场真实性。

本次新增的是研究脚本与证据。22 份生成的 Pine 文件在
`.local/strategy-research-20261010/run/`，尚未安装到生产插件或更改默认市场配方。
外部源码保存在 `.local/`，保留作者、来源和上游许可。解释器仓库原有改动未触碰。

## 数据边界

- 单商品 OHLCV 足够：均线、MACD、RSI、布林、通道、ATR、ADX、WaveTrend、
  Squeeze、Ichimoku、CCI、Stochastic、ROC、成交量指标。
- 多周期可以由同一商品的成交历史聚合，不能让未收盘高周期 bar 泄漏未来数据。
- 现货与永续价差只需两个商品，但交易需要两边可执行报价、实际账户与成交反馈。
- 做市、订单流和微观盘口策略需要平台自身盘口/成交/撤单数据；只有 OHLCV 无法
  重建队列、挂单存活、逆向选择或逐笔成交强度。
- 资金费率策略需要平台实际公布的费率、结算时间和保证金/清算规则，不能仅从
  两张 K 线猜测费率。内部派生数据可以用，外部 BTC 指数、新闻、链上、财报不能伪造。
- 现货无借币时只做多/持币出售；负基差反向套利不能假设无限可卖的现货库存。

## TradingView 直接读取的公开脚本

人气为本次浏览器页面显示的 boosts，抓取日为 2026-10-10；不是收益排名。
源码从页面 Source code 的可见文本转录，NBSP 归一为空格，非上游字节级归档。
结果是 512 根合成震荡行情、默认参数、`V-BTC-SPOT`、1 分钟的运行结果。

| 脚本/作者 | boosts | 实测结果 | 作为背景 bot 的角色 |
|---|---:|---|---|
| [Squeeze Momentum / LazyBear](https://www.tradingview.com/script/nqQ1DT5a-Squeeze-Momentum-Indicator-LazyBear/) | 117042 | 2 个 plot，无错误 | 压缩后动量交易；需补开平仓规则 |
| [SuperTrend / KivancOzbilgic](https://www.tradingview.com/script/r6dAP7yi/) | 83265 | 3 个 plot、26 个 alert，无错误 | ATR 趋势、反转与退出 |
| [WaveTrend / LazyBear](https://www.tradingview.com/script/2KE8wTuF-Indicator-WaveTrend-Oscillator-WT/) | 61097 | 8 个 plot，无错误 | 超买超卖和均值回归 |
| [UT Bot Alerts / QuantNomad](https://www.tradingview.com/script/n8ss8BID-UT-Bot-Alerts/) | 59915 | 19 个 alert，无错误 | ATR 跟随；默认关闭 Heikin Ashi |
| [Chandelier Exit / everget](https://www.tradingview.com/script/AqXxNS7j-Chandelier-Exit-everget/) | 29464 | 3 个 plot、26 个 alert，无错误 | 波动率追踪退出、趋势翻转 |
| [UT Bot Strategy / QuantNomad](https://www.tradingview.com/script/VKMgZdYr-UT-Bot-Strategy/) | 8915 | 19 次订单、18 笔闭合交易 | 原版确实能交易；其多空语义不符合当前现货插件 |
| [SSL Channel / ErwinBeckers](https://www.tradingview.com/script/xzIoaIJC-SSL-channel/) | 4422 | 2 个 plot，无错误 | 高低均线通道、状态持续趋势 |

Squeeze 页面作者明确指出旧版源码的 BB multiplier 误用 `multKC`，并链接修正版。
原源码测试保留这个问题，公式适配版使用 2.0 BB、1.5 KC；不能声称与旧版原脚本一致。
UT 的默认 HA=false 能运行；切换 HA=true 时缺失 Heikin Ashi 请求数据，明确拒绝。

## GitHub 扩展样本

| 来源 | 固定提交 | 文件实测 | 用途 |
|---|---|---|---|
| [everget 指标库](https://github.com/everget/tradingview-pinescript-indicators) | `7037c4bcc3181ab24af4b15a8ecf265487841126` | 210 个：153 通过、57 拒绝 | 大量可拆分的指标/退出模块；不是完整交易策略库 |
| [James Bachini 示例](https://github.com/jamesbachini/Pine-Script-Examples) | `6d14308f8ce42b2d60ec6c80eb224e8f4ffb3a79` | 6 个：5 通过、1 拒绝 | MIT 示例，便于进一步适配 |
| [Freqtrade 官方策略库](https://github.com/freqtrade/freqtrade-strategies) | `f3340ce11f5bdf62f598522e64d1f5638eaa13f5` | 68 个 Python 文件静态检查范围；未执行 Freqtrade | Python 行为来源，无须强行转换 Pine |
| [NostalgiaForInfinity](https://github.com/iterativv/NostalgiaForInfinity) | `918b34e3730496839a9aac670fee5c5b05039b84` | 静态核查 X7 数据依赖；未执行 | 多规则组合与风险管理参考 |

James `crossover.ps` 实际产生 17 次订单、8 笔闭合交易；`price_channels.ps`
产生 13 次订单、6 笔闭合交易。`breakout_sniper.ps` 虽可运行，在此次震荡样本
365 根回看默认参数下没有订单，不能把“可运行”当成“已验证产生交易”。

everget 可用模块还包括 HalfTrend、Chandelier、SuperTrend、HMA、FRAMA、ALMA、
Awesome Oscillator、MFI、TSI、Stochastic Momentum Index。它们适合作为策略组合
的构件，需要分别定义触发、退出、仓位与执行规则。

### Freqtrade 值得再适配的具名方案

| 方案 | 源码观察到的机制 | 本约束下的处理 |
|---|---|---|
| Bandtastic | 多宽度布林＋RSI/MFI/EMA 条件、止盈与追踪退出 | 单商品可用，但 `startup_candle_count=999`；默认 512 horizon 不够 |
| CombinedBinHAndCluc | 布林偏离、实体/下影、EMA、成交量条件 | 单商品可用；保留组合条件，不能简化后声称等价 |
| ADXMomentum | ADX、DI、MOM、SAR，双向动量退出 | 单商品可用；当前 `adx` probe 仅核心 DI/ADX，不是完整移植 |
| MultiRSI | 同一商品多周期 RSI | 能聚合周期；Pine 宿主需补 request 输入与收盘对齐 |
| FAdxSmaStrategy | ADX 和快慢 SMA 条件 | 可做永续双向方案，需真实保证金/成交适配 |
| FSupertrendStrategy / Supertrend | 多个 SuperTrend 条件组合 | 单商品足够；不同周期/ATR 参数可增加响应差异 |
| TrendFollowingStrategy / VolatilitySystem | 期货趋势/波动系统 | 数据可来自本永续；未执行原 Python 策略 |
| CustomStoplossWithPSAR / FixedRiskRewardLoss / BreakEven | 退出与风险管理模块 | 用于分散退出行为，当前 Pine 插件的 `strategy.exit` 未支持 |
| TWAPStrategy / AlmgrenChrissStrategy | 执行与拆单行为 | 更适合原生/Python bot；目标交易需求应先给定，不能假装来自外部机构 |
| NostalgiaForInfinity X7 | 多规则组合、自身多周期与 BTC informative context | 默认有 `btc_info_timeframes=["4h"]`；虚拟单标的不能伪造真实 BTC 宏观过滤 |
| TrendRiderStrategy | 多周期且读取 `BTC/USDT:USDT` | 删除外部过滤会改变策略定义，应单独命名适配版 |

Freqtrade 的 `lookahead_bias/` 明确是有未来偏差的示例目录：DevilStra、GodStraNew、
Zeus、wtc 使用全样本 min/max 或 scaler 等，不应进入背景策略池。
[官方 lookahead-analysis](https://www.freqtrade.io/en/stable/lookahead-analysis/)
给出了按信号切片比较结果的验证方式。本次没有声称为所有 Python 源码完成偏差审计。

## 22 个实测公式适配版

全部使用固定整数 qty=1、单 entry ID、现货 long/flat，每根收盘 bar 最多一个意图。
这些是常见公式的研究实现，**不是 22 份完整热门策略的等价复刻**。

| 类别 | 本次可执行文件 | 行为 |
|---|---|---|
| 趋势 | ema_cross、macd、supertrend、ut_atr、ssl、adx、ichimoku、roc | 延续、转向、风险退出 |
| 通道突破 | bb_breakout、donchian、turtle、keltner | 突破追随；Donchian/Turtle 使用前一根历史通道 |
| 均值回归 | rsi_reversion、bb_reversion、wavetrend、cci、stochastic、stoch_rsi、williams、vwap_reversion | 下跌承接与反弹卖出 |
| 波动压缩释放 | squeeze | 低波动后释放，线性回归动量过滤 |
| 成交量趋势 | obv | 量价累计与均线判断 |

验证：6 类行情各 512 根，共 132 个完整适配运行；产生 1600 次原生模拟订单。
每次额外检查 128/257 两个历史前缀，264 次前缀比较全部通过。恒定价格样本中
22 个适配版都没有订单。所有策略在至少一种非恒定行情中产生订单。

适配器测试：22×256=5632 根逐步收盘观察，使用完整 OHLCV、账户和累计成交回执；
73 次买入、64 次卖出意图，重复观察不重新下单。调用的是当前插件 `decide()`，
没有调用真实 server/process scheduler；填单是测试夹具按收盘价格生成，不能称为
真实撮合验收。缺少 TradingView 对照数据，因此没有验证数值或 broker 行为等价。

## 解释器与宿主分别有哪些缺口

| 功能探针 | 解释器 | MarketForge 当前 Pine 适配 |
|---|---|---|
| short entry | 通过、1 次订单 | 拒绝现货做空 |
| limit entry | 通过、1 次订单 | 拒绝挂单 |
| stop＋limit bracket | 通过、2 次订单、1 笔闭合交易 | 拒绝 exit 意图 |
| request.security 永续 close | 显式提供第二商品数据后通过；价差逐根核验为 100 | 没有传入第二商品请求数据，拒绝 |
| MLExtensions import | 没有提供库源码时拒绝 | 同样拒绝；不能声称整个 Lorentzian 原脚本已测试 |
| UT 的 HA=true | 未提供变换数据时拒绝 | 缺少宿主变换与输入支持 |

everget 的 57 个拒绝结果按首条错误分组：35 个未知旧版函数、7 个 `behind_chart`
声明参数、4 个未知变量、3 个缺少显式 `timenow`、2 个解析失败、5 个缺少外部
request 数据、1 个函数中集合变更限制。James Gaussian Regression 另有
`matrix.sum` 参数数目/后续 `pinv` 接收者错误。

旧别名 `roc/cum/sin/sar` 等失败不代表对应 v6 `ta.*`/`math.*` 公式必然不可用；
本次 ROC、OBV 等已通过 v6 probe。但尚未逐个迁移这 58 个失败文件。
`behind_chart` 属于绘图元数据，可以考虑仅去除该参数后另立适配结果；本次没有
改掉原脚本再把它计为原版通过。缺失财报、国债、真实 BTC/hashrate 数据的脚本
在这个市场定义下应排除，而不是补假数据。

宿主还有 `history_limit` 默认 512、最大 2048，达到边界暂停；每次观察进程启动并
重建历史。此次没有证明几千个 Pine bot 长期运行的性能和存档接续。

## 直接改善深度的非 Pine 来源

| 外部机制来源 | 作用 | 与现有代码关系 |
|---|---|---|
| [Hummingbot Pure Market Making](https://hummingbot.org/strategies/v1-strategies/pure-market-making/) | 多层双边挂单、刷新、库存偏移 | 扩展/校准现有 `DynamicMarketMaker` |
| [Avellaneda Market Making](https://hummingbot.org/strategies/v1-strategies/avellaneda-market-making/) | 库存、波动率和成交强度共同决定报价 | 参考模型，不声称现有做市商已等价实现 AS |
| [Perpetual Market Making](https://hummingbot.org/strategies/v1-strategies/perpetual-market-making/) | 永续双边报价与仓位管理 | 原生 adapter 更适合保证金、reduce-only 与持续撤挂 |
| [Spot Perpetual Arbitrage](https://hummingbot.org/strategies/v1-strategies/spot-perpetual-arbitrage/) | 价差扩大建双腿、收敛平双腿 | 已有 `BasisArbitrageTrader`，实际双腿成交协调、超时解套 |
| 平台自身资金费率与拆单执行 | 跨结算持仓需求、持续买卖需求 | 已有 `FundingRateTrader`、TWAP/POV 原生行为 |

当前代码已有这些原生基础；本次是源码确认，没有重新做匹配闭环验收。
基差机器人不是无风险原子交易，部分成交、费用、仓位残差与保证金风险都必须保留。

策略更多可以扩大反应方式，但市场深度由持续的双边挂单、可用库存/资本、撤单和
补单速度决定。大量同周期、同参数 Pine 趋势 bot 会一起吃单，可能放大单边流动性
枯竭。建议先增加策略族，再分散周期、阈值、资金、响应延迟、退出、订单类型；
用信号相关性和订单方向聚集检验“有效多样性”，不按脚本个数算独立行为。

下一步应先用现有做市/套利原生 bot 建立有限资金的双边底盘，再安装挑选的
Pine long/flat 样本。永续和持久挂单的 Pine adapter 扩展单独验收。比较同总资金、
同订单活动预算、多个 seed 的有/无新增策略配方，观察 spread、近档深度、冲击后
恢复、收益自相关、波动聚集、基差、清算和一侧枯竭；禁止以图形更像行情作为唯一证据。

## 复现与证据

完整逐文件结果、源码 SHA256、适配版订单和错误见
`docs/validation/2026-10-10-strategy-research-results.json`。

```powershell
.venv/Scripts/python.exe scripts/research_strategy_compat.py --originals .local/strategy-research-20261010/originals --output .local/strategy-research-20261010/run --bot-replay
.venv/Scripts/python.exe scripts/research_strategy_compat.py --originals .local/strategy-research-20261010/everget --output .local/strategy-research-20261010/everget-run --skip-adaptations
.venv/Scripts/python.exe scripts/research_strategy_compat.py --originals .local/strategy-research-20261010/jamesbachini --output .local/strategy-research-20261010/james-run --skip-adaptations
.venv/Scripts/python.exe scripts/research_strategy_boundaries.py .local/strategy-research-20261010/boundaries.json
```

GitHub 样本可由上表固定提交恢复；页面转录文件仍保存在本机 `.local/`。
首轮 adapter 夹具遗漏了 60 秒 interval 字段，因宿主契约检查全部拒绝；修正的是
研究夹具，不是生产逻辑。报告使用修正后的完整复测结果。

运行中若干 Rust 宿主文件出现其他工作的修改，本次没有编辑、提交或覆盖。
