"""Frozen12h full-window diagnostics; no outcome-based interval selection."""
import argparse,csv,json,math
from pathlib import Path
import research_numeric
import numpy as np
from research_parent_liquidity import BASE,SEEDS,active_main
from research_market_mechanisms import sha,write
from analyze_parent_liquidity import business
from analyze_wick_full import enrich
from analyze_wick_basis_capacity import one as capacity,records
from analyze_price_formation_links import one as same_wave
from analyze_basis_execution import candles
from analyze_market_mechanisms import bar_metrics,rebar
from diagnose_flow_surprise_horizons import variance_ratios

CONTROL=BASE/'long12h-controls-20261026'
CANDIDATE=BASE/'long12h-candidates-20261026'


def prefix(folder,row):
    """A longer horizon must reproduce the entire existing4h print prefix."""
    prior_folder=BASE/'controls'if row['profile']=='legacy'else active_main()
    prior_row=next(r for r in json.loads((prior_folder/'plan.json').read_text())['runs']if r['seed']==row['seed'])
    old=records(prior_folder/(prior_row['name']+'.receipt.json'),'trade_receipts')
    new=records(folder/(row['name']+'.receipt.json'),'trade_receipts');count=0
    try:
        for t in old:
            assert next(new)==t,'Long-run trade prefix differs from frozen4h outcome'
            count+=1
        tail=next(new,None)
        assert tail is None or tail['time_ms']>=14400000
    finally:
        old.close();new.close()
    return dict(seed=row['seed'],profile=row['profile'],trade_prefix_records=count,byte_value_identical=True,
        scope='All4h trade objects including exact boundary match in order. A longer duration on the same seed is temporal validation, not independent random-seed evidence.')


def chart(out,seed,pair,minutes,start,end):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    fig,axes=plt.subplots(4,2,figsize=(16,10),sharex=True,gridspec_kw={'height_ratios':[3,1,3,1]})
    for leg,market in enumerate(('V-BTC-SPOT','V-BTC-PERP')):
        bounds=[];vmax=0
        for col,profile in enumerate(('legacy','liquidity-parent')):
            folder,row=pair[profile]
            bs=rebar(candles(folder/(row['name']+'-'+market+'.candles.csv')),minutes)
            for k,b in sorted(bs.items()):
                if k*minutes*60000<start or (k+1)*minutes*60000>end:continue
                x=k*minutes/60;op,hi,lo,cl=[(b[z]/100000-1)*100 for z in ('open','high','low','close')]
                color='#245b80'if cl>=op else'#ac6529';width=minutes/60*.68
                axes[2*leg,col].vlines(x,lo,hi,color=color,lw=.7)
                axes[2*leg,col].add_patch(Rectangle((x-width/2,min(op,cl)),width,max(abs(cl-op),.00001),facecolor='white'if cl>=op else color,edgecolor=color,lw=.7))
                axes[2*leg+1,col].bar(x,b['volume'],width=width*1.1,color=color,alpha=.7)
                bounds.extend((lo,hi));vmax=max(vmax,b['volume'])
            axes[2*leg,col].set_title(market.replace('V-BTC-','')+' | '+profile);axes[2*leg,col].grid(axis='y',alpha=.2)
            axes[2*leg+1,col].set_xlabel('Simulation hour')
        if bounds:
            pad=max(.005,(max(bounds)-min(bounds))*.06)
            for ax in axes[2*leg]:ax.set_ylim(min(bounds)-pad,max(bounds)+pad)
        for ax in axes[2*leg+1]:ax.set_ylim(0,max(vmax*1.08,1))
        axes[2*leg,0].set_ylabel('Change from initial price (%)');axes[2*leg+1,0].set_ylabel('Actual lots')
    fig.suptitle(f'Actual {minutes}min OHLCV | fixed seed {seed} | hours{start/3600000:g}–{end/3600000:g}',fontsize=15)
    fig.text(.5,.015,'Exact frozen4h economic recipes and binaries; only simulation duration extended.\nAll actual trades, common market scales; no smoothing, filling, filtering or clipping. Fixed windows for every seed.',ha='center')
    fig.tight_layout(rect=[0,.055,1,.95]);fig.savefig(out/f'parent-liquidity-seed{seed}-{minutes}m.png',dpi=145);plt.close(fig)


def one_seed(seed):
    rows=[];metrics=[];links=[];parents=[];capacities=[];quotes=[];horizons=[];prefixes=[];pair={}
    for folder,profile in ((CONTROL,'legacy'),(CANDIDATE,'liquidity-parent')):
        plan=json.loads((folder/'plan.json').read_text());assert sha(folder/'candidate.exe')==plan['binary_sha256']
        r=next(r for r in plan['runs']if r['seed']==seed)
        done=json.loads((folder/(r['name']+'.run.json')).read_text());assert done['complete'] and done['inputs']['steps']==43200
        assert done['inputs']['binary_sha256']==plan['binary_sha256']
        m,rs,ls=enrich(folder,r);metrics.append(m);rows.extend(rs);links.append(ls)
        parents.append(business(folder,r));capacities.extend(capacity(folder,r));quotes.append(same_wave(folder,r));prefixes.append(prefix(folder,r))
        for market in m['markets']:
            bs=candles(folder/(r['name']+'-'+market+'.candles.csv'))
            selected={k:b for k,b in bs.items()if 7200000<=k*60000 and (k+1)*60000<=43200000}
            horizons.append(dict(seed=seed,profile=profile,market=market,horizons=variance_ratios(selected),
                terminal=bar_metrics(bs,36000000,43200000),hourly=[dict(hour=h,**bar_metrics(bs,h*3600000,(h+1)*3600000))for h in range(2,12)]))
        pair[profile]=(folder,r)
    chart(CANDIDATE,seed,pair,5,7200000,43200000)
    chart(CANDIDATE,seed,pair,1,36000000,43200000)
    result=dict(seed=seed,rows=rows,metrics=metrics,market_links=links,parents=parents,capacities=capacities,
        same_wave_links=quotes,horizons=horizons,prefixes=prefixes,complete=True,completed_pairs=1,
        analyzer_sha256=sha(Path(__file__)),numpy_version=np.__version__,
        scope='Actual paired12h native runs; entire2–12h primary, fixed10–12h terminal and all hourly windows. Same four previously examined seeds, temporal extension not independent holdout. No economic tuning, clipped prints, filled bars, or market-realism certification.')
    write(CANDIDATE/f'seed{seed}-comparison.json',result);return result


def combine():
    data=[json.loads((CANDIDATE/f'seed{s}-comparison.json').read_text())for s in SEEDS]
    assert all(d['complete'] for d in data)
    keys=('rows','metrics','market_links','parents','capacities','same_wave_links','horizons','prefixes')
    result={k:[r for d in data for r in d[k]]for k in keys}
    result.update(complete=True,completed_pairs=4,seeds=list(SEEDS),analyzer_sha256=sha(Path(__file__)),
        scope=data[0]['scope'])
    write(CANDIDATE/'comparison.json',result)
    with (CANDIDATE/'comparison.csv').open('w',encoding='utf-8',newline='')as f:
        w=csv.DictWriter(f,fieldnames=list(result['rows'][0]));w.writeheader();w.writerows(result['rows'])
    print(json.dumps(dict(complete=True,completed_pairs=4,rows=result['rows']),indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--seed',type=int,choices=SEEDS);a=p.parse_args()
    assert np.__version__=='2.5.3'
    one_seed(a.seed)if a.seed is not None else combine()
