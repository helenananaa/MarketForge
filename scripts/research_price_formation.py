"""Frozen mechanism studies following the wick repair; serial owned processes only."""
import argparse, copy, json, os, shutil, subprocess, sys, time
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT, sha, write, ensure_runtime_inputs
from research_receipts import finish_result
from research_resources import windows_memory, stop_reason, reclaim_guest_file_cache

BASE = ROOT/'.local/strategy-research-price-formation-20261025'
PREVIOUS = ROOT/'.local/strategy-research-wicks-20261025/full-dual-retry'
PROFILES={'control':(), 'anchor':('persistent_index_anchor',), 'slope':('book_slope_flow',),
          'anchor-slope':('persistent_index_anchor','book_slope_flow')}


def recipe_for(control,profile):
    recipe=copy.deepcopy(control)
    for a in recipe['agents']:
        p=a['Plugin']
        if p['plugin_id']=='AdaptiveLiquidityMaker':
            assert p['plugin_version']=='1.1.0'
            for flag in PROFILES[profile]:p['config'][flag]=True
    reverse=copy.deepcopy(recipe)
    for a in reverse['agents']:
        if a['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
            for flag in PROFILES[profile]:assert a['Plugin']['config'].pop(flag)
    assert reverse==control
    return recipe


def pilot_plan():
    out=BASE/'pilot';out.mkdir(parents=True,exist_ok=False)
    shutil.copyfile(ROOT/'target/release/examples/strategy_ecosystem_market.exe',out/'candidate.exe')
    paths=['Cargo.lock','exchange-core/src/adaptive_liquidity.rs','exchange-core/src/reactive_flow.rs',
        'exchange-core/src/reactive_flow_tests.rs','exchange-core/src/style_switching.rs','exchange-core/src/style_switching_tests.rs',
        'exchange-core/src/basis_link.rs','exchange-core/examples/strategy_market.rs','exchange-core/examples/support/research_trace.rs',
        'exchange-server/examples/strategy_ecosystem_market.rs','scripts/strategy_ecosystem.py',
        'scripts/research_price_formation.py','scripts/analyze_price_formation_trace.py',
        'exchange-core/examples/support/research_output.rs','scripts/research_receipts.py',
        'scripts/research_resources.py','scripts/run_price_formation_linux.py',
        'exchange-core/src/room/research_history.rs','exchange-core/src/room.rs',
        'exchange-core/src/trading.rs','exchange-core/src/actor.rs','exchange-core/src/log.rs',
        'exchange-core/src/simulation.rs','exchange-core/examples/support/research_checkpoint.rs']
    paths+=['scripts/research_json.py','scripts/research_numeric.py','scripts/analyze_market_mechanisms.py']
    files=[]
    for name in paths:
        src=ROOT/name;dst=out/'compiled-source'/name;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,dst)
        files.append(dict(path=name,sha256=sha(src)))
    write(out/'compiled-inputs.json',dict(files=files,binary_sha256=sha(out/'candidate.exe')))
    runs=[];designs=[]
    for seed in (41,7,19,73):
        control=json.loads((PREVIOUS/f'native-s{seed}-dual-execution.recipe.json').read_text(encoding='utf-8'))
        for profile in PROFILES:
            path=out/f'native-s{seed}-{profile}.recipe.json';write(path,recipe_for(control,profile))
            r=dict(name=path.name.removesuffix('.recipe.json'),seed=seed,profile=profile,bots=300,
                steps=43200,seconds=1,warmup_ms=7200000,recipe=str(path),recipe_sha256=sha(path))
            designs.append(r)
            if seed==41:runs.append({**r,'steps':14400})
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=runs,all_seed_full_designs=designs,profiles=PROFILES,
        binary_sha256=sha(out/'candidate.exe'),minimum_launch_commit_gib=12,
        control_receipt_sha256=json.loads((ROOT/'.local/strategy-research-wicks-20261025/pilot-style/native-s41-dual-execution.run.json').read_text(encoding='utf-8'))['receipt_sha256'],
        control='Previous wick-repair dual-execution, not the older sweeping depth control. New binary4h control must byte-match frozen dual4h; previous full12h seed7 gets separately traced and byte-checked.',
        fixed='Only two explicit maker boolean mechanism flags differ. All numerical model/size/flow/risk/spread/index/TTL/fees/account/capacity inputs, execution lifecycle, population, seeds and1s clock remain equal. Actual paths endogenous.',
        primary='Pilot seed41,4h,primary[2h,4h); all profiles retained. Selected candidate completes12h on41,7,19,73,primary[2h,12h); all16 full recipes frozen now. Same previously examined seeds, not independent holdout.',
        selection='Prefer the narrow persistent-index fix if it restores linkage without a joint market regression. Supply-curve alternative must be judged jointly on wick shape, return/absolute-return correlation, volatility, basis, tails, volume and quote availability. Higher volatility alone is insufficient; no numerical retuning. Retain failed profiles and explain any adaptive follow-up.',
        mechanism='Anchor separates live independent index attraction from differenced local inventory/microprice reservation bias. Slope replaces half-spread-scaled flow with the inverse of observed external cumulative supply, linearly interpolated from(0,mid) through displayed quantity/price knots and not extrapolated beyond visible liquidity or unchanged reach; original flow_skew coefficient retained. Slope is an experimental response hypothesis, not a required mathematical correction or hypothetical fill.',
        resource='Serial owned child, minimum12GiB commit before launch, kill only this child below3GiB or3h timeout. Wall scheduling only; immutable market inputs.'))
    ensure_runtime_inputs(out,runs)


