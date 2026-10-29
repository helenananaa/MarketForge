"""Evidence tables for frozen four-seed short and long comparisons."""
import argparse,json,statistics
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write
from research_parent_liquidity import BASE,SEEDS,active_main

FIELDS=('std_bp','acf1','abs_acf1','p99_abs_return_bp','tail_p99_over_std','wick_1m','wick_5m','wick_15m',
        'spread_bp','volume','volume_cv','volume_abs_return_corr','two_sided_pct','zero_return_fraction_1m')


def read(p):return json.loads(p.read_text(encoding='utf-8'))


def enrich_rows(data):
    metrics={(m['seed'],m['profile']):m for m in data['metrics']};rows=[]
    for r in data['rows']:
        a=metrics[r['seed'],r['profile']]['markets'][r['market']];minute=a['minute']
        rows.append(dict(**r,abs_acf1=minute['abs_return_acf1'],volume_cv=minute['volume_cv'],
            volume_abs_return_corr=minute['volume_abs_return_corr'],wick_15m=a['wick_anatomy_15m']['median']['wick_fraction'],
            tail_p99_over_std=r['p99_abs_return_bp']/r['std_bp']if r['std_bp']else None))
    return rows


def paired(data):
    rows=enrich_rows(data);deltas=[];summary=[]
    for market in ('V-BTC-SPOT','V-BTC-PERP'):
        for field in FIELDS:
            group=[]
            for seed in SEEDS:
                a=next(r for r in rows if (r['seed'],r['profile'],r['market'])==(seed,'legacy',market))
                b=next(r for r in rows if (r['seed'],r['profile'],r['market'])==(seed,'liquidity-parent',market))
                if a[field] is None or b[field] is None:continue
                value=dict(seed=seed,market=market,metric=field,control=a[field],candidate=b[field],delta=b[field]-a[field])
                group.append(value);deltas.append(value)
            if group:summary.append(dict(market=market,metric=field,seeds=len(group),
                mean_control=statistics.mean(x['control']for x in group),mean_candidate=statistics.mean(x['candidate']for x in group),
                mean_abs_control=statistics.mean(abs(x['control'])for x in group),mean_abs_candidate=statistics.mean(abs(x['candidate'])for x in group),
                median_delta=statistics.median(x['delta']for x in group),min_delta=min(x['delta']for x in group),
                median_abs_delta=statistics.median(abs(x['candidate'])-abs(x['control'])for x in group),
                max_delta=max(x['delta']for x in group),lower_count=sum(x['delta']<0 for x in group),
                closer_to_zero_count=sum(abs(x['candidate'])<abs(x['control'])for x in group)))
    return dict(rows=rows,paired_deltas=deltas,paired_summary=summary)


def format_number(n):return '—'if n is None else f'{n:.3f}'


