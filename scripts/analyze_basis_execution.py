"""Compare the frozen executable-basis policy against unchanged maker controls."""
from __future__ import annotations
import argparse,csv,json,sys,shutil
from pathlib import Path
import research_numeric  # Configure numerical workers before NumPy loads.
import numpy as np
from analyze_market_mechanisms import ANALYSIS_VERSION,analyze_run,sha,write,rebar,summary_csv
from research_basis_execution import DEFAULT_OUT,DEFAULT_SOURCE

def candles(path):
    with path.open() as f:
        return {int(r['bucket']):{k:float(v) for k,v in r.items()} for r in csv.DictReader(f)}

def morphology(bars,start,end,interval=60000):
    selected=[b for k,b in sorted(bars.items()) if k*interval>=start and (k+1)*interval<=end]
    ranges=[];wicks=[]
    for b in selected:
        span=b['high']-b['low'];body=abs(b['close']-b['open'])
        assert 0<=body<=span+1e-9 and b['low']>0
        ranges.append(span/b['open']*10000)
        if span>0:wicks.append(max(0.,1-body/span))
    return dict(bars=len(selected),nonzero_range_bars=len(wicks),
                median_range_bp=float(np.median(ranges)) if ranges else None,
                p99_range_bp=float(np.percentile(ranges,99)) if ranges else None,
                median_wick_fraction=float(np.median(wicks)) if wicks else None)

def compare(out,source):
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));results=[];failures=[]
    environment=dict(python=sys.version,numpy=np.__version__,base_analyzer_sha256=sha(Path(__file__).with_name('analyze_market_mechanisms.py')))
    reference_environment=json.loads((source/'analysis-environment.json').read_text(encoding='utf-8'))
    assert np.__version__==reference_environment['numpy'],'Use the same isolated NumPy version as the reference analysis'
    write(out/'analysis-environment.json',environment)
    for row in plan['runs']:
        done=out/(row['name']+'.run.json')
        if not done.exists():continue
        run=json.loads(done.read_text(encoding='utf-8'))
        if not run['complete']:
            failures.append(run);continue
        assert sha(Path(row['recipe']))==row['recipe_sha256']==run['inputs']['recipe_sha256']
        assert sha(out/(row['name']+'.receipt.json'))==run['receipt_sha256']
        path=out/(row['name']+'.metrics.json')
        metric=json.loads(path.read_text(encoding='utf-8')) if path.exists() else None
        if metric is None or metric['analysis_version']!=ANALYSIS_VERSION or metric.get('analysis_environment')!=environment:
            if path.exists():
                archived=out/('analysis-prior-'+row['name']+'-'+sha(path)[:12]+'.json')
                if not archived.exists():shutil.copyfile(path,archived)
            metric=analyze_run(out,row)
            metric['analysis_environment']=environment;write(path,metric)
        assert metric['receipt_sha256']==run['receipt_sha256']
        results.append(metric)
    pairs=[];compatibility=None
    for control in plan['controls']:
        assert sha(Path(control['receipt']))==control['receipt_sha256']
        assert sha(Path(control['recipe']))==control['recipe_sha256']
        assert sha(Path(control['metrics']))==control['metrics_sha256']
        base=json.loads(Path(control['metrics']).read_text(encoding='utf-8'))
        rerun=next((r for r in results if r['seed']==control['seed'] and r['profile']=='control'),None)
        if rerun is not None:
            compatibility=dict(seed=control['seed'],full_steps=rerun['steps'],
                byte_identical=rerun['receipt_sha256']==control['receipt_sha256'],
                prior_sha256=control['receipt_sha256'],rerun_sha256=rerun['receipt_sha256'])
            assert compatibility['byte_identical'],'Legacy control diverged; investigate before causal comparison'
        candidate=next((r for r in results if r['seed']==control['seed'] and r['profile']==plan.get('candidate_profile','executable')),None)
        if candidate is None:continue
        pair=dict(seed=control['seed'],control=base,candidate=candidate)
        for profile,r,directory in [('control',base,source),('candidate',candidate,out)]:
            for market,m in r['markets'].items():
                path=directory/(r['name']+'-'+market+'.candles.csv')
                b=candles(path)
                m['morphology_1m']=morphology(b,r['warmup_ms'],r['steps']*1000)
                m['morphology_5m']=morphology(rebar(b,5),r['warmup_ms'],r['steps']*1000,300000)
                m['candle_sha256']=sha(path)
        pairs.append(pair)
    result=dict(plan_sha256=sha(out/'plan.json'),complete=len(results)==len(plan['runs']),
        study_title=plan.get('study_title','Basis execution policy'),figure_prefix=plan.get('figure_prefix','basis'),
        control_label=plan.get('control_label','Midpoint basis'),candidate_label=plan.get('candidate_label','Executable basis'),
        terminal=len(results)+len(failures)==len(plan['runs']),completed_runs=len(results),failed_runs=failures,
        compatibility=compatibility,pairs=pairs,
        morphology_scope='Specified before the first follow-up result; descriptive candle shape, not a fitted acceptance score. Zero-range bars excluded from wick fraction only.')
    write(out/'basis-comparison.json',result)
    rows=[]
    for p in pairs:
        for profile in ('control','candidate'):
            row=dict(p[profile]);row['profile']=profile;rows.append(row)
    summary_csv(out,rows)
    for p in pairs:
        print('seed',p['seed'],{market:{profile:{k:round(p[profile]['markets'][market]['minute'][k],4) for k in ('return_std_bp','return_acf1','abs_return_p99_bp')} for profile in ('control','candidate')} for market in ('V-BTC-SPOT','V-BTC-PERP')},flush=True)
    print('status',result['completed_runs'],'/',len(plan['runs']),'compatibility',compatibility,flush=True)
    return result

