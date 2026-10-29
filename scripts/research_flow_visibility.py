"""Exploratory per-second last-32 trade-tape censoring pressure, not signal replay."""
import argparse,json
from collections import Counter,defaultdict
from pathlib import Path
from analyze_market_mechanisms import sha,write

def audit(receipt,recipe,out,start=7200000,end=43200000):
    spec=json.loads(recipe.read_text());raw=json.loads(receipt.read_text())
    owners={(a['Plugin']['participant']['account_id'],a['Plugin']['participant']['instrument_id']):a['Plugin']['plugin_id'] for a in spec['agents']}
    result=defaultdict(Counter);tail_roles=defaultdict(Counter);all_roles=defaultdict(Counter)
    def flush(groups):
        for market,trades in groups.items():
            c=result[market];c['seconds_with_trades']+=1;c['trade_prints']+=len(trades)
            c['quantity']+=sum(t['qty'] for t in trades)
            for t in trades:all_roles[market][owners.get((t['taker_account_id'],market),'external')]+=t['qty']
            for t in trades[-32:]:tail_roles[market][owners.get((t['taker_account_id'],market),'external')]+=t['qty']
            if len(trades)>32:
                c['seconds_exceeding_32']+=1;c['omitted_prints']+=len(trades)-32
                c['omitted_quantity']+=sum(t['qty'] for t in trades[:-32])
                net=lambda ts:sum(t['qty']*(1 if t['taker_side']=='Buy' else -1) for t in ts)
                full,tail=net(trades),net(trades[-32:])
                c['opposite_nonzero_net_sign']+=full*tail<0
    time=None;groups=defaultdict(list)
    for t in raw['trade_receipts']:
        if not start<=t['time_ms']<end:continue
        if time is not None and time!=t['time_ms']:
            assert t['time_ms']>time;flush(groups);groups=defaultdict(list)
        time=t['time_ms'];groups[t['instrument_id']].append(t)
    flush(groups)
    markets={}
    for market,c in result.items():
        markets[market]=dict(c,omitted_print_fraction=c['omitted_prints']/c['trade_prints'],
            omitted_quantity_fraction=c['omitted_quantity']/c['quantity'],all_role_quantities=dict(all_roles[market]),tail_role_quantities=dict(tail_roles[market]))
    write(out,dict(receipt_sha256=sha(receipt),recipe_sha256=sha(recipe),start_ms=start,end_ms=end,markets=markets,
        scope='Post hoc diagnostic. Groups actual prints by one-second market timestamp and examines the last 32 per market. Estimates censoring pressure; it is not a replay of any particular bot observation or signal. Intra-wave/clock-triggered execution order and strategy wake times are not reconstructed.'))
    print(json.dumps({m:{k:v[k] for k in ('trade_prints','seconds_exceeding_32','omitted_print_fraction','omitted_quantity_fraction','opposite_nonzero_net_sign')} for m,v in markets.items()}),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('receipt',type=Path);p.add_argument('recipe',type=Path);p.add_argument('output',type=Path)
    a=p.parse_args();audit(a.receipt,a.recipe,a.output)
