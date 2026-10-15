"""Market-quality receipts for strategy-led populations; no optimization work."""
from __future__ import annotations
import argparse,collections,hashlib,json,math,subprocess,time
from pathlib import Path
from strategy_ecosystem import population
from compare_behavior_reference import summarize

ROOT=Path(__file__).resolve().parents[1]
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def write(path,value):path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')

def analyze(receipt,roster):
    owners={p['participant_id']:p for p in roster['participants']};role_qty=collections.Counter();family_qty=collections.Counter();active_roles=collections.Counter()
    for name,qty in receipt['fills'].items():
        if name in owners:
            role_qty[owners[name]['role']]+=qty;active_roles[owners[name]['role']]+=1
            if owners[name]['family']:family_qty[owners[name]['family']]+=qty
    ready=0;bars=collections.defaultdict(list)
    for entry in receipt['states']:
        owner=owners.get(entry['id']);state=entry['state']['Plugin']['data']
        if owner and owner['role']=='technical':
            ready+=bool(state.get('frame',{}).get('ready'));bars[owner['config']['bar_interval_ms']].append(state.get('bars',0))
    summary=summarize(receipt,{'price_tick_usdt':'1','quantity_lot_btc':'1'},require_order_ids=True)
    total=sum(role_qty.values())
    order_flow={}
    for leg in ('SPOT','PERP'):
        rows=[r for r in receipt['trade_receipts']if r['instrument_id'].endswith(leg)]
        signs=[];last=None
        for r in rows:
            key=(r['time_ms'],r['taker_order_id'])
            if key!=last:signs.append(1 if r['taker_side']=='Buy'else-1);last=key
        order_flow[leg]={'filled_taker_orders':len(signs),'same_direction_fraction':sum(a==b for a,b in zip(signs,signs[1:]))/(len(signs)-1)if len(signs)>1 else None,
            'buy_fraction':sum(x==1 for x in signs)/len(signs)if signs else None,'definition':'one event per actual filled taker order, not per fill or 100ms aggregation proxy'}
    return {'raw_trades':receipt['trades'],'summary_in_internal_units':summary,'active_instances':len(receipt['fills']),
        'active_by_role':dict(active_roles),'technical_ready_at_end':ready,'active_technical_families':len(family_qty),'family_participation_qty':dict(family_qty),
        'role_participation_qty':dict(role_qty),'role_participation_shares':{r:v/total for r,v in role_qty.items()},
        'bars_by_period':{str(p):{'min':min(v),'max':max(v)}for p,v in bars.items()},'rejected_actions':sum(receipt['rejections'].values()),'filled_taker_order_flow':order_flow,
        'participation_definition':'each matched quantity contributes once to maker and once to taker; this is not capital share or taker share'}

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--counts',type=int,nargs='+',default=[100,300]);p.add_argument('--seeds',type=int,nargs='+',default=[7,19,41])
    p.add_argument('--steps',type=int,default=1200);p.add_argument('--output',type=Path,default=ROOT/'.local/strategy-ecosystem-20261014/market-study')
    a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True);binary=ROOT/'target/release/examples/strategy_market.exe'
    report={'binary_sha256':sha(binary),'steps_per_run':a.steps,'capital_scale':10,'noise_instances':0,'technical_count_fraction':.7,
        'periods_ms':[1000,5000,10000],'runs':[],'qualification':'synthetic market-quality exploration; not minimum-population or real-market qualification',
        'limitations':['population size also changes coverage of parameter/period combinations and per-account budgets; not a pure count effect',
            'cash pools are constant, initial spot stock and integer allocation constraints must be compared separately',
            'no external fundamental oracle or scheduled price/news path; private initial valuations and finite parent orders are explicit model inputs',
            'larger periods through 5 minutes are available in the full recipe but require much longer causal warmup',
            'real persistent HTTP behavior and interpreter semantics have separate acceptance receipts']}
    for count in a.counts:
        for seed in a.seeds:
            name=f'n{count}-s{seed}';recipe,roster=population(name,seed,count,capital_scale=10,periods=[1000,5000,10000])
            recipe_path=a.output/f'{name}.recipe.json';write(recipe_path,recipe);write(a.output/f'{name}.roster.json',roster)
            receipt_path=a.output/f'{name}.receipt.json';stderr_path=a.output/f'{name}.stderr';start=time.monotonic()
            try:
                with receipt_path.open('w',encoding='utf-8')as stdout,stderr_path.open('w',encoding='utf-8')as stderr:
                    subprocess.run([str(binary),str(recipe_path),str(a.steps)],stdout=stdout,stderr=stderr,check=True,timeout=180)
                receipt=json.loads(receipt_path.read_text(encoding='utf-8'))
                row={'count':count,'seed':seed,'recipe_sha256':sha(recipe_path),'receipt_sha256':sha(receipt_path),**analyze(receipt,roster)}
            except Exception as e:row={'count':count,'seed':seed,'error':str(e)}
            row['wall_seconds']=round(time.monotonic()-start,3);report['runs'].append(row);write(a.output/'report.json',report)
            print(json.dumps({k:row[k]for k in ['count','seed','wall_seconds']},ensure_ascii=True),flush=True)
            if 'error'in row:return 1
    return 0

if __name__=='__main__':raise SystemExit(main())
