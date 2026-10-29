"""Reconstruct quotes/inventory/hedging around paired worst quoted-edge waves."""
import argparse
import json
from collections import Counter
from pathlib import Path
from research_market_mechanisms import sha, write
from analyze_wick_basis_capacity import records


def audit(out, seed):
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'))
    links=json.loads((out/'same-wave-links.json').read_text(encoding='utf-8'))['records']
    selected=[r for r in links if r['seed']==seed]
    assert {r['profile'] for r in selected}=={'raw-flow','surprise'}
    intervals=sorted((max(7200000,r['worst_samples'][0]['time_ms']-60000),
        min(14400000,r['worst_samples'][0]['time_ms']+60000)) for r in selected)
    windows=[]
    for start,end in intervals:
        if windows and start<=windows[-1][1]:windows[-1][1]=max(end,windows[-1][1])
        else:windows.append([start,end])
    inside=lambda t:any(a<=t<b for a,b in windows)
    result=[]
    for row in [r for r in plan['runs'] if r['seed']==seed]:
        recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'))
        summary=json.loads((out/(row['name']+'.summary.json')).read_text(encoding='utf-8'))
        done=json.loads((out/(row['name']+'.run.json')).read_text(encoding='utf-8'))
        assert sha(out/(row['name']+'.receipt.json'))==done['receipt_sha256']==summary['receipt_sha256']
        config={(a['Plugin']['participant']['account_id'],a['Plugin']['participant']['instrument_id']):a['Plugin'] for a in recipe['agents']}
        makers={k:p for k,p in config.items() if p['plugin_id']=='AdaptiveLiquidityMaker'}
        basis={k[0]:p for k,p in config.items() if p['plugin_id']=='BasisLinkTrader' and p['config']['leg']=='Perp'}
        positions=Counter()
        for account in recipe['scenario']['accounts']:
            if 'Spot' in account:
                a=account['Spot'];positions[a['account_id'],'V-BTC-SPOT']=a['position_qty']
        snapshots={}
        for s in records(out/(row['name']+'.receipt.json'),'book_snapshots'):
            if inside(s['time_ms']):snapshots[s['time_ms'],s['instrument_id']]=s['book']
        expected={}
        for t in records(out/(row['name']+'.receipt.json'),'trade_receipts'):
            if inside(t['time_ms']):expected[t['instrument_id'],t['trade_id']]=(t['time_ms'],t['price_tick'],t['qty'],t['maker_account_id'],t['taker_account_id'])
        orders={}; observed={}; waves=[]; current=None; actions=Counter(); prefix_records=0
        def flush(t):
            if t is None or not inside(t):return
            quotes=[]
            for key,p in makers.items():
                account,market=key
                live=[o for o in orders.values() if (o['account'],o['market'])==key]
                bids=[o['price'] for o in live if o['side']=='Buy'];asks=[o['price'] for o in live if o['side']=='Sell']
                book=snapshots[t,market]
                if bids:
                    assert book['bids'] and max(bids)<=book['bids'][0]['price_tick']
                if asks:
                    assert book['asks'] and min(asks)>=book['asks'][0]['price_tick']
                b=max(bids) if bids else None;a=min(asks) if asks else None
                mid=(book['bids'][0]['price_tick']+book['asks'][0]['price_tick'])/2 if book['bids'] and book['asks'] else None
                quotes.append(dict(account=account,market=market,bid=b,ask=a,
                    resting_quote_mid=(a+b)/2 if a is not None and b is not None else None,
                    public_after_wave_mid=mid,inventory=positions[key],inventory_target=p['config']['inventory_target']))
            hedges=[]
            for account,p in basis.items():
                c=p['config'];spot=positions[account,c['hedge_instrument_id']];perp=positions[account,p['participant']['instrument_id']]
                hedges.append(dict(account=account,spot=spot,perp=perp,residual=spot+perp-c['neutral_spot'],
                    position_at_cap=abs(perp)>=c['max_hedge']))
            waves.append(dict(time_ms=t,makers=quotes,basis=hedges,actions=dict(actions)))
        assert len(summary['execution_archives'])==1
        arc=summary['execution_archives'][0]; path=Path('E:/'+arc['path'][7:])
        assert sha(path)==arc['sha256']
        with path.open(encoding='utf-8') as f:
            for line in f:
                r=json.loads(line);t=r['market_time_ms']
                if current is not None and t!=current:
                    assert t>current
                    flush(current);actions=Counter()
                current=t
                if t>=windows[-1][1]:break
                prefix_records+=1;market=r['instrument_id'];command=r['command']['command']
                new=command.get('NewOrder');info=new or command.get('CancelOrder',{})
                owner=info.get('account_id')
                role=config.get((owner,market),{}).get('plugin_id','other')
                if inside(t) and new:actions[role+':submitted']+=1
                for wrapped in r.get('events',[]):
                    for kind,event in wrapped['event'].items():
                        if kind=='TradePrinted':
                            sign=1 if event['taker_side']=='Buy' else -1
                            positions[event['taker_account_id'],market]+=sign*event['qty']
                            positions[event['maker_account_id'],market]-=sign*event['qty']
                            if inside(t):
                                key=market,event['trade_id'];assert key not in observed
                                observed[key]=(t,event['price_tick'],event['qty'],event['maker_account_id'],event['taker_account_id'])
                                for side in ('maker','taker'):
                                    kind2=config.get((event[side+'_account_id'],market),{}).get('plugin_id','other')
                                    actions[kind2+':'+side+'_filled_lots']+=event['qty']
                        elif kind=='OrderRested' and new and (owner,market) in makers:
                            assert event['order_id']==new['order_id'] and set(new['kind']) <= {'Limit','PostOnly'}
                            orders[market,event['order_id']]=dict(account=owner,market=market,side=new['side'],price=event['price_tick'],qty=event['remaining_qty'])
                        elif kind in ('OrderFilled','OrderCanceled','OrderExpired'):
                            orders.pop((market,event['order_id']),None)
                        elif kind=='OrderPartiallyFilled' and (market,event['order_id']) in orders:
                            orders[market,event['order_id']]['qty']=event['remaining_qty']
                        if inside(t) and kind in ('OrderRejected','CancelRejected','OrderExpired'):
                            actions[role+':'+kind+':'+str(event.get('reason',''))]+=1
        if current is not None and current<windows[-1][1]:flush(current)
        assert observed==expected
        assert len(waves)==sum((b-a)//1000 for a,b in windows)
        inventories=[abs(m['inventory']-m['inventory_target']) for w in waves for m in w['makers']]
        residuals=[abs(p['residual']) for w in waves for p in w['basis']]
        result.append(dict(seed=seed,profile=row['profile'],receipt_sha256=done['receipt_sha256'],
            archive_sha256=arc['sha256'],prefix_records_read=prefix_records,windows=windows,waves=waves,
            summary=dict(waves=len(waves),maker_inventory_deviation_max=max(inventories),
                basis_residual_max=max(residuals),basis_unhedged_account_waves=sum(p['residual']!=0 for w in waves for p in w['basis']),
                basis_at_cap_account_waves=sum(p['position_at_cap'] for w in waves for p in w['basis']),
                actions=dict(sum((Counter(w['actions']) for w in waves),Counter()))),
            actual_window_prints_reconciled=len(observed)))
    write(out/f'wave-audit-seed{seed}.json',dict(records=result,analyzer_sha256=sha(Path(__file__)),
        scope='Post-hoc paired inspection around each profile highest gross public quoted-edge wave, union of +/-60s windows clipped to primary2–4h. Identical clock windows inspected for both profiles, no best-case selection. Resting maker orders and inventories reconstructed from authoritative complete command events, then sampled after every whole1s wave; maker quote extrema checked against saved public BBO. Every selected-window execution print independently matches the raw tape. Wave exposure is descriptive; the full-run ordered-fill capacity audit remains the duration measure. Private account/archive fields are offline evidence only, never supplied to agents. No assertion that quoted edges are atomically executable or every tail is caused by the inventory cap.'))
    print(json.dumps([dict(profile=r['profile'],windows=r['windows'],summary=r['summary']) for r in result],indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);p.add_argument('--seed',type=int,required=True);a=p.parse_args();audit(a.directory,a.seed)