def tables(data,hours):
    lines=[f'## {hours}小时完整配对结果','',
        '|种子／市场|分钟标准差bp 旧→新|收益ACF1 旧→新|绝对收益ACF1 旧→新|5m影线占比 旧→新|成交量 旧→新|',
        '|---|---:|---:|---:|---:|---:|']
    rows=enrich_rows(data)
    for seed in SEEDS:
        for market in ('V-BTC-SPOT','V-BTC-PERP'):
            a,b=[next(r for r in rows if (r['seed'],r['profile'],r['market'])==(seed,p,market))for p in ('legacy','liquidity-parent')]
            lines.append(f"|{seed}/{market[6:]}|{a['std_bp']:.3f}→{b['std_bp']:.3f}|{format_number(a['acf1'])}→{format_number(b['acf1'])}|{format_number(a['abs_acf1'])}→{format_number(b['abs_acf1'])}|{a['wick_5m']:.1%}→{b['wick_5m']:.1%}|{a['volume']:,}→{b['volume']:,}|")
    lines+=['','|种子／机制|计划／已激活母需求|真实成交|过期|期限内余量|成交／已激活|',
        '|---|---:|---:|---:|---:|---:|']
    for p in data['parents']:
        lines.append(f"|{p['seed']}/{p['profile']}|{p['planned_demand_lots']}/{p['activated_demand_lots']}|{p['filled_lots']}|{p['expired_lots']}|{p['remaining_lots']}|{p['filled_fraction']:.1%}|")
    lines+=['','|种子／机制|绝对成交收盘基差P99 bp|同波中间价基差P99 bp|对冲账户最大绝对残差 手|最长缺单边盘口 秒|',
        '|---|---:|---:|---:|---:|']
    for link in data['market_links']:
        seed,profile=link['seed'],link['profile']
        quote=next(q for q in data['same_wave_links']if (q['seed'],q['profile'])==(seed,profile))
        caps=[c for c in data['capacities']if(c['seed'],c['profile'])==(seed,profile)]
        metric=next(m for m in data['metrics']if(m['seed'],m['profile'])==(seed,profile))
        lines.append(f"|{seed}/{profile}|{format_number(link['absolute_close_basis_p99_bp'])}|{format_number(quote['absolute_same_wave_mid_basis_bp'].get('p99'))}|{max(c['max_abs_residual']for c in caps)}|{max(a['longest_one_sided_outage_seconds']for a in metric['markets'].values())}|")
    lines+=['','|种子／市场|5m方差比 旧→新|15m方差比 旧→新|P99绝对收益／标准差 旧→新|真实成交分钟数 旧→新／应有|','|---|---:|---:|---:|---:|']
    for seed in SEEDS:
        for market in ('V-BTC-SPOT','V-BTC-PERP'):
            a,b=[next(r for r in rows if(r['seed'],r['profile'],r['market'])==(seed,p,market))for p in ('legacy','liquidity-parent')]
            ah,bh=[next(r['horizons']for r in data['horizons']if(r['seed'],r['profile'],r['market'])==(seed,p,market))for p in ('legacy','liquidity-parent')]
            vr=lambda hs,q:next(h['variance_ratio']for h in hs if h['minutes']==q)
            lines.append(f"|{seed}/{market[6:]}|{format_number(vr(ah,5))}→{format_number(vr(bh,5))}|{format_number(vr(ah,15))}→{format_number(vr(bh,15))}|{format_number(a['tail_p99_over_std'])}→{format_number(b['tail_p99_over_std'])}|{a['nonempty_minutes']}→{b['nonempty_minutes']}/{(hours-2)*60}|")
    return lines


