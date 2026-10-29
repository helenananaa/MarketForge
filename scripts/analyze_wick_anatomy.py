"""Separate transaction candles, sampled quote movement and extreme executions."""
from __future__ import annotations
import argparse,csv,gc,json,math,sys
from collections import Counter,defaultdict
from pathlib import Path
import research_numeric  # Configure numerical workers before NumPy loads.
import numpy as np
from analyze_market_mechanisms import ROOT,add_bar,rebar,sha,write

BASE=ROOT/'.local/strategy-research-wicks-20261025'
PRIOR=ROOT/'.local/strategy-research-causal-20261025/full-study'
MARKETS=('V-BTC-SPOT','V-BTC-PERP')


def shape(bars,start,end,interval):
    result=[]
    for k,b in sorted(bars.items()):
        if not start<=k*interval or (k+1)*interval>end:continue
        span=b['high']-b['low'];body=abs(b['close']-b['open']);upper=b['high']-max(b['open'],b['close']);lower=min(b['open'],b['close'])-b['low']
        assert min(upper,lower,body)>=0 and abs(span-upper-lower-body)<1e-7
        result.append(dict(bucket=k,open=b['open'],high=b['high'],low=b['low'],close=b['close'],volume=b['volume'],trades=b['trades'],
            range_bp=span/b['open']*1e4,body_bp=body/b['open']*1e4,upper_bp=upper/b['open']*1e4,lower_bp=lower/b['open']*1e4,
            wick_fraction=(upper+lower)/span if span>0 else None,
            both_outer_quarters=upper>=span*.25 and lower>=span*.25 if span>0 else False,
            both_wicks_gt_body=upper>body and lower>body if span>0 else False))
    valid=[r for r in result if r['wick_fraction'] is not None]
    metric=dict(bars=len(result),nonzero_range_bars=len(valid),
        median={k:float(np.median([r[k] for r in valid])) if valid else None for k in ('wick_fraction','range_bp','body_bp','upper_bp','lower_bp')},
        p95={k:float(np.percentile([r[k] for r in valid],95)) if valid else None for k in ('range_bp','body_bp','upper_bp','lower_bp')},
        both_outer_quarters_fraction=sum(r['both_outer_quarters'] for r in valid)/len(valid) if valid else None,
        both_wicks_gt_body_fraction=sum(r['both_wicks_gt_body'] for r in valid)/len(valid) if valid else None)
    return result,metric


