"""Join quote arithmetic, actual fills and hedge residuals in the fixed trace window."""
from __future__ import annotations
import argparse,csv,gc,json
from collections import Counter,defaultdict
from pathlib import Path
from analyze_market_mechanisms import ROOT,sha,write
from analyze_causal_trace import order_details


def csv_rows(path):
    with path.open(encoding='utf-8') as f:return list(csv.DictReader(f))


def summarize(root):
    summaries=[]
    for profile in ('control','hedged','depth'):
        directory=root/(profile+'-analysis-v3')
        base=json.loads((directory/'trace-summary.json').read_text(encoding='utf-8'))
        assert base['analyzer_schema']==3 and base['maker_center_max_reconstruction_error']<1e-6
        basis=csv_rows(directory/'basis-decisions.csv');makers=csv_rows(directory/'maker-decisions.csv');trades=csv_rows(directory/'trades.csv')
        gateways=json.loads((directory/'gateway-accounts.json').read_text(encoding='utf-8'))
        by=defaultdict(list)
        for row in gateways:by[(row['time_ms'],row['participant'])].append(row)
        fills=Counter()
        for row in trades:fills[(row['market'],int(row['command_seq']))]+=int(row['qty'])
        entries=[r for r in basis if r['lead_entry']=='True'];entry_qty=filled=entry_filled=bad_filled=stale_entries=0
        for row in entries:
            entry_qty+=int(row['quoted_qty']);actual=0;stale=False
            for gateway in by[(int(row['time_ms']),row['participant'])]:
                action=order_details(gateway['action'])
                if not action:continue
                actual+=fills[(row['market'],gateway['command_seq'])]
                side=gateway['book_before']['asks'] if action['side']=='Buy' else gateway['book_before']['bids']
                reachable=bool(side and (side[0]['price_tick']<=action['price_tick'] if action['side']=='Buy' else side[0]['price_tick']>=action['price_tick']))
                stale|=not reachable
            filled+=actual;entry_filled+=actual>0;stale_entries+=stale
            if row['executable_entry']=='False':bad_filled+=actual
        lead=[r for r in basis if r['leg']=='Perp'];groups=defaultdict(list)
        for row in lead:groups[row['participant']].append(row)
        longest=0
        for group in groups.values():
            assert len(group)==660
            run=0;previous=None
            for row in group:
                t=int(row['time_ms']);nonzero=int(row['residual'])!=0
                run=(run+1 if previous is not None and t-previous==1000 else 1) if nonzero else 0
                longest=max(longest,run);previous=t
        recipe=root/'full-study'/f'native-s41-{profile}.recipe.json'
        population=json.loads(recipe.read_text(encoding='utf-8'))
        participants={a['Plugin']['participant']['participant_id']:a['Plugin'] for a in population['agents']}
        positions={};neutral={}
        for name,group in groups.items():
            p=participants[name];account_id=p['participant']['account_id'];first=group[0]
            positions[account_id]={'V-BTC-SPOT':int(first['spot_position']),'V-BTC-PERP':int(first['perp_position'])};neutral[account_id]=p['config']['neutral_spot']
        ledger=[]
        for gateway in gateways:
            p=participants[gateway['participant']]
            if p['plugin_id']!='BasisLinkTrader':continue
            account_id=p['participant']['account_id'];market=gateway['market']
            before=next(iter(gateway['before'].values()));after=next(iter(gateway['after'].values()))
            assert positions[account_id][market]==int(before['position_qty']),'Unexplained position change: inspect authoritative executions before interpreting this ledger'
            positions[account_id][market]=int(after['position_qty'])
            residual=sum(positions[account_id].values())-neutral[account_id]
            ledger.append(dict(time_ms=gateway['time_ms'],command_seq=gateway['command_seq'],participant=gateway['participant'],account_id=account_id,market=market,
                position_before=int(before['position_qty']),position_after=int(after['position_qty']),spot_position=positions[account_id]['V-BTC-SPOT'],perp_position=positions[account_id]['V-BTC-PERP'],residual=residual,
                available_cash_before=int(before['available_cash']),available_cash_after=int(after['available_cash']),fees_paid_before=int(before['fees_paid']),fees_paid_after=int(after['fees_paid'])))
        with (directory/'hedge-ledger.csv').open('w',newline='',encoding='utf-8') as f:
            w=csv.DictWriter(f,fieldnames=list(ledger[0]));w.writeheader();w.writerows(ledger)
        previous={};jumps=[]
        for row in makers:
            if int(row['quoted_qty'])<=0:continue
            at=(int(row['time_ms']),row['participant']);receipts=by[at]
            rested=sum(gateway['rested'] for gateway in receipts)
            if not rested:continue
            name=row['participant'];old=previous.get(name)
            if old:
                change=float(row['center'])-float(old['center'])
                jumps.append(dict(participant=name,from_ms=int(old['time_ms']),to_ms=at[0],center_change=change,
                    mid_change=float(row['mid'])-float(old['mid']),flow_change=float(row['flow_impact'])-float(old['flow_impact']),
                    inventory_from=int(old['position']),inventory_to=int(row['position']),
                    current_offset=float(row['reservation_offset']),previous_offset=float(row['previous_offset']),rested_orders=rested))
            previous[name]=row
        role_qty=Counter()
        for row in trades:role_qty[row['taker_role']]+=int(row['qty'])
        summary=dict(profile=profile,trace_sha256=base['trace_sha256'],entry_proposals=len(entries),entry_requested_qty=entry_qty,entry_filled_qty=filled,
            entries_with_real_fills=entry_filled,entries_unreachable_at_submission=stale_entries,
            entries_below_executable_threshold=base['unexecutable_at_configured_threshold'],bad_entry_filled_qty=bad_filled,
            entries_while_unhedged=sum(int(r['residual'])!=0 for r in entries),
            observed_pair_seconds=len(lead),unhedged_pair_seconds=sum(int(r['residual'])!=0 for r in lead),
            mean_absolute_residual=sum(abs(int(r['residual'])) for r in lead)/len(lead),max_absolute_residual=max(abs(int(r['residual'])) for r in lead),
            longest_observed_unhedged_seconds=longest,liquidation_trades=base['liquidation_trades'],role_taker_qty=dict(role_qty),
            gateway_position_chain_verified=True,max_post_action_absolute_residual=max(abs(r['residual']) for r in ledger),
            largest_confirmed_quote_center_jumps=sorted(jumps,key=lambda r:abs(r['center_change']),reverse=True)[:8],
            source_files={name:sha(directory/name) for name in ('trace-summary.json','maker-decisions.csv','basis-decisions.csv','trades.csv','gateway-accounts.json')})
        summaries.append(summary)
        print(json.dumps({k:v for k,v in summary.items() if k not in ('source_files','largest_confirmed_quote_center_jumps')}),flush=True)
        del gateways,by,fills,makers,basis,trades;gc.collect()
    result=dict(window_ms=[9000000,9660000],records=summaries,
        scope='Same previously selected absolute 11-minute window; endogenous paths differ. Proposal threshold is checked against pre-decision quotes. Real fills use authoritative market/command IDs, not action counters. Submission reachability uses the actual book just before the ordered gateway call. Pre-wave residual durations are lower bounds within this window; hedge-ledger.csv separately chains both legs through each actual gateway account update, verifies every before-position, and includes intra-wave residual maxima. Quote-center jumps require at least one actual OrderRested event, not mere actor acceptance; these are proposed centers, not a guarantee of two-sided displayed midpoints.')
    write(root/'window-comparison.json',result)
    return result


