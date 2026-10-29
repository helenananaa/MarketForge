"""All-seed evaluation of the frozen causal mechanism study; no volatility-only score."""
from __future__ import annotations
import argparse,csv,json,sys
from pathlib import Path
import numpy as np
from analyze_market_mechanisms import ROOT,ANALYSIS_VERSION,analyze_run,rebar,sha,write
from analyze_basis_execution import candles,morphology
from summarize_market_feedback import market_link
from research_causal_mechanisms import DEFAULT_OUT,DEFAULT_SOURCE

MARKETS=('V-BTC-SPOT','V-BTC-PERP')
PROFILES=('control','hedged','depth')
LABELS={'control':'Prior reservation policy','hedged':'Executable touch hedge','depth':'Hedge + public-depth flow'}
SHORT_LABELS={'control':'Control','hedged':'Hedge','depth':'Hedge + depth'}


def analyze(out,source):
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));runs=[];failures=[];pairs=[]
    env=dict(python=sys.version,numpy=np.__version__,base_analyzer_sha256=sha(Path(__file__).with_name('analyze_market_mechanisms.py')),schema=1)
    assert np.__version__=='2.5.3','Use the recorded isolated analysis environment'
    write(out/'analysis-environment.json',env)
    for row in plan['runs']:
        path=out/(row['name']+'.run.json')
        if not path.exists():continue
        run=json.loads(path.read_text(encoding='utf-8'))
        assert sha(Path(row['recipe']))==row['recipe_sha256']==run['inputs']['recipe_sha256']
        if not run['complete']:failures.append(run);continue
        assert sha(out/(row['name']+'.receipt.json'))==run['receipt_sha256']
        metric_path=out/(row['name']+'.metrics.json')
        if metric_path.exists():
            metric=json.loads(metric_path.read_text(encoding='utf-8'))
            assert metric['analysis_version']==ANALYSIS_VERSION and metric['analysis_environment']==env
        else:
            metric=analyze_run(out,row);metric['analysis_environment']=env;write(metric_path,metric)
        assert metric['receipt_sha256']==run['receipt_sha256'];runs.append(metric)
    compatibility=None;summary=[];links=[]
    for control in plan['controls']:
        for key in ('recipe','receipt','metrics'):assert sha(Path(control[key]))==control[key+'_sha256']
        old=json.loads(Path(control['metrics']).read_text(encoding='utf-8'));seed=control['seed']
        repeat=next((r for r in runs if r['seed']==seed and r['profile']=='control'),None)
        if repeat:
            compatibility=dict(seed=seed,byte_identical=repeat['receipt_sha256']==control['receipt_sha256'],receipt_sha256=repeat['receipt_sha256'],steps=repeat['steps'])
            assert compatibility['byte_identical'],'Legacy control diverged; stop causal claims'
        pair=dict(seed=seed,control=old)
        for profile in PROFILES[1:]:
            found=next((r for r in runs if r['seed']==seed and r['profile']==profile),None)
            if found:pair[profile]=found
        for profile in PROFILES:
            if profile not in pair:continue
            metric=pair[profile];directory=source if profile=='control' else out;bars={}
            for market,m in metric['markets'].items():
                candle_path=directory/(metric['name']+'-'+market+'.candles.csv');b=candles(candle_path);bars[market]=b
                m['morphology_1m']=morphology(b,7200000,43200000)
                m['morphology_5m']=morphology(rebar(b,5),7200000,43200000,300000)
                m['candle_sha256']=sha(candle_path)
                summary.append(dict(seed=seed,profile=profile,market=market,
                    **{k:m['minute'][k] for k in ('return_std_bp','return_acf1','abs_return_acf1','abs_return_p99_bp','abs_return_max_bp','volume_cv','volume_abs_return_corr')},
                    quote_mid_std_bp=m['quoted_mid_minute']['return_std_bp'],quote_mid_acf1=m['quoted_mid_minute']['return_acf1'],
                    wick_5m=m['morphology_5m']['median_wick_fraction'],range_5m_bp=m['morphology_5m']['median_range_bp'],range_5m_p99_bp=m['morphology_5m']['p99_range_bp'],
                    two_sided_pct=m['two_sided_fraction']*100,spread_median_bp=m['spread_median_bp'],
                    basis_taker_qty_share=m['taker_share_by_type'].get('BasisLinkTrader',0),active_instances=m['active_instances']))
            links.append(dict(seed=seed,profile=profile,**market_link(bars[MARKETS[0]],bars[MARKETS[1]],7200000,43200000)))
        pairs.append(pair)
    aggregates=[]
    for profile in PROFILES:
        for market in MARKETS:
            rows=[r for r in summary if r['profile']==profile and r['market']==market]
            keys=[k for k in rows[0] if k not in ('seed','profile','market')] if rows else []
            valid={k:all(r[k] is not None for r in rows) for k in keys}
            aggregates.append(dict(profile=profile,market=market,seeds=sorted(r['seed'] for r in rows),
                missing={k:sum(r[k] is None for r in rows) for k in keys if not valid[k]},
                mean={k:float(np.mean([r[k] for r in rows])) if valid[k] else None for k in keys},
                minimum={k:min(r[k] for r in rows) if valid[k] else None for k in keys},maximum={k:max(r[k] for r in rows) if valid[k] else None for k in keys}))
    result=dict(plan_sha256=sha(out/'plan.json'),completed_runs=len(runs),complete=len(runs)==len(plan['runs']),terminal=len(runs)+len(failures)==len(plan['runs']),
        failures=failures,compatibility=compatibility,pairs=pairs,summary=summary,market_links=links,aggregates=aggregates,
        scope=plan.get('scope','All frozen seed/profile outcomes retained. Primary [2h,12h). Control is the prior failed reservation candidate, not original touch. Same previously examined seeds, not holdouts. Quote-mid ACF, wick shape, cross-market linkage, activity and book availability matter alongside returns; lower volatility alone is not success.'))
    write(out/'causal-comparison.json',result)
    if summary:
        with (out/'comparison.csv').open('w',newline='',encoding='utf-8') as f:
            w=csv.DictWriter(f,fieldnames=list(summary[0]));w.writeheader();w.writerows(summary)
    print(json.dumps(dict(complete=result['complete'],completed_runs=len(runs),compatibility=compatibility,rows=[{k:r[k] for k in ('seed','profile','market','return_std_bp','return_acf1','abs_return_p99_bp','quote_mid_acf1','wick_5m')} for r in summary if r['profile']!='control'])),flush=True)
    return result


