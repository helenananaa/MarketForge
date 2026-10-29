"""Render all fixed-seed outcomes, including failed mechanism profiles."""
import argparse,csv,json,shutil,statistics
from pathlib import Path
from research_market_mechanisms import ROOT,write,sha
from research_price_formation import BASE,PREVIOUS


def render(out):
    new=json.loads((out/'comparison.json').read_text(encoding='utf-8'));assert new['complete']
    old=json.loads((PREVIOUS/'full-comparison.json').read_text(encoding='utf-8'))
    if new.get('control_directory'):
        old=dict(rows=new['previous_full_rows'],market_links=new['previous_full_market_links'],metrics=new['control_metrics'])
    pilot=json.loads((BASE/'pilot/comparison.json').read_text(encoding='utf-8'))
    selection=json.loads((BASE/'selection-decision.json').read_text(encoding='utf-8'))
    verification=json.loads((BASE/'verification.json').read_text(encoding='utf-8'))
    short=verification.get('short_horizon_only',False)
    duration=new['metrics'][0]['steps']/3600
    primary_minutes=int((new['metrics'][0]['steps']*1000-new['metrics'][0]['warmup_ms'])/60000)
    profile=new['rows'][0]['profile'];dest=ROOT/'docs/validation';prefix='2026-10-25-market-price-formation'
    rows=[r for r in old['rows']if r['profile']=='dual-execution']+new['rows']
    links=[r for r in old['market_links']if r['profile']=='dual-execution']+new['market_links']
    means=[]
    for market in ('V-BTC-SPOT','V-BTC-PERP'):
        for label in ('dual-execution',profile):
            rs=[r for r in rows if r['market']==market and r['profile']==label]
            means.append(dict(market=market,profile=label,**{k:statistics.mean(r[k]for r in rs)for k in ('std_bp','wick_1m','wick_5m','both_outer_quarters_5m','acf1','zero_return_fraction_1m','two_sided_pct')},volume=sum(r['volume']for r in rs)))
    lines=['# 拆单之后：持续价格形成与现货／永续联动','',
        '本轮控制为上一轮长影线修复后的双触价执行组合（dual-execution），并非更早的扫单市场。只改变显式做市机制开关，没有调整波动率、价差、订单量、风险、指数权重、手续费、资金或套利容量的数值。OHLCV直接聚合真实撮合成交。','',
        '## 结论与选择','',json.loads((BASE/'final-assessment.json').read_text(encoding='utf-8'))['conclusion']if (BASE/'final-assessment.json').exists()else selection['conclusion'],'',selection['reason'],'',
        '## 四小时固定种子配对结果'if short else'## 完整固定种子对照','',
        f'300个有限资金bot，种子41、7、19、73，每组{duration:g}小时，1秒观察节奏；主统计固定为[2h,{duration:g}h)，每市场每种子{primary_minutes}根1m、{primary_minutes//5}根5m柱。主比较使用Linux同平台控制／候选配对，没有混入Windows旧控制。种子41复用已完整跑出的Linux4h原始回执并核对哈希，其余三种子为新配对。下面是逐种子指标的算术均值（影线为逐种子中位数再取均值），成交量为四种子合计。没有合并中位数或选择性丢弃种子。','',
        '**12小时长周期复验未完成。** 两次长尝试被主机内存保护停止，用户明确选择以四小时对照收尾；本轮短窗口改善不能替代长周期资格。'if short else'完整长窗口结果如下。','',
        '|市场／机制|分钟标准差bp|1m影线占比|5m影线占比|5m双长影线|分钟收益ACF1|零收益分钟|双边盘口|实际成交手数|',
        '|---|---:|---:|---:|---:|---:|---:|---:|---:|']
    for r in means:lines.append(f"|{r['market']} / {r['profile']}|{r['std_bp']:.3f}|{r['wick_1m']:.1%}|{r['wick_5m']:.1%}|{r['both_outer_quarters_5m']:.1%}|{r['acf1']:.3f}|{r['zero_return_fraction_1m']:.1%}|{r['two_sided_pct']:.2f}%|{r['volume']:,.0f}|")
    lines+=['','影线占比为(上影+下影)/振幅的非零振幅柱中位数；双长影线为上下影线各占振幅至少25%。零振幅、非空柱、15m／60m、启动30分钟、尾部和成交角色另保存在逐种子指标中；短主窗口仅两根60m柱，不能据其认证小时形态。','',
        '|种子／市场|分钟标准差bp：前→后|5m影线：前→后|双长影线：前→后|收益ACF1：前→后|成交量保留|','|---|---:|---:|---:|---:|---:|']
    regressions=[]
    for n in new['rows']:
        o=next(r for r in rows if r['seed']==n['seed']and r['market']==n['market']and r['profile']=='dual-execution')
        lines.append(f"|{n['seed']} / {n['market']}|{o['std_bp']:.3f} → {n['std_bp']:.3f}|{o['wick_5m']:.1%} → {n['wick_5m']:.1%}|{o['both_outer_quarters_5m']:.1%} → {n['both_outer_quarters_5m']:.1%}|{o['acf1']:.3f} → {n['acf1']:.3f}|{n['volume']/o['volume']:.1%}|")
        for key in ('wick_5m','both_outer_quarters_5m'):
            if n[key]>o[key]:regressions.append(f"{n['seed']}/{n['market']}/{key}: {o[key]:.3f}→{n[key]:.3f}")
    lines+=['','形态回退分组：'+('; '.join(regressions)or'无')+'。不能只凭总体均值隐藏这些回退。','',
        '|种子|绝对收盘基差中位bp：前→后|P99bp：前→后|最大bp：前→后|分钟收益相关：前→后|','|---|---:|---:|---:|---:|']
    for n in new['market_links']:
        o=next(r for r in links if r['seed']==n['seed']and r['profile']=='dual-execution')
        vals=[f"{o[k]:.2f} → {n[k]:.2f}" for k in ('absolute_close_basis_median_bp','absolute_close_basis_p99_bp','absolute_close_basis_max_bp','return_correlation')]
        lines.append('|'+str(n['seed'])+'|'+'|'.join(vals)+'|')
    if (out/'same-wave-links.json').exists():
        matched=json.loads((out/'same-wave-links.json').read_text(encoding='utf-8'))
        lines+=['','分钟收盘价可能不同步，因此另按同一完整1秒行动波次之后的公开BBO核对。下表毛两腿价差尚未扣手续费／资金费，也不保证下一次下单时两腿都能成交；P99采用无插值的较低顺序统计量。缺少双边报价单独计数，不延用旧报价。','',
            '|种子／机制|同波中间价绝对基差中位bp|P99bp|最大bp|毛价差>25bp比例|连续>25bp最长秒|','|---|---:|---:|---:|---:|---:|']
        for r in matched['records']:
            s=r['absolute_same_wave_mid_basis_bp'];t=next(x for x in r['thresholds']if x['entry_bp']==25)
            lines.append(f"|{r['seed']} / {r['profile']}|{s['median']:.2f}|{s['p99']:.2f}|{s['max']:.2f}|{t['fraction_above']:.2%}|{t['longest_run_seconds']}|")
    if (out/'basis-capacity.json').exists():
        cap=json.loads((out/'basis-capacity.json').read_text(encoding='utf-8'))['records']
        lines+=['','|种子|套利平均上限占用：前→后|平均未对冲时间：前→后|最大事件残差手数：前→后|结束最大绝对残差：前→后|','|---|---:|---:|---:|---:|']
        for seed in (41,7,19,73):
            before=[r for r in cap if r['seed']==seed and r['profile']=='dual-execution'];after=[r for r in cap if r['seed']==seed and r['profile']==profile]
            cells=[f"{statistics.mean(r[k]for r in before):.2%} → {statistics.mean(r[k]for r in after):.2%}"for k in ('at_cap_fraction','unhedged_time_fraction')]
            cells.append(f"{max(r['max_abs_residual']for r in before)} → {max(r['max_abs_residual']for r in after)}")
            cells.append(f"{max(abs(r['end_residual'])for r in before)} → {max(abs(r['end_residual'])for r in after)}")
            lines.append('|'+str(seed)+'|'+'|'.join(cells)+'|')
        lines+=['','残差与容量按真实有序成交重建。结束残差包含运行末边界成交；事件最大残差可含同时间戳内腿间暴露。容量统计不能替代每次实际决策／拒单回执。']
        seed7=[r for r in cap if r['seed']==7 and r['profile']==profile]
        if seed7 and all(r['at_cap_fraction']==0 and r['end_residual']==0 for r in seed7):
            lines+=['','种子7候选的五组套利账户在主窗口均未触及仓位上限，结束残差也都为零，但仍有报价基差尾部。不能把该尾部继续简单归因于仓位满或结束时未对冲；执行吞吐、时序或报价响应的具体责任还需逐秒决策回执验证。']
    lines+=['','|种子／市场|报价中间价分钟标准差bp：前→后|绝对收益ACF1：前→后|最长同向分钟：前→后|分钟绝对收益最大bp：前→后|新机制最长单边盘口秒|',
        '|---|---:|---:|---:|---:|---:|']
    for m in new['metrics']:
        prior=next(x for x in old['metrics']if x['seed']==m['seed']and x['profile']=='dual-execution')
        for market,n in m['markets'].items():
            o=prior['markets'][market]
            lines.append(f"|{m['seed']} / {market}|{o['quoted_mid_minute']['return_std_bp']:.3f} → {n['quoted_mid_minute']['return_std_bp']:.3f}|{o['minute']['abs_return_acf1']:.3f} → {n['minute']['abs_return_acf1']:.3f}|{o['minute']['longest_directional_run']} → {n['minute']['longest_directional_run']}|{o['minute']['abs_return_max_bp']:.2f} → {n['minute']['abs_return_max_bp']:.2f}|{n['longest_one_sided_outage_seconds']}|")
    lines+=['','中间价只取每分钟末实际可得的双边盘口，不前向填充缺失报价；它不是成交K线。上述表用于区分价差反弹与价格本身的变化，并披露方向性过强及尾部／流动性风险。更长同向序列、较大尾部不能因为影线减少而自动视为改善。']
    lines+=['','## 全部探索性初筛','',
        '四组均为种子41、4小时、主窗口[2h,4h)，每市场只有24根5m柱。在任何候选运行前已冻结所有16份四种子长运行配方。初筛控制通过与上一轮4小时组合的完整回执字节一致检查。初筛和长运行使用同一二进制及不变配方；选择并非独立留出评估。','',
        '|机制／市场|分钟标准差bp|5m影线占比|双长影线|收益ACF1|成交量|','|---|---:|---:|---:|---:|---:|']
    for r in pilot['rows']:lines.append(f"|{r['profile']} / {r['market']}|{r['std_bp']:.3f}|{r['wick_5m']:.1%}|{r['both_outer_quarters_5m']:.1%}|{r['acf1']:.3f}|{r['volume']:,.0f}|")
    holdout=Path(verification['holdout_directory'])
    if (holdout/'comparison.json').exists():
        h=json.loads((holdout/'comparison.json').read_text(encoding='utf-8'));assert h['complete']
        lines+=['','## 未参与本轮选择的种子101短检验','',
            '四种机制的种子101配方在任何该种子结果产生前另行冻结，仅执行控制和已选候选。两组各4小时，固定主窗口[2h,4h)。不据此重新挑选机制，不与四个12小时种子混算；这是一个短留出，而非长周期或广泛泛化认证。','',
            '|机制／市场|分钟标准差bp|5m影线占比|双长影线|收益ACF1|成交量|','|---|---:|---:|---:|---:|---:|']
        for r in h['rows']:lines.append(f"|{r['profile']} / {r['market']}|{r['std_bp']:.3f}|{r['wick_5m']:.1%}|{r['both_outer_quarters_5m']:.1%}|{r['acf1']:.3f}|{r['volume']:,.0f}|")
        for r in h['market_links']:lines.append(f"\n{r['profile']}：基差绝对值中位数{r['absolute_close_basis_median_bp']:.2f}bp，P99 {r['absolute_close_basis_p99_bp']:.2f}bp；收益相关{r['return_correlation']:.3f}。")
    lines+=['','## 因果线索与机制边界','',
        '旧程序对种子7做了12小时观察性重跑，在第2、6、10小时各记录5分钟报价／库存／套利决策，完整回执SHA256与此前相同。三个窄窗口不代表独立事件样本。',
        '在1547次指数分量至少10 ticks的永续有效报价中，实际中心相对中间价移动中位数仅0.237 tick。旧代码把指数锚和本地库存偏移一起差分，稳定偏离的回归力在首次报价确认后被抵消；对应的新恢复／持续指数／过期指数测试通过。',
        '1951次已到期套利主腿决策中：1015次仍有入场价差但达到仓位上限，930次无入场／退出边际，5次真实残差待对冲，1次提交新入场。有限套利资本是实际约束，不能把它全部归为对冲故障，也不能通过增大资金证明报价修复。',
        '旧新流量对中心的影响中位数仅现货0.049 tick、永续0.035 tick。原公式将其乘以随波动下降而收窄的半价差；实验性供给曲线方案将数量映射到实际可见深度与价格。它可能与已体现在盘口中的价格变化发生反馈，必须以实际路径、形态、尾部和自相关共同判定。',
        '机制定义、约束和复现入口见[实验协议](../MARKET_PRICE_FORMATION_EXPERIMENTS.md)。指数方案只用当前有效的公开指数；两项方案默认关闭、数值系数不变。','',
        '## 守恒、复现与资格范围','',
        f"本轮验证了{verification['native_runs']}次完整原生运行、合计{verification['simulated_hours']:g}小时模拟时间。另有{verification.get('cached_artifacts_verified',0)}份主窗口缓存副本核对，不重复计作新运行。冻结程序、配方和回执哈希核对通过；原四个证据ZIP保持原SHA256。没有候选4h／12h前缀通过声明，因为候选长运行没有完成。",
        '另有2次长尝试中断：Windows最后日志33100步，Linux控制最后日志34200步，准确停止步均未知；标准输出均为空，没有市场统计或成功小时。Windows原失败元数据保留；Linux守护逻辑停止单元后，systemd代理返回0，旧启动器误尝试解析空回执，遗漏了结束元数据。修复前源码／异常日志／资源采样及注明重建来源的failure-audit保留，新增三项回归测试验证代理退出0不能冒充完成。原长计划与未启动分组均保留。',
        'Linux四小时兼容性检查中，控制和候选的逐笔成交、盘口、资产和所有非状态字段与Windows完全一致，但最终部分浮点状态末位不同，因此不宣称原始回执字节一致。主比较采用Linux同平台配对；种子41复用兼容性数据，其余三组重新执行。',
        '其余四小时任务使用本任务自己的临时systemd单元，4GiB回收阈值、8GiB硬上限和最多8GiB已有交换空间；两侧内存余量不足3GiB仍只停止本任务。没有更改WSL全局设置或其他项目进程。资源等待／换页仅影响墙钟时间，不改变市场输入或模拟时钟。早先未启动的资源计划和长运行的8GiB阈值均留档，不伪装成成功。',
        'Windows核心测试472通过、12忽略；Linux做市机制19通过；配方测试13通过，原分析工具10通过，启动器回归3通过，同波报价分析2通过。主比较／留出的全量成交计数、唯一ID、无自成交及端点余额检查通过，现货基础资产总量不变。反应型需求按真实逐账户主动成交核对，不把子单提交或到期当作成交。完整期内费用、保险、每时刻保证金账本尚未单独资格验证。',
        '原始逐笔重建的套利容量与未对冲时间另存basis-capacity.json；同时间戳腿间风险可能有事件残差而持续时间为零。该表不等于跨市场原子执行保证。',
        '四个固定种子此前已参与探索；额外的种子101只做短检验，没有独立长留出、亚秒队列／延迟、3000或10000bot规模及生产部署资格。历史参考来自此前缓存的有限窗口：原10小时窗口的分钟标准差约3.2–13.8bp；等长2小时背景另列下表。这些不是置信区间或必须匹配的收益目标，不能为了进入区间调高参数。任何剩余低波动、形态回退或基差回退均需保留。','',
        ]
    if (BASE/'reference-short-context.json').exists():
        ref=json.loads((BASE/'reference-short-context.json').read_text(encoding='utf-8'))
        lines+=['## 等长历史窗口的描述性背景','',
            '在看到初筛趋势过长及用户缩短复验范围之后，将此前每市场8个已缓存的10小时窗口切成40个不重叠2小时窗口，使用全部窗口、不重新挑日期。这样时长与本轮主窗口一致。数据仅四个2025日期；下列最小／中位／最大值不是置信区间、验收阈值或独立现实资格，也没有拿来反调数值。','',
            '|市场|分钟标准差bp：最小／中位／最大|收益ACF1|最长同向分钟|5m影线占比|','|---|---:|---:|---:|---:|']
        for r in ref['summary']:
            vals=[]
            for key in ('return_std_bp','return_acf1','longest_directional_run','wick_5m'):
                d=r[key];vals.append(' / '.join(f'{d[x]:.1%}'if key=='wick_5m'else f'{d[x]:.3f}'for x in ('min','median','max')))
            lines.append('|'+r['market']+'|'+'|'.join(vals)+'|')
        lines+=['']
    lines+=['## 实际成交K线','']
    for seed in (41,7,19,73):
        name=f'{prefix}-seed{seed}-5m.png';shutil.copyfile(out/f'price-formation-seed{seed}-5m.png',dest/name)
        lines += [f'![种子{seed}全主窗口实际5m OHLCV]({name})','']
    for suffix,data in (('comparison.csv',rows),('market-links.csv',links)):
        with (dest/(prefix+'-'+suffix)).open('w',encoding='utf-8',newline='') as f:
            w=csv.DictWriter(f,fieldnames=list(data[0]));w.writeheader();w.writerows(data)
    target=dest/(prefix+'.md');target.write_bytes(('\n'.join(lines)+'\n').encode('utf-8'))
    write(BASE/'report-summary.json',dict(profile=profile,means=means,shape_regressions=regressions,report_sha256=sha(target)))
    print(target)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);render(p.parse_args().directory)
