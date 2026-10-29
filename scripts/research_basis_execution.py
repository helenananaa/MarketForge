"""Freeze a mechanism-only follow-up to the failed maker quote experiment."""
from __future__ import annotations
import argparse, copy, json, subprocess
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT, sha, write, ensure_runtime_inputs

DEFAULT_SOURCE=ROOT/'.local/strategy-research-mechanisms-20261024'
DEFAULT_OUT=ROOT/'.local/strategy-research-basis-20261024'

def executable_recipe(control):
    candidate=copy.deepcopy(control)
    changed=0
    for a in candidate['agents']:
        p=a['Plugin']
        if p['plugin_id']=='BasisLinkTrader':
            assert p['plugin_version']=='1.0.0'
            p['plugin_version']='1.1.0';changed+=1
    assert changed>0
    return candidate

def plan(source,out):
    out.mkdir(parents=True,exist_ok=True)
    if (out/'plan.json').exists():raise RuntimeError('Plan is frozen; use a fresh directory')
    previous=json.loads((source/'comparison.json').read_text(encoding='utf-8'))
    assert previous['terminal']
    controls=[];runs=[]
    for seed in (41,7,19,73):
        row=next(r for r in previous['runs'] if r['seed']==seed and r['suite']=='native' and r['profile']=='model_ack')
        recipe_path=Path(row['recipe']);receipt=source/(row['name']+'.receipt.json')
        assert sha(recipe_path)==row['recipe_sha256'] and sha(receipt)==row['receipt_sha256']
        recipe=json.loads(recipe_path.read_text(encoding='utf-8'))
        controls.append(dict(name=row['name'],seed=seed,recipe=str(recipe_path),recipe_sha256=sha(recipe_path),
                             receipt=str(receipt),receipt_sha256=sha(receipt),metrics=str(source/(row['name']+'.metrics.json')),
                             metrics_sha256=sha(source/(row['name']+'.metrics.json'))))
        profiles=['control','executable'] if seed==41 else ['executable']
        for profile in profiles:
            name=f'native-s{seed}-{profile}'
            candidate=recipe if profile=='control' else executable_recipe(recipe)
            path=out/(name+'.recipe.json');write(path,candidate)
            runs.append(dict(name=name,suite='native',seed=seed,profile=profile,bots=row['bots'],
                             steps=row['steps'],seconds=row['seconds'],warmup_ms=row['warmup_ms'],
                             recipe=str(path),recipe_sha256=sha(path)))
    write(out/'plan.json',dict(purpose='Executable basis policy, no fitted numerical parameters',
        frozen_at=datetime.now().astimezone().isoformat(),
        head=subprocess.check_output(['git','-c','safe.directory='+ROOT.as_posix(),'rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        previous_plan_sha256=sha(source/'plan.json'),controls=controls,runs=runs,
        fixed=['all scenarios and finite assets','all numerical strategy parameters','maker model_ack policy',
               'seeds and parent task schedules','12h duration and [2h,12h) evaluation'],
        treatment='Only BasisLinkTrader plugin_version 1.0.0 -> 1.1.0: executable entry/exit quotes, observed two-book depth and hedge budget, actual hedge synchronization.',
        evaluation=['all four seeds; no dropping failures','trade and quote-mid volatility, ACF, tails',
                    'quote availability/spread/depth and basis account flow','full-window candles and no price-path smoothing'],
        compatibility='Full 12h seed-41 control rerun must match the prior authoritative receipt byte for byte.',
        scope='Seed 41 is an in-sample diagnostic of the previously observed spike; all four seeds were already examined in the maker study. No claim of independent unseen-seed qualification.'))
    ensure_runtime_inputs(out,runs)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--source',type=Path,default=DEFAULT_SOURCE)
    p.add_argument('--output',type=Path,default=DEFAULT_OUT)
    a=p.parse_args();plan(a.source.resolve(),a.output.resolve())