def charts(out,source,result):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False})
    for pair in result['pairs']:
        for multiple,last_two in ((5,False),(1,True)):
            fig,axes=plt.subplots(4,2,figsize=(14,10),sharex=True,gridspec_kw={'height_ratios':[3,1,3,1]})
            for leg,market in enumerate(('V-BTC-SPOT','V-BTC-PERP')):
                bounds=[];volume_max=0
                for col,profile in enumerate(('control','candidate')):
                    r=pair[profile];directory=source if profile=='control' else out
                    bars=rebar(candles(directory/(r['name']+'-'+market+'.candles.csv')),multiple)
                    start=max(r['warmup_ms'],r['steps']*1000-7200000) if last_two else r['warmup_ms']
                    selected=[b for k,b in sorted(bars.items()) if k*60000*multiple>=start and (k+1)*60000*multiple<=r['steps']*1000]
                    ax,vol=axes[2*leg,col],axes[2*leg+1,col]
                    for b in selected:
                        x=b['bucket']*multiple/60;op,hi,lo,cl=[(b[k]/100000-1)*100 for k in ('open','high','low','close')]
                        color='#1f4e79' if cl>=op else '#b46b25';width=.0112*multiple
                        ax.vlines(x,lo,hi,color=color,lw=.7)
                        ax.add_patch(Rectangle((x-width/2,min(op,cl)),width,max(abs(cl-op),.00005),edgecolor=color,facecolor='white' if cl>=op else color,lw=.8))
                        vol.bar(x,b['volume'],width=width*1.15,color=color,alpha=.7)
                        bounds.extend((lo,hi));volume_max=max(volume_max,b['volume'])
                    ax.set_title(f"{market.replace('V-BTC-','')} | {result['control_label'] if col==0 else result['candidate_label']}")
                    ax.axhline(0,color='#666',lw=.7,ls=':');ax.grid(axis='y',alpha=.18);vol.grid(axis='y',alpha=.18)
                    if col==0:ax.set_ylabel('Return from initial price (%)');vol.set_ylabel(f'Lots / {multiple}m')
                    vol.set_xlabel('Simulation hour')
                pad=max((max(bounds)-min(bounds))*.06,.02)
                for ax in axes[2*leg]:ax.set_ylim(min(bounds)-pad,max(bounds)+pad)
                for ax in axes[2*leg+1]:ax.set_ylim(0,volume_max*1.08)
            fig.suptitle(f"{result['study_title']} | seed {pair['seed']} | actual {multiple}-minute OHLCV",fontsize=15)
            fig.text(.5,.015,'Same makers, numerical parameters, finite assets, signals and task schedules. Common scales per market; no empty-bar filling.',ha='center',fontsize=9)
            fig.tight_layout(rect=[0,.04,1,.96]);fig.savefig(out/f"{result['figure_prefix']}-seed{pair['seed']}-candles-{multiple}m{'-last2h' if last_two else '-full'}.png",dpi=150);plt.close(fig)
    if not result['pairs']:return
    metrics=[('minute','return_std_bp','1m return standard deviation','bp',1),('minute','return_acf1','1m return lag-1 correlation','correlation',1),('minute','abs_return_p99_bp','1m absolute return P99','bp',1),(None,'two_sided_fraction','Two-sided quote availability','%',100),(None,'spread_median_bp','Median quoted spread','bp',1)]
    fig,axes=plt.subplots(2,5,figsize=(18,7))
    reference=json.loads((source/'reference-statistics.json').read_text(encoding='utf-8'))
    for leg,market in enumerate(('V-BTC-SPOT','V-BTC-PERP')):
        for col,(section,key,title,unit,scale) in enumerate(metrics):
            ax=axes[leg,col]
            for pair in sorted(result['pairs'],key=lambda p:p['seed']):
                vals=[]
                for profile in ('control','candidate'):
                    m=pair[profile]['markets'][market];vals.append((m[section][key] if section else m[key])*scale)
                ax.plot([0,1],vals,marker='o',label=f"Seed {pair['seed']}")
            if section=='minute':
                ref=reference['markets']['spot' if leg==0 else 'perp'];xs=[w[key] for w in ref['two_hour_windows']]
                ax.axhspan(min(xs),max(xs),color='#777',alpha=.15)
                ax.axhline(ref['full_day'][key],ls='--',color='#555',lw=1)
            ax.set_xticks([0,1],['Control','Candidate']);ax.grid(axis='y',alpha=.18);ax.set_ylabel(f"{'SPOT' if leg==0 else 'PERP'} / {unit}")
            if leg==0:ax.set_title(title,fontsize=10)
    for col in range(5):
        lo=min(ax.get_ylim()[0] for ax in axes[:,col]);hi=max(ax.get_ylim()[1] for ax in axes[:,col])
        for ax in axes[:,col]:ax.set_ylim((-1,1) if col==1 else (lo,hi))
    handles,labels=axes[0,0].get_legend_handles_labels();fig.legend(handles,labels,loc='upper center',ncol=4,bbox_to_anchor=(.5,.96))
    fig.suptitle(result['study_title']+': all completed seed pairs',fontsize=15)
    fig.text(.5,.015,'Each run: hours 2–12. Gray: one historical day\'s descriptive 2h range, dashed: full day; not confidence intervals. No historical book data.',ha='center',fontsize=9)
    fig.tight_layout(rect=[0,.05,1,.89]);fig.savefig(out/(result['figure_prefix']+'-all-seeds-statistics.png'),dpi=150);plt.close(fig)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,default=DEFAULT_OUT)
    p.add_argument('--source',type=Path,default=DEFAULT_SOURCE);p.add_argument('--charts',action='store_true')
    a=p.parse_args();r=compare(a.output.resolve(),a.source.resolve())
    if a.charts:charts(a.output.resolve(),a.source.resolve(),r)
