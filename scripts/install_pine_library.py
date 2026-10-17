"""Install offline-qualified Pine originals from the local strategy library.

Input is a report from qualify_original_strategies.py. Each source is re-read,
checked against its recorded hash and extracted the same way, then compiled in
the plugin's own external broker runtime and run over three synthetic regimes.
Sources keep third-party licenses (the main corpus is GPL-3.0), so they go to
the ignored scripts/ecosystem/library directory; the manifest records names,
paths, hashes, inputs and preflight results, and this tool rebuilds the files.
Preflight is not an actual-fill qualification; market runs supply that.
"""
from __future__ import annotations
import argparse,hashlib,importlib.util,json,re,sys
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]
PACKAGE=ROOT/'bot-plugins/pine-strategy'
# The plugin's patched runtime, not the generic wheel, decides what can run.
sys.path.insert(0,str(PACKAGE/'.runtime/strategy-ecosystem'))
import pine_compat
from qualify_original_strategies import extract_source
from research_strategy_compat import bars_for
spec=importlib.util.spec_from_file_location('research_broker',PACKAGE/'broker.py')
broker=importlib.util.module_from_spec(spec);spec.loader.exec_module(broker)

# Candle sizes the population may assign, in minutes. Scripts may read the
# chart timeframe (for example `time("1")`), so each is checked separately.
TIMEFRAMES=[1,3,5]

def preflight(source,minutes):
    program=pine_compat.compile_script(broker.external_pass_source(broker.lower_order_calls(source)))
    counts=[]
    for regime in ['range','trend_reversal','random_walk']:
        bars=bars_for(regime,n=320);start=bars[0]['time'];step=minutes*60000
        # Market prices sit near 100000 ticks (price scale 10).
        bars=[{**b,'time':start+i*step,**{k:b[k]*10 for k in ('open','high','low','close')}}for i,b in enumerate(bars)]
        frames=[{'time':b['time'],'position_size':0.,'position_avg_price':None,'equity':1000000.,'initial_capital':1000000.,'netprofit':0.,'openprofit':0.}for b in bars]
        passes=[[{'account':f,'bar':b,'event_time_ms':b['time']+step,'confirmed':True}]for f,b in zip(frames,bars)]
        r=program.run_external(bars,frames,chart_symbol='V-BTC-PERP',chart_timeframe=str(minutes),execution_passes=passes)
        counts.append(len(r['intents']))
    return counts

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--report',type=Path,required=True,help='qualify_original_strategies.py report.json')
    p.add_argument('--manifest',type=Path,default=ROOT/'docs/strategy-library-originals.json')
    a=p.parse_args()
    report=json.loads(a.report.read_text(encoding='utf-8'))
    out={'qualification':'offline causal execution with orders, then external broker runtime preflight; actual fills come from market runs',
        'runtime':getattr(pine_compat,'__version__','unknown'),'offline_report_cases':len(report['cases']),'qualified':[],'excluded':[]}
    seen=set()
    for row in report['qualified']:
        if row['installed_sha256'] in seen:continue
        seen.add(row['installed_sha256'])
        path=Path(row['path'])
        relative=path.relative_to(ROOT) if path.is_absolute() else path
        entry={'name':row['name'],'family_hint':row['family_hint'],'source_path':relative.as_posix(),
            'source_sha256':row['source_sha256'],'installed_sha256':row['installed_sha256'],'inputs':row.get('inputs',[]),'total_orders':row['total_orders']}
        try:
            raw=(ROOT/relative).read_bytes()
            if hashlib.sha256(raw).hexdigest()!=row['source_sha256']:raise ValueError('library source changed since qualification')
            source=extract_source(raw);data=source.encode('utf-8')
            if hashlib.sha256(data).hexdigest()!=row['installed_sha256']:raise ValueError('extraction differs from qualification')
            entry['intent_counts']={};failures={}
            for minutes in TIMEFRAMES:
                try:
                    counts=preflight(source,minutes)
                    if sum(counts):entry['intent_counts'][str(minutes)]=counts
                    else:failures[str(minutes)]='no order intents'
                except Exception as exc:failures[str(minutes)]=str(exc)[:200]
            if not entry['intent_counts']:raise ValueError(json.dumps(failures))
            entry['timeframes_minutes']=[int(m) for m in entry['intent_counts']]
            if failures:entry['timeframe_failures']=failures
            entry['has_short']=bool(re.search(r'\bstrategy\.short\b',source))
            script=f'ecosystem/library/{row["installed_sha256"][:16]}.pine';entry['installed_script']=script
            destination=PACKAGE/'scripts'/script;destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(data)
            out['qualified'].append(entry)
        except Exception as exc:
            out['excluded'].append({'name':row['name'],'source_sha256':row['source_sha256'],'error':str(exc)[:300]})
    a.manifest.write_text(json.dumps(out,ensure_ascii=False,indent=1)+'\n',encoding='utf-8')
    print(json.dumps({'installed':len(out['qualified']),'excluded':len(out['excluded']),'manifest':str(a.manifest)}))

if __name__=='__main__':main()
