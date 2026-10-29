"""Same-wave public quote linkage, separate from asynchronous trade-close basis."""
import argparse,json,statistics
from pathlib import Path
from analyze_wick_basis_capacity import records
from research_market_mechanisms import sha,write


def dist(values):
    s=sorted(values)
    return dict(samples=len(s),median=statistics.median(s),p99=s[min(len(s)-1,int(.99*(len(s)-1)))],max=s[-1])if s else dict(samples=0)


def one(out,row):
    path=out/(row['name']+'.receipt.json');done=json.loads((out/(row['name']+'.run.json')).read_text(encoding='utf-8'));assert done['complete']and sha(path)==done['receipt_sha256']
    recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'))
    thresholds=sorted({a['Plugin']['config']['entry_ppm']/100 for a in recipe['agents']if a['Plugin']['plugin_id']=='BasisLinkTrader'})
    mids=[];minute_mids=[];edges=[];worst=[];available=0;missing=0;current=None;pair={};runs={t:0 for t in thresholds};longest=dict(runs);counts=dict(runs)
    def flush(t,pair):
        nonlocal available,missing
        if t is None:return
        assert set(pair)=={'V-BTC-SPOT','V-BTC-PERP'}
        s,p=pair['V-BTC-SPOT'],pair['V-BTC-PERP']
        if not all((s['bids'],s['asks'],p['bids'],p['asks'])):
            missing+=1
            for t in thresholds:runs[t]=0
            return
        sb,sa,pb,pa=[x[0]['price_tick']for x in (s['bids'],s['asks'],p['bids'],p['asks'])]
        mid=(pb+pa)/(sb+sa)-1;rich=pb/sa-1;cheap=pa/sb-1;edge=max(rich,-cheap)*10000
        available+=1;mids.append(abs(mid)*10000);edges.append(edge)
        if t%60000==59000:minute_mids.append(abs(mid)*10000)
        for threshold in thresholds:
            runs[threshold]=runs[threshold]+1 if edge>threshold else 0
            longest[threshold]=max(longest[threshold],runs[threshold]);counts[threshold]+=edge>threshold
        worst.append(dict(time_ms=t,absolute_mid_basis_bp=abs(mid)*10000,gross_pair_edge_bp=edge,spot_bid=sb,spot_ask=sa,perp_bid=pb,perp_ask=pa))
        if len(worst)>100:worst.sort(key=lambda r:r['gross_pair_edge_bp'],reverse=True);del worst[10:]
    for sample in records(path,'book_snapshots'):
        t=sample['time_ms']
        if not row['warmup_ms']<=t<row['steps']*1000:continue
        assert sample['phase']=='after_all_actions'
        if current is not None and current!=t:flush(current,pair);pair={}
        current=t;assert sample['instrument_id']not in pair;pair[sample['instrument_id']]=sample['book']
    flush(current,pair);expected=(row['steps']*1000-row['warmup_ms'])//1000;assert available+missing==expected
    worst.sort(key=lambda r:r['gross_pair_edge_bp'],reverse=True)
    return dict(name=row['name'],seed=row['seed'],profile=row['profile'],receipt_sha256=done['receipt_sha256'],available_waves=available,missing_two_sided_waves=missing,
        absolute_same_wave_mid_basis_bp=dist(mids),absolute_minute_end_mid_basis_bp=dist(minute_mids),gross_pair_edge_bp=dist(edges),
        thresholds=[dict(entry_bp=t,fraction_above=counts[t]/available if available else None,longest_run_seconds=longest[t])for t in thresholds],worst_samples=worst[:10])


def analyze(out,control=None,seed=None):
    result=[]
    for folder in ([control]if control else[])+[out]:
        plan=json.loads((folder/'plan.json').read_text(encoding='utf-8'))
        for row in plan['runs']:
            if seed is not None and row['seed']!=seed:continue
            done=folder/(row['name']+'.run.json')
            if not done.exists()or not json.loads(done.read_text(encoding='utf-8'))['complete']:continue
            result.append(one(folder,row))
    write(out/'same-wave-links.json',dict(records=result,analyzer_sha256=sha(Path(__file__)),scope='Post-hoc linkage diagnostic: observed public BBOs after the same complete1s action wave, not simultaneous guaranteed fills. Gross two-leg quoted edge before fees/funding/legging risk; compare only to existing configured entry thresholds. Minute-end quote mids are observed at second59 with no forward filling. Not the asynchronous last-trade closes. Missing sides are counted, never carried forward. P99 uses the empirical lower order statistic without interpolation.'))
    print(json.dumps([{k:v for k,v in r.items()if k!='worst_samples'}for r in result],indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);p.add_argument('--control-directory',type=Path);p.add_argument('--seed',type=int);a=p.parse_args();analyze(a.directory,a.control_directory,a.seed)
