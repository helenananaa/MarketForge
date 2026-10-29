"""Test removal of derivative-only reservation bias after the observed residual wave."""
from __future__ import annotations
import argparse,copy,json,shutil
from datetime import datetime
from pathlib import Path
from analyze_market_mechanisms import ROOT,sha,write
from research_market_mechanisms import ensure_runtime_inputs

ROOT_STUDY=ROOT/'.local/strategy-research-causal-20261025'
DEFAULT_OUT=ROOT_STUDY/'inventory-level-study'


def level_recipe(control):
    candidate=copy.deepcopy(control)
    for a in candidate['agents']:
        p=a['Plugin']
        if p['plugin_id']=='AdaptiveLiquidityMaker':
            assert p['config']['reservation_innovations'] is True and p['config']['market_depth_flow'] is True
            del p['config']['reservation_innovations']
        if p['plugin_id']=='BasisLinkTrader':assert p['plugin_version']=='1.2.0'
    restored=copy.deepcopy(candidate)
    for a in restored['agents']:
        if a['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':a['Plugin']['config']['reservation_innovations']=True
    assert restored==control
    return candidate


def design(out):
    source=ROOT_STUDY/'full-study';out.mkdir(parents=True,exist_ok=True)
    assert not (out/'design.json').exists(),'Preserve the frozen design'
    plan=json.loads((source/'plan.json').read_text(encoding='utf-8'));rows=[]
    for seed in (41,7,19,73):
        row=next(r for r in plan['runs'] if r['seed']==seed and r['profile']=='depth')
        control=json.loads(Path(row['recipe']).read_text(encoding='utf-8'));path=out/f'native-s{seed}-level.recipe.json';write(path,level_recipe(control))
        rows.append(dict(seed=seed,name=f'native-s{seed}-level',suite='native',profile='level',bots=300,steps=43200,seconds=1,warmup_ms=7200000,recipe=str(path.resolve()),recipe_sha256=sha(path),baseline_name=row['name'],baseline_recipe_sha256=row['recipe_sha256']))
    shutil.copyfile(source/'candidate.exe',out/'candidate.exe')
    write(out/'design.json',dict(frozen_at=datetime.now().astimezone().isoformat(),source_plan_sha256=sha(source/'plan.json'),binary_sha256=sha(out/'candidate.exe'),runs=rows,
        cause='In the post hoc remaining wave, an underweight maker moving toward its inventory target acquires a negative private reservation innovation. Test restoring the inventory level instead of the prior differenced-bias hypothesis, while retaining executable touch hedges and public-depth flow.',
        treatment='Only remove reservation_innovations=true; the existing default false restores level-based reservation. All numeric inputs, assets, positions, other mechanisms, role allocations and exogenous tasks unchanged. Same frozen executable; no production-source change.',
        scope='New exploratory hypothesis after the initial seed41 depth result; design and all four recipe hashes frozen before pilot or these four full runs. Previously examined seeds, no holdout. The isolated quote-only peer-drift defect of level pricing is already known and must not be hidden if it returns in the full population.',
        pilot=dict(seed=41,steps=11280,window='10620000:11280000',scope='Same early residual-wave window selected on the preceding frozen depth result, not reselected after this treatment. Pilot does not determine which seeds get retained.')))
    print('DESIGN FROZEN: four level-policy recipes, no numerical changes',flush=True)


def plan(out):
    source=ROOT_STUDY/'full-study';design=json.loads((out/'design.json').read_text(encoding='utf-8'))
    assert not (out/'plan.json').exists()
    previous=json.loads((source/'causal-comparison.json').read_text(encoding='utf-8'))
    assert previous['complete'] and previous['completed_runs']==9
    assert sha(source/'plan.json')==design['source_plan_sha256'] and sha(out/'candidate.exe')==design['binary_sha256']
    controls=[];runs=[]
    for row in design['runs']:
        assert sha(Path(row['recipe']))==row['recipe_sha256']
        name=row['baseline_name'];run=json.loads((source/(name+'.run.json')).read_text(encoding='utf-8'))
        receipt=source/(name+'.receipt.json');recipe=source/(name+'.recipe.json');metric=source/(name+'.metrics.json')
        assert run['complete'] and run['inputs']['binary_sha256']==design['binary_sha256'] and sha(receipt)==run['receipt_sha256'] and sha(recipe)==row['baseline_recipe_sha256']
        controls.append(dict(seed=row['seed'],name=name,recipe=str(recipe),recipe_sha256=sha(recipe),receipt=str(receipt),receipt_sha256=sha(receipt),metrics=str(metric),metrics_sha256=sha(metric)))
        runs.append({k:v for k,v in row.items() if not k.startswith('baseline_')})
    ensure_runtime_inputs(out,runs)
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),design_sha256=sha(out/'design.json'),controls=controls,runs=runs,
        scope='Four frozen seeds; level pricing versus the depth candidate, both produced by the identical frozen executable. No duplicate control rerun is required for a binary that was not rebuilt; each archived control receipt and input identity is verified. All numeric inputs unchanged. Evaluate return/midpoint reversals, candle shape, capital, liquidity and linkage, not volatility alone.'))
    print('FULL PLAN FROZEN after verified source baseline completion',flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['design','plan']);p.add_argument('--output',type=Path,default=DEFAULT_OUT)
    a=p.parse_args();(design if a.action=='design' else plan)(a.output.resolve())