def summarize():
    short=read(active_main()/'comparison.json');assert short['complete']and short['completed_pairs']==4
    long_path=BASE/'long12h-candidates-20261026/comparison.json'
    long=read(long_path)if long_path.exists()else None
    summary=dict(short=paired(short),long=paired(long)if long else None,
        scope='Descriptive paired changes across four fixed seeds; markets share shocks and are not independent samples. Lower volatility or shorter wicks are not automatic realism improvements. No fitted composite score or tuning from outcomes.')
    write(BASE/'followup-summary.json',summary)
    lines=['# 冻结机制：四种子4小时对照与12小时复验（2026-10-26）','',
        '机制：有限母订单按当前可见最优价执行，真实子单成交扣减需求；默认旧路径保持。原目标、资金、费用、期限、做市与套利政策、种子和程序不变。12h只延长总模拟时长，不延长单个母订单期限。','',
        '固定种子41、7、19、73，300bots、1秒时钟。4h主窗口[2h,4h)，12h主窗口[2h,12h)，末段[10h,12h)和逐小时检查均预先固定。完整成交生成OHLCV，不补空柱、不平滑、不删异常成交、不裁剪窗口。','',
        '4h控制来自此前相同冻结配方的完整真实运行，种子41候选复用已完成试跑；缓存不计新增运行。新程序关闭机制的4h完整回执及执行档案已与旧控制逐字节一致。12h控制和候选均重新运行，前4h成交序列逐条与短跑核对。12h是同种子的时间延伸，不是独立留出。','',
        '收益ACF偏离0反映价格记忆，绝对收益ACF反映波动聚集；两者须分开。收益ACF的汇总另列绝对偏离，避免正负相关在均值中抵消。影线占比、波动、成交量和尾部联合查看，不能把影线越短或波动越低作为越真实。绝对成交收盘基差包含异步收盘影响，同时列同一动作波后报价基差和实际对冲残差。','',
        '以下数值为描述性配对结果；四个种子不是广泛泛化证据。另复用此前按日历预选的2025-01-02、04-02、07-02、10-02四日BTCUSDT真实成交，各市场40个2h窗口，未按新结果重选日期或拟合验收带。历史方差比的2h窗口范围仅作背景，不能与12h主窗口直接当成等长度检验；数据没有真实L2。分钟缺成交保留为空缺，零收益比例只针对实际相邻分钟，不把空柱计作零。','']
    interpretation=BASE/'followup-interpretation.json'
    if interpretation.exists():
        lines+=['## 结论','']
        for paragraph in read(interpretation)['paragraphs']:lines+=[paragraph,'']
    lines+=tables(short,4)
    if long:
        lines+=['']+tables(long,12)
        lines+=['','## 末段与资源检查','',
            '|种子／市场|最后2h收益ACF1 旧→新|最后2h绝对收益ACF1 旧→新|最后2h波动bp 旧→新|最后2h零收益比例 旧→新|',
            '|---|---:|---:|---:|---:|']
        for seed in SEEDS:
            for market in ('V-BTC-SPOT','V-BTC-PERP'):
                a,b=[next(r['terminal']for r in long['horizons']if(r['seed'],r['profile'],r['market'])==(seed,p,market))for p in ('legacy','liquidity-parent')]
                lines.append(f"|{seed}/{market[6:]}|{format_number(a['return_acf1'])}→{format_number(b['return_acf1'])}|{format_number(a['abs_return_acf1'])}→{format_number(b['abs_return_acf1'])}|{format_number(a['return_std_bp'])}→{format_number(b['return_std_bp'])}|{format_number(a['zero_return_fraction'])}→{format_number(b['zero_return_fraction'])}|")
    else:lines+=['','12h复验进行中，尚无完整四组长周期结论。']
    qualification=BASE/'followup-completion.json'
    if qualification.exists():
        q=read(qualification)
        lines+=['',f"完成证据：本次续跑新增{q['new_runs_this_followup']}次原生运行、{q['new_simulated_hours_this_followup']}个模拟小时；研究全程新增{q['unique_new_runs_whole_study']}次完整运行、{q['unique_new_simulated_hours_whole_study']}个模拟小时（包含兼容性复跑与早先试跑，不含缓存或中断）。四种子4h、12h共8组配对完成；8条长跑成交前缀与4h短跑逐对象、顺序核对一致。大型执行档案另行流式复核哈希和记录数。",'',
            '|12h种子／机制|墙钟秒|内存采样峰值MiB|交换采样峰值MiB|','|---|---:|---:|---:|']
        for r in q['long_runs']:
            lines.append(f"|{r['name']}|{r['wall_seconds']:.1f}|{r['sampled_peak_memory_kib']/1024:.1f}|{r['sampled_peak_swap_kib']/1024:.1f}|")
    lines+=['','## 完整K线对照','']
    for seed in SEEDS:
        lines+=[f'### 种子{seed}','',f'![4h实际5m OHLCV](../../.local/strategy-research-parent-liquidity-20261025/{active_main().name}/parent-liquidity-seed{seed}-5m.png)','']
        if long:lines+=[f'![12h实际5m OHLCV](../../.local/strategy-research-parent-liquidity-20261025/long12h-candidates-20261026/parent-liquidity-seed{seed}-5m.png)','',f'[固定末段1m OHLCV](../../.local/strategy-research-parent-liquidity-20261025/long12h-candidates-20261026/parent-liquidity-seed{seed}-1m.png)','']
    lines+=['## 验证边界','',
        '核心487项通过、12项忽略，Python37项通过的实现证据保持；本轮另有3项长跑前缀、2项中继文件占用恢复验证测试通过。仅新增运行计划和分析／资源框架／打包辅助脚本，未改冻结交易源码或程序。前缀拒绝测试暴露Windows异常路径未及时关闭输入生成器，已增加finally关闭；种子73一次文件替换临时占用导致中继线程退出和陈旧保护停止，修复为出错后以新测量重试发布、持续错误仍触发原保护。原日志、修复前源码、部分执行流和两项框架哈希修订记录保留，比较逻辑与统计窗口不变。每次完成必须有匹配时长、配方／程序哈希、原始回执哈希和完整执行档案哈希的小摘要，代理退出码0不能代替完成。端点资产、真实成交计数、自成交、母需求守恒和套利持仓残差单独核对。','',
        '之前的种子7时钟误判和用户选择停止两次尝试保留，不算成功小时。主机提交启动6GiB、客体8GiB，运行保护3GiB；单个任务MemoryHigh2G/Max4G/SwapMax2G，串行运行。未更改主机分页、WSL配置或其他项目进程。','',
        '仍缺独立新种子271、未见过日期与真实L2验证、完整日内费用／保险账本及毫秒级异步到达认证。既有四个历史日期只作描述性背景，不算本轮新增跨日期泛化证据。报价补充／撤退、独立信息到达不是本轮机制内容。不会仅据K线观感切换默认。','']
    path=ROOT/'docs/validation/2026-10-26-market-parent-liquidity-results.md'
    path.write_text('\n'.join(lines)+'\n',encoding='utf-8');print(path)
    print(json.dumps(summary,ensure_ascii=False,indent=2))


if __name__=='__main__':summarize()