def full_plan(profile):
    pilot=BASE/'pilot';p=json.loads((pilot/'plan.json').read_text(encoding='utf-8'));assert profile!='control'
    assert all(json.loads((pilot/(r['name']+'.run.json')).read_text(encoding='utf-8'))['complete']for r in p['runs'])
    out=BASE/('full-'+profile);out.mkdir(exist_ok=False)
    shutil.copyfile(pilot/'candidate.exe',out/'candidate.exe');shutil.copytree(pilot/'compiled-source',out/'compiled-source')
    shutil.copyfile(pilot/'compiled-inputs.json',out/'compiled-inputs.json');shutil.copyfile(pilot/'comparison.json',out/'selection-pilot-snapshot.json')
    runs=[]
    for row in p['all_seed_full_designs']:
        if row['profile']!=profile:continue
        dest=out/Path(row['recipe']).name;shutil.copyfile(row['recipe'],dest);assert sha(dest)==row['recipe_sha256']
        runs.append({**row,'recipe':str(dest)})
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=runs,binary_sha256=p['binary_sha256'],
        minimum_launch_commit_gib=12,source_plan_sha256=sha(pilot/'plan.json'),selection_sha256=sha(out/'selection-pilot-snapshot.json'),
        primary=p['primary'],fixed=p['fixed'],mechanism=p['mechanism'],resource=p['resource']))
    ensure_runtime_inputs(out,runs)


def free_commit():
    return windows_memory()['free_commit_kib']


def trace_plan():
    out=BASE/'diagnostic-control';out.mkdir(parents=True,exist_ok=False)
    shutil.copyfile(PREVIOUS/'candidate.exe',out/'candidate.exe')
    src=PREVIOUS/'native-s7-dual-execution.recipe.json'
    dest=out/src.name;shutil.copyfile(src,dest)
    row=dict(name='native-s7-dual-execution',seed=7,profile='dual-execution',bots=300,
        steps=43200,seconds=1,warmup_ms=7200000,recipe=str(dest),recipe_sha256=sha(dest),
        trace_windows='7200000:7500000,21600000:21900000,36000000:36300000')
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=[row],
        binary_sha256=sha(out/'candidate.exe'),reference_receipt_sha256=json.loads((PREVIOUS/(row['name']+'.run.json')).read_text(encoding='utf-8'))['receipt_sha256'],
        scope='Observation only. Same seed7,300bots,12h,1s and recipe bytes as the last completed wick repair. Preselected 5min trace windows at hours2,6,10. Native receipt must be byte-identical. No numeric tuning.'))
    ensure_runtime_inputs(out,[row])


