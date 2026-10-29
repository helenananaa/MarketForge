"""Actual market outcomes and finite business-demand accounting, with cached controls."""
import argparse,csv,json,math,statistics
from collections import Counter,defaultdict
from pathlib import Path
import research_numeric
import numpy as np
from research_market_mechanisms import sha,write
from research_parent_liquidity import BASE
from analyze_market_mechanisms import corr,rebar,bar_metrics
from analyze_wick_full import enrich
from analyze_basis_execution import candles
from analyze_wick_basis_capacity import records,one as capacity
from analyze_price_formation_links import one as same_wave
from diagnose_flow_surprise_horizons import variance_ratios


def business(folder,row):
    recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'))
    configs={a['Plugin']['participant']['participant_id']:a['Plugin']for a in recipe['agents']}
    by_account={(p['participant']['account_id'],p['participant']['instrument_id']):p for p in configs.values()}
    path=folder/(row['name']+'.receipt.json');actual=Counter();all_orders={};books={}
    start,end=row['warmup_ms'],row['steps']*1000
    for t in records(path,'trade_receipts'):
        actual[t['taker_account_id'],t['instrument_id'],t['taker_side']]+=t['qty']
        k=t['instrument_id'],t['taker_order_id']
        if k not in all_orders:
            all_orders[k]=dict(market=t['instrument_id'],time_ms=t['time_ms'],side=t['taker_side'],qty=0,prices=set(),
                role=by_account.get((t['taker_account_id'],t['instrument_id']),{}).get('plugin_id','other'))
        o=all_orders[k];assert (o['time_ms'],o['side'])==(t['time_ms'],t['taker_side'])
        o['qty']+=t['qty'];o['prices'].add(t['price_tick'])
    details=[]
    for s in records(path,'states'):
        p=configs[s['id']]
        if p['plugin_id']!='ExecutionTrader':continue
        c=p['config'];state=s['state']['Plugin']['data'];market=p['participant']['instrument_id']
        filled=actual[p['participant']['account_id'],market,c['side']]
        active=state.get('execution_start_ms') is not None
        ledger=state.get('execution_ledger')
        if c.get('liquidity_aware'):
            assert not active or ledger is not None
            if ledger:
                assert ledger['demand_lots']==c['target_qty']
                assert ledger['demand_lots']==ledger['filled_lots']+ledger['remaining_lots']+ledger['expired_lots']
                assert ledger['filled_lots']==state['completed_qty']==filled
                assert ledger['pending'] is None,'Native child acknowledgement missing'
        ended=active and end>=state['execution_start_ms']+c['horizon_ms']
        expired=(ledger['expired_lots'] if ledger else max(0,c['target_qty']-filled) if ended else 0)
        remaining=(ledger['remaining_lots'] if ledger else max(0,c['target_qty']-filled-expired) if active else 0)
        if active:
            assert filled<=c['target_qty']
            # Legacy progress is observed before its last child, whereas the
            # new ledger is committed by feedback. Preserve the visible lag.
            assert 0<=filled-state['completed_qty']<=c['max_qty'],'Inspect original-side risk/liquidation or progress beyond last-child observation lag'
        details.append(dict(participant=s['id'],account=p['participant']['account_id'],market=market,side=c['side'],
            planned_demand_lots=c['target_qty'],activated=active,scheduled_start_ms=c['start_after_ms'],
            actual_start_ms=state.get('execution_start_ms'),actual_deadline_ms=state.get('execution_start_ms',0)+c['horizon_ms'] if active else None,
            activated_demand_lots=c['target_qty'] if active else 0,filled_lots=filled,expired_lots=expired,remaining_lots=remaining,
            saved_completed_lots=state['completed_qty'],saved_progress_lag_lots=filled-state['completed_qty'],demand_ledger=ledger))
    # Build post-wave quote series; use actually available sides only.
    depth=defaultdict(list)
    for s in records(path,'book_snapshots'):
        t=s['time_ms'];market=s['instrument_id'];b=s['book'];assert s['phase']=='after_all_actions'
        if b['bids'] and b['asks']:
            books[t,market]=(b['bids'][0]['price_tick']+b['asks'][0]['price_tick'])/2
            if start<=t<end:
                depth[market].append((b['bids'][0]['qty'],b['asks'][0]['qty'],sum(x['qty']for x in b['bids'][:5]),sum(x['qty']for x in b['asks'][:5])))
    flows=[]
    for market in ('V-BTC-SPOT','V-BTC-PERP'):
        ordered=[o for o in all_orders.values()if o['market']==market and start<=o['time_ms']<end]
        signs=[1 if o['side']=='Buy'else -1 for o in ordered]
        arrivals=[b['time_ms']-a['time_ms']for a,b in zip(ordered,ordered[1:])]
        assert all(d>=0 for d in arrivals)
        run=0;previous=0;lengths=[]
        for sign in signs:
            if sign==previous:run+=1
            else:
                if run:lengths.append(run)
                run=1;previous=sign
        if run:lengths.append(run)
        parent_orders=[o for o in ordered if o['role']=='ExecutionTrader']
        response=[]
        for horizon in (0,1000,5000,30000):
            values=[]
            for o in parent_orders:
                t=o['time_ms'];before=books.get((t-1000,market));after=books.get((t+horizon,market))
                if before and after and t+horizon<end:
                    values.append((1 if o['side']=='Buy'else -1)*math.log(after/before)*10000)
            response.append(dict(horizon_ms=horizon,samples=len(values),median_signed_bp=statistics.median(values)if values else None))
        ds=depth[market]
        flows.append(dict(market=market,executed_taker_orders=len(ordered),same_side_fraction=sum(a==b for a,b in zip(signs,signs[1:]))/(len(signs)-1)if len(signs)>1 else None,
            sign_acf={str(lag):corr(signs[lag:],signs[:-lag])for lag in (1,5,10,20,60)},
            same_side_run_median=statistics.median(lengths)if lengths else None,same_side_run_max=max(lengths,default=0),
            zero_interarrival_fraction=sum(x==0 for x in arrivals)/len(arrivals)if arrivals else None,
            positive_interarrival_median_ms=statistics.median([x for x in arrivals if x>0])if any(x>0 for x in arrivals)else None,
            parent_executed_children=len(parent_orders),parent_multilevel_children=sum(len(o['prices'])>1 for o in parent_orders),
            best_bid_qty_median=statistics.median(x[0]for x in ds)if ds else None,best_ask_qty_median=statistics.median(x[1]for x in ds)if ds else None,
            visible5_bid_qty_median=statistics.median(x[2]for x in ds)if ds else None,visible5_ask_qty_median=statistics.median(x[3]for x in ds)if ds else None,
            parent_signed_post_wave_response=response))
    totals={k:sum(d[k]for d in details)for k in ('planned_demand_lots','activated_demand_lots','filled_lots','expired_lots','remaining_lots')}
    return dict(seed=row['seed'],profile=row['profile'],name=row['name'],receipt_sha256=sha(path),**totals,
        filled_fraction=totals['filled_lots']/totals['activated_demand_lots']if totals['activated_demand_lots']else None,
        expired_fraction=totals['expired_lots']/totals['activated_demand_lots']if totals['activated_demand_lots']else None,
        accounts=len(details),activated_accounts=sum(d['activated']for d in details),details=details,flow=flows)


