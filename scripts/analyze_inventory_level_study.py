"""Evaluate the separately frozen inventory-level hypothesis, including all seeds."""
import argparse,json
from pathlib import Path
import analyze_causal_mechanisms as engine
from analyze_market_mechanisms import sha,write
from research_inventory_level_study import DEFAULT_OUT,ROOT_STUDY


def analyze(out):
    source=ROOT_STUDY/'full-study'
    engine.PROFILES=('control','level')
    engine.LABELS={'control':'Hedge + public-depth flow','level':'Inventory-level reservation'}
    engine.SHORT_LABELS={'control':'Depth baseline','level':'Inventory level'}
    result=engine.analyze(out,source)
    expected=sha(out/'candidate.exe')
    plan=json.loads((out/'plan.json').read_text(encoding='utf-8'))
    for control in plan['controls']:
        metadata=json.loads((source/(control['name']+'.run.json')).read_text(encoding='utf-8'))
        assert metadata['inputs']['binary_sha256']==expected
    result['compatibility']=dict(kind='identical frozen executable, not a rebuild',all_controls_same_binary=True,binary_sha256=expected,scope='Each control was already executed by this exact binary; full receipts, recipes and metrics are verified against the plan. The earlier seed41 old-reservation rerun also reproduced its archived receipt byte for byte.')
    write(out/'causal-comparison.json',result)
    engine.charts(out,source,result,reference_path=ROOT_STUDY.parent/'strategy-research-reservation-20261024/summary/feedback-summary.json')
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,default=DEFAULT_OUT);analyze(p.parse_args().output.resolve())
