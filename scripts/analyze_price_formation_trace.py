"""Separate observed index/flow quote contributions and actual basis constraints."""
import argparse,json,statistics
from collections import Counter,defaultdict
from pathlib import Path
from research_market_mechanisms import sha,write


def summary(xs):
    xs=sorted(xs)
    return dict(n=len(xs),median=statistics.median(xs),p99=xs[min(len(xs)-1,int(.99*len(xs)))],max=xs[-1]) if xs else dict(n=0)


def analyze(out):
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));result=[]
    for row in plan['runs']:
        trace=out/(row['name']+'.trace.jsonl')
        if not trace.exists():continue
        recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'))
        byid={a['Plugin']['participant']['participant_id']:a['Plugin']for a in recipe['agents']}
        makers=defaultdict(lambda:defaultdict(list));basis=defaultdict(Counter);examples=[]
        with trace.open(encoding='utf-8') as f:
            for line in f:
                try:d=json.loads(line)
                except json.JSONDecodeError:break # permit a separate live preview; final requires completed metadata
                if d['kind']!='decision':continue
                p=byid[d['participant']];c=p['config'];v=d['observation'];before=d['before'];after=d['after'];now=v['time_ms'];market=v['instrument']
                if p['plugin_id']=='AdaptiveLiquidityMaker':
                    if not d['actions'] or not any(k.startswith('Place') for a in d['actions'] for k in a):continue
                    b,a=v['external_bid'],v['external_ask']
                    if not b or not a:continue
                    mid=(b['price_tick']+a['price_tick'])/2
                    signed=before.get('pending_signed',0);total=before.get('pending_total',0)
                    for t in v['public_trades']:
                        if before.get('last_trade_id') is not None and t['trade_id']<=before['last_trade_id']:continue
                        signed+=(1 if t['taker_side']=='Buy'else -1)*t['qty'];total+=t['qty']
                    levels=v['external_ask_levels']if signed>=0 else v['external_bid_levels']
                    depth=sum(l['qty']for l in levels if abs(l['price_tick']-mid)<=c['max_half_spread_ticks'])
                    oldimpact=signed/(total+depth)*after['half_spread']*c['flow_skew_ppm']/1e6 if total+depth>0 else 0
                    m=makers[market];m['half_spread_tick'].append(after['half_spread']);m['abs_flow_impact_tick'].append(abs(oldimpact));m['abs_center_minus_mid_tick'].append(abs(after['center']-mid))
                    if total>0:m['flow_nonzero_impact_below_half_tick'].append(float(abs(oldimpact)<.5))
                    index=v.get('perp_price');weight=c['index_weight_ppm']/1e6
                    if index and index.get('index_price_tick') is not None and weight:
                        micro=(b['price_tick']*a['qty']+a['price_tick']*b['qty'])/(b['qty']+a['qty'])
                        reference=mid+(micro-mid)*c['microprice_ppm']/1e6
                        anchor=(index['index_price_tick']-reference)*weight
                        m['abs_index_anchor_tick'].append(abs(anchor))
                        if abs(anchor)>=10:
                            m['large_anchor_abs_center_shift_tick'].append(abs(after['center']-mid))
                            if len(examples)<12:examples.append(dict(participant=d['participant'],time_ms=now,index_component_tick=anchor,actual_center_minus_mid_tick=after['center']-mid,flow_tick=oldimpact,prior_reservation=before.get('last_reservation_offset')))
                elif p['plugin_id']=='BasisLinkTrader' and c['leg']=='Perp':
                    if before.get('last_ms')==now or now<before.get('next_ms',0):continue
                    q=basis[d['participant']];q['scheduled_decisions']+=1
                    if v['orders']:q['own_orders']+=1;continue
                    peer=v['related'][0];s=peer['account']['Spot']['position_qty'];pos=v['account']['Perp']['position_qty']
                    if s+pos!=c['neutral_spot']:q['unhedged_residual']+=1;continue
                    sb,sa,pb,pa=peer['book']['bids'],peer['book']['asks'],v['book']['bids'],v['book']['asks']
                    if not all((sb,sa,pb,pa)):q['missing_book_side']+=1;continue
                    cheap=pa[0]['price_tick']/sb[0]['price_tick']-1;rich=pb[0]['price_tick']/sa[0]['price_tick']-1
                    exit=c['exit_ppm']/1e6;entry=c['entry_ppm']/1e6
                    if (pos>0 and rich>=-exit)or(pos<0 and cheap<=exit):reason='unwind'
                    elif cheap<-entry:reason='long_at_cap'if pos>=min(c['max_hedge'],max(0,s)+max(0,pos)) else 'long_entry'
                    elif rich>entry:reason='short_at_cap'if pos<=-c['max_hedge']else'short_entry'
                    else:reason='no_entry_or_exit_edge'
                    q[reason]+=1
                    q[reason+('_submitted'if d['actions']else'_no_action')]+=1
        result.append(dict(name=row['name'],trace_sha256=sha(trace),makers={k:{a:summary(b)for a,b in v.items()}for k,v in makers.items()},basis=dict(basis),examples=examples))
    write(out/'price-formation-diagnosis.json',dict(results=result,analyzer_sha256=sha(Path(__file__)),
        scope='Preselected narrow trace windows, quoted maker decisions and scheduled basis lead decisions. Flow contributions reconstructed from the observed last32 tape and before-pending ledger, not a complete signed-flow inference. Constraints classify observed decision inputs, not every market opportunity. A trace preview can omit its last partial line; completed-run byte identity is separately required. Index-gap level and actual quote shifts are descriptive, not a causal counterfactual.'))
    print(json.dumps(result,indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);analyze(p.parse_args().directory)