def chart(out,seed,pair,minutes=5):
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
                if k*minutes*60000<7200000 or (k+1)*minutes*60000>14400000:continue
                x=k*minutes/60;op,hi,lo,cl=[(b[z]/100000-1)*100 for z in ('open','high','low','close')]
                color='#245b80'if cl>=op else'#ac6529';width=minutes/60*.68
                axes[2*leg,col].vlines(x,lo,hi,color=color,lw=.7)
                axes[2*leg,col].add_patch(Rectangle((x-width/2,min(op,cl)),width,max(abs(cl-op),.00001),facecolor='white'if cl>=op else color,edgecolor=color,lw=.7))
                axes[2*leg+1,col].bar(x,b['volume'],width=width*1.1,color=color,alpha=.7);bounds.extend((lo,hi));vmax=max(vmax,b['volume'])
            axes[2*leg,col].set_title(market.replace('V-BTC-','')+' | '+profile);axes[2*leg,col].grid(axis='y',alpha=.2)
            axes[2*leg+1,col].set_xlabel('Simulation hour')
        pad=max(.005,(max(bounds)-min(bounds))*.06)
        for ax in axes[2*leg]:ax.set_ylim(min(bounds)-pad,max(bounds)+pad)
        for ax in axes[2*leg+1]:ax.set_ylim(0,vmax*1.08)
        axes[2*leg,0].set_ylabel('Change from initial price (%)');axes[2*leg+1,0].set_ylabel('Actual lots')
    fig.suptitle(f'Actual {minutes}min OHLCV | fixed seed {seed} | complete primary hours2–4',fontsize=15)
    fig.text(.5,.015,'Same business-parent targets, start configuration, deadlines, funds, fees and maker/hedge policy.\nOne child-execution mechanism; common market scales, all actual trades, no smoothing, filling, filtering or clipping.',ha='center')
    fig.tight_layout(rect=[0,.055,1,.95]);fig.savefig(out/f'parent-liquidity-seed{seed}-{minutes}m.png',dpi=145);plt.close(fig)


