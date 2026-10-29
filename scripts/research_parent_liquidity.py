"""Freeze unchanged business targets and a single liquidity-aware execution policy."""
import argparse
import copy
import json
import shutil
import subprocess
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write,ensure_runtime_inputs,options
from strategy_ecosystem import population

BASE=ROOT/'.local/strategy-research-parent-liquidity-20261025'
PREVIOUS=ROOT/'.local/strategy-research-flow-surprise-20261025'
SEEDS=(41,7,19,73)


def active_main():
    pointer=BASE/'active-main.json'
    return Path(json.loads(pointer.read_text())['directory'])if pointer.exists()else BASE/'main'


def recipe_for(control):
    result=copy.deepcopy(control)
    for agent in result['agents']:
        p=agent['Plugin']
        if p['plugin_id']=='ExecutionTrader':
            assert p['plugin_version']=='1'
            p['plugin_version']='1.1.0';p['config']['liquidity_aware']=True
    reverse=copy.deepcopy(result)
    for agent in reverse['agents']:
        p=agent['Plugin']
        if p['plugin_id']=='ExecutionTrader':
            p['plugin_version']='1';assert p['config'].pop('liquidity_aware') is True
    assert reverse==control
    return result


def generated(seed,mode='sweep'):
    return population(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,
        basis_execution='touch-hedge',reservation_innovations=True,market_depth_flow=True,
        reactive_execution='sliced',style_execution='touch',persistent_index_anchor=True,
        book_slope_flow=True,parent_execution=mode)[0]


def plan(out,rows,binary):
    out.mkdir(exist_ok=False)
    shutil.copyfile(binary,out/'candidate.exe')
    unix='/mnt/e/'+(out/'candidate.exe').relative_to('E:/').as_posix()
    subprocess.run(['wsl','--exec','chmod','755',unix],check=True)
    prepared=[]
    for seed,label,name,recipe in rows:
        path=out/(name+'.recipe.json');write(path,recipe)
        prepared.append(dict(name=name,seed=seed,profile=label,bots=300,steps=14400,seconds=1,
            warmup_ms=7200000,recipe=str(path),recipe_sha256=sha(path)))
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=prepared,
        binary_sha256=sha(out/'candidate.exe'),minimum_linux_available_gib=8,minimum_host_commit_gib=6,
        task_cgroup=dict(MemoryHigh='2G',MemoryMax='4G',MemorySwapMax='2G',User='helenanana'),
        scope='Single native at a time; offline analysis runs only after native exits.6GiB host start gate is a new pre-outcome resource policy based on previous9 complete4h runs below0.8GiB native peak;3GiB stop guard unchanged. No global cache reclaim/WSL changes/other process stops. Fixed300bots,1s,4h,primary2–4h. Candidate changes only business-parent child execution and its authoritative ledger. Every economic numeric input and maker/hedge policy frozen.'))
    ensure_runtime_inputs(out,prepared)
    return prepared


def freeze():
    design=BASE/'design';design.mkdir(exist_ok=False)
    native='/home/helenanana/.cache/marketforge-parent-liquidity-20261025/release/examples/strategy_ecosystem_market'
    binary=design/'candidate-linux.elf'
    subprocess.run(['wsl','--exec','cp',native,'/mnt/e/'+binary.relative_to('E:/').as_posix()],check=True)
    controls=[];candidates=[];all_rows=[]
    for seed in SEEDS:
        original=PREVIOUS/'main'/f'native-s{seed}-raw-flow.recipe.json'
        control=json.loads(original.read_text(encoding='utf-8'))
        assert control==generated(seed)
        treatment=recipe_for(control);assert treatment==generated(seed,'touch-budget')
        a=(seed,'legacy',f'native-s{seed}-raw-flow',control)
        b=(seed,'liquidity-parent',f'native-s{seed}-liquidity-parent',treatment)
        controls.append(a);candidates.append(b);all_rows.extend((a,b))
    new=generated(271)
    holdout=[(271,'legacy','native-s271-legacy',new),(271,'liquidity-parent','native-s271-liquidity-parent',recipe_for(new))]
    for seed,label,name,recipe in all_rows+holdout:write(design/(name+'.recipe.json'),recipe)
    write(design/'frozen-design.json',dict(frozen_at=datetime.now().astimezone().isoformat(),
        files=[dict(path=p.name,sha256=sha(p))for p in sorted(design.glob('*.recipe.json'))],
        binary_sha256=sha(binary),seeds=list(SEEDS),optional_independent_seed=271,
        scope='All four pairs and optional271 pair frozen before any new outcome. Four controls are immutable actual previous4h Linux runs, cached and not counted as new work. Require newbinary legacy41 replay byte identity before comparing.271 is only executed if joint four-seed results justify independent qualification; rejection does not require another seed. No more flow_surprise policy.'))
    changed={'exchange-core/src/market_bots.rs','exchange-core/src/market_bots_tests.rs','scripts/strategy_ecosystem.py'}
    for f in json.loads((BASE/'control-inputs.json').read_text())['files']:
        if f['path'] not in changed:assert sha(ROOT/f['path'])==f['sha256'],f['path']
    model=[]
    for name in sorted(changed|{'exchange-core/src/market_bots_execution.rs','exchange-core/src/market_bots_execution_tests.rs'}):
        path=design/'model-source'/name;path.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(ROOT/name,path)
        model.append(dict(path=name,sha256=sha(path)))
    write(design/'model-inputs.json',dict(files=model,binary_sha256=sha(binary),
        mechanism='Existing finite signed parent and TWAP entitlement, clipped to visible external touch and original affordability/slippage bounds. One persisted in-flight child. Only matching authoritative order fills debit demand; partial/rejected children retry on a new clock after acknowledgement. Hard clock-driven residual expiry, delayed acknowledgement correction, no same-clock loops. No new target, capital, horizon, price path or hand-tuned coefficient.'))
    plan(BASE/'compatibility',[(41,'legacy','native-s41-legacy',controls[0][3])],binary)
    plan(BASE/'pilot',[candidates[0]],binary)
    plan(BASE/'main',candidates,binary)
    plan(BASE/'holdout',holdout,binary)
    rows=plan(BASE/'controls',controls,BASE/'control-linux.elf')
    provenance={}
    for row in rows:
        name=row['name'];source=PREVIOUS/'main'
        for suffix in ('.recipe.json','.run.json','.summary.json','.log','.receipt.json'):
            if suffix=='.recipe.json':assert sha(source/(name+suffix))==row['recipe_sha256'];continue
            shutil.copyfile(source/(name+suffix),BASE/'controls'/(name+suffix))
        provenance[name]=dict(directory=str(source),run_sha256=sha(source/(name+'.run.json')),
            receipt_sha256=sha(source/(name+'.receipt.json')),qualification='Actual complete4h cache; no new native run or hours. Original execution archive path preserved.')
    write(BASE/'controls/cache-provenance.json',provenance)


if __name__=='__main__':
    argparse.ArgumentParser(description=__doc__).parse_args();freeze()
