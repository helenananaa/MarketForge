#!/usr/bin/env python3
"""Build a curated entry-point catalog over the preserved source library."""
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
import hashlib
import json
import re
import zipfile

ROOT=Path(__file__).resolve().parents[1]
LIB=ROOT/'.local/strategy-library-20261010'
# These are reading/porting candidates, not certified independent strategies.
CANDIDATES=[
 ('Pine Turtle','hasnocool',r'^Turtle Trend Trading System','行情','OHLCV','海龟趋势候选；需审计通道引用和仓位'),
 ('Pine 三重 SuperTrend','hasnocool',r'^3x SuperTrend Strategy','行情','OHLCV','多个 ATR 趋势条件，保留原版'),
 ('Pine Bollinger breakout','hasnocool',r'^Bollinger Band Breakout\.','行情','OHLCV','突破候选'),
 ('Pine Squeeze strategy','hasnocool',r'^Squeeze Momentum Strategy\.','行情','OHLCV','完整策略候选，区别于原始指标'),
 ('Pine Ichimoku pyramiding','hasnocool',r'^2Tier Ichimoku Pyramiding','行情','OHLCV','云图与加仓候选'),
 ('Pine VWAP mean reversion','hasnocool',r'^HYE Mean Reversion VWAP','行情','OHLCV','成交量加权均值回归候选'),
 ('Pine Grid','hasnocool',r'^\(IK\) Grid Script','做市网格','OHLCV+限价成交','网格候选；库存、订单生命周期必须适配'),
 ('James MA crossover','jamesbachini',r'^crossover\.ps$','行情','OHLCV','前一轮原脚本运行产生订单'),
 ('James price channels','jamesbachini',r'^price_channels\.ps$','行情','OHLCV','前一轮原脚本运行产生订单'),
 ('HalfTrend indicator','everget',r'/halftrend\.pine$','行情','OHLCV','指标构件；需定义交易规则'),
 ('ALMA indicator','everget',r'/arnaud_legoux_moving_average\.pine$','行情','OHLCV','平滑构件；需定义交易规则'),
 ('Bandtastic','freqtrade-strategies',r'/Bandtastic\.py$','行情','OHLCV','长预热、多指标组合'),
 ('CombinedBinHAndCluc','freqtrade-strategies',r'/CombinedBinHAndCluc\.py$','行情','OHLCV','布林偏离与量价条件'),
 ('ADXMomentum','freqtrade-strategies',r'/ADXMomentum\.py$','行情','OHLCV','ADX/DI/MOM/SAR 组合'),
 ('MultiRSI','freqtrade-strategies',r'/MultiRSI\.py$','行情','同商品多周期','多周期收盘对齐'),
 ('Freqtrade Supertrend','freqtrade-strategies',r'/Supertrend\.py$','行情','OHLCV','多个 SuperTrend 组合'),
 ('Freqtrade MACD','freqtrade-strategies',r'/MACDStrategy\.py$','行情','OHLCV','MACD 条件候选'),
 ('PSAR stoploss','freqtrade-strategies',r'/CustomStoplossWithPSAR\.py$','风险退出','OHLCV+持仓成交','退出模块'),
 ('BreakEven','freqtrade-strategies',r'/BreakEven\.py$','风险退出','持仓成交','保本退出模块'),
 ('Fixed risk reward','freqtrade-strategies',r'/FixedRiskRewardLoss\.py$','风险退出','OHLCV+持仓成交','风险收益退出模块'),
 ('NostalgiaForInfinity X7','nostalgia',r'^NostalgiaForInfinityX7\.py$','组合策略','原版含外部多资产','保留完整原版；单标的改写须另命名'),
 ('Freqtrade biased examples','freqtrade-strategies',r'/lookahead_bias/Zeus\.py$','隔离研究','未来偏差示例','保存作反例，排除候选生产池'),
 ('Jesse Dual Thrust','jesse-examples',r'^DUAL_THRUST/__init__\.py$','行情','OHLCV','区间突破候选'),
 ('Jesse Donchian','jesse-examples',r'^Donchian/__init__\.py$','行情','OHLCV','通道候选'),
 ('Jesse RSI2','jesse-examples',r'^RSI2/__init__\.py$','行情','OHLCV','短周期 RSI 候选'),
 ('Jesse KDJ','jesse-examples',r'^KDJstrategy/__init__\.py$','行情','OHLCV','摆动指标候选'),
 ('Jesse MACD EMA','jesse-examples',r'^MACD_EMA/__init__\.py$','行情','OHLCV','均线与动量候选'),
 ('VeighNa ATR RSI','vnpy-cta',r'/atr_rsi_strategy\.py$','行情','OHLCV','ATR 与 RSI 候选'),
 ('VeighNa Boll channel','vnpy-cta',r'/boll_channel_strategy\.py$','行情','OHLCV','布林通道候选'),
 ('VeighNa Dual Thrust','vnpy-cta',r'/dual_thrust_strategy\.py$','行情','OHLCV','区间突破候选'),
 ('VeighNa King Keltner','vnpy-cta',r'/king_keltner_strategy\.py$','行情','OHLCV','Keltner 候选'),
 ('VeighNa Turtle signals','vnpy-cta',r'/turtle_signal_strategy\.py$','行情','OHLCV','海龟信号组合'),
 ('VeighNa multi timeframe','vnpy-cta',r'/multi_timeframe_strategy\.py$','行情','同商品多周期','同商品聚合与收盘对齐'),
 ('Hummingbot PMM simple','hummingbot',r'^controllers/market_making/pmm_simple\.py$','做市网格','盘口+账户成交','基础报价控制器'),
 ('Hummingbot PMM dynamic','hummingbot',r'^controllers/market_making/pmm_dynamic\.py$','做市网格','盘口+OHLCV+账户成交','动态报价控制器'),
 ('Hummingbot Avellaneda','hummingbot',r'/avellaneda_market_making\.pyx$','做市网格','盘口+库存+到达率','库存风险做市；需本市场参数估计'),
 ('Hummingbot perpetual maker','hummingbot',r'/perpetual_market_making\.py$','做市网格','永续盘口+保证金成交','永续做市'),
 ('Hummingbot Bollinger','hummingbot',r'^controllers/directional_trading/bollinger_v1\.py$','行情','OHLCV','方向控制器'),
 ('Hummingbot MACD BB','hummingbot',r'^controllers/directional_trading/macd_bb_v1\.py$','行情','OHLCV','动量与布林控制器'),
 ('Hummingbot spot perpetual arb','hummingbot',r'/spot_perpetual_arbitrage\.py$','双商品套利','现货永续报价+两边成交','基差回归与双腿风险'),
 ('Hummingbot hedge','hummingbot',r'^hummingbot/strategy/hedge/hedge\.py$','双商品套利','两边账户+可执行报价','库存对冲；不能假设免费对冲成交'),
 ('Hummingbot funding arbitrage','hummingbot',r'^scripts/v2_funding_rate_arb\.py$','双商品套利','原版跨市场+实际资金费率','原版不是单现货单永续即插即用'),
 ('HftBacktest GLFT grid','hftbacktest',r'^examples/GLFT Market Making Model and Grid Trading\.ipynb$','做市网格','盘口逐笔+延迟队列','模型、参数估计、网格报价笔记本'),
 ('HftBacktest order book alpha','hftbacktest',r'^examples/Market Making with Alpha - Order Book Imbalance\.ipynb$','订单流','盘口逐笔+库存成交','盘口不平衡信号'),
 ('HftBacktest basis alpha','hftbacktest',r'^examples/Market Making with Alpha - Basis\.ipynb$','双商品套利','原版基差上下文待核查','单现货永续可研究的机制；未做等价适配'),
 ('HftBacktest queue maker','hftbacktest',r'^examples/Queue-Based Market Making in Large Tick Size Assets\.ipynb$','做市网格','队列+逐笔+延迟','排队与大 tick 报价'),
 ('HftBacktest Rust grid','hftbacktest',r'^hftbacktest/examples/algo\.rs$','做市网格','盘口逐笔+库存成交','原生 Rust 算法入口'),
 ('Nautilus Rust grid','nautilus',r'^crates/trading/src/examples/strategies/grid_mm/strategy\.rs$','做市网格','盘口+账户成交','原生 Rust 网格做市候选'),
 ('Nautilus Rust composite maker','nautilus',r'^crates/trading/src/examples/strategies/composite_market_maker/strategy\.rs$','做市网格','盘口逐笔+账户成交','组合做市候选，适配模型输入'),
 ('Nautilus Rust Hurst VPIN','nautilus',r'^crates/trading/src/examples/strategies/hurst_vpin_directional/strategy\.rs$','订单流','逐笔成交+成交方向','Hurst/VPIN 方向候选'),
 ('Nautilus Rust EMA','nautilus',r'^crates/trading/src/examples/strategies/ema_cross/strategy\.rs$','行情','OHLCV','原生 Rust 均线候选'),
 ('VeighNa spread arb','vnpy-spread',r'/statistical_arbitrage_strategy\.py$','双商品套利','两腿盘口+合约单位+成交','价差机制可研究；须改现货永续腿定义'),
 ('VeighNa TWAP','vnpy-algo',r'/twap_algo\.py$','拆单执行','目标需求+时间+成交','时间拆单；需要真实目标需求'),
 ('VeighNa iceberg','vnpy-algo',r'/iceberg_algo\.py$','拆单执行','目标需求+盘口成交','隐藏大单分批执行'),
 ('VeighNa best limit','vnpy-algo',r'/best_limit_algo\.py$','拆单执行','目标需求+最优报价成交','追踪最佳限价'),
 ('VeighNa sniper','vnpy-algo',r'/sniper_algo\.py$','拆单执行','目标需求+盘口成交','达到目标价格时执行'),
 ('ABIDES TWAP','abides',r'/TWAPExecutionAgent\.py$','拆单执行','目标需求+时间+成交','独立需求执行者'),
 ('ABIDES POV','abides',r'/POVExecutionAgent\.py$','拆单执行','目标需求+逐笔成交量','参与率执行者'),
 ('ABIDES VWAP','abides',r'/VWAPExecutionAgent\.py$','拆单执行','目标需求+历史成交量','量曲线执行者'),
 ('Hummingbot DCA executor','hummingbot',r'/dca_executor/dca_executor\.py$','拆单执行','目标需求+账户成交','分批买卖执行器'),
 ('Hummingbot TWAP executor','hummingbot',r'/twap_executor/twap_executor\.py$','拆单执行','目标需求+账户成交','时间执行器'),
 ('Almgren Chriss','almgren-chriss',r'^almgren_chriss/model\.py$','拆单执行','目标库存+波动率冲击参数','最优执行模型；不是方向 alpha'),
 ('LEAN C# VWAP','lean',r'^Algorithm.Framework/Execution/VolumeWeightedAveragePriceExecutionModel\.cs$','拆单执行','目标需求+成交量+成交','C# 执行模型'),
 ('LEAN C# std execution','lean',r'^Algorithm.Framework/Execution/StandardDeviationExecutionModel\.cs$','拆单执行','目标需求+OHLCV成交','波动条件执行'),
 ('LEAN C# trailing risk','lean',r'^Algorithm.Framework/Risk/TrailingStopRiskManagementModel\.cs$','风险退出','实际仓位成交','追踪止损风险模块'),
 ('LEAN C# EMA alpha','lean',r'^Algorithm.Framework/Alphas/EmaCrossAlphaModel\.cs$','行情','OHLCV','C# 信号模块'),
 ('LEAN C# RSI alpha','lean',r'^Algorithm.Framework/Alphas/RsiAlphaModel\.cs$','行情','OHLCV','C# 信号模块'),
 ('ABIDES Noise','abides',r'^agent/NoiseAgent\.py$','交易者决策','盘口+账户+私有需求','噪声参与者，保留资金与库存边界'),
 ('ABIDES Momentum','abides',r'^agent/examples/MomentumAgent\.py$','交易者决策','同商品历史报价','动量参与者'),
 ('ABIDES order imbalance','abides',r'^agent/OrderBookImbalanceAgent\.py$','订单流','盘口+账户成交','订单簿不平衡参与者'),
 ('ABIDES HBL','abides',r'^agent/HeuristicBeliefLearningAgent\.py$','交易者决策','原版 oracle+近期订单流','学习成交信念；原版价值 oracle 需替换且另命名'),
 ('ABIDES Value','abides',r'^agent/ValueAgent\.py$','隔离研究','原版外生价值 oracle','只有两个行情源不能获得原版真实价值'),
 ('ABIDES Zero Intelligence','abides',r'^agent/ZeroIntelligenceAgent\.py$','交易者决策','原版 oracle+私有价值','可研究私有偏好与报价机制；不能伪造 oracle'),
 ('ABIDES adaptive maker','abides-jpmc',r'/adaptive_market_maker_agent\.py$','做市网格','盘口+库存成交','自适应做市参与者'),
 ('BSE ZIC','bse',r'^BSE\.py$','交易者决策','私有需求限价+盘口','类 TraderZIC；有约束随机报价'),
 ('BSE ZIP','bse',r'^BSE\.py$','交易者决策','私有需求限价+报价成交','类 TraderZIP；自适应利润报价'),
 ('BSE PRZI PRSH PRDE','bse',r'^BSE\.py$','交易者决策','私有需求限价+成交反馈','类 TraderPRZI；参数化随机、自适应搜索变体'),
 ('BSE AA','bse',r'^Trader_AA\.py$','交易者决策','私有需求限价+报价成交','自适应激进程度；需求供给不可照搬外部 oracle'),
 ('BSE shaver sniper','bse',r'^BSE\.py$','交易者决策','私有需求限价+盘口','类 TraderShaver/TraderSniper；报价/时机机制'),
 ('FMZ Python grid','fmz',r'^grid\.md$','做市网格','同商品行情+账户成交','保留 FMZ 平台 API 依赖'),
 ('FMZ Javascript iceberg','fmz',r'^Iceberg-Buy-Order\.md$','拆单执行','目标需求+盘口成交','JavaScript 执行源码已提取'),
 ('FMZ Pine Dual Thrust','fmz',r'^Dual-Thrust-Trading-Algorithm-ps4\.md$','行情','OHLCV','Pine 文档源码已提取'),
 ('FMZ MyLanguage turtle','fmz',r'^M-Language-Turtle-Trading-strategy-implementationsV-10\.md$','行情','OHLCV','麦语言原版已保留'),
 ('FinRL SB3 agents','finrl',r'^finrl/agents/stablebaselines3/models\.py$','学习模型','训练环境+权重+原版多资产','候选算法框架，尚无本市场训练/策略资格'),
 ('TensorTrade DQN','tensortrade',r'^tensortrade/agents/dqn_agent\.py$','学习模型','训练环境+权重','DQN 决策框架，需构造因果状态与奖励'),
 ('Qlib RL execution','qlib',r'^qlib/rl/order_execution/policy\.py$','学习模型','目标需求+训练权重+市场适配','学习执行政策框架'),
 ('C++ orderbook agent','cpp-mm',r'^agent/rl_agent\.cpp$','学习模型','盘口成交+学习机制待核查','名字含 RL 不代表已经验证训练能力'),
]


