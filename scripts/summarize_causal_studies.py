"""Join both complete frozen studies; preserve seed-level tradeoffs and missing values."""
import csv,json,sys,re
from pathlib import Path
import numpy as np
from analyze_market_mechanisms import ROOT,sha,write
from analyze_causal_mechanisms import candles,MARKETS

BASE=ROOT/'.local/strategy-research-causal-20261025'
PROFILES=('control','hedged','depth','level')
LABELS={'control':'Prior reservation','hedged':'Touch hedge','depth':'Hedge + depth','level':'Inventory level'}


def read(path):return json.loads(path.read_text(encoding='utf-8'))


def summarize():
    assert np.__version__=='2.5.3','Use the recorded isolated analysis environment'
    main=read(BASE/'full-study/causal-comparison.json');level=read(BASE/'inventory-level-study/causal-comparison.json');retry=read(BASE/'inventory-level-retry-s7/causal-comparison.json')
    assert main['complete'] and main['terminal'] and main['completed_runs']==9 and not main['failures']
    assert not level['complete'] and level['terminal'] and level['completed_runs']==3 and len(level['failures'])==1
    assert level['failures'][0]['name']=='native-s7-level' and level['failures'][0]['exit_code']==4294967295
    assert retry['complete'] and retry['terminal'] and retry['completed_runs']==1 and not retry['failures']
    assert (BASE/'inventory-level-study/resource-cancel-s7.json').is_file()
    assert [p['seed'] for p in retry['pairs']]==[7]
    old_attempt=read(BASE/'inventory-level-study/native-s7-level.run.json');new_attempt=read(BASE/'inventory-level-retry-s7/native-s7-level.run.json')
    assert old_attempt['inputs']==new_attempt['inputs']
    count_log=lambda p:{int(a):(int(b),int(c)) for a,b,c in re.findall(r'step=(\d+) trades=(\d+) active_bots=(\d+)',p.read_text(encoding='utf-8'))}
    old_counts=count_log(BASE/'inventory-level-study/native-s7-level.log');new_counts=count_log(BASE/'inventory-level-retry-s7/native-s7-level.log')
    assert old_counts and max(old_counts)==22200
    mismatch=[dict(step=k,original=list(v),retry=list(new_counts[k]) if k in new_counts else None) for k,v in old_counts.items() if new_counts.get(k)!=v]
    write(BASE/'cancelled-retry-prefix-verification.json',dict(inputs_identical=True,compared_log_samples=len(old_counts),through_step=max(old_counts),
        cumulative_trade_and_active_bot_counts_equal=not mismatch,mismatches=mismatch,original_partial_log_sha256=sha(BASE/'inventory-level-study/native-s7-level.log'),
        full_retry_log_sha256=sha(BASE/'inventory-level-retry-s7/native-s7-level.log'),
        scope='Only cumulative trade counts and active-bot counts at logged steps can be compared: the cancelled original never emitted a complete receipt. This is not byte identity of original/retry prices, accounts or full states. The full retry independently passes ordinary receipt/schema/input verification.'))
    assert not mismatch,'Serial retry prefix counts diverged; preserve evidence and investigate before causal claims'
    level_rows=[r for source in (level,retry) for r in source['summary'] if r['profile']=='level']
    summary=main['summary']+level_rows
    links=main['market_links']+[r for source in (level,retry) for r in source['market_links'] if r['profile']=='level'];runs=[]
    assert len(summary)==32 and len(links)==16
    assert len({(r['profile'],r['seed'],r['market']) for r in summary})==32
    assert len({(r['profile'],r['seed']) for r in links})==16
    for profile in PROFILES:
        for seed in (7,19,41,73):
            if profile=='level':pair=next(p for p in (retry if seed==7 else level)['pairs'] if p['seed']==seed)
            else:pair=next(p for p in main['pairs'] if p['seed']==seed)
            metric=pair[profile];directory=(ROOT/'.local/strategy-research-reservation-20261024' if profile=='control' else BASE/(('inventory-level-retry-s7' if seed==7 else 'inventory-level-study') if profile=='level' else 'full-study'))
            if profile=='level' and seed==41:
                preview=read(BASE/'level41-raw-preview.json')
                assert metric['receipt_sha256']==preview['receipt_sha256']
                for market in MARKETS:
                    for key in ('minute','quoted_mid_minute','morphology_5m'):assert metric['markets'][market][key]==preview['markets'][market][key]
                write(BASE/'level41-preview-final-verification.json',dict(receipt_sha256=metric['receipt_sha256'],ordinary_runner_metadata_verified=True,all_preview_metrics_identical=True))
            assert metric['integrity']['count_matches'] and metric['integrity']['self_trades']==0
            for market in MARKETS:
                row=next(r for r in summary if (r['profile'],r['seed'],r['market'])==(profile,seed,market));path=directory/(metric['name']+'-'+market+'.candles.csv')
                bars=candles(path);selected=[r for k,r in sorted(bars.items()) if 120<=k<720]
                row.update(primary_actual_minute_bars=len(selected),primary_expected_minute_bars=600,
                    primary_price_change_pct=(selected[-1]['close']/selected[0]['open']-1)*100 if selected else None,
                    primary_low_from_initial_pct=(min(b['low'] for b in selected)/100000-1)*100 if selected else None,
                    primary_high_from_initial_pct=(max(b['high'] for b in selected)/100000-1)*100 if selected else None)
                m=metric['markets'][market]
                row.update(visible_8level_bid_depth_25bp_median=m['bid_depth_25bp_median'],visible_8level_ask_depth_25bp_median=m['ask_depth_25bp_median'],
                    longest_one_sided_outage_seconds=m['longest_one_sided_outage_seconds'],spread_p90_bp=m['spread_p90_bp'],
                    primary_trade_count=m['trades'],primary_volume_lots=m['volume_lots'],
                    basis_taker_quantity_lots=m['taker_qty_by_type'].get('BasisLinkTrader',0),
                    reactive_taker_quantity_lots=m['taker_qty_by_type'].get('ReactiveFlowTrader',0),
                    filled_taker_orders=m['filled_taker_orders'],taker_order_same_direction_fraction=m['taker_order_same_direction'],
                    zero_return_fraction_1m=m['minute']['zero_return_fraction'],longest_directional_run_1m=m['minute']['longest_directional_run'],
                    actual_5m_bars=m['morphology_5m']['bars'],nonzero_range_5m_bars=m['morphology_5m']['nonzero_range_bars'])
            runs.append(dict(profile=profile,seed=seed,name=metric['name'],receipt_sha256=metric['receipt_sha256'],
                binary_sha256=metric['binary_sha256'],integrity=metric['integrity'],asset_checks=metric['asset_checks'],
                rejected_actions=metric['rejected_actions'],rejection_reasons=metric['rejection_reasons'],
                technical_ready=sum(r['ready'] for r in metric['coverage']['technical']),technical_count=len(metric['coverage']['technical']),
                execution_target_qty=sum(r['target_qty'] for r in metric['coverage']['execution']),execution_completed_qty=sum(r['completed_qty'] for r in metric['coverage']['execution'])))
    prior=read(ROOT/'.local/strategy-research-reservation-20261024/summary/feedback-summary.json')
    reference=prior['reference_windows'];touch_background=[r for r in prior['aggregate'] if r['profile']=='baseline']
    assert len(touch_background)==2 and all(r['seeds']==[7,19,41,73] for r in touch_background)
    reference_ranges=[]
    for market in MARKETS:
        rs=[r for r in reference if r['market']==market]
        reference_ranges.append(dict(market=market,windows=len(rs),ranges={k:[min(r[k] for r in rs),max(r[k] for r in rs)] for k in ('return_std_bp','return_acf1','wick_5m')}))
    aggregates=[]
    for profile in PROFILES:
        for market in MARKETS:
            rs=[r for r in summary if r['profile']==profile and r['market']==market];keys=[k for k in rs[0] if k not in ('seed','profile','market')]
            aggregates.append(dict(profile=profile,market=market,seeds=sorted(r['seed'] for r in rs),
                missing={k:sum(r[k] is None for r in rs) for k in keys if any(r[k] is None for r in rs)},
                mean={k:float(np.mean([r[k] for r in rs])) if all(r[k] is not None for r in rs) else None for k in keys},
                minimum={k:min(r[k] for r in rs) if all(r[k] is not None for r in rs) else None for k in keys},
                maximum={k:max(r[k] for r in rs) if all(r[k] is not None for r in rs) else None for k in keys}))
    result=dict(study_hashes={p:sha(BASE/p/'causal-comparison.json') for p in ('full-study','inventory-level-study','inventory-level-retry-s7')},
        completed_new_runs=13,simulated_hours=156,cancelled_attempts=level['failures'],total_new_native_attempts=14,summary=summary,market_links=links,aggregates=aggregates,runs=runs,reference_ranges=reference_ranges,touch_background=touch_background,
        scope='Exploratory sequential mechanism studies, all four previously examined seeds retained, primary [2h,12h), no holdout. One incomplete seed7 attempt was explicitly resource-cancelled with last logged step22200, preserved, and rerun serially with the identical frozen recipe/executable in a new directory. The exact stop step was not saved; progress is logged every100 steps. 14 full-study attempts, 13 full successes; diagnostic replays are separate, and no seed is omitted. Control is the prior rejected reservation candidate. Identical seed does not imply identical endogenous signals or event ordering across mechanisms. Historical trade windows are descriptive, with no order-book baseline. Endpoint asset checks do not establish every intraday reservation or a full fee/insurance ledger. No promotion decision is automated from these statistics.')
    write(BASE/'combined-comparison.json',result)
    import matplotlib
    write(BASE/'combined-analysis-environment.json',dict(python=sys.version,numpy=np.__version__,matplotlib=matplotlib.__version__,
        sources={name:sha(Path(__file__).with_name(name)) for name in ('summarize_causal_studies.py','analyze_causal_mechanisms.py','analyze_market_mechanisms.py')}))
    with (BASE/'combined-comparison.csv').open('w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=list(summary[0]));w.writeheader();w.writerows(summary)
    chart(result)
    print(json.dumps(dict(completed_new_runs=13,aggregates=aggregates,market_links=links,reference_ranges=reference_ranges),indent=2))
    return result


def chart(result):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':9,'axes.spines.top':False,'axes.spines.right':False})
    columns=[('return_std_bp','1m return std (bp)'),('return_acf1','1m return lag-1 correlation'),('quote_mid_acf1','1m quote-mid lag-1 correlation'),('wick_5m','5m median wick fraction'),('two_sided_pct','Two-sided quote availability (%)')]
    fig,axes=plt.subplots(2,5,figsize=(19,8.5));colors=('#8b6d45','#1f4e79','#5b7e40','#67517d')
    for leg,market in enumerate(MARKETS):
        for col,(key,title) in enumerate(columns):
            ax=axes[leg,col]
            for x,(profile,color) in enumerate(zip(PROFILES,colors)):
                rs=sorted((r for r in result['summary'] if r['market']==market and r['profile']==profile),key=lambda r:r['seed'])
                missing=sum(r[key] is None for r in rs)
                if missing:ax.text(x,.04,f'{missing} missing',transform=ax.get_xaxis_transform(),ha='center',fontsize=8)
                valid=[r for r in rs if r[key] is not None]
                if valid:
                    vals=[r[key] for r in valid];ax.scatter([x+((7,19,41,73).index(r['seed'])-1.5)*.08 for r in valid],vals,color=color,s=30)
                    ax.plot([x-.22,x+.22],[np.mean(vals)]*2,color=color,lw=2)
            ref=next(r for r in result['reference_ranges'] if r['market']==market)
            if key in ref['ranges']:ax.axhspan(*ref['ranges'][key],color='#888',alpha=.13)
            ax.set_xticks(range(4),[LABELS[p] for p in PROFILES],rotation=28,ha='right');ax.grid(axis='y',alpha=.18)
            ax.set_ylabel(market.replace('V-BTC-',''))
            if leg==0:ax.set_title(title)
            if key.endswith('acf1'):ax.set_ylim(-1,1)
            elif key=='wick_5m':ax.set_ylim(0,1)
    fig.suptitle('Both frozen mechanism studies | all four seeds | primary hours 2–12',fontsize=15)
    fig.text(.5,.015,'Dots left to right: seeds 7, 19, 41, 73. Bars: means of per-run statistics. Gray: eight descriptive historical 10h trade windows.\nInventory level is separately frozen; one resource-cancelled seed7 attempt was retained and retried serially. Same executable and controls. No historical quote-mid/book baseline or holdout.',ha='center')
    fig.tight_layout(rect=[0,.07,1,.94]);fig.savefig(BASE/'combined-statistics.png',dpi=150);plt.close(fig)


if __name__=='__main__':summarize()
