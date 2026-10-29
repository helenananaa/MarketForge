"""Freeze finite-capital recipes and run native matching without candle targets.

The JSON plan is written before the first process starts. Existing results are
never overwritten. Each receipt retains the exact frozen binary/recipe hashes.
The default restores the pre-experiment population. The rejected combined
economic candidate remains available explicitly for reproducibility only.
"""
from __future__ import annotations
import argparse, concurrent.futures, hashlib, json, os, shutil, subprocess, time
from pathlib import Path
from strategy_ecosystem import population

ROOT=Path(__file__).resolve().parents[1]
def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def write(path,value):path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output-dir',type=Path,required=True);p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--seeds',type=int,nargs='+',default=[7,19,41,73]);p.add_argument('--seconds',type=int,default=14400)
    p.add_argument('--session-seconds',type=int,default=14400);p.add_argument('--jobs',type=int,default=2)
    p.add_argument('--profile',choices=['legacy-stop','rejected-economic'],default='legacy-stop',
        help='legacy-stop restores prior population; rejected-economic reproduces the failed candidate')
    p.add_argument('--maker-depth-scale',type=float);p.add_argument('--perp-maker-depth-scale',type=float)
    p.add_argument('--probes',action='store_true');a=p.parse_args()
    if a.seconds<1 or a.seconds>100000 or a.seconds>a.session_seconds or not 1<=a.jobs<=4:raise ValueError('invalid run bounds')
    out=a.output_dir.resolve();out.mkdir(parents=True,exist_ok=True)
    if (out/'frozen-plan.json').exists():raise ValueError('output already contains a frozen run; choose a new directory')
    binary=out/'runner.exe';shutil.copy2(a.binary.resolve(),binary)
    plan={'purpose':'mechanism regression comparison, no price-shape acceptance',
          'profile':a.profile,'population_status':'rejected' if a.profile=='rejected-economic' else 'restored_control_not_realism_qualified',
          'stop_semantics':'determined by the frozen binary; profile selects population only',
          'seeds':a.seeds,'duration_seconds':a.seconds,'session_seconds':a.session_seconds,'clock_step_seconds':1,
          'binary':str(binary),'binary_sha256':sha(binary),'cases':[],
          'source_hashes':{str(f.relative_to(ROOT)):sha(f) for f in [ROOT/'scripts/strategy_ecosystem.py',Path(__file__),
              ROOT/'exchange-core/src/adaptive_liquidity.rs',ROOT/'exchange-core/src/style_switching.rs',ROOT/'exchange-core/src/basis_link.rs',ROOT/'exchange-core/examples/strategy_market.rs',
              ROOT/'docs/validation/2026-10-16-economic-market-plan.json',ROOT/'Cargo.lock']}}
    patch=subprocess.check_output(['git','-c','safe.directory='+str(ROOT),'diff','--binary'],cwd=ROOT)
    (out/'source.patch').write_bytes(patch);plan['source_patch_sha256']=sha(out/'source.patch')
    for seed in a.seeds:
        name=f'seed-{seed}';recipe,roster=population(room='economic-market-s'+str(seed),seed=seed,count=300,
            ecology=True,noise_fraction=.1,value_model='endogenous',value_fraction=.2,flow_mix='rebalanced',
            switching_fraction=.06,price_scale=10,fast_flow=True,economic_flow=a.profile=='rejected-economic',session_ms=a.session_seconds*1000,
            periods=[10000,30000,60000,300000],maker_depth_scale=a.maker_depth_scale,perp_maker_depth_scale=a.perp_maker_depth_scale)
        # An explicit cost contract, not an assumed fee for a different venue.
        fees={m[k]['instrument']['instrument_id']:m[k]['clearing']['maker_fee_ppm']
              for m in [recipe['scenario']['market'],*recipe['scenario']['extra_markets']] for k in m}
        for bot in recipe['agents']:
            c=bot['Plugin']
            if c['plugin_id']=='AdaptiveLiquidityMaker' and c['plugin_version']=='1.2.0':
                assert c['config']['maker_fee_ppm']==fees[c['participant']['instrument_id']]
        write(out/(name+'.recipe.json'),recipe);write(out/(name+'.roster.json'),roster)
        plan['cases'].append({'name':name,'seed':seed,'recipe_sha256':sha(out/(name+'.recipe.json')),
            'roster_sha256':sha(out/(name+'.roster.json')),'cash_pool':roster['total_cash_pool'],'roles':roster['role_counts'],
            'maker_depth_scale':roster['maker_depth_scale'],'perp_maker_depth_scale':roster['perp_maker_depth_scale']})
    write(out/'frozen-plan.json',plan)
    def run(case):
        name=case['name'];command=[str(binary),str(out/(name+'.recipe.json')),str(a.seconds),'1']
        env=dict(os.environ)
        for key in ['MF_RESEARCH_CHECKPOINT','MF_RESEARCH_DEADLINE_UNIX','MF_RESEARCH_STOP_FILE']:env.pop(key,None)
        if a.probes and case['seed']==7:
            command.append(str(out/'impact-seed-7'));env['MF_RESEARCH_PROBE_QTYS']='0,10,100';env['MF_RESEARCH_PROBE_SECONDS']='180'
        started=time.monotonic()
        with (out/(name+'.receipt.json')).open('wb') as receipt,(out/(name+'.stderr')).open('wb') as log:
            result=subprocess.run(command,cwd=ROOT,env=env,stdout=receipt,stderr=log)
        row={**case,'exit_code':result.returncode,'wall_seconds':time.monotonic()-started,
            'receipt_sha256':sha(out/(name+'.receipt.json')),'binary_sha256':sha(binary)}
        if result.returncode==0:
            receipt=json.loads((out/(name+'.receipt.json')).read_text())
            row.update(simulation_time_ms=receipt['simulation_time_ms'],trades=receipt['trades'],
                economic_quote_checks=receipt.get('economic_quote_checks',0),forks_complete=receipt.get('forks_complete'))
        else:row['error_tail']=(out/(name+'.stderr')).read_text(errors='replace')[-2500:]
        write(out/(name+'.run.json'),row);return row
    results=[]
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as executor:
        futures=[executor.submit(run,c) for c in plan['cases']]
        for future in concurrent.futures.as_completed(futures):
            row=future.result();results.append(row);write(out/'runs.json',results);print(json.dumps(row),flush=True)
    return int(any(x['exit_code'] for x in results))
if __name__=='__main__':raise SystemExit(main())
