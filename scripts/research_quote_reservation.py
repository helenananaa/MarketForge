"""Freeze a reservation-feedback ablation; preserve all maker numeric inputs."""
from __future__ import annotations
import argparse,copy,json,subprocess
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write,ensure_runtime_inputs
from research_basis_execution import DEFAULT_SOURCE

DEFAULT_OUT=ROOT/'.local/strategy-research-reservation-20261024'

def innovation_recipe(control):
    candidate=copy.deepcopy(control);changed=0
    for a in candidate['agents']:
        p=a['Plugin']
        if p['plugin_id']=='AdaptiveLiquidityMaker':
            assert p['plugin_version']=='1.1.0'
            p['config']['reservation_innovations']=True;changed+=1
    assert changed>0
    return candidate

def plan(source,out):
    out.mkdir(parents=True,exist_ok=True)
    if (out/'plan.json').exists():raise RuntimeError('Plan is frozen; use a fresh directory')
    previous=json.loads((source/'comparison.json').read_text(encoding='utf-8'));assert previous['terminal']
    controls=[];runs=[]
    for seed in (41,7,19,73):
        row=next(r for r in previous['runs'] if r['seed']==seed and r['suite']=='native' and r['profile']=='model_ack')
        rp=Path(row['recipe']);receipt=source/(row['name']+'.receipt.json')
        assert sha(rp)==row['recipe_sha256'] and sha(receipt)==row['receipt_sha256']
        recipe=json.loads(rp.read_text(encoding='utf-8'))
        controls.append(dict(name=row['name'],seed=seed,recipe=str(rp),recipe_sha256=sha(rp),receipt=str(receipt),
            receipt_sha256=sha(receipt),metrics=str(source/(row['name']+'.metrics.json')),metrics_sha256=sha(source/(row['name']+'.metrics.json'))))
        for profile in (['control','reservation'] if seed==41 else ['reservation']):
            name=f'native-s{seed}-{profile}';candidate=recipe if profile=='control' else innovation_recipe(recipe)
            path=out/(name+'.recipe.json');write(path,candidate)
            runs.append(dict(name=name,suite='native',seed=seed,profile=profile,bots=row['bots'],steps=row['steps'],
                seconds=row['seconds'],warmup_ms=row['warmup_ms'],recipe=str(path),recipe_sha256=sha(path)))
    write(out/'plan.json',dict(purpose='Remove repeated reservation bias from quote feedback; no fitted numerical parameter',
        frozen_at=datetime.now().astimezone().isoformat(),head=subprocess.check_output(['git','-c','safe.directory='+ROOT.as_posix(),'rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        previous_plan_sha256=sha(source/'plan.json'),controls=controls,runs=runs,
        candidate_profile='reservation',candidate_label='Reservation innovations',control_label='Repeated reservation level',
        study_title='Maker reservation feedback',figure_prefix='reservation',
        fixed=['all numerical parameters','all finite assets and parent tasks','all other roles including original midpoint basis policy','12h, [2h,12h), seeds 41/7/19/73'],
        treatment='Only reservation_innovations=true for AdaptiveLiquidityMaker 1.1.0; persist confirmed bounded reservation bias and apply its change, keeping fresh flow reaction.',
        compatibility='Full 12h seed-41 control must reproduce the prior authoritative receipt byte for byte.',
        evaluation=['return and quote-mid statistics','spread/availability/depth and role activity','fixed full-window OHLCV and candle morphology'],
        scope='Independent ablation from executable basis, not their combination. Same previously examined seeds; not out-of-sample qualification.'))
    ensure_runtime_inputs(out,runs)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--source',type=Path,default=DEFAULT_SOURCE);p.add_argument('--output',type=Path,default=DEFAULT_OUT)
    a=p.parse_args();plan(a.source.resolve(),a.output.resolve())
