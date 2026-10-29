"""Paired market statistics against the immediately preceding wick-repair control."""
import argparse,csv,json
import research_numeric  # Configure numerical workers before NumPy loads.
import numpy as np
from pathlib import Path
from research_market_mechanisms import sha,write
from research_price_formation import PREVIOUS
from analyze_wick_full import enrich
from analyze_market_mechanisms import rebar
from analyze_basis_execution import candles


def chart(out,seed,row,control_directory=None,control_name=None):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    fig,axs=plt.subplots(4,2,figsize=(16,10),sharex=True,gridspec_kw={'height_ratios':[3,1,3,1]})
    for leg,market in enumerate(('V-BTC-SPOT','V-BTC-PERP')):
        bounds=[];maxvol=0
        for col,(folder,name,label) in enumerate(((control_directory or PREVIOUS,control_name or f'native-s{seed}-dual-execution','paired wick-repair control'),(out,row['name'],row['profile']))):
            bs=rebar(candles(folder/(name+'-'+market+'.candles.csv')),5)
            for k,b in sorted(bs.items()):
                if not row['warmup_ms']<=k*300000 or (k+1)*300000>row['steps']*1000:continue
                x=k/12;op,hi,lo,cl=[(b[z]/100000-1)*100 for z in ('open','high','low','close')]
                color='#245b80'if cl>=op else'#ac6529';w=.057
                axs[2*leg,col].vlines(x,lo,hi,color=color,lw=.7)
                axs[2*leg,col].add_patch(Rectangle((x-w/2,min(op,cl)),w,max(abs(cl-op),.00001),facecolor='white'if cl>=op else color,edgecolor=color,lw=.7))
                axs[2*leg+1,col].bar(x,b['volume'],width=.07,color=color,alpha=.7)
                bounds.extend((lo,hi));maxvol=max(maxvol,b['volume'])
            axs[2*leg,col].set_title(market.replace('V-BTC-','')+' | '+label)
            axs[2*leg,col].grid(axis='y',alpha=.2);axs[2*leg+1,col].set_xlabel('Simulation hour')
        pad=max(.005,(max(bounds)-min(bounds))*.06)
        for ax in axs[2*leg]:ax.set_ylim(min(bounds)-pad,max(bounds)+pad)
        for ax in axs[2*leg+1]:ax.set_ylim(0,maxvol*1.08)
        axs[2*leg,0].set_ylabel('Change from initial price (%)');axs[2*leg+1,0].set_ylabel('Actual lots / 5min')
    fig.suptitle(f'Actual 5min OHLCV | fixed seed {seed} | entire primary window',fontsize=15)
    fig.text(.5,.015,'Common price and volume scales within each market. Raw trade OHLCV; no smoothing, filtering or clipping.\nOnly declared maker mechanism flags differ; finite capital and all numerical recipe inputs unchanged.',ha='center')
    fig.tight_layout(rect=[0,.055,1,.95]);fig.savefig(out/f'price-formation-seed{seed}-5m.png',dpi=145);plt.close(fig)


def analyze(out,control_directory=None):
    assert np.__version__=='2.5.3','Use the frozen analysis-deps runtime'
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));rows=[];links=[];metrics=[]
    control_rows=[];control_links=[];control_metrics=[]
    for row in plan['runs']:
        f=out/(row['name']+'.run.json')
        if not f.exists():continue
        done=json.loads(f.read_text(encoding='utf-8'));assert done['complete']
        assert done['inputs']['binary_sha256']==plan['binary_sha256']==sha(out/'candidate.exe')
        control=None
        if control_directory:
            cp=json.loads((control_directory/'plan.json').read_text(encoding='utf-8'))
            control=next(x for x in cp['runs']if x['seed']==row['seed'])
            cm,cr,cl=enrich(control_directory,{**control,'profile':'dual-execution'})
            control_rows.extend(cr);control_links.append(cl);control_metrics.append(cm)
        m,r,l=enrich(out,row);rows.extend(r);links.append(l);metrics.append(m)
        if row['steps']==43200 or control_directory:chart(out,row['seed'],row,control_directory,control['name']if control else None)
    old=json.loads((PREVIOUS/'full-comparison.json').read_text(encoding='utf-8'))
    oldrows=control_rows or [r for r in old['rows']if r['profile']=='dual-execution'];oldlinks=control_links or [r for r in old['market_links']if r['profile']=='dual-execution']
    result=dict(completed_runs=len(metrics),planned_runs=len(plan['runs']),complete=len(metrics)==len(plan['runs']),rows=rows,market_links=links,metrics=metrics,
        previous_full_rows=oldrows,previous_full_market_links=oldlinks,plan_sha256=sha(out/'plan.json'),analyzer_sha256=sha(Path(__file__)),
        control_metrics=control_metrics,control_directory=str(control_directory)if control_directory else None,
        numpy_version=np.__version__,
        primary_window_ms=[plan['runs'][0]['warmup_ms'],plan['runs'][0]['steps']*1000],
        scope='Use the exact horizon/window in each frozen phase plan. With control_directory, main rows are paired against the same-platform same-window controls stored there. Without it, pilot/holdout comparisons use their own same-window control rows; previous_full_rows are archived Windows context only and must not be mixed into those short comparisons. All attempted profiles/seeds retained. Seed101 is an additional short holdout; no long-duration or calibrated realism qualification.')
    write(out/'comparison.json',result)
    for name,data in [('comparison.csv',rows),('market-links.csv',links)]:
        if data:
            with (out/name).open('w',encoding='utf-8',newline='') as f:
                w=csv.DictWriter(f,fieldnames=list(data[0]));w.writeheader();w.writerows(data)
    print(json.dumps(dict(completed_runs=len(metrics),rows=rows,links=links),indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);p.add_argument('--control-directory',type=Path);a=p.parse_args();analyze(a.directory,a.control_directory)
