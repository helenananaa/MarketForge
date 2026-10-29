"""Inspect every frozen pilot outcome; no winner is selected by low volatility."""
import argparse,csv,json,sys
from pathlib import Path
import numpy as np
from analyze_market_mechanisms import analyze_run,sha,write,rebar
from analyze_basis_execution import candles
from analyze_wick_anatomy import shape,MARKETS
from summarize_market_feedback import market_link
from research_wick_observations import BASE,PROFILES


def analyze(out):
    assert np.__version__=='2.5.3'
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));metrics=[];rows=[];failures=[];links=[]
    for r in plan['runs']:
        path=out/(r['name']+'.run.json')
        if not path.exists():continue
        done=json.loads(path.read_text(encoding='utf-8'))
        if not done['complete']:failures.append(done);continue
        assert done['inputs']['recipe_sha256']==r['recipe_sha256']==sha(Path(r['recipe']))
        assert done['inputs']['binary_sha256']==sha(out/'candidate.exe')
        target=out/(r['name']+'.metrics.json')
        if target.exists():m=json.loads(target.read_text(encoding='utf-8'));assert m['receipt_sha256']==sha(out/(r['name']+'.receipt.json'))
        else:m=analyze_run(out,r)
        bs={}
        for market in MARKETS:
            bs[market]=candles(out/(r['name']+'-'+market+'.candles.csv'));a=m['markets'][market]
            one,s1=shape(bs[market],r['warmup_ms'],r['steps']*1000,60000)
            five,s5=shape(rebar(bs[market],5),r['warmup_ms'],r['steps']*1000,300000)
            a['wick_anatomy_1m']=s1;a['wick_anatomy_5m']=s5
            rows.append(dict(seed=r['seed'],profile=r['profile'],market=market,std_bp=a['minute']['return_std_bp'],acf1=a['minute']['return_acf1'],
                quote_mid_acf1=a['quoted_mid_minute']['return_acf1'],p99_abs_return_bp=a['minute']['abs_return_p99_bp'],
                wick_1m=s1['median']['wick_fraction'],wick_5m=s5['median']['wick_fraction'],
                upper_5m_bp=s5['median']['upper_bp'],lower_5m_bp=s5['median']['lower_bp'],body_5m_bp=s5['median']['body_bp'],range_5m_bp=s5['median']['range_bp'],
                both_outer_quarters_5m=s5['both_outer_quarters_fraction'],both_wicks_gt_body_5m=s5['both_wicks_gt_body_fraction'],
                nonempty_minutes=len(one),two_sided_pct=100*a['two_sided_fraction'],spread_bp=a['spread_median_bp'],volume=a['volume_lots'],active=a['active_instances']))
        links.append(dict(seed=r['seed'],profile=r['profile'],**market_link(bs[MARKETS[0]],bs[MARKETS[1]],r['warmup_ms'],r['steps']*1000)))
        write(target,m);metrics.append(m)
    result=dict(plan_sha256=sha(out/'plan.json'),completed_runs=len(metrics),complete=len(metrics)==len(plan['runs']),failures=failures,
        rows=rows,metrics=metrics,market_links=links,analysis_environment=dict(python=sys.version,numpy=np.__version__,base_analyzer_sha256=sha(Path(__file__).with_name('analyze_market_mechanisms.py'))),
        pilot_analyzer_sha256=sha(Path(__file__)),
        scope=plan.get('analysis_scope','Fixed seed41 four-hour pilot, primary hours2–4. Each profile is an observation-only ablation with all numeric inputs held fixed. A screen, not full multi-seed qualification. All frozen outcomes retained; price-range reduction alone is not a wick correction. Real trade OHLCV is unchanged by analysis.'))
    write(out/'pilot-comparison.json',result)
    if rows:
        with (out/'pilot-comparison.csv').open('w',newline='',encoding='utf-8') as f:
            w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)
    chart(out,result)
    print(json.dumps({k:result[k] for k in ('completed_runs','complete','rows','market_links')},indent=2),flush=True)


def chart(out,result):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    by={m['profile']:m for m in result['metrics']}
    if 'control' not in by:return
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False})
    for profile in by:
        if profile=='control' or profile not in by:continue
        fig,axes=plt.subplots(4,2,figsize=(14,10),sharex=True,gridspec_kw={'height_ratios':[3,1,3,1]})
        for leg,market in enumerate(MARKETS):
            bounds=[];vmax=0
            for col,p in enumerate(('control',profile)):
                m=by[p];bars=rebar(candles(out/(m['name']+'-'+market+'.candles.csv')),5)
                for k,b in sorted(bars.items()):
                    if not m['warmup_ms']<=k*300000 or (k+1)*300000>m['steps']*1000:continue
                    x=k*5/60;op,hi,lo,cl=[(b[z]/100000-1)*100 for z in ('open','high','low','close')];color='#1f4e79' if cl>=op else '#b46b25'
                    axes[2*leg,col].vlines(x,lo,hi,color=color,lw=.8)
                    axes[2*leg,col].add_patch(Rectangle((x-.028,min(op,cl)),.056,max(abs(cl-op),.00005),facecolor='white' if cl>=op else color,edgecolor=color,lw=.8))
                    axes[2*leg+1,col].bar(x,b['volume'],width=.063,color=color,alpha=.7);bounds.extend((lo,hi));vmax=max(vmax,b['volume'])
                axes[2*leg,col].set_title(market.replace('V-BTC-','')+' | '+p);axes[2*leg,col].grid(axis='y',alpha=.2)
                axes[2*leg+1,col].set_xlabel('Simulation hour')
            pad=max(.02,(max(bounds)-min(bounds))*.07)
            for ax in axes[2*leg]:ax.set_ylim(min(bounds)-pad,max(bounds)+pad)
            for ax in axes[2*leg+1]:ax.set_ylim(0,vmax*1.08)
            axes[2*leg,0].set_ylabel('Change from initial price (%)');axes[2*leg+1,0].set_ylabel('Lots / 5m')
        fig.suptitle('Wick mechanism screen | seed41 | actual 5m OHLCV',fontsize=15)
        fig.text(.5,.015,'Same numerical recipe inputs and finite accounts; only declared mechanism differs. Common axes per market. No smoothing, filling or clipping.\nFixed hours2–4 from four-hour runs; not final qualification. Control = prior touch-hedge + depth + reservation-innovation profile.',ha='center')
        fig.tight_layout(rect=[0,.055,1,.95]);fig.savefig(out/f'wick-screen-{profile}.png',dpi=150);plt.close(fig)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,default=BASE/'pilot-observations');analyze(p.parse_args().output.resolve())
