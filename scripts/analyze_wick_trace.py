"""Decompose actual extremum fills against their ordered pre-command books."""
import argparse,csv,gc,json
from collections import Counter,defaultdict
from pathlib import Path
import numpy as np
from analyze_market_mechanisms import ROOT,sha,write,add_bar


def mid(book):
    if book['bids'] and book['asks']:return (book['bids'][0]['price_tick']+book['asks'][0]['price_tick'])/2


def analyze(base,out):
    records=[];out.mkdir(parents=True,exist_ok=True)
    for profile in ('control','hedged','depth'):
        folder=base/(profile+'-analysis-v3')
        with (folder/'trades.csv').open(encoding='utf-8') as f:trades=list(csv.DictReader(f))
        recipe=json.loads((base/'full-study'/f'native-s41-{profile}.recipe.json').read_text(encoding='utf-8'))
        roles={p['Plugin']['participant']['participant_id']:p['Plugin']['plugin_id'] for p in recipe['agents']}
        gateways=json.loads((folder/'gateway-accounts.json').read_text(encoding='utf-8'));lookup={};persecond={};cancel_records=[]
        for g in gateways:
            key=(g['market'],g['time_ms']);counts=persecond.setdefault(key,dict(cancels=0,maker_rest=0))
            lookup[(g['market'],g['command_seq'])]=(g,dict(counts))
            if roles[g['participant']]=='AdaptiveLiquidityMaker':
                if isinstance(g['action'],dict) and 'Cancel' in g['action']:
                    counts['cancels']+=1
                    before,after=mid(g['book_before']),mid(g['book_after'])
                    cancel_records.append(dict(time_ms=g['time_ms'],market=g['market'],participant=g['participant'],
                        removed_two_sided=before is not None and after is None,
                        quote_mid_jump_bp=(after/before-1)*1e4 if before and after else None))
                counts['maker_rest']+=g['rested']
        bars={};extrema={}
        for r in trades:
            market=r['market'];t=int(r['time_ms']);price=int(r['price_tick']);key=(market,t//60000)
            if key not in extrema:extrema[key]={'high':r,'low':r}
            else:
                if price>int(extrema[key]['high']['price_tick']):extrema[key]['high']=r
                if price<int(extrema[key]['low']['price_tick']):extrema[key]['low']=r
        detail=[]
        for (market,bucket),ends in sorted(extrema.items()):
            for end,r in ends.items():
                g,counts=lookup[(market,int(r['command_seq']))];book=g['book_before'];reference=mid(book)
                side=book['asks'] if r['taker_side']=='Buy' else book['bids'];assert side
                price=int(r['price_tick']);touch=side[0]['price_tick'];signed=1 if r['taker_side']=='Buy' else -1
                assert (price-touch)*signed>=0
                depth_index=next((i+1 for i,l in enumerate(side) if l['price_tick']==price),None)
                detail.append(dict(profile=profile,market=market,bucket=bucket,extreme=end,time_ms=int(r['time_ms']),command_seq=int(r['command_seq']),
                    maker_account_id=int(r['maker_account_id']),taker_role=r['taker_role'],taker_order_id=int(r['taker_order_id']),
                    price_tick=price,pre_command_mid=reference,pre_command_spread_bp=(book['asks'][0]['price_tick']-book['bids'][0]['price_tick'])/reference*1e4 if reference else None,
                    execution_beyond_touch_bp=(price-touch)*signed/(reference or touch)*1e4,visible_consumed_level=depth_index,
                    visible_depth_before=sum(l['qty'] for l in side),prior_same_second_maker_cancels=counts['cancels'],prior_same_second_maker_rest_events=counts['maker_rest']))
        with (out/(profile+'-extrema.csv')).open('w',newline='',encoding='utf-8') as f:
            w=csv.DictWriter(f,fieldnames=list(detail[0]));w.writeheader();w.writerows(detail)
        for market in ('V-BTC-SPOT','V-BTC-PERP'):
            rs=[r for r in detail if r['market']==market];cs=[r for r in cancel_records if r['market']==market]
            record=dict(profile=profile,market=market,minute_extrema=len(rs),
                median_beyond_touch_bp=float(np.median([r['execution_beyond_touch_bp'] for r in rs])),
                median_pre_command_spread_bp=float(np.median([r['pre_command_spread_bp'] for r in rs if r['pre_command_spread_bp'] is not None])),
                deeper_than_touch_fraction=sum(r['visible_consumed_level']!=1 for r in rs)/len(rs),
                prior_maker_cancels_same_second_fraction=sum(r['prior_same_second_maker_cancels']>0 for r in rs)/len(rs),
                cancels=len(cs),cancels_removing_two_sided=sum(r['removed_two_sided'] for r in cs),
                cancel_mid_jump_abs_p99_bp=float(np.percentile([abs(r['quote_mid_jump_bp']) for r in cs if r['quote_mid_jump_bp'] is not None],99)),
                maximum_beyond_touch=sorted(rs,key=lambda r:r['execution_beyond_touch_bp'],reverse=True)[:5])
            records.append(record);print(json.dumps({k:v for k,v in record.items() if k!='maximum_beyond_touch'}),flush=True)
        del gateways,lookup;gc.collect()
    write(out/'wick-trace-summary.json',dict(records=records,analyzer_sha256=sha(Path(__file__)),
        scope='Exact retrospectively selected minutes150–161. Each minute high/low fill joins its authoritative command to the ordered pre-command book. Beyond-touch displacement is actual execution through displayed liquidity (including any self-trade-prevention changes), distinct from the current spread. Same-second prior cancellations are temporal association, not causal attribution. At most8 visible levels in gateway records; an unlisted fill level is not assumed to be level9. No quote/reference/price changes are made by this analyzer.'))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--base',type=Path,default=ROOT/'.local/strategy-research-causal-20261025');p.add_argument('--output',type=Path,default=ROOT/'.local/strategy-research-wicks-20261025/trace-diagnosis');a=p.parse_args();analyze(a.base.resolve(),a.output.resolve())
