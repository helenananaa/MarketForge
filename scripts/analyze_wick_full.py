"""Compare every fixed-seed full wick outcome to the archived depth control."""
import argparse,csv,json,sys
from pathlib import Path
import research_numeric  # Configure numerical workers before NumPy loads.
import numpy as np
from analyze_market_mechanisms import analyze_run,sha,write,rebar
from analyze_basis_execution import candles
from analyze_wick_anatomy import shape,MARKETS
from summarize_market_feedback import market_link
from research_wick_observations import SOURCE
from analyze_wick_demand import analyze as demand


def enrich(out,row):
    target=out/(row['name']+'.metrics.json')
    if out==SOURCE:assert target.exists(),'Do not recreate or overwrite archived baseline exports'
    if target.exists():
        m=json.loads(target.read_text(encoding='utf-8'));assert m['receipt_sha256']==sha(out/(row['name']+'.receipt.json'))
    else:m=analyze_run(out,row)
    assert sha(Path(row['recipe']))==row['recipe_sha256']==m['recipe_sha256']
    m={**m,'profile':row['profile']}
    bs={};records=[]
    for market in MARKETS:
        bs[market]=candles(out/(row['name']+'-'+market+'.candles.csv'));a=m['markets'][market]
        for minutes in (1,5,15,60):
            _,s=shape(rebar(bs[market],minutes),row['warmup_ms'],row['steps']*1000,minutes*60000)
            a[f'wick_anatomy_{minutes}m']=s
        for minutes in (1,5):
            _,s=shape(rebar(bs[market],minutes),0,1800000,minutes*60000)
            a[f'startup_wick_anatomy_{minutes}m']=s
        one,five=a['wick_anatomy_1m'],a['wick_anatomy_5m']
        records.append(dict(seed=row['seed'],profile=row['profile'],market=market,std_bp=a['minute']['return_std_bp'],acf1=a['minute']['return_acf1'],
            quote_mid_acf1=a['quoted_mid_minute']['return_acf1'],p99_abs_return_bp=a['minute']['abs_return_p99_bp'],max_abs_return_bp=a['minute']['abs_return_max_bp'],
            wick_1m=one['median']['wick_fraction'],wick_5m=five['median']['wick_fraction'],
            upper_5m_bp=five['median']['upper_bp'],lower_5m_bp=five['median']['lower_bp'],body_5m_bp=five['median']['body_bp'],range_5m_bp=five['median']['range_bp'],
            both_outer_quarters_1m=one['both_outer_quarters_fraction'],both_outer_quarters_5m=five['both_outer_quarters_fraction'],
            both_wicks_gt_body_5m=five['both_wicks_gt_body_fraction'],nonempty_minutes=one['bars'],two_sided_pct=100*a['two_sided_fraction'],
            nonzero_range_minutes=one['nonzero_range_bars'],zero_range_fraction_1m=1-one['nonzero_range_bars']/one['bars'] if one['bars'] else None,
            zero_range_fraction_5m=1-five['nonzero_range_bars']/five['bars'] if five['bars'] else None,zero_return_fraction_1m=a['minute']['zero_return_fraction'],
            spread_bp=a['spread_median_bp'],volume=a['volume_lots'],active=a['active_instances'],taker_same_direction=a['taker_order_same_direction']))
    if row['profile']!='control':write(target,m)
    return m,records,dict(seed=row['seed'],profile=row['profile'],**market_link(bs[MARKETS[0]],bs[MARKETS[1]],row['warmup_ms'],row['steps']*1000))