def analyze(out,only_seed=None):
    assert np.__version__=='2.5.3'
    sources=[BASE/'controls',out] if out.name!='holdout' else[out]
    complete=[];rows=[];metrics=[];links=[];parents=[];capacities=[];quotes=[];horizons=[]
    for folder in sources:
        plan=json.loads((folder/'plan.json').read_text())
        assert sha(folder/'candidate.exe')==plan['binary_sha256']
        for r in plan['runs']:
            if only_seed is not None and r['seed']!=only_seed:continue
            # Restrict cached controls to candidates already complete.
            if folder!=out and not (out/f"native-s{r['seed']}-liquidity-parent.run.json").exists():continue
            path=folder/(r['name']+'.run.json')
            if not path.exists():continue
            done=json.loads(path.read_text());assert done['complete']
            assert done['inputs']['binary_sha256']==plan['binary_sha256']
            m,rs,ls=enrich(folder,r);metrics.append(m);rows.extend(rs);links.append(ls)
            parents.append(business(folder,r));capacities.extend(capacity(folder,r));quotes.append(same_wave(folder,r))
            for market in m['markets']:
                bs=candles(folder/(r['name']+'-'+market+'.candles.csv'))
                selected={k:b for k,b in bs.items()if 7200000<=k*60000 and (k+1)*60000<=14400000}
                horizons.append(dict(seed=r['seed'],profile=r['profile'],market=market,horizons=variance_ratios(selected),hourly=[dict(hour=h,**bar_metrics(bs,h*3600000,(h+1)*3600000))for h in (2,3)]))
            complete.append((folder,r))
    pairs=0
    for seed in sorted({r['seed']for _,r in complete}):
        pair={r['profile']:(folder,r)for folder,r in complete if r['seed']==seed}
        if set(pair)=={'legacy','liquidity-parent'}:
            pairs+=1
            for minutes in (1,5):chart(out,seed,pair,minutes)
    required=1 if only_seed is not None else len(json.loads((out/'plan.json').read_text())['runs'])//(2 if out.name=='holdout'else 1)
    result=dict(rows=rows,metrics=metrics,market_links=links,parents=parents,capacities=capacities,same_wave_links=quotes,horizons=horizons,
        completed_pairs=pairs,complete=pairs==required,analyzer_sha256=sha(Path(__file__)),numpy_version=np.__version__,
        scope='Actual4h native runs, primary2–4h. Cached real controls from prior study, not new native work. Whole-run business demand accounting separately distinguishes planned vs activated parents. Executed order signs collapse same taker-order multi-level prints. Arrival time has1s resolution; post-wave response combines all same-wave activity and is observational, not isolated causal impact or an atomic fill guarantee. No parameter tuning or independent long-duration market qualification.')
    write(out/'comparison.json',result)
    with (out/'comparison.csv').open('w',encoding='utf-8',newline='')as f:
        w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)
    print(json.dumps(dict(completed_pairs=pairs,complete=result['complete'],rows=rows,parent_totals=[{k:v for k,v in p.items()if k not in ('details','flow')}for p in parents]),indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);p.add_argument('--seed',type=int);a=p.parse_args();analyze(a.directory,a.seed)
