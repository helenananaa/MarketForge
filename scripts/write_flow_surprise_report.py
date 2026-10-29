"""Render all paired outcomes, including regressions and qualification gaps."""
import json
from pathlib import Path
from statistics import mean
from research_market_mechanisms import ROOT, write
from research_flow_surprise import BASE


def read(path):
    return json.loads(path.read_text(encoding='utf-8'))


def report():
    completion = read(BASE/'completion.json')
    decision = read(BASE/'decision.json')
    lines = ['# 订单流预测误差报价实验（2026-10-25）', '', decision['conclusion']]
    for note in decision['notes']:
        lines += ['',note]
    lines += ['', '## 机制与对照', '',
        '控制组为上一轮指数锚／供给曲线候选。新组仅启用 flow_surprise：先用此前已观察的有符号订单流记忆预测本观察间隔的流量，再把实际流量减去预测量，映射到当前可见外部供给曲线。预测沿用原半衰期；突然反转可能产生更强响应。实际扫单、原始流量毒性／价差、库存、指数锚、资金、手续费、套利容量和执行生命周期均保持不变，没有新增或调整数值系数。', '',
        '当前输出／历史归档改动的基线，以及包含新代码但开关关闭的程序，都重现了旧 Linux 种子41完整回执的同一 SHA256。其余三个关闭开关的控制也分别与此前完整回执字节一致。新旧程序关闭开关时的种子41完整执行归档也逐字节一致，共2,005,944条记录。全量源码快照和新两文件源码分别冻结；配方反向删除这一个布尔值后与各自控制相同。实验开关默认关闭。', '',
        '所有已完成配对组300 bots、1秒时钟、每次完整4小时；主窗口固定[2h,4h)。每组每市场120根1m、24根5m柱，统计样本很短。四个既有种子属于探索性复验；211配方在任何本轮候选结果出现前冻结，原定新增短留出，实际未完成，见缺口。没有用结果重新调整参数。', '']
    for folder_text in completion['folders']:
        folder = Path(folder_text)
        if folder.name == 'pilot' and (BASE/'main/comparison.json').exists():
            continue
        data = read(folder/'comparison.json')
        lines += ['## ' + ('新增种子211短留出' if folder.name == 'holdout' else '固定种子配对结果'), '',
            '|种子／市场|分钟标准差bp 前→后|收益ACF1 前→后|5m影线占比 前→后|5m双长影线 前→后|成交量 前→后|',
            '|---|---:|---:|---:|---:|---:|']
        pairs = {}
        for row in data['rows']:
            pairs.setdefault((row['seed'], row['market']), {})[row['profile']] = row
        for (seed, market), pair in sorted(pairs.items()):
            a, b = pair['raw-flow'], pair['surprise']
            lines.append(f"|{seed}/{market}|{a['std_bp']:.3f} → {b['std_bp']:.3f}|{a['acf1']:.3f} → {b['acf1']:.3f}|{a['wick_5m']:.1%} → {b['wick_5m']:.1%}|{a['both_outer_quarters_5m']:.1%} → {b['both_outer_quarters_5m']:.1%}|{a['volume']:,.0f} → {b['volume']:,.0f}|")
        lines += ['', '影线占比为同根柱上下影线总长／全幅的中位数；双长影线要求实体上下各有至少全幅1/4的影线。影线减少本身不是通过标准。1m、15m、60m及启动期形态也保存在 comparison.json；原始成交没有裁剪、平滑或补柱。', '',
            '|种子|成交收盘绝对基差中位数bp 前→后|P99 bp 前→后|同波盘口中间价绝对基差P99 bp 前→后|报价套利边际>25bp最长秒 前→后|',
            '|---|---:|---:|---:|---:|']
        quotes = read(folder/'same-wave-links.json')['records']
        for seed in sorted({r['seed'] for r in data['market_links']}):
            a, b = [next(r for r in data['market_links'] if r['seed']==seed and r['profile']==p) for p in ('raw-flow','surprise')]
            qa, qb = [next(r for r in quotes if r['seed']==seed and r['profile']==p) for p in ('raw-flow','surprise')]
            tails = [next(r['longest_run_seconds'] for r in q['thresholds'] if r['entry_bp']==25) for q in (qa,qb)]
            lines.append(f"|{seed}|{a['absolute_close_basis_median_bp']:.2f} → {b['absolute_close_basis_median_bp']:.2f}|{a['absolute_close_basis_p99_bp']:.2f} → {b['absolute_close_basis_p99_bp']:.2f}|{qa['absolute_same_wave_mid_basis_bp']['p99']:.2f} → {qb['absolute_same_wave_mid_basis_bp']['p99']:.2f}|{tails[0]} → {tails[1]}|")
        lines += ['', '成交收盘价存在异步性，因此另列同一秒所有动作完成后的公开盘口。报价边际未扣手续费、资金费和腿间风险，并不保证两腿可原子成交。', '',
            '|种子|五账户平均未对冲时间 前→后|平均触顶时间 前→后|最大事件残差 前→后|结束最大残差 前→后|四小时两腿成交手数 前→后|',
            '|---|---:|---:|---:|---:|---:|']
        cap = read(folder/'basis-capacity.json')['records']
        for seed in sorted({r['seed'] for r in cap}):
            a, b = [[r for r in cap if r['seed']==seed and r['profile']==p] for p in ('raw-flow','surprise')]
            lines.append(f"|{seed}|{mean(r['unhedged_time_fraction'] for r in a):.2%} → {mean(r['unhedged_time_fraction'] for r in b):.2%}|{mean(r['at_cap_fraction'] for r in a):.2%} → {mean(r['at_cap_fraction'] for r in b):.2%}|{max(r['max_abs_residual'] for r in a)} → {max(r['max_abs_residual'] for r in b)}|{max(abs(r['end_residual']) for r in a)} → {max(abs(r['end_residual']) for r in b)}|{sum(r['filled_lots'] for r in a)} → {sum(r['filled_lots'] for r in b)}|")
        lines += ['', '两腿仓位依据实际有序成交重建，包含主窗口之前携入的暴露；同一时间戳内的腿间变化只影响事件最大值。未对冲时长并不能单独证明对冲故障，仍包含有限执行速度和策略时序。另列整段四小时两腿成交手数：若候选没有达到入场条件、套利账户根本未交易，其零残差不能当作对冲执行变快的证据。', '']
        horizon = read(folder/'horizon-diagnostics.json')['records']
        lines += ['|种子／市场|5m方差比 前→后|15m方差比 前→后|最长同向分钟 前→后|最后1h分钟标准差bp 前→后|',
            '|---|---:|---:|---:|---:|']
        for (seed, market), pair in sorted(pairs.items()):
            a, b = [next(r for r in horizon if r['seed']==seed and r['market']==market and r['profile']==p) for p in ('raw-flow','surprise')]
            vr = lambda r,q: next(h['variance_ratio'] for h in r['horizons'] if h['minutes']==q)
            ma, mb = [next(m for m in data['metrics'] if m['seed']==seed and m['profile']==p)['markets'][market]['minute'] for p in ('raw-flow','surprise')]
            lines.append(f"|{seed}/{market}|{vr(a,5):.2f} → {vr(b,5):.2f}|{vr(a,15):.2f} → {vr(b,15):.2f}|{ma['longest_directional_run']} → {mb['longest_directional_run']}|{a['hourly'][1]['return_std_bp']:.3f} → {b['hourly'][1]['return_std_bp']:.3f}|")
        lines += ['', '方差比＝重叠q分钟收益的样本方差／（q×分钟收益样本方差）。这是看过首组之后补充的描述性诊断，短样本且重叠，不进行显著性或验收区间推断，也不据此调系数。20阶收益与绝对收益自相关仍在完整指标中。最后1小时单列检查后段是否失去波动，不替换事先指定的主窗口。', '']
        for seed in sorted({r['seed'] for r in data['rows']}):
            lines += [f'![种子{seed}实际5m OHLCV](../../.local/strategy-research-flow-surprise-20261025/{folder.name}/flow-surprise-seed{seed}-5m.png)', '']
    audit = read(BASE/'main/wave-audit-seed73.json')
    lines += ['## 种子73异常波段的实际订单核对', '',
        '以两组各自最高同波报价套利边际时刻为中心，各取前后60秒并取并集；两组检查相同时钟窗口，共240秒。它是事后诊断，不能代表整段主窗口。逐命令重建存量做市报价与实际库存；每秒报价极值不越过已保存的公共BBO，选中窗口的每笔执行成交与原始成交回执一致。', '',
        '|机制|做市库存偏离目标最大手数|套利事件残差最大手数|套利未对冲账户秒数|套利触顶账户秒数|套利提交单数|套利实际taker成交手数|套利IOC过期事件|',
        '|---|---:|---:|---:|---:|---:|---:|---:|']
    for r in audit['records']:
        s=r['summary']; a=s['actions']
        lines.append(f"|{r['profile']}|{s['maker_inventory_deviation_max']}|{s['basis_residual_max']}|{s['basis_unhedged_account_waves']}|{s['basis_at_cap_account_waves']}|{a.get('BasisLinkTrader:submitted',0)}|{a.get('BasisLinkTrader:taker_filled_lots',0)}|{a.get('BasisLinkTrader:OrderExpired:',0)}|")
    lines += ['', '控制窗口存在实际对冲残差与许多未完全成交的IOC，但没有套利仓位触顶。不能继续只用资金／上限解释这些波段。候选窗口没有套利提交或成交，零残差来自偏离没有触发交易；没有证据表明它修复了原执行器。窗口级汇总也不能把每个IOC过期都等同于对冲故障，完整事件顺序与各秒库存保存在wave-audit-seed73.json。', '']
    context = read(ROOT/'.local/strategy-research-price-formation-20261025/reference-short-context.json')
    href = read(BASE/'reference-horizon-context.json')['summary']
    lines += ['## 同长度历史描述性参照', '',
        '继续使用此前缓存的四个2025日期，每市场40个不重叠2小时窗口，没有新增下载或挑日期。这些范围只是小样本描述，不是拟合目标、验收带或独立认证。尤其不能把影线持续降到越低越好。', '',
        '|市场|分钟标准差bp中位数（范围）|5m影线占比中位数（范围）|5m方差比中位数（范围）|15m方差比中位数（范围）|',
        '|---|---:|---:|---:|---:|']
    for r in context['summary']:
        h5,h15=[next(x for x in href if x['market']==r['market'] and x['minutes']==q) for q in (5,15)]
        s,w=r['return_std_bp'],r['wick_5m']
        lines.append(f"|{r['market']}|{s['median']:.2f}（{s['min']:.2f}–{s['max']:.2f}）|{w['median']:.1%}（{w['min']:.1%}–{w['max']:.1%}）|{h5['median']:.2f}（{h5['min']:.2f}–{h5['max']:.2f}）|{h15['median']:.2f}（{h15['min']:.2f}–{h15['max']:.2f}）|")
    lines += ['', '## 验证与保留缺口', '',
        f"实际完成 {completion['native_runs']} 次原生运行，合计 {completion['simulated_hours']} 小时模拟时间，其中4小时为额外兼容性复跑，四种子配对本体为8×4＝32小时；{completion['cached_artifacts']} 份种子41缓存副本未重复计数。重放不等于独立市场样本。全部成功回执、程序、配方、执行归档 SHA256 和归档记录数已核对。逐笔唯一性、数量、无自成交、期初期末资产恒等式及无负可用余额检查通过；原始需求＝实际子单成交＋过期＋剩余，逐账户与真实 taker 成交一致。", '',
        '原五个实验ZIP的SHA256保持不变。原始大回执和完整执行归档保留本地，ZIP包含外部数据清单与哈希，不将数GB归档重复装包。使用本任务自己的临时单元及内存余量守护，没有更改全局缓存、WSL设置或停止其他项目进程。', '',
        '额外种子211控制组被原有3GiB主机提交余量守护停止，最后进度日志7100步，精确停止步未知。标准输出为空、无完成摘要，不计成功小时、不截取成市场结果。代理退出0也已正确保留失败。211候选在8GiB启动门槛处等待，未启动原生程序；四种子触发联合拒绝规则后取消等待，冻结配方／原计划、失败元数据及部分档案均保留。因此没有完成新增独立留出。结束说明中最初“让仍运行控制组完成”的意图记录有状态延迟，之后的失败元数据核对为最终事实。', '',
        '资源处理只对本任务已经完成的20个文件发出fsync／POSIX_FADV_DONTNEED缓存提示，没有清理全局缓存或更改门槛；提示返回成功，但观测提交余量未明显改善，不宣称解决了主机内存问题。', '',
        '仍未完成12小时复验，四小时结果不能排除长周期库存漂移、资本耗尽、流动性断层或更慢反馈。真实历史样本仅用于先前描述性参照；本轮没有真实订单簿校准、独立多日检验、真实市场拟合认证或默认配置升级。保存的端点恒等式也不是盘中完整手续费／保证金账本的证明。', '',
        '复现与机制定义见[实验协议](../MARKET_FLOW_SURPRISE_EXPERIMENTS.md)。478项核心测试通过，12项忽略；29项配方与输出Python测试通过。23项做市专项测试是核心测试的子集，不重复计数。完整记录见研究目录中的测试日志与test-summary.json。']
    target = ROOT/'docs/validation/2026-10-25-market-flow-surprise.md'
    target.write_text('\n'.join(lines)+'\n', encoding='utf-8')
    print(target)


if __name__ == '__main__':
    report()