def run(out):
    p=json.loads((out/'plan.json').read_text(encoding='utf-8'));binary=out/'candidate.exe'
    assert sha(binary)==p['binary_sha256'];ensure_runtime_inputs(out,p['runs'])
    reclaim_left=min(4,max(0,int(p.get('file_cache_reclaim_budget_gib',4))))
    for row in p['runs']:
        done=out/(row['name']+'.run.json');assert not done.exists(),'Preserve attempts; choose a fresh plan directory'
        while True:
            free=free_commit()
            with (out/'resource-gates.jsonl').open('a',encoding='utf-8') as f:f.write(json.dumps(dict(time=time.time(),free_commit_kib=free))+'\n')
            if free>=p.get('minimum_launch_commit_gib',20)*1024*1024:break
            if reclaim_left:
                reclaimed=reclaim_guest_file_cache();reclaim_left-=1
                with (out/'resource-gates.jsonl').open('a',encoding='utf-8')as f:f.write(json.dumps(dict(time=time.time(),cache_reclaim=reclaimed,reclaim_budget_remaining_gib=reclaim_left))+'\n')
                time.sleep(20 if reclaimed.get('reclaimed')else 1)
                continue
            time.sleep(30)
        assert sha(Path(row['recipe']))==row['recipe_sha256']
        env=dict(os.environ,PYTHONUTF8='1',RESEARCH_PLUGIN_WAKES='events',MARKETFORGE_PINE_IN_PROCESS='1',MARKETFORGE_PINE_REPLAY_STATE='1',MARKETFORGE_BOT_WORKERS='8',RAYON_NUM_THREADS='8')
        env['PATH']=str(ROOT.parent/'MarketForge/.venv/Scripts')+os.pathsep+env['PATH']
        for key in ('MF_RESEARCH_CHECKPOINT','MF_RESEARCH_STOP_FILE','MF_RESEARCH_DEADLINE_UNIX','RESEARCH_PLUGIN_POLL_MS','RESEARCH_AUDIT','RESEARCH_PROFILE','RESEARCH_SCRIPT_PROFILE','MF_RESEARCH_TRACE_PATH','MF_RESEARCH_TRACE_WINDOWS'):env.pop(key,None)
        if row.get('trace_windows'):
            env.update(MF_RESEARCH_TRACE_PATH=str(out/(row['name']+'.trace.jsonl')),MF_RESEARCH_TRACE_WINDOWS=row['trace_windows'])
        receipt=out/(row['name']+'.receipt.json');start=time.monotonic()
        summary=out/(row['name']+'.summary.json')
        assert not summary.exists(),'Preserve completed summaries; choose a fresh attempt directory'
        env.update(MF_RESEARCH_SUMMARY_PATH=str(summary),MF_RESEARCH_SPOOL_DIR=str(out/(row['name']+'.spool')),MF_RESEARCH_OUTPUT='disk')
        result=dict(name=row['name'],complete=False,inputs=dict(recipe_sha256=row['recipe_sha256'],binary_sha256=p['binary_sha256'],steps=row['steps'],seconds=row['seconds']))
        print('START '+row['name'],flush=True)
        with receipt.open('w',encoding='utf-8') as stdout,(out/(row['name']+'.log')).open('w',encoding='utf-8') as stderr:
            child=subprocess.Popen([str(binary),row['recipe'],str(row['steps']),str(row['seconds'])],cwd=ROOT,env=env,stdout=stdout,stderr=stderr)
            result.update(pid=child.pid,command=[str(binary),row['recipe'],str(row['steps']),str(row['seconds'])])
            while child.poll() is None:
                sample=windows_memory(child.pid)
                with (out/'resource-samples.jsonl').open('a',encoding='utf-8') as f:f.write(json.dumps(dict(time=time.time(),pid=child.pid,**sample))+'\n')
                reason=stop_reason(sample,time.monotonic()-start,p.get('maximum_native_private_gib',6))
                if reason:
                    child.kill();result.update(failure='Owned native stopped: '+reason,stop_reason=reason);break
                time.sleep(2)
            result['exit_code']=child.wait()
        result['wall_seconds']=time.monotonic()-start
        finish_result(result,receipt,row,summary)
        write(done,result);print(json.dumps(result),flush=True);assert result['complete']
        if p.get('reference_receipt_sha256'):
            assert result['receipt_sha256']==p['reference_receipt_sha256']
            write(out/'observation-identity.json',dict(byte_identical=True,receipt_sha256=result['receipt_sha256']))
        if row['profile']=='control' and p.get('control_receipt_sha256'):
            assert result['receipt_sha256']==p['control_receipt_sha256']
            write(out/'control-compatibility.json',dict(byte_identical=True,receipt_sha256=result['receipt_sha256'],steps=row['steps']))
    ensure_runtime_inputs(out,p['runs'])


if __name__=='__main__':
    a=argparse.ArgumentParser(description=__doc__);a.add_argument('action',choices=['trace-plan','pilot-plan','full-plan','run']);a.add_argument('--output',type=Path);a.add_argument('--profile',choices=list(PROFILES));args=a.parse_args()
    if args.action=='trace-plan':trace_plan()
    elif args.action=='pilot-plan':pilot_plan()
    elif args.action=='full-plan':full_plan(args.profile)
    else:run(args.output.resolve())
