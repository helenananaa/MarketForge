"""Check configurable native formulas against installed Pine, retaining failures."""
from __future__ import annotations
import argparse,hashlib,json,math,subprocess,time
from pathlib import Path
import pine_compat
from research_strategy_compat import bars_for
from technical_strategy_catalog import FORMULAS,defaults,source_for

ROOT=Path(__file__).resolve().parents[1]
SIGNALS=('long_entry','long_exit','short_entry','short_exit')

def variant(family,index):
    p=defaults(family);keys=FORMULAS[family][-1]
    if index:
        for k in keys:
            if k in ('fast','slow','signal','length','exit_length','smooth','displacement'):
                p[k]=max(1,round(p[k]*(.7 if index==1 else 1.4)))
            elif k in ('factor','channel_factor'):p[k]*=.8 if index==1 else 1.2
        if p['fast']>=p['slow']:p['slow']=p['fast']+1
    return {k:p[k] for k in keys}

def native_bars(bars):
    return [dict(schema_version=1,open_time_ms=b['time'],close_time_ms=b['time']+60000,open_tick=b['open'],high_tick=b['high'],low_tick=b['low'],close_tick=b['close'],volume=b['volume'],quote_volume=b['close']*b['volume'],trades=1,is_final=True)for b in bars]

def overrides(source,values):
    inputs=pine_compat.analyze_script(source)['inputs']
    return {v['callSiteId']:values[v['title']]for v in inputs if v.get('title')in values}

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,default=ROOT/'.local/strategy-ecosystem-20261014/formulas')
    parser.add_argument('--families',nargs='+',default=list(FORMULAS))
    parser.add_argument('--variants',type=int,default=3)
    parser.add_argument('--regimes',nargs='+',default=['flat','trend_reversal','range','squeeze_release','jumps','random_walk'])
    args=parser.parse_args();args.output.mkdir(parents=True,exist_ok=True)
    probe=ROOT/'target/release/examples/technical_strategy_probe.exe'
    process=subprocess.Popen([str(probe)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,encoding='utf-8')
    report={'pine_version':pine_compat.__version__,'binary_sha256':hashlib.sha256(probe.read_bytes()).hexdigest(),'source_sha256':{f:hashlib.sha256((ROOT/f).read_bytes()).hexdigest()for f in ['exchange-core/src/technical_signals.rs','exchange-core/src/technical_trader.rs','scripts/technical_strategy_catalog.py']},'cases':[]}
    try:
        for family in args.families:
            source=source_for(family)
            for vi in range(args.variants):
                params=variant(family,vi);input_values=overrides(source,params)
                for regime in args.regimes:
                    bars=bars_for(regime,n=320);start=time.monotonic()
                    row={'family':family,'variant':vi,'parameters':params,'regime':regime}
                    try:
                        output=pine_compat.run_script(source,bars,input_overrides=input_values,chart_symbol='TEST',chart_timeframe='1')
                        errors=[d for d in output.get('diagnostics',[])if d.get('severity')=='error']
                        if errors:raise ValueError(errors)
                        process.stdin.write(json.dumps({'family':family,'parameters':params,'bars':native_bars(bars)})+'\n');process.stdin.flush()
                        native=json.loads(process.stdout.readline())
                        if 'error'in native:raise ValueError(native['error'])
                        plots={p['title']:p['values']for p in output['plots']}
                        mismatches=[]
                        for i,frame in enumerate(native['frames']):
                            for name in SIGNALS:
                                value=plots[name][i]
                                expected=value is not None and math.isfinite(value) and value>.5
                                if frame[name]!=expected:mismatches.append({'bar':i,'signal':name,'native':frame[name],'pine':expected,'native_plots':frame['plots']})
                        row.update(status='pass'if not mismatches else'failed',mismatch_count=len(mismatches),first_mismatches=mismatches[:6],native_ready_bars=sum(x['ready']for x in native['frames']))
                        # Pine signal prefixes must remain identical when future bars are removed.
                        prefix=pine_compat.run_script(source,bars[:157],input_overrides=input_values,chart_symbol='TEST',chart_timeframe='1')
                        assert [p['values']for p in prefix['plots']]==[p['values'][:157]for p in output['plots']], 'future-dependent Pine signals'
                        row['prefix_check']=True
                    except Exception as exc:row.update(status='error',error=str(exc))
                    row['seconds']=round(time.monotonic()-start,4);report['cases'].append(row)
            rows=[r for r in report['cases']if r['family']==family]
            print(json.dumps({'family':family,'cases':len(rows),'failed':sum(r['status']!='pass'for r in rows),'mismatches':sum(r.get('mismatch_count',0)for r in rows)}),flush=True)
            (args.output/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    finally:
        process.stdin.close();process.wait(timeout=10)
    print(args.output/'report.json')
    return int(any(r['status']!='pass'for r in report['cases']))

if __name__=='__main__':raise SystemExit(main())