def charts(out,source,result,reference_path=None):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':9,'axes.spines.top':False,'axes.spines.right':False})
    for pair in result['pairs']:
        profiles=[p for p in PROFILES if p in pair]
        if len(profiles)==1:continue
        for multiple,last_two in ((5,False),(1,True)):
            fig,axes=plt.subplots(4,len(profiles),figsize=(7*len(profiles),10),sharex=True,gridspec_kw={'height_ratios':[3,1,3,1]},squeeze=False)
            for leg,market in enumerate(MARKETS):
                bounds=[];vmax=0
                for col,profile in enumerate(profiles):
                    r=pair[profile];directory=source if profile=='control' else out
                    bars=rebar(candles(directory/(r['name']+'-'+market+'.candles.csv')),multiple)
                    start=36000000 if last_two else 7200000
                    selected=[b for k,b in sorted(bars.items()) if k*multiple*60000>=start and (k+1)*multiple*60000<=43200000]
                    ax,vol=axes[2*leg,col],axes[2*leg+1,col]
                    for b in selected:
                        x=b['bucket']*multiple/60;op,hi,lo,cl=[(b[k]/100000-1)*100 for k in ('open','high','low','close')]
                        color='#1f4e79' if cl>=op else '#b46b25';width=.0112*multiple
                        ax.vlines(x,lo,hi,color=color,lw=.7)
                        ax.add_patch(Rectangle((x-width/2,min(op,cl)),width,max(abs(cl-op),.00005),edgecolor=color,facecolor='white' if cl>=op else color,lw=.8))
                        vol.bar(x,b['volume'],width=width*1.15,color=color,alpha=.7);bounds.extend((lo,hi));vmax=max(vmax,b['volume'])
                    ax.set_title(market.replace('V-BTC-','')+' | '+LABELS[profile]);ax.grid(axis='y',alpha=.18);ax.axhline(0,color='#777',ls=':',lw=.7)
                    if col==0:ax.set_ylabel('Change from initial price (%)');vol.set_ylabel(f'Lots / {multiple}m')
                    vol.set_xlabel('Simulation hour')
                pad=max((max(bounds)-min(bounds))*.06,.02)
                for ax in axes[2*leg]:ax.set_ylim(min(bounds)-pad,max(bounds)+pad)
                for ax in axes[2*leg+1]:ax.set_ylim(0,vmax*1.08)
            fig.suptitle(f'Causal mechanism study | seed {pair["seed"]} | actual {multiple}-minute OHLCV',fontsize=15)
            fig.text(.5,.015,'Same numerical inputs, signals, finite assets and task schedules. Common axes per market. No smoothing or empty-bar filling.',ha='center')
            fig.tight_layout(rect=[0,.04,1,.96]);fig.savefig(out/f'causal-seed{pair["seed"]}-candles-{multiple}m{"-last2h" if last_two else "-full"}.png',dpi=140);plt.close(fig)
    ref_path=reference_path or source/'summary/feedback-summary.json';reference=json.loads(ref_path.read_text(encoding='utf-8'))
    columns=[('return_std_bp','1m return standard deviation (bp)'),('return_acf1','1m return lag-1 correlation'),('quote_mid_acf1','1m quoted-mid lag-1 correlation'),('wick_5m','5m median wick fraction'),('two_sided_pct','Two-sided quote availability (%)')]
    fig,axes=plt.subplots(2,5,figsize=(18,8));colors=('#8b6d45','#1f4e79','#5b7e40')
    for leg,market in enumerate(MARKETS):
        for col,(key,title) in enumerate(columns):
            ax=axes[leg,col]
            for x,(profile,color) in enumerate(zip(PROFILES,colors)):
                rows=sorted((r for r in result['summary'] if r['market']==market and r['profile']==profile),key=lambda r:r['seed'])
                missing=sum(r[key] is None for r in rows)
                if missing:ax.text(x,.05,f'{missing} missing',transform=ax.get_xaxis_transform(),ha='center',fontsize=8)
                rows=[r for r in rows if r[key] is not None]
                vals=[r[key] for r in rows]
                if vals:
                    ax.scatter([x+(list((7,19,41,73)).index(r['seed'])-1.5)*.08 for r in rows],vals,color=color,s=30)
                    ax.plot([x-.2,x+.2],[np.mean(vals)]*2,color=color,lw=2)
            if key in ('return_std_bp','return_acf1','wick_5m'):
                vals=[r[key] for r in reference['reference_windows'] if r['market']==market];ax.axhspan(min(vals),max(vals),color='#888',alpha=.13)
            ax.set_xticks(range(len(PROFILES)),[SHORT_LABELS[p] for p in PROFILES],rotation=20,ha='right');ax.grid(axis='y',alpha=.18);ax.set_ylabel(market.replace('V-BTC-',''))
            if leg==0:ax.set_title(title)
            if key.endswith('acf1'):ax.set_ylim(-1,1)
            elif key=='wick_5m':ax.set_ylim(0,1)
    fig.suptitle('Fixed-seed mechanism comparisons | each dot is one complete run',fontsize=15)
    fig.text(.5,.015,'Dots left to right: seeds 7, 19, 41, 73. Lines: means of per-run statistics. Gray: eight calendar-selected historical 10h windows, descriptive only.\nNo historical order-book or quoted-mid reference. Control: '+LABELS['control']+'; all numeric inputs unchanged.',ha='center')
    fig.tight_layout(rect=[0,.07,1,.94]);fig.savefig(out/'causal-all-seeds-statistics.png',dpi=150);plt.close(fig)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,default=DEFAULT_OUT);p.add_argument('--source',type=Path,default=DEFAULT_SOURCE);p.add_argument('--charts',action='store_true')
    a=p.parse_args();r=analyze(a.output.resolve(),a.source.resolve())
    if a.charts:charts(a.output.resolve(),a.source.resolve(),r)
