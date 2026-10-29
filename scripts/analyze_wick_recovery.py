"""Describe quote recovery following ordered extremum executions, not causality."""
import csv,json,bisect
from collections import defaultdict
from pathlib import Path
import numpy as np
from analyze_market_mechanisms import ROOT,sha,write

BASE=ROOT/'.local/strategy-research-wicks-20261025'
PRIOR=ROOT/'.local/strategy-research-causal-20261025'


def analyze():
    folder=PRIOR/'depth-analysis-v3'
    gateways=json.loads((folder/'gateway-accounts.json').read_text(encoding='utf-8'))
    lookup={(g['market'],g['command_seq']):g for g in gateways}
    # End-of-second gateway midpoint; future observations only describe recovery.
    samples=defaultdict(dict)
    for g in gateways:
        b=g['book_after']
        if b['bids'] and b['asks']:
            samples[g['market']][g['time_ms']]=(b['bids'][0]['price_tick']+b['asks'][0]['price_tick'])/2
    details=[]
    for r in csv.DictReader((BASE/'trace-diagnosis/depth-extrema.csv').open()):
        market=r['market'];t=int(r['time_ms']);g=lookup[market,int(r['command_seq'])]
        b=g['book_before'];sign=1 if r['extreme']=='high' else -1
        before=(b['bids'][0]['price_tick']+b['asks'][0]['price_tick'])/2
        price=int(r['price_tick']);amplitude=(price-before)*sign
        row=dict(market=market,time_ms=t,extreme=r['extreme'],role=r['taker_role'],price=price,pre_mid=before,excursion_ticks=amplitude,
                 beyond_touch_bp=float(r['execution_beyond_touch_bp']))
        for delay in (1000,3000,10000):
            after=samples[market].get(t+delay)
            row[f'mid_after_{delay}ms']=after
            row[f'fraction_of_excursion_remaining_{delay}ms']=(after-before)*sign/amplitude if after is not None and amplitude>0 else None
        details.append(row)
    result=[]
    for market in samples:
        rows=[r for r in details if r['market']==market]
        summary=dict(market=market,n=len(rows))
        for delay in (1000,3000,10000):
            v=[r[f'fraction_of_excursion_remaining_{delay}ms'] for r in rows if r[f'fraction_of_excursion_remaining_{delay}ms'] is not None]
            summary[f'valid_excursion_samples_{delay}ms']=len(v)
            summary[f'fraction_remaining_median_{delay}ms']=float(np.median(v)) if v else None
            summary[f'recovered_at_least_half_fraction_{delay}ms']=sum(x<.5 for x in v)/len(v) if v else None
        result.append(summary)
    write(BASE/'trace-diagnosis/recovery-v2.json',dict(records=result,details=details,
        inputs={str(p.relative_to(ROOT)):sha(p) for p in (folder/'gateway-accounts.json',BASE/'trace-diagnosis/depth-extrema.csv')},
        analyzer_sha256=sha(Path(__file__)),scope='Selected old-depth minutes150–161 only, first tied extremum per minute. Pre-command midpoint is exact; later midpoints are the final recorded gateway book that second, at exact1/3/10s horizons. Ratios can be negative or exceed1. Samples are dependent and endogenous; no event is isolated, and recovery does not establish a cause.'))
    print(json.dumps(result))


if __name__=='__main__':analyze()
