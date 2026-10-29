"""Post-pilot descriptive horizon diagnostics; never a retuned selection target."""
import argparse
import json
import math
import statistics
from pathlib import Path
import research_numeric
import numpy as np
from analyze_market_mechanisms import bar_metrics, sha, write
from analyze_basis_execution import candles
from research_market_mechanisms import ROOT


def variance_ratios(selected):
    r1 = [math.log(b['close']/selected[k-1]['close'])*10000 for k,b in selected.items() if k-1 in selected]
    base = float(np.var(r1, ddof=1))
    result = []
    for q in (5,15):
        returns = [math.log(b['close']/selected[k-q]['close'])*10000 for k,b in selected.items() if all(k-j in selected for j in range(q+1))]
        result.append(dict(minutes=q, overlapping_samples=len(returns),
            variance_ratio=float(np.var(returns, ddof=1))/(q*base) if len(returns)>1 and base>0 else None))
    return result


def reference(out):
    source = ROOT/'.local/strategy-research-reservation-20261024/reference-calendar/calendar-reference.json'
    data = json.loads(source.read_text(encoding='utf-8'))
    rows = []
    for r in data['records']:
        bars = {b['bucket']:b for b in r['candles']}
        for parent in (0,12):
            for hour in range(parent,parent+10,2):
                selected = {k:b for k,b in bars.items() if hour*3600000<=k*60000 and (k+1)*60000<=(hour+2)*3600000}
                rows.append(dict(market='V-BTC-'+r['leg'].upper(), date=r['date'], start_hour=hour, horizons=variance_ratios(selected)))
    summaries = []
    for market in ('V-BTC-SPOT','V-BTC-PERP'):
        rs = [r for r in rows if r['market']==market]
        assert len(rs)==40
        for q in (5,15):
            values = [next(h['variance_ratio'] for h in r['horizons'] if h['minutes']==q) for r in rs]
            summaries.append(dict(market=market, minutes=q, windows=40, min=min(values), median=statistics.median(values), max=max(values)))
    write(out/'reference-horizon-context.json', dict(records=rows, summary=summaries,
        source_sha256=sha(source), analyzer_sha256=sha(Path(__file__)),
        scope='Post-pilot descriptive context only. Same four previously used2025 dates and40 nonoverlapping2h windows per market; no new date selection or data download. Not fitted acceptance bands, significance tests, independent qualification or market-realism certification.'))
    print(json.dumps(summaries, indent=2))


def analyze(out):
    comparison = json.loads((out/'comparison.json').read_text(encoding='utf-8'))
    rows = []
    for m in comparison['metrics']:
        for market, metrics in m['markets'].items():
            bars = candles(out/(m['name']+'-'+market+'.candles.csv'))
            selected = {k:b for k,b in bars.items() if k*60000>=m['warmup_ms'] and (k+1)*60000<=m['steps']*1000}
            horizons = variance_ratios(selected)
            rows.append(dict(seed=m['seed'], profile=m['profile'], market=market,
                receipt_sha256=m['receipt_sha256'], minute_return_acf=metrics['minute']['return_acf'],
                hourly=[dict(hour=h, **bar_metrics(bars,h*3600000,(h+1)*3600000)) for h in (2,3)], horizons=horizons))
    write(out/'horizon-diagnostics.json', dict(records=rows, analyzer_sha256=sha(Path(__file__)),
        scope='Diagnostic added after viewing pilot41, before other candidate/holdout outcomes. Same primary2–4h, original actual1m candles, no filling. VR(q)=sample variance of overlapping q-minute log returns / (q * variance of adjacent1m log returns). Short, dependent overlapping samples: descriptive only, no significance or realism pass-band. Shows longer-lag cancellation that ACF1 alone can miss; not a newly fitted acceptance target. Hourly stats retain both halves without replacing the primary window.'))
    print(json.dumps(rows, indent=2))


if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__); p.add_argument('directory', type=Path); p.add_argument('--reference', action='store_true')
    args=p.parse_args(); reference(args.directory) if args.reference else analyze(args.directory)