def chart(root,result):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import numpy as np
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':9,'axes.spines.top':False,'axes.spines.right':False})
    fig,axes=plt.subplots(5,3,figsize=(19,13),sharex=True,sharey='row')
    labels={'control':'Prior reservation control','hedged':'Executable touch hedge','depth':'Hedge + public-depth flow'}
    for col,profile in enumerate(('control','hedged','depth')):
        directory=root/(profile+'-analysis-v3');makers=csv_rows(directory/'maker-decisions.csv');basis=csv_rows(directory/'basis-decisions.csv');trades=csv_rows(directory/'trades.csv')
        for market,color in [('V-BTC-SPOT','#b46b25'),('V-BTC-PERP','#1f4e79')]:
            mids={int(r['time_ms']):float(r['book_mid']) if r['book_mid'] else np.nan for r in makers if r['market']==market}
            axes[0,col].plot([k/60000 for k in mids],[v for v in mids.values()],color=color,lw=.8,label=market.replace('V-BTC-',''))
            net=Counter()
            for r in trades:
                if r['market']==market and r['taker_role']=='BasisLinkTrader':net[int(r['time_ms'])]+=int(r['qty'])*(1 if r['taker_side']=='Buy' else -1)
            seconds=range(9000000,9660000,1000)
            axes[3,col].plot([k/60000 for k in seconds],[net[k] for k in seconds],color=color,lw=.7,label=market.replace('V-BTC-',''))
        rows=[r for r in makers if r['participant']=='maker-perp-17']
        times=[int(r['time_ms'])/60000 for r in rows]
        axes[1,col].plot(times,[float(r['center']) for r in rows],label='Computed center',color='#5b7e40',lw=.9)
        axes[1,col].plot(times,[float(r['mid']) for r in rows],label='External reference mid',color='#1f4e79',lw=.7,alpha=.8)
        axes[2,col].plot(times,[int(r['position']) for r in rows],color='#67517d',lw=.8)
        residual=defaultdict(list)
        for r in basis:
            if r['leg']=='Perp':residual[int(r['time_ms'])].append(abs(int(r['residual'])))
        axes[4,col].plot([k/60000 for k in sorted(residual)],[max(residual[k]) for k in sorted(residual)],color='#a34d3f',lw=.8)
        axes[0,col].set_title(labels[profile]);axes[4,col].set_xlabel('Simulation minute')
        for ax in axes[:,col]:ax.grid(axis='y',alpha=.18);ax.set_xlim(150,161)
    for row,label in enumerate(('Actual pre-wave quote mid (tick)','Maker 17 center / reference (tick)','Maker 17 actual inventory (lots)','Basis net taker flow / second (lots)','Max actual hedge residual (lots)')):axes[row,0].set_ylabel(label)
    axes[0,0].legend(loc='best');axes[1,0].legend(loc='best')
    fig.suptitle('Exact anomaly-window mechanism traces | same seed, signals and funded accounts',fontsize=16)
    fig.text(.5,.015,'Retrospectively selected minutes 150–161, unchanged under interventions. Common row scales, actual 1s observations and executions; no smoothing.\nCenters may persist between reconsiderations; net flow aggregates only authoritative basis fills. Residual: spot + perp − neutral inventory. This window is not full-run qualification.',ha='center')
    fig.tight_layout(rect=[0,.055,1,.96]);fig.savefig(root/'causal-window-comparison.png',dpi=150);plt.close(fig)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--root',type=Path,default=ROOT/'.local/strategy-research-causal-20261025');a=p.parse_args();r=summarize(a.root.resolve());chart(a.root.resolve(),r)
