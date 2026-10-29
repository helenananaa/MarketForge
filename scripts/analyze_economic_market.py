"""Analyze every frozen seed, including cost, demand and actual intervention receipts."""
from __future__ import annotations
import argparse, collections, hashlib, json, sys
from pathlib import Path
import numpy as np
from stylized_facts import analyze as stylized

ROOT=Path(__file__).resolve().parents[1]
def read(p):return json.loads(p.read_text(encoding='utf-8'))
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def quantile(a):return [float(x) for x in np.quantile(a,[.1,.5,.9,1])] if a else None
def midpoint(book):return (book['bids'][0]['price_tick']+book['asks'][0]['price_tick'])/2 if book['bids'] and book['asks'] else None

def summarize(r,roster,recipe,skip=1200000,end=None):
    end=end or r['simulation_time_ms'];owners={(p['account_id'],p['instrument']):p for p in roster['participants']}
    all_trades=r['trade_receipts'];assert len(all_trades)==sum(r['trades'].values())
    assert len({(t['instrument_id'],t['trade_id']) for t in all_trades})==len(all_trades)
    assert all(t['qty']>0 and t['price_tick']>0 and t['maker_account_id']!=t['taker_account_id'] for t in all_trades)
    output={'simulation_time_ms':end,'analysis_start_ms':skip,'trades':r['trades'],'economic_quote_checks':r.get('economic_quote_checks',0),'markets':{},'demand':{},'stops':{}}
    for inst in r['trades']:
        trades=[t for t in all_trades if t['instrument_id']==inst and skip<=t['time_ms']<end]
        roles=collections.Counter();actors=collections.Counter();makers=collections.Counter()
        for t in trades:
            p=owners.get((t['taker_account_id'],inst),{});roles[p.get('role','other')]+=t['qty'];actors[p.get('participant_id','other')]+=t['qty'];makers[owners.get((t['maker_account_id'],inst),{}).get('role','other')]+=t['qty']
        volume=sum(roles.values());books=[s for s in r['book_snapshots'] if s['instrument_id']==inst and skip<=s['time_ms']<end]
        spreads=[];depth=[];mids=[]
        for s in books:
            b=s['book'];mid=midpoint(b)
            if mid is None:continue
            mids.append(mid);spreads.append((b['asks'][0]['price_tick']-b['bids'][0]['price_tick'])/mid*10000)
            depth.append(sum(v['qty'] for side in ['bids','asks'] for v in b[side] if abs(v['price_tick']/mid-1)<=.0025))
        metrics=stylized([t for t in all_trades if t['instrument_id']==inst and t['time_ms']<end],end,skip,roster)
        internal_stop_qty=sum(e['filled_qty'] for e in r.get('market_exit_receipts',[]) if e['instrument_id']==inst and
            e['participant_id'].startswith('switching-') and skip<=e['time_ms']<end)
        mechanisms={'internal_market_stop':internal_stop_qty,
            'other_gateway':sum(t['qty'] for t in trades if t['taker_order_id']<7_000_000_000_000_000_000)-internal_stop_qty,
            'system_conditional':sum(t['qty'] for t in trades if 7_000_000_000_000_000_000<=t['taker_order_id']<8_000_000_000_000_000_000),
            'system_position_protection':sum(t['qty'] for t in trades if 8_000_000_000_000_000_000<=t['taker_order_id']<9_000_000_000_000_000_000),
            'system_liquidation':sum(t['qty'] for t in trades if t['taker_order_id']>=9_000_000_000_000_000_000)}
        assert all(q>=0 for q in mechanisms.values()) and sum(mechanisms.values())==volume
        # Same source, independent addition paths: per-role and per-minute totals.
        bars=collections.defaultdict(list)
        for t in trades:bars[t['time_ms']//60000].append(t)
        assert sum(sum(t['qty'] for t in b) for b in bars.values())==volume
        # Diagnostic only: rank every closed minute by actual close-to-close
        # displacement, retaining the full-seed chart and actual traded volume.
        extremes=[];previous=None
        for minute,ts in sorted(bars.items()):
            close=ts[-1]['price_tick'];role_qty=collections.Counter()
            for t in ts:role_qty[owners.get((t['taker_account_id'],inst),{}).get('role','other')]+=t['qty']
            if previous is not None:
                movement=(close/previous-1)*10000
                exits=[e for e in r.get('market_exit_receipts',[]) if e['instrument_id']==inst and e['time_ms']//60000==minute]
                extremes.append({'minute':minute,'return_bps':movement,'traded_qty':sum(role_qty.values()),
                    'taker_qty_by_role':dict(role_qty),'market_exit_qty':sum(e['filled_qty'] for e in exits)})
            previous=close
        output['markets'][inst]={'volume':volume,'taker_qty_by_role':dict(roles),'taker_share_by_role':{k:v/volume for k,v in roles.items()} if volume else {},
            # room.rs reserved IDs are valid here because no external IDs enter.
            'taker_qty_by_execution_mechanism':mechanisms,
            'passive_qty_by_role':dict(makers),'largest_taker_share':max(actors.values(),default=0)/max(1,volume),
            'two_sided_fraction':len(mids)/max(1,len(books)),'spread_bps_p10_p50_p90_max':quantile(spreads),
            'depth_25bps_both_sides_p10_p50_p90_max':quantile(depth),'stylized_metrics':metrics,
            'largest_absolute_close_moves':sorted(extremes,key=lambda e:abs(e['return_bps']),reverse=True)[:5]}
    paired=collections.defaultdict(dict)
    for s in r['book_snapshots']:
        if skip<=s['time_ms']<end:
            m=midpoint(s['book'])
            if m is not None:paired[s['time_ms']][s['instrument_id']]=m
    gaps=[abs(v['V-BTC-PERP']/v['V-BTC-SPOT']-1)*10000 for v in paired.values() if 'V-BTC-PERP' in v and 'V-BTC-SPOT' in v]
    output['spot_perp_abs_basis_bps_p10_p50_p90_max']=quantile(gaps)
    states={s['id']:s['state']['Plugin']['data'] for s in r['states'] if 'Plugin' in s['state']}
    parent_exec_qty=collections.Counter()
    for t in all_trades:
        owner=owners.get((t['taker_account_id'],t['instrument_id']))
        if owner and owner['role']=='execution' and t['taker_order_id']<7_000_000_000_000_000_000 and t['taker_side']==owner['config']['side']:
            parent_exec_qty[owner['participant_id']]+=t['qty']
    for role in ['execution','value','technical','basis']:
        participants=[p for p in roster['participants'] if p['role']==role]
        output['demand'][role]={'instances':len(participants),'with_actual_fills':sum(r['fills'].get(p['participant_id'],0)>0 for p in participants),
            'filled_participation_qty':sum(r['fills'].get(p['participant_id'],0) for p in participants)}
        if role=='execution':
            output['demand'][role].update(planned_qty=sum(p['config']['target_qty'] for p in participants),
                parents_started=sum(states[p['participant_id']].get('execution_start_ms') is not None for p in participants),
                with_actual_parent_fills=sum(parent_exec_qty[p['participant_id']]>0 for p in participants),
                parent_order_filled_qty=sum(parent_exec_qty[p['participant_id']] for p in participants),
                unfilled_planned_qty=sum(max(0,p['config']['target_qty']-parent_exec_qty[p['participant_id']]) for p in participants),
                parents_fully_filled=sum(parent_exec_qty[p['participant_id']]>=p['config']['target_qty'] for p in participants),
                parents_overfilled=sum(parent_exec_qty[p['participant_id']]>p['config']['target_qty'] for p in participants),
                observed_deadline_reached=sum(states[p['participant_id']].get('deadline_reached',False) for p in participants),
                all_schedules_fit_session=all(p['config']['start_after_ms']+p['config']['horizon_ms']<=end for p in participants))
        if role=='value':
            for k in ['parents_started','parents_completed','parents_abandoned','children_passive','children_aggressive']:
                output['demand'][role][k]=sum(states[p['participant_id']].get(k,0) for p in participants)
    switchers=[states[p['participant_id']] for p in roster['participants'] if p['role']=='switching']
    for k in ['stop_triggers','exit_requests','exit_submissions','exit_filled_qty','exit_rejections','exit_completions','exit_remaining_qty']:
        output['stops'][k]=sum(s.get(k,0) for s in switchers)
    output['stops']['pending_instances']=sum(s.get('exit_pending',False) for s in switchers)
    exits=[x for x in r.get('market_exit_receipts',[]) if x['participant_id'].startswith('switching-')]
    assert sum(x['filled_qty'] for x in exits)==output['stops']['exit_filled_qty']
    assert len(exits)==output['stops']['exit_submissions']
    for e in exits:
        command=e['command']['NewOrder'];assert command['kind']=='Market'
        if e['instrument_id'].endswith('PERP'):assert command['reduce_only']
        assert e['filled_qty']<=e['requested_qty']
    output['stops'].update(partial_fill_attempts=sum(0<e['filled_qty']<e['requested_qty'] for e in exits),no_fill_attempts=sum(e['filled_qty']==0 for e in exits),
        receipt_count=len(exits),market_command_contract_passed=True)
    before={a['account_id']:{v['asset_id']:v['total'] for v in a['assets']} for a in r['assets_before']['accounts']}
    after={a['account_id']:{v['asset_id']:v['total'] for v in a['assets']} for a in r['assets_after']['accounts']}
    spotbook=next(s['book'] for s in reversed(r['book_snapshots']) if s['instrument_id']=='V-BTC-SPOT');mark=midpoint(spotbook)
    initial=[o['NewOrder'] for o in recipe['scenario']['seed_orders'] if 'NewOrder' in o and 'Limit' in o['NewOrder']['kind']]
    initial_mark=(max(o['kind']['Limit']['price_tick'] for o in initial if o['side']=='Buy')+
        min(o['kind']['Limit']['price_tick'] for o in initial if o['side']=='Sell'))/2
    venues=[recipe['scenario']['market'],*recipe['scenario']['extra_markets']]
    fee_tables={market['instrument']['instrument_id']:market['clearing'] for venue in venues for market in venue.values()}
    fees=collections.Counter()
    for t in all_trades:
        n=t['qty']*t['price_tick'];fee=fee_tables[t['instrument_id']]
        fees[t['maker_account_id']]+=n*fee['maker_fee_ppm']//1000000
        fees[t['taker_account_id']]+=n*fee['taker_fee_ppm']//1000000
    pnl=[]
    if mark is not None:
        for p in roster['participants']:
            if p['role']!='maker' or p['instrument']!='V-BTC-SPOT':continue
            a=p['account_id'];excess=after[a].get('BTC',0)-before[a].get('BTC',0)+(after[a].get('V',0)-before[a].get('V',0))*mark
            absolute=excess+before[a].get('V',0)*(mark-initial_mark)
            pnl.append({'id':p['participant_id'],'excess_vs_hold':excess,'absolute_pnl':absolute,'fees':fees[a]})
    output['spot_maker_economics']={'initial_seed_book_mark':initial_mark,'final_mark':mark,'accounts':pnl,
        'absolute_pnl_quote_units':sum(x['absolute_pnl'] for x in pnl),'negative_absolute_pnl':sum(x['absolute_pnl']<0 for x in pnl),
        'excess_pnl_quote_units':sum(x['excess_vs_hold'] for x in pnl),
        'fees_quote_units':sum(x['fees'] for x in pnl),'underperformed_hold':sum(x['excess_vs_hold']<0 for x in pnl)}
    reject=collections.Counter()
    import re
    for key,count in r['rejections'].items():reject[re.sub(r'order_id: \d+','order_id: ID',key)]+=count
    output['rejections']=dict(reject)
    return output

def probe_analysis(directory):
    out=[]
    for p in sorted(directory.glob('*.json')):
        if p.name=='parent.json' or p.name.endswith('-Buy-0.json'):continue
        r=read(p);intervention=r['intervention'];inst=intervention['instrument_id'];control_path=directory/(inst+'-Buy-0.json');control=read(control_path)
        before=midpoint(r['before_book']);row={'branch':p.stem,'requested_qty':intervention['requested_qty'],'filled_qty':intervention['filled_qty'],
            'before_mid':before,'immediate_mid':midpoint(r['immediate_book'])}
        if before and intervention['filled_qty']:
            row['execution_cost_from_mid_bps']=(int(intervention['notional'])/intervention['filled_qty']/before-1)*10000*(1 if intervention['side']=='Buy' else -1)
        t0=intervention['time_ms']
        for delta in [1000,30000,180000]:
            mids=[]
            for receipt in [r,control]:
                snapshots=[s for s in receipt['book_snapshots'] if s['instrument_id']==inst and s['time_ms']<=t0+delta]
                mids.append(midpoint(snapshots[-1]['book']) if snapshots else None)
            row['net_control_bps_'+str(delta//1000)+'s']=(mids[0]-mids[1])/before*10000 if before and all(x is not None for x in mids) else None
        row['receipt_sha256']=sha(p);row['control_receipt_sha256']=sha(control_path);out.append(row)
    return out

def charts(out,cases):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    fig,axes=plt.subplots(2*len(cases),2,figsize=(17,4*len(cases)),squeeze=False,layout='constrained',
        gridspec_kw={'height_ratios':[3,1]*len(cases)})
    for row,(name,r) in enumerate(cases):
        for col,inst in enumerate(['V-BTC-SPOT','V-BTC-PERP']):
            ax=axes[row*2,col];vol=axes[row*2+1,col];bins=collections.defaultdict(list)
            for t in r['trade_receipts']:
                if t['instrument_id']==inst and 1200000<=t['time_ms']<r['simulation_time_ms']:bins[t['time_ms']//60000].append(t)
            for minute,ts in bins.items():
                ps=[t['price_tick'] for t in ts];o,c=ps[0],ps[-1];color='#c94444' if c>=o else '#13866f'
                ax.vlines(minute,min(ps),max(ps),color=color,lw=.6)
                ax.add_patch(Rectangle((minute-.35,min(o,c)),.7,max(.3,abs(c-o)),facecolor=color,edgecolor=color,lw=.3))
                vol.bar(minute,sum(t['qty'] for t in ts),width=.7,color=color)
            ax.set_title(name+' '+inst+' | actual 1m OHLC');ax.set_ylabel('Price (internal ticks)');ax.grid(alpha=.15)
            vol.set_xlabel('Simulation minutes (first 20m warmup excluded)');vol.set_ylabel('Volume (lots)');vol.grid(alpha=.15)
            ax.set_xlim(20,r['simulation_time_ms']/60000);vol.set_xlim(ax.get_xlim())
    fig.suptitle('All frozen seeds: economic/execution repairs; no candle-shape selection')
    fig.savefig(out/'all-seeds-candles.png',dpi=145);plt.close(fig)

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);p.add_argument('--charts',action='store_true');a=p.parse_args()
    out=a.directory.resolve();plan=read(out/'frozen-plan.json');assert sha(Path(plan['binary']))==plan['binary_sha256']
    result={'plan':plan,'analysis_script_sha256':sha(Path(__file__)),'analysis_runtime':{'python':sys.version.split()[0],'numpy':np.__version__},'cases':{},'limitations':['Four synthetic hours per seed are not a real-market similarity qualification.','One-second frozen observation waves and ordered submissions remain.','Changes are combined; no individual causal performance gain is claimed.']};cases=[]
    for case in plan['cases']:
        name=case['name'];p=out/(name+'.receipt.json');r=read(p);roster=read(out/(name+'.roster.json'));run=read(out/(name+'.run.json'))
        assert run['exit_code']==0 and r['simulation_time_ms']==plan['duration_seconds']*1000
        assert sha(p)==run['receipt_sha256'] and sha(out/(name+'.recipe.json'))==case['recipe_sha256']
        assert sha(out/(name+'.roster.json'))==case['roster_sha256'] and run['binary_sha256']==plan['binary_sha256']
        metrics=summarize(r,roster,read(out/(name+'.recipe.json')),min(1200000,r['simulation_time_ms']//2));result['cases'][name]=metrics;cases.append((name,{'trade_receipts':r['trade_receipts'],'simulation_time_ms':r['simulation_time_ms']}))
        if r.get('fork_receipts'):
            assert r['fork_parent_unchanged'] and r['forks_complete'];result['paired_interventions']=probe_analysis(out/'impact-seed-7')
        print(name,json.dumps({'demand':metrics['demand'],'stops':metrics['stops'],'markets':{i:{k:v for k,v in m.items() if k not in ['stylized_metrics','passive_qty_by_role']} for i,m in metrics['markets'].items()},'maker_economics':{k:v for k,v in metrics['spot_maker_economics'].items() if k!='accounts'}}),flush=True)
    (out/'analysis.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    if a.charts:charts(out,cases)
    return 0
if __name__=='__main__':raise SystemExit(main())
