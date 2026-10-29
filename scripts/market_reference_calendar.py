"""Calendar-selected real-market reference; never used to fit the bot inputs."""
from __future__ import annotations
import argparse,csv,io,json,math,sys,urllib.request,zipfile
from datetime import datetime,timezone
from pathlib import Path
from analyze_market_mechanisms import ROOT,add_bar,bar_metrics,rebar,sha,write
from analyze_basis_execution import morphology

DATES=('2025-01-02','2025-04-02','2025-07-02','2025-10-02')

def retrieve(url,path):
    if path.exists():return
    partial=path.with_suffix(path.suffix+'.partial')
    with urllib.request.urlopen(url,timeout=60) as response,partial.open('wb') as f:
        for chunk in iter(lambda:response.read(1024*1024),b''):f.write(chunk)
    partial.replace(path)

def collect(out):
    out.mkdir(parents=True,exist_ok=True)
    selection=out/'selection.json'
    if selection.exists():assert json.loads(selection.read_text(encoding='utf-8'))['dates']==list(DATES)
    else:write(selection,dict(dates=DATES,selected_at=datetime.now(timezone.utc).isoformat(),
        rule='The second UTC day of the first month of each quarter of 2025, chosen by calendar, not by price appearance.',
        scope='Exploratory validation expansion after the first basis result; never used to change frozen recipes. No order-book reference or population-wide confidence interval.'))
    records=[]
    for date in DATES:
        start=int(datetime.fromisoformat(date).replace(tzinfo=timezone.utc).timestamp()*1000)
        for leg,folder in [('spot','spot'),('perp','futures/um')]:
            base=f'https://data.binance.vision/data/{folder}/daily/aggTrades/BTCUSDT/BTCUSDT-aggTrades-{date}.zip'
            archive=out/f'{leg}-{date}.zip';checksum=out/(archive.name+'.CHECKSUM')
            if date=='2025-01-02':
                archive=ROOT.parent/'MarketForge/.local/market-calibration/source'/archive.name
            retrieve(base+'.CHECKSUM',checksum);retrieve(base,archive)
            expected=checksum.read_text(encoding='utf-8').split()[0];actual=sha(archive);assert actual==expected,(leg,date)
            cache=out/f'{leg}-{date}.metrics.json'
            if cache.exists():
                result=json.loads(cache.read_text(encoding='utf-8'));assert result['source']['sha256']==actual
            else:
                bars={};last_id=last_time=-1;count=0
                with zipfile.ZipFile(archive) as z:
                    assert len(z.namelist())==1
                    with z.open(z.namelist()[0]) as f:
                        for row in csv.reader(io.TextIOWrapper(f,encoding='utf-8')):
                            if not row[0].isdigit():continue
                            ident=int(row[0]);time=int(row[5])//(1000 if leg=='spot' else 1)
                            assert ident>last_id and time>=last_time and start<=time<start+86400000
                            price,qty=float(row[1]),float(row[2]);assert math.isfinite(price) and math.isfinite(qty) and price>0 and qty>0
                            add_bar(bars,time-start,price,qty);last_id,last_time=ident,time;count+=1
                result=dict(leg=leg,date=date,records=count,source=dict(url=base,checksum_url=base+'.CHECKSUM',sha256=actual,bytes=archive.stat().st_size,path=str(archive)),
                    full_day=bar_metrics(bars,0,86400000),two_hour_windows=[dict(start_hour=h,**bar_metrics(bars,h*3600000,(h+2)*3600000)) for h in range(0,24,2)],
                    ten_hour_windows=[dict(start_hour=h,**bar_metrics(bars,h*3600000,(h+10)*3600000)) for h in (0,12)],
                    morphology_1m=morphology(bars,0,86400000),morphology_5m=morphology(rebar(bars,5),0,86400000,300000),candles=list(bars.values()))
                write(cache,result)
            records.append(result)
            print(date,leg,result['records'],result['full_day']['return_std_bp'],flush=True)
    write(out/'calendar-reference.json',dict(selection_sha256=sha(selection),python=sys.version,records=records,
        source_documentation='https://github.com/binance/binance-public-data',scope='Four calendar-selected days. AggTrades preserve price/volume bars, but are not a historical order book or individual taker-order tape. Window ranges are descriptive, not confidence intervals.'))

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,default=ROOT/'.local/strategy-research-reservation-20261024/reference-calendar')
    collect(p.parse_args().output.resolve())
