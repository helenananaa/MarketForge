"""Qualify unchanged sources in the installed external runtime before matching.

This preflight is not an actual-fill qualification; the mixed run supplies that.
"""
import importlib.util
import json
import sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
PACKAGE=ROOT/'bot-plugins/pine-strategy'
sys.path.insert(0,str(PACKAGE/'.runtime/strategy-ecosystem'))
import pine_compat
from research_strategy_compat import bars_for
spec=importlib.util.spec_from_file_location('research_broker',PACKAGE/'broker.py')
broker=importlib.util.module_from_spec(spec);spec.loader.exec_module(broker)

def main():
    manifest=json.loads((ROOT/'docs/strategy-ecosystem-originals.json').read_text(encoding='utf-8'))
    report={'qualification':'external runtime and frozen account frames only; actual fills are separate','cases':[],'qualified_names':[]}
    for original in manifest['qualified']:
        case={'name':original['name'],'source_sha256':original['source_sha256'],'installed_sha256':original['installed_sha256']}
        try:
            source=(PACKAGE/'scripts'/original['installed_script']).read_text(encoding='utf-8')
            program=pine_compat.compile_script(broker.external_pass_source(broker.lower_order_calls(source)))
            counts=[]
            for regime in ['range','trend_reversal','random_walk']:
                bars=bars_for(regime,n=320)
                frames=[{'time':b['time'],'position_size':0.,'position_avg_price':None,'equity':1000000.,'initial_capital':1000000.,'netprofit':0.,'openprofit':0.}for b in bars]
                passes=[[{'account':f,'bar':b,'event_time_ms':b['time']+60000,'confirmed':True}]for f,b in zip(frames,bars)]
                r=program.run_external(bars,frames,chart_symbol='V-BTC-PERP',chart_timeframe='1',execution_passes=passes)
                counts.append(len(r['intents']))
            case.update(status='runtime_preflight_pass',intent_counts=counts)
            report['qualified_names'].append(original['name'])
        except Exception as exc:case.update(status='excluded',error=str(exc))
        report['cases'].append(case)
    path=ROOT/'.local/strategy-ecosystem-20261014/round2/original-runtime-preflight.json'
    path.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(report,ensure_ascii=True,indent=2))
if __name__=='__main__':main()