def chart(out,seed,old,new,minutes=5):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False})
    fig,axes=plt.subplots(4,2,figsize=(16,10),sharex=True,gridspec_kw={'height_ratios':[3,1,3,1]})
    start=7200000 if minutes==5 else 36000000;end=43200000
    for leg,market in enumerate(MARKETS):
        bounds=[];vmax=0
        for col,(folder,m) in enumerate(((SOURCE,old),(out,new))):
            bs=rebar(candles(folder/(m['name']+'-'+market+'.candles.csv')),minutes)
            for k,b in sorted(bs.items()):
                if not start<=k*minutes*60000 or (k+1)*minutes*60000>end:continue
                x=k*minutes/60;op,hi,lo,cl=[(b[z]/100000-1)*100 for z in ('open','high','low','close')];color='#1f4e79' if cl>=op else '#b46b25';width=minutes/60*.67
                axes[2*leg,col].vlines(x,lo,hi,color=color,lw=.7)
                axes[2*leg,col].add_patch(Rectangle((x-width/2,min(op,cl)),width,max(abs(cl-op),.00001),facecolor='white' if cl>=op else color,edgecolor=color,lw=.7))
                axes[2*leg+1,col].bar(x,b['volume'],width=width*1.15,color=color,alpha=.7);bounds.extend((lo,hi));vmax=max(vmax,b['volume'])
            label='control (old depth)' if col==0 else m['profile']
            axes[2*leg,col].set_title(market.replace('V-BTC-','')+' | '+label);axes[2*leg,col].grid(axis='y',alpha=.2)
            axes[2*leg+1,col].set_xlabel('Simulation hour')
        pad=max(.01,(max(bounds)-min(bounds))*.06)
        for ax in axes[2*leg]:ax.set_ylim(min(bounds)-pad,max(bounds)+pad)
        for ax in axes[2*leg+1]:ax.set_ylim(0,vmax*1.08)
        axes[2*leg,0].set_ylabel('Change from initial price (%)');axes[2*leg+1,0].set_ylabel(f'Lots / {minutes}m')
    fig.suptitle(f'Actual {minutes}m OHLCV | fixed seed {seed} | unchanged numerical recipe inputs',fontsize=15)
    fig.text(.5,.015,'Same numerical size/slippage/TTL inputs and finite accounts; execution lifecycle changes. Common scales per market.\nActual trade OHLCV, without smoothing, empty-bar filling, trade filtering or price clipping. '+('Entire primary hours2–12.' if minutes==5 else 'Fixed final hours10–12, all seeds.'),ha='center')
    fig.tight_layout(rect=[0,.055,1,.95]);fig.savefig(out/f'wick-seed{seed}-{minutes}m.png',dpi=150);plt.close(fig)


def analyze(out):
    assert np.__version__=='2.5.3'
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));prior=json.loads((SOURCE/'plan.json').read_text(encoding='utf-8'))
    rows=[];metrics=[];links=[];n=0
    for r in plan['runs']:
        path=out/(r['name']+'.run.json')
        if not path.exists():continue
        done=json.loads(path.read_text(encoding='utf-8'));assert done['complete'];assert done['inputs']['binary_sha256']==sha(out/'candidate.exe')
        oldrow=next(x for x in prior['runs'] if x['seed']==r['seed'] and x['profile']=='depth');old={**oldrow,'profile':'control'}
        om,rr,ll=enrich(SOURCE,old);rows.extend(rr);links.append(ll);metrics.append(om)
        nm,rr,ll=enrich(out,r);rows.extend(rr);links.append(ll);metrics.append(nm);n+=1
        chart(out,r['seed'],om,nm);chart(out,r['seed'],om,nm,1)
    result=dict(completed_candidates=n,complete=n==4,rows=rows,metrics=metrics,market_links=links,plan_sha256=sha(out/'plan.json'),
        analyzer_sha256=sha(Path(__file__)),base_analyzer_sha256=sha(Path(__file__).with_name('analyze_market_mechanisms.py')),
        scope='All available predeclared seed outcomes.12h runs, primary hours2–12; no empty-bar filling, smoothing, filtering or clipping. Control metrics are recomputed from immutable archived old-depth candles without changing their raw receipts/metadata. Pilot-selected mechanism and previously examined seeds, not independent holdout or full market-realism qualification.')
    write(out/'full-comparison.json',result)
    if rows:
        with (out/'full-comparison.csv').open('w',newline='',encoding='utf-8') as f:
            w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)
    if links:
        with (out/'market-links.csv').open('w',newline='',encoding='utf-8') as f:
            w=csv.DictWriter(f,fieldnames=list(links[0]));w.writeheader();w.writerows(links)
    demand(out)
    print(json.dumps(dict(completed_candidates=n,complete=n==4,rows=rows,links=links),indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);analyze(p.parse_args().directory)
