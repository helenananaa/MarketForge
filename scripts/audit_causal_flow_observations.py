"""Measure actual visible-flow loss for two traced makers against the full receipt tape."""
from __future__ import annotations
import argparse,bisect,csv,json
from collections import defaultdict
from pathlib import Path
from analyze_market_mechanisms import ROOT,sha,write


def audit(root):
    results=[];all_rows=[]
    for profile,name in [('control','trace-s41-control-v2'),('hedged','diagnostic-s41-hedged'),('depth','diagnostic-s41-depth')]:
        receipt=root/(name+'.receipt.json');trace=root/(name+'.trace.jsonl')
        data=json.loads(receipt.read_text(encoding='utf-8'));tapes=defaultdict(list)
        for trade in data['trade_receipts']:tapes[trade['instrument_id']].append(trade)
        del data
        prefixes={}
        for market,trades in tapes.items():
            ids=[];qty=[0];signed=[0]
            for t in trades:
                assert not ids or ids[-1]<t['trade_id']
                ids.append(t['trade_id']);qty.append(qty[-1]+t['qty']);signed.append(signed[-1]+t['qty']*(1 if t['taker_side']=='Buy' else -1))
            prefixes[market]=(ids,qty,signed)
        del tapes
        rows=[]
        with trace.open(encoding='utf-8') as f:
            for line in f:
                row=json.loads(line)
                if row['kind']!='decision' or row['participant'] not in ('maker-spot-0','maker-perp-17'):continue
                v=row['observation'];previous=row['before'].get('last_trade_id',-1)
                visible=[t for t in v['public_trades'] if t['trade_id']>previous]
                newest=max([previous]+[t['trade_id'] for t in visible])
                ids,qty,signed=prefixes[v['instrument']];a=bisect.bisect_right(ids,previous);b=bisect.bisect_right(ids,newest)
                full_qty=qty[b]-qty[a];full_signed=signed[b]-signed[a]
                visible_qty=sum(t['qty'] for t in visible);visible_signed=sum(t['qty']*(1 if t['taker_side']=='Buy' else -1) for t in visible)
                assert full_qty>=visible_qty
                rows.append(dict(profile=profile,participant=row['participant'],time_ms=v['time_ms'],market=v['instrument'],previous_id=previous,newest_id=newest,
                    visible_prints=len(visible),full_prints=b-a,visible_qty=visible_qty,full_qty=full_qty,visible_signed=visible_signed,full_signed=full_signed,
                    opposite_nonzero_sign=visible_signed*full_signed<0))
        for market in prefixes:
            selected=[r for r in rows if r['market']==market]
            full=sum(r['full_qty'] for r in selected);visible=sum(r['visible_qty'] for r in selected)
            results.append(dict(profile=profile,market=market,observations=len(selected),full_qty=full,visible_qty=visible,
                omitted_qty=full-visible,omitted_fraction=(full-visible)/full if full else 0,
                observations_with_omission=sum(r['full_qty']>r['visible_qty'] for r in selected),opposite_nonzero_signs=sum(r['opposite_nonzero_sign'] for r in selected),
                receipt_sha256=sha(receipt),trace_sha256=sha(trace)))
        all_rows.extend(rows)
        print(profile,results[-2:],flush=True)
    with (root/'actual-flow-visibility.csv').open('w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=list(all_rows[0]));w.writeheader();w.writerows(all_rows)
    write(root/'actual-flow-visibility.json',dict(records=results,
        scope='Post hoc exact observation audit for maker-spot-0 and maker-perp-17 in the selected window. Full tape is queried only between the strategy\'s previous ID and its newest actually visible ID, preventing future-trade leakage. Quantifies actual discarded public prints for these consumers; does not reconstruct a hypothetical strategy with complete flow, establish that omission caused a price move, or qualify a full run.'))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--root',type=Path,default=ROOT/'.local/strategy-research-causal-20261025');audit(p.parse_args().root.resolve())
