"""Read actual pair-position ledgers with bounded-memory JSON array streaming."""
import argparse,codecs,csv,json,mmap
from collections import Counter
from pathlib import Path
from analyze_market_mechanisms import ROOT,sha,write
from research_wick_observations import SOURCE


def records(path,key):
    token=('"'+key+'":').encode()
    with path.open('rb') as f:
        with mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as view:
            pos=view.find(token);assert pos>=0,key;pos+=len(token)
        f.seek(pos)
        while True:
            c=f.read(1)
            if not c:raise ValueError('Missing root array: '+key)
            if c not in b' \t\r\n':break
        assert c==b'[',key
        utf8=codecs.getincrementaldecoder('utf-8')();decoder=json.JSONDecoder();buf='';i=0;ended=False
        while True:
            while i<len(buf) and buf[i]in ' \t\r\n,':i+=1
            if i<len(buf) and buf[i]==']':return
            if i<len(buf):
                try:obj,j=decoder.raw_decode(buf,i)
                except json.JSONDecodeError:
                    if ended:raise
                else:i=j;yield obj;continue
            if ended:raise ValueError('Unclosed root array: '+key)
            chunk=f.read(1024*1024);ended=not chunk
            buf=buf[i:]+utf8.decode(chunk,final=ended);i=0


def one(out,row):
    recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'));assert sha(Path(row['recipe']))==row['recipe_sha256']
    done=json.loads((out/(row['name']+'.run.json')).read_text(encoding='utf-8'));assert done['complete']
    path=out/(row['name']+'.receipt.json');assert sha(path)==done['receipt_sha256']
    byid={a['Plugin']['participant']['participant_id']:a['Plugin']for a in recipe['agents']};pairs={}
    for a in byid.values():
        if a['plugin_id']!='BasisLinkTrader' or a['config']['leg']!='Perp':continue
        c=a['config'];account=a['participant']['account_id'];initial=next(x['Spot']['position_qty']for x in recipe['scenario']['accounts']if 'Spot'in x and x['Spot']['account_id']==account)
        assert initial==c['neutral_spot']
        pairs[account]=dict(account_id=account,spot_instrument=c['hedge_instrument_id'],perp_instrument=a['participant']['instrument_id'],neutral=c['neutral_spot'],max_hedge=c['max_hedge'],entry_ppm=c['entry_ppm'],spot=initial,perp=0,
            unhedged_ms=0,at_cap_ms=0,max_abs_residual=0,max_abs_perp=0,filled_lots=0)
    start,end=row['warmup_ms'],row['steps']*1000;last=0;counts=Counter()
    def elapsed(t):
        nonlocal last
        assert last<=t<=end
        dt=max(0,min(t,end)-max(last,start))
        for p in pairs.values():
            if p['spot']+p['perp']!=p['neutral']:p['unhedged_ms']+=dt
            if abs(p['perp'])>=p['max_hedge']:p['at_cap_ms']+=dt
            if dt>0:
                # A position/residual may have been carried into the primary
                # window and close on its first trade; include that exposure.
                p['max_abs_residual']=max(p['max_abs_residual'],abs(p['spot']+p['perp']-p['neutral']))
                p['max_abs_perp']=max(p['max_abs_perp'],abs(p['perp']))
        last=t
    for t in records(path,'trade_receipts'):
        counts[t['instrument_id']]+=1;elapsed(t['time_ms'])
        for side in ('taker','maker'):
            account=t[side+'_account_id'];p=pairs.get(account)
            if p is None:continue
            market=t['instrument_id'];which='spot'if market==p['spot_instrument']else'perp'
            assert market in (p['spot_instrument'],p['perp_instrument'])
            sign=1 if t['taker_side']=='Buy' else -1
            if side=='maker':sign=-sign
            p[which]+=sign*t['qty'];p['filled_lots']+=t['qty']
            if start<=t['time_ms']<end:
                p['max_abs_residual']=max(p['max_abs_residual'],abs(p['spot']+p['perp']-p['neutral']))
                p['max_abs_perp']=max(p['max_abs_perp'],abs(p['perp']))
    elapsed(end);assert dict(counts)==done['trades']
    for s in records(path,'states'):
        a=byid[s['id']]
        if a['plugin_id']=='BasisLinkTrader':pairs[a['participant']['account_id']][a['config']['leg'].lower()+'_saved_state']=s['state']['Plugin']['data']
    rows=[]
    for p in pairs.values():
        p.update(seed=row['seed'],profile=row['profile'],name=row['name'],receipt_sha256=done['receipt_sha256'],end_residual=p['spot']+p['perp']-p['neutral'],at_cap_fraction=p['at_cap_ms']/(end-start),unhedged_time_fraction=p['unhedged_ms']/(end-start))
        rows.append(p)
    return rows


def analyze(out,seed=None):
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));prior=json.loads((SOURCE/'plan.json').read_text(encoding='utf-8'));result=[]
    for row in plan['runs']:
        if seed is not None and row['seed']!=seed:continue
        done=out/(row['name']+'.run.json')
        if not done.exists() or not json.loads(done.read_text(encoding='utf-8'))['complete']:continue
        old=next(x for x in prior['runs']if x['seed']==row['seed']and x['profile']=='depth')
        result.extend(one(SOURCE,{**old,'profile':'control'}));result.extend(one(out,row))
    write(out/'basis-capacity-audit-v2.json',dict(records=result,selected_seed=seed,analyzer_sha256=sha(Path(__file__)),
        scope='Actual ordered trade prints reconstruct both legs from the recipe initial spot inventory and zero initial perp position. All printed fills, including any forced liquidation, contribute. Primary hours2–12; unhedged and cap occupancy durations integrate between actual timestamps; same-timestamp legging has zero duration but can raise eventwise max residual. Final residual includes the final boundary. Cap occupancy is a position constraint, not proof every missed opportunity was caused by it. Saved states are final observed bot states, not a substitute for the execution ledger. No private/future data is provided to agents. Bounded-memory streaming independently checks total print counts.'))
    print(json.dumps([{k:v for k,v in r.items()if not k.endswith('_saved_state')}for r in result],indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);p.add_argument('--seed',type=int);a=p.parse_args();analyze(a.directory,a.seed)
