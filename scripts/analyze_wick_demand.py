"""Reconcile saved parent-demand ledgers with actual native taker fills."""
import argparse,json,gc
from collections import Counter
from pathlib import Path
from analyze_market_mechanisms import sha,write


def analyze(out):
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));result=[]
    for row in plan['runs']:
        done=out/(row['name']+'.run.json')
        if not done.exists():continue
        run=json.loads(done.read_text(encoding='utf-8'));
        if not run['complete']:continue
        receipt=out/(row['name']+'.receipt.json');assert sha(receipt)==run['receipt_sha256']
        recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'));assert sha(Path(row['recipe']))==row['recipe_sha256']
        configs={a['Plugin']['participant']['participant_id']:a['Plugin'] for a in recipe['agents']}
        data=json.loads(receipt.read_text(encoding='utf-8'));actual=Counter()
        for t in data['trade_receipts']:actual[t['taker_account_id'],t['instrument_id']]+=t['qty']
        details=[]
        for entry in data['states']:
            p=configs[entry['id']]
            if p['plugin_id']!='ReactiveFlowTrader' or p['plugin_version']!='1.1.0':continue
            s=entry['state']['Plugin']['data'];ledger=s.get('slicing')
            if ledger is None:continue
            pending=s.get('parent',{}).get('remaining',0)
            assert ledger['demand_lots']==ledger['filled_lots']+ledger['expired_lots']+pending
            filled=actual[p['participant']['account_id'],p['participant']['instrument_id']]
            # A forced liquidation could also print a taker fill; expose any
            # difference explicitly instead of quietly attributing it to a child.
            details.append(dict(participant=entry['id'],market=p['participant']['instrument_id'],**ledger,pending_lots=pending,
                actual_taker_lots=filled,actual_minus_child_fills=filled-ledger['filled_lots']))
        for market in ('V-BTC-SPOT','V-BTC-PERP'):
            rs=[r for r in details if r['market']==market]
            if not rs:continue
            totals={k:sum(r[k] for r in rs) for k in ('parents','demand_lots','filled_lots','expired_lots','children','pending_lots','actual_taker_lots','actual_minus_child_fills')}
            result.append(dict(name=row['name'],seed=row['seed'],profile=row['profile'],market=market,accounts=len(rs),**totals,
                filled_fraction=totals['filled_lots']/totals['demand_lots'],expired_fraction=totals['expired_lots']/totals['demand_lots'],receipt_sha256=run['receipt_sha256'],details=rs))
        del data;gc.collect()
    write(out/'demand-reconciliation.json',dict(records=result,analyzer_sha256=sha(Path(__file__)),scope='Entire run, including warmup. Saved original aggressive demand is partitioned into authoritative child fills, expired residual intent and still-live residual. Passive orders are unchanged and excluded from this demand ledger. Actual taker prints independently reconcile each account, with any difference explicitly exposed; forced liquidation would need separate investigation. This is demand accounting, not a promise that realized volumes/arrival counts equal the sweep baseline.'))
    print(json.dumps([{k:v for k,v in r.items() if k!='details'} for r in result],indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);analyze(p.parse_args().directory)
