"""Post-hoc descriptive return ranges; not fitted thresholds or confidence bands."""
import json,statistics,sys
from pathlib import Path
import numpy as np
from analyze_market_mechanisms import ROOT,sha,write,bar_metrics
from research_wick_observations import BASE


def analyze():
    assert np.__version__=='2.5.3'
    path=ROOT/'.local/strategy-research-reservation-20261024/reference-calendar/calendar-reference.json'
    source=json.loads(path.read_text(encoding='utf-8'));rows=[]
    for r in source['records']:
        bars={b['bucket']:b for b in r['candles']}
        for h in (0,12):
            m=bar_metrics(bars,h*3600000,(h+10)*3600000)
            rows.append(dict(market='V-BTC-'+r['leg'].upper(),date=r['date'],start_hour=h,**m))
    summaries=[]
    for market in ('V-BTC-SPOT','V-BTC-PERP'):
        rs=[r for r in rows if r['market']==market];assert len(rs)==8
        summaries.append(dict(market=market,windows=len(rs),std_bp_min=min(r['return_std_bp']for r in rs),std_bp_max=max(r['return_std_bp']for r in rs),std_bp_mean=statistics.mean(r['return_std_bp']for r in rs)))
    write(BASE/'diagnosis-v2/reference-return-summary.json',dict(records=rows,summary=summaries,source_sha256=sha(path),analyzer_sha256=sha(Path(__file__)),numpy=np.__version__,
        scope='Additional post-hoc descriptive return context, after seeing the first full dual-execution outcome. Same eight cached10h windows per market. Not used to tune coefficients, choose the fixed primary window, construct an acceptance threshold or claim independent real-market qualification. Historical min–max is not a confidence interval.'))
    print(json.dumps(summaries,indent=2))


if __name__=='__main__':analyze()