def analyze(source,out,profile,seed=None):
    out.mkdir(parents=True,exist_ok=True);plan=json.loads((source/'plan.json').read_text(encoding='utf-8'));records=[]
    for row in plan['runs']:
        if row['profile']!=profile:continue
        if seed is not None and row['seed']!=seed:continue
        done=json.loads((source/(row['name']+'.run.json')).read_text(encoding='utf-8'));assert done['complete']
        receipt=source/(row['name']+'.receipt.json');assert sha(receipt)==done['receipt_sha256']
        recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'));assert sha(Path(row['recipe']))==row['recipe_sha256']
        roles={(p['Plugin']['participant']['account_id'],p['Plugin']['participant']['instrument_id']):p['Plugin']['plugin_id'] for p in recipe['agents']}
        data=json.loads(receipt.read_text(encoding='utf-8-sig'));start,end=row['warmup_ms'],data['simulation_time_ms'];bars=defaultdict(dict);mids=defaultdict(dict);spreads=defaultdict(list)
        extrema={};snapshots=defaultdict(dict)
        for t in data['trade_receipts']:
            if not start<=t['time_ms']<end:continue
            market=t['instrument_id'];add_bar(bars[market],t['time_ms'],t['price_tick'],t['qty'])
            for multiple in (1,5):
                key=(market,multiple,t['time_ms']//(60000*multiple));old=extrema.get(key)
                if old is None:extrema[key]={'high':t,'low':t}
                else:
                    if t['price_tick']>old['high']['price_tick']:old['high']=t
                    if t['price_tick']<old['low']['price_tick']:old['low']=t
        for s in data['book_snapshots']:
            if not start<=s['time_ms']<end:continue
            market=s['instrument_id'];bid,ask=s['book']['bids'],s['book']['asks'];snapshots[market][s['time_ms']]=s
            if bid and ask:
                mid=(bid[0]['price_tick']+ask[0]['price_tick'])/2
                add_bar(mids[market],s['time_ms'],mid,0)
                spreads[market].append((s['time_ms'],(ask[0]['price_tick']-bid[0]['price_tick'])/mid*1e4))
        wanted={(market,t['taker_order_id']) for (market,_,_),x in extrema.items() for t in x.values()};orders={}
        for t in data['trade_receipts']:
            key=(t['instrument_id'],t['taker_order_id'])
            if key not in wanted:continue
            if key not in orders:orders[key]=dict(time_ms=t['time_ms'],qty=0,prices=set(),fills=0)
            o=orders[key];assert o['time_ms']==t['time_ms'];o['qty']+=t['qty'];o['prices'].add(t['price_tick']);o['fills']+=1
        for market in MARKETS:
            for multiple in (1,5):
                actual=rebar(bars[market],multiple);quoted=rebar(mids[market],multiple)
                details,trade_shape=shape(actual,start,end,multiple*60000);_,mid_shape=shape(quoted,start,end,multiple*60000)
                extreme_roles=Counter();sweep=[];excess=[];spread_range=[]
                perbar_spread=defaultdict(list)
                for t,s in spreads[market]:perbar_spread[t//(multiple*60000)].append(s)
                for d in details:
                    k=d['bucket'];q=quoted.get(k);trange=d['high']-d['low']
                    d['sampled_mid_range_bp']=(q['high']-q['low'])/q['open']*1e4 if q else None
                    d['outside_sampled_mid_envelope_fraction']=1-max(0,min(d['high'],q['high'])-max(d['low'],q['low']))/trange if q and trange>0 else None
                    d['median_quoted_spread_bp']=float(np.median(perbar_spread[k])) if perbar_spread[k] else None
                    if d['outside_sampled_mid_envelope_fraction'] is not None:excess.append(d['outside_sampled_mid_envelope_fraction'])
                    if d['range_bp']>0 and d['median_quoted_spread_bp'] is not None:spread_range.append(d['median_quoted_spread_bp']/d['range_bp'])
                    for direction,t in extrema[(market,multiple,k)].items():
                        o=orders[(market,t['taker_order_id'])];role=roles.get((t['maker_account_id'],market),'external');extreme_roles[role]+=1
                        d[direction+'_taker_role']=roles.get((t['taker_account_id'],market),'external');d[direction+'_maker_role']=role
                        d[direction+'_time_ms']=t['time_ms'];d[direction+'_taker_order_id']=t['taker_order_id'];d[direction+'_maker_account_id']=t['maker_account_id']
                        d[direction+'_order_qty']=o['qty'];d[direction+'_order_price_levels']=len(o['prices']);sweep.append(len(o['prices'])>1)
                csv_path=out/(row['name']+'-'+market+f'-{multiple}m.wicks.csv')
                with csv_path.open('w',newline='',encoding='utf-8') as f:
                    w=csv.DictWriter(f,fieldnames=list(details[0]));w.writeheader();w.writerows(details)
                record=dict(name=row['name'],seed=row['seed'],profile=profile,market=market,interval_minutes=multiple,trade_candles=trade_shape,
                    sampled_quote_mid_candles=mid_shape,median_outside_sampled_mid_envelope_fraction=float(np.median(excess)) if excess else None,
                    median_spread_to_trade_range=float(np.median(spread_range)) if spread_range else None,
                    extreme_taker_order_multilevel_fraction=float(np.mean(sweep)),extreme_maker_role_counts=dict(extreme_roles),
                    worst_wick_bars=sorted(details,key=lambda r:r['upper_bp']+r['lower_bp'],reverse=True)[:5],
                    receipt_sha256=done['receipt_sha256'],recipe_sha256=row['recipe_sha256'],detail_csv_sha256=sha(csv_path))
                records.append(record)
                print(json.dumps({k:v for k,v in record.items() if k in ('seed','market','interval_minutes','trade_candles','sampled_quote_mid_candles','median_outside_sampled_mid_envelope_fraction','median_spread_to_trade_range','extreme_taker_order_multilevel_fraction','extreme_maker_role_counts')}),flush=True)
        del data,bars,mids,orders,extrema,snapshots;gc.collect()
    reference=json.loads((ROOT/'.local/strategy-research-reservation-20261024/reference-calendar/calendar-reference.json').read_text(encoding='utf-8'));references=[]
    for r in reference['records']:
        bs={b['bucket']:b for b in r['candles']}
        for hour in (0,12):
            for multiple in (1,5):
                _,s=shape(rebar(bs,multiple),hour*3600000,(hour+10)*3600000,multiple*60000)
                references.append(dict(market='V-BTC-'+r['leg'].upper(),date=r['date'],start_hour=hour,interval_minutes=multiple,shape=s))
    write(out/'anatomy-summary.json',dict(records=records,reference_windows=references,selected_seed=seed,analyzer_sha256=sha(Path(__file__)),numpy=np.__version__,
        scope='All seed/market outcomes, primary hours2–12, same raw OHLCV. Quote mids/spreads are one-second after-wave samples; outside-envelope fractions are descriptive proxies and cannot prove which intrawave event caused a wick. Extremum supplier/order analysis uses actual native trades; tied extrema use first occurrence. Order-price-level counts describe actual fills, not submitted quantity. No smoothing, filling or clipping. Both-long definitions are reported descriptively, not trading filters or calibration targets. Historical eight10h windows per market are descriptive only.'))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--source',type=Path,default=PRIOR);p.add_argument('--output',type=Path,default=BASE/'diagnosis');p.add_argument('--profile',default='depth');p.add_argument('--seed',type=int);a=p.parse_args();analyze(a.source.resolve(),a.output.resolve(),a.profile,a.seed)