def link(path):
    return '<'+Path(path).as_posix()+'>'


def verify_source(source, rows):
    errors=[]
    path=Path(source['snapshot'])
    if hashlib.sha256(path.read_bytes()).hexdigest()!=source['snapshot_sha256']:
        errors.append('snapshot_hash:'+source['id'])
    with zipfile.ZipFile(path) as archive:
        if set(archive.namelist())!={x['path'] for x in rows}:
            errors.append('snapshot_inventory:'+source['id'])
        for row in rows:
            # ZipFile.read checks CRC as well; no second decompression pass.
            local=Path(row['local_path']).read_bytes(); zipped=archive.read(row['path'])
            if hashlib.sha256(local).hexdigest()!=row['sha256'] or zipped!=local:
                errors.append('file_hash:'+source['id']+'/'+row['path'])
    if source.get('original_name_archive'):
        with zipfile.ZipFile(source['original_name_archive']) as archive:
            for row in rows:
                if hashlib.sha256(archive.read(row['path'])).hexdigest()!=row['sha256']:
                    errors.append('original_archive:'+row['path'])
    return source['id'],len(rows),errors


def main():
    manifest=json.loads((LIB/'manifest.json').read_text(encoding='utf-8'))
    inventory=[json.loads(x) for x in (LIB/'inventory.jsonl').read_text(encoding='utf-8').splitlines()]
    extracts=[json.loads(x) for x in (LIB/'extracts.jsonl').read_text(encoding='utf-8').splitlines()]
    candidates=[]; unresolved=[]
    for name,sid,pattern,role,data,note in CANDIDATES:
        hits=[x for x in inventory if x['source_id']==sid and re.search(pattern,x['path'],re.I)]
        if not hits:
            unresolved.append(dict(name=name,source_id=sid,pattern=pattern)); continue
        item=sorted(hits,key=lambda x:(len(x['path']),x['path']))[0]
        candidates.append(dict(name=name,role=role,data_dependencies=data,note=note,source_id=sid,
            path=item['path'],local_path=item['local_path'],source_url=item['source_url'],sha256=item['sha256'],
            extracted_fragments=[x['path'] for x in extracts if x['source_id']==sid and x['parent']==item['path']],
            status='reading_and_porting_candidate',execution_status='not_qualified',license_review='per_file_pending'))
    (LIB/'candidates.json').write_text(json.dumps(dict(candidates=candidates,unresolved=unresolved),ensure_ascii=False,indent=2),encoding='utf-8')
    documents=json.loads((LIB/'documents.json').read_text(encoding='utf-8'))
    tv=json.loads((LIB/'tradingview.json').read_text(encoding='utf-8'))
    receipt=dict(sources=len(manifest['sources']),saved_sources=sum(x['status']=='saved' for x in manifest['sources']),
        files=len(inventory),pine_files=sum(x['language']=='pine' for x in inventory),extracts=len(extracts),
        extract_languages=dict(Counter(x['language'] for x in extracts)),tv_captures=len(tv),
        papers_saved=sum(x['pdf']['status']=='saved' for x in documents),candidate_entries=len(candidates),
        unresolved_candidates=unresolved,failed_sources=[x for x in manifest['sources'] if x['status']!='saved'])
    errors=[]; checked_files=0; checked_zips=0
    with ThreadPoolExecutor(max_workers=3) as pool:
        jobs=[pool.submit(verify_source,source,[x for x in inventory if x['source_id']==source['id']]) for source in manifest['sources'] if source['status']=='saved']
        for job in as_completed(jobs):
            sid,count,issues=job.result()
            checked_files+=count; checked_zips+=1; errors.extend(issues)
            print('verified',sid,count,'errors',len(issues),flush=True)
    for row in extracts:
        if hashlib.sha256((LIB/row['path']).read_bytes()).hexdigest()!=row['sha256']:
            errors.append('extract_hash:'+row['path'])
    for row in documents:
        pdf=row['pdf']
        if pdf['status']=='saved':
            data=Path(pdf['local_path']).read_bytes()
            if not data.startswith(b'%PDF-') or hashlib.sha256(data).hexdigest()!=pdf['sha256']:
                errors.append('document_hash:'+row['id'])
    receipt.update(integrity_errors=errors,checked_source_files=checked_files,checked_snapshots=checked_zips,
        snapshot_bytes=sum(Path(x['snapshot']).stat().st_size for x in manifest['sources'] if x['status']=='saved'),
        source_bytes=sum(x['size_bytes'] for x in inventory))
    (LIB/'verification.json').write_text(json.dumps(receipt,ensure_ascii=False,indent=2),encoding='utf-8')
    lines=['# 本地背景策略来源库','', '采集日期：2026-10-10。此轮目标是广泛保存，解释器支持范围不作为淘汰条件。', '',
        f"已保存 **{receipt['saved_sources']} 个 GitHub 来源、{receipt['files']:,} 个文件、{receipt['extracts']:,} 个代码片段、{receipt['tv_captures']} 份 TradingView 源码转录和 {receipt['papers_saved']} 份 PDF**。",'',
        f"原始文件中直接识别的 Pine 文件 {receipt['pine_files']:,} 个；代码片段语言分布 `{json.dumps(receipt['extract_languages'],ensure_ascii=False)}`。",'',
        f"下面 {len(candidates)} 个具名入口包括指标、策略、执行器、风险模块、同公式的不同实现和学习框架，**不是独立可用 bot 数量，也不是盈利排名**。此前热门 TradingView 的页面人气与 223 个 Pine 文件测试见 [前轮报告]({link(ROOT/'docs/validation/2026-10-10-strategy-research.md')})。",'',
        '## 入口与保存范围','',
        f"- [离线检索全部文件与代码片段]({link(LIB/'index.html')})",f"- [具名候选 JSON]({link(LIB/'candidates.json')})",f"- [固定提交、许可文件和 ZIP 清单]({link(LIB/'manifest.json')})",f"- [完整性验证回执]({link(LIB/'verification.json')})",'',
        '`repos/` 是任务内下载的仓库；4 个前轮来源复用原目录。`snapshots/` 为独立 ZIP，选定策略路径及根目录文件的快照；不是所有来源的完整依赖。`extracts/` 保留 Notebook 代码及 Markdown 源码片段，每个片段有来源与哈希 sidecar。`papers/` 保留论文/作者讲义和部分摘要页。`tradingview/` 为可见源码文本转录（NBSP 归一），非字节精确的上游文件。`previous-probes/` 保存前轮 22 个公式适配和受控测试证据。', '',
        '仓库 ZIP 对应本地 checkout 内容，可能含 Git 换行规范化；哈希针对保存的本地字节。Windows 非法文件名另保留原名 ZIP、可读副本和映射。不支持、含外部数据依赖或疑似未来偏差的原版仍保留，接入资格单独判定。', '',
        '## 数据与接入判断','',
        '- OHLCV 类可使用这两个商品各自历史；多周期必须因果聚合、避免未收盘高周期泄漏。',
        '- 做市与订单流只需本市场数据，但需要盘口、逐笔成交、订单生命周期和真实账户反馈，K 线不足以重建队列。',
        '- 现货永续基差需要两边可执行报价和成交；资金费率需平台实际费率、结算时点及保证金规则。',
        '- TWAP/VWAP/POV/冰山/Almgren–Chriss 是执行政策，需要给定目标需求，不能自行假定市场方向收益。',
        '- BSE 私有需求限价和 ABIDES 外生价值 oracle 是不同概念。可研究私有偏好与预算约束；只有两种行情时不能照搬真实价值 oracle。',
        '- RL/机器学习保存的是算法、环境与示例。训练权重、输入状态、奖励、时间切分与本市场资格均待验证。',
        '- `lookahead_bias` 原版作隔离反例。静态标记只是搜索线索，尚未逐个因果/重绘审计。',
        '- 保存许可文件不等于已取得统一商用或再分发许可；接入时逐文件核对，保留署名和许可要求。', '',
        '策略数量增加本身不能保证深度或真实性。后续应按角色、资本、库存目标、时钟/延迟、风险预算分散，并实测价差、可成交深度、冲击、撤单、拥挤和双市场联动。', '',
        '## 具名入口','', '| 入口 | 角色 | 数据/适配依赖 | 备注 | 本地原版 |','|---|---|---|---|---|']
    for x in candidates:
        lines.append('| '+ ' | '.join([x['name'],x['role'],x['data_dependencies'],x['note'],f"[源码]({link(x['local_path'])})"])+' |')
    lines.extend(['','## 来源固定版本','', '| 来源 | 类型 | 文件 | 固定提交 | 许可文件 |','|---|---|---:|---|---|'])
    for x in manifest['sources']:
        lines.append(f"| [{x['repo']}]({x['url']}) | {x['category']} | {x.get('file_count',0)} | `{x.get('commit','')[:12]}` | {len(x.get('license_files',[]))} |")
    lines.extend(['','## 决策模型与论文','', '| 文献/讲义 | 本地 PDF | 原始来源 |','|---|---|---|'])
    for x in documents:
        pdf=x['pdf']; local=f"[PDF]({link(pdf['local_path'])})" if pdf['status']=='saved' else '下载失败，原因保留在 documents.json'
        lines.append(f"| {x['title']} | {local} | [作者/论文]({pdf['url']}) |")
    lines.extend(['','## 验证边界','',
        f"完整性校验：{checked_zips} 个 ZIP、{checked_files:,} 个原始文件和 {len(extracts):,} 个片段哈希，错误 {len(errors)} 个。PDF 校验文件头和 SHA256；未声称完整通读全部论文。",'',
        '本轮未安装或运行这些第三方框架，未将候选接入默认市场，也未修改解释器。前轮仅受控运行/适配验证，不能延伸到本轮新增样本或真实市场拟合。', '',
        '复现：先运行 `scripts/archive_strategy_sources.py`，再运行 `scripts/archive_strategy_documents.py` 和 `scripts/build_strategy_catalog.py`。均使用仓库 `.venv/Scripts/python.exe`。`--only source-id` 可定向重采并保留其他清单；普通重采重新生成快照，历史提交以现有 manifest 为准。'])
    content='\n'.join(lines)+'\n'
    (LIB/'README.md').write_text(content,encoding='utf-8')
    (ROOT/'docs/STRATEGY_SOURCE_LIBRARY.md').write_text(content,encoding='utf-8')
    display={k:v for k,v in receipt.items() if k!='integrity_errors'}
    display.update(integrity_error_count=len(errors),error_samples=errors[:5])
    print(json.dumps(display,ensure_ascii=False),flush=True)
    if errors or unresolved or receipt['failed_sources']:
        raise SystemExit(1)


if __name__=='__main__':
    main()
