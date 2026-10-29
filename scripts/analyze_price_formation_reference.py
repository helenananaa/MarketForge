"""Post-hoc same-length context, never used to tune mechanism coefficients."""
import json,statistics
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write
from research_price_formation import BASE
from analyze_market_mechanisms import bar_metrics,rebar
from analyze_wick_anatomy import shape


def analyze():
    source=ROOT/'.local/strategy-research-reservation-20261024/reference-calendar/calendar-reference.json'
    data=json.loads(source.read_text(encoding='utf-8'));rows=[]
    for r in data['records']:
        bars={b['bucket']:b for b in r['candles']}
        for parent in (0,12):
            for hour in range(parent,parent+10,2):
                start,end=hour*3600000,(hour+2)*3600000
                m=bar_metrics(bars,start,end)
                _,s=shape(rebar(bars,5),start,end,300000)
                rows.append(dict(market='V-BTC-'+r['leg'].upper(),date=r['date'],start_hour=hour,**m,wick_5m=s['median']['wick_fraction'],both_outer_quarters_5m=s['both_outer_quarters_fraction']))
    summaries=[]
    for market in ('V-BTC-SPOT','V-BTC-PERP'):
        rs=[r for r in rows if r['market']==market];assert len(rs)==40
        summaries.append(dict(market=market,windows=40,**{k:dict(min=min(r[k]for r in rs),median=statistics.median(r[k]for r in rs),max=max(r[k]for r in rs))for k in ('return_std_bp','return_acf1','abs_return_acf1','longest_directional_run','wick_5m','both_outer_quarters_5m')}))
    write(BASE/'reference-short-context.json',dict(records=rows,summary=summaries,source_sha256=sha(source),analyzer_sha256=sha(Path(__file__)),
        scope='Post-hoc descriptive context after the pilot risk and user horizon change. Partition all previously used eight10h windows per market into40 non-overlapping2h windows; same length as the new primary[2h,4h). Four2025 dates, no new download or date selection. Not calibration, independent qualification, acceptance bands or confidence intervals.'))
    print(json.dumps(summaries,indent=2))


if __name__=='__main__':analyze()
