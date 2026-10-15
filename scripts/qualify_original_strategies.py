"""Select and run archived original Pine strategies without changing their code.

Qualification here means causal offline execution and observed order generation.
Live broker/account qualification is reported separately. Failures are retained.
"""
from __future__ import annotations
import argparse,hashlib,json,random,re,shutil,time
from pathlib import Path
import pine_compat
from research_strategy_compat import bars_for

ROOT=Path(__file__).resolve().parents[1]
KEYWORDS={'trend':['trend','ema','macd','sma','alma','hull','ichimoku','ssl'],
 'reversion':['rsi','stoch','bollinger','cci','williams','reversion','wavetrend'],
 'breakout':['breakout','donchian','turtle','channel','keltner','dual thrust'],
 'volume':['volume','vwap','obv','mfi'], 'volatility':['atr','squeeze','volatility','supertrend']}

def family(path):
    name=path.stem.lower()
    return next((f for f,words in KEYWORDS.items()if any(w in name for w in words)),'other')

def select(limit):
    paths=json.loads((ROOT/'.local/strategy-ecosystem-20261014/original-strategy-paths.json').read_text(encoding='utf-8'))
    groups={k:[]for k in [*KEYWORDS,'other']}
    for name in paths:groups[family(Path(name))].append(name)
    rng=random.Random(20261014)
    for entries in groups.values():rng.shuffle(entries)
    result=[]
    while len(result)<limit and any(groups.values()):
        for entries in groups.values():
            if entries and len(result)<limit:result.append(Path(entries.pop()))
    return result

def extract_source(raw):
    text=raw.decode('utf-8-sig')
    if 'PineScript code:' in text:
        # Some archived .pine files are page transcriptions, including prose
        # and a preceding column of line numbers. Preserve their byte archive,
        # extract the actual code block and retain its license/author comments.
        text=text.split('PineScript code:',1)[1]
        lines=text.splitlines()
        start=next((i for i,line in enumerate(lines)if line.lstrip().startswith(('//','//@version='))),None)
        if start is None:raise ValueError('no identifiable code block in page transcription')
        tail=lines[start:]
        end=next((i for i,line in enumerate(tail)if re.fullmatch(r'\s*Expand\s*\(\d+\s+lines\)\s*',line)),len(tail))
        text='\n'.join(tail[:end])+'\n'
    # Presentation whitespace normalization, not a rewrite of trading logic.
    return text.replace('\u00a0',' ').replace('\u200b','')

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--limit',type=int,default=80)
    p.add_argument('--output',type=Path,default=ROOT/'.local/strategy-ecosystem-20261014/originals');p.add_argument('--install',action='store_true')
    a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
    report={'pine_version':pine_compat.__version__,'archived_order_strategy_files':1749,'selection':'fixed seed stratified sample by filename behavior keywords, not exhaustive coverage','cases':[],'qualified':[],
        'qualification':'original bytes, offline execution, sampled prefix causality and order generation; not live broker qualification'}
    for path in select(a.limit):
        path=ROOT/path if not path.is_absolute()else path
        raw=path.read_bytes();sha=hashlib.sha256(raw).hexdigest();row={'name':path.stem,'path':str(path),'family_hint':family(path),'source_sha256':sha}
        start=time.monotonic()
        try:
            source=extract_source(raw);source_bytes=source.encode('utf-8');row['installed_sha256']=hashlib.sha256(source_bytes).hexdigest()
            row['source_transform']='remove page prose and line-number column when present; normalize NBSP and zero-width presentation whitespace'
            if len(source_bytes)>65536:raise ValueError('source exceeds installed plugin 64KiB bound')
            if re.search(r'(?:lookahead\s*=\s*(?:barmerge\.)?lookahead_on|barmerge\.lookahead_on)',source):raise ValueError('explicit lookahead-on excluded')
            if re.search(r'\b(?:request\.security|security)\s*\(',source):raise ValueError('additional market/timeframe dependency requires separate data qualification')
            analysis=pine_compat.analyze_script(source)
            errors=[d for d in analysis.get('diagnostics',[])if d.get('severity')=='error']
            if errors:raise ValueError(json.dumps(errors[:3]))
            row['inputs']=analysis.get('inputs',[]);row['runs']=[]
            total_orders=0
            for regime in ['range','trend_reversal','random_walk']:
                bars=bars_for(regime,n=320)
                output=pine_compat.run_script(source,bars,chart_symbol='V-BTC-SPOT',chart_timeframe='1')
                errors=[d for d in output.get('diagnostics',[])if d.get('severity')=='error']
                errors += [d for d in (output.get('strategy')or{}).get('diagnostics',[])if d.get('severity')=='error']
                if errors:raise ValueError(json.dumps(errors[:3]))
                orders=(output.get('strategy')or{}).get('orders',[]);total_orders+=len(orders)
                prefix=pine_compat.run_script(source,bars[:157],chart_symbol='V-BTC-SPOT',chart_timeframe='1')
                if [p['values']for p in prefix.get('plots',[])]!=[p['values'][:157]for p in output.get('plots',[])]:raise ValueError('plot prefix changes with future data')
                if (prefix.get('strategy')or{}).get('orders',[])!=[o for o in orders if o['barIndex']<157]:raise ValueError('order prefix changes with future data')
                row['runs'].append({'regime':regime,'orders':len(orders),'prefix_check':True})
            if total_orders==0:raise ValueError('runs but no orders in three tested regimes; not an active population candidate')
            row.update(status='offline_qualified',total_orders=total_orders)
            # Preserve bytes and original headers; installing does not turn an
            # offline receipt into a live broker qualification.
            script=f'ecosystem/originals/{sha[:16]}.pine';row['installed_script']=script
            if a.install:
                destination=ROOT/'bot-plugins/pine-strategy/scripts'/script;destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(source_bytes)
                assert hashlib.sha256(destination.read_bytes()).hexdigest()==row['installed_sha256']
            report['qualified'].append(row)
        except Exception as exc:row.update(status='excluded',error=str(exc))
        row['seconds']=round(time.monotonic()-start,4);report['cases'].append(row)
        (a.output/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(json.dumps({'name':row['name'],'status':row['status'],'orders':row.get('total_orders',0),'error':row.get('error','')[:120]},ensure_ascii=True),flush=True)
    print(json.dumps({'sampled':len(report['cases']),'offline_qualified':len(report['qualified']),'report':str(a.output/'report.json')}))

if __name__=='__main__':main()
