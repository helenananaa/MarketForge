"""Mixed original Pine, long-horizon, liquidity and intervention experiments.

Existing process adapter and frozen observations; no production performance changes.
"""
from __future__ import annotations
import argparse, hashlib, json, os, subprocess, time
from pathlib import Path
from strategy_ecosystem import population
ROOT=Path(__file__).resolve().parents[1]
OUT=ROOT/'.local/strategy-ecosystem-20261014/round2'
CASES=[('legacy-n5-s7',300,7,.05,'legacy'),('adaptive-n0-s7',300,7,0.,'adaptive'),
       ('adaptive-n5-s7',300,7,.05,'adaptive'),('adaptive-n10-s7',300,7,.10,'adaptive'),
       ('adaptive-n5-s19',300,19,.05,'adaptive'),('adaptive-1000-n5-s7',1000,7,.05,'adaptive')]
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def write(p,r):p.write_text(json.dumps(r,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--cases',nargs='+');p.add_argument('--steps',type=int,default=8640)
    p.add_argument('--clock-seconds',type=int,default=5);p.add_argument('--plan-only',action='store_true');p.add_argument('--forks',action='store_true')
    p.add_argument('--output-dir',type=Path);p.add_argument('--checkpoint',action='store_true')
    p.add_argument('--deadline-unix',type=int);p.add_argument('--skip-complete',action='store_true')
    p.add_argument('--count-override',type=int)
    p.add_argument('--binary',type=Path)
    p.add_argument('--stop-file',type=Path)
    a=p.parse_args()
    global OUT
    qualification=OUT/'original-runtime-preflight.json'
    if a.output_dir:OUT=a.output_dir.resolve()
    OUT.mkdir(parents=True,exist_ok=True)
    names=json.loads(qualification.read_text(encoding='utf-8'))['qualified_names']
    standard_binary=ROOT/('target/release/examples/strategy_ecosystem_checkpoint.exe' if a.checkpoint else 'target/release/examples/strategy_ecosystem_market.exe')
    binary=a.binary.resolve() if a.binary else standard_binary
    env=dict(os.environ);env['PATH']=str(ROOT/'.venv/Scripts')+os.pathsep+env['PATH'];env['PYTHONUTF8']='1'
    inputs=[binary,ROOT/'bot-plugins/pine-strategy/broker.py',ROOT/'bot-plugins/pine-strategy/bot.py',
        ROOT/'bot-plugins/pine-strategy/.runtime/strategy-ecosystem/pine_compat.pyd',ROOT/'docs/strategy-ecosystem-originals.json']
    inputs+=sorted((ROOT/'bot-plugins/pine-strategy/scripts/ecosystem/originals').glob('*.pine'))
    fingerprint={str((standard_binary if f==binary else f).relative_to(ROOT)):sha(f) for f in inputs}
    env['MF_RESEARCH_BUILD_ID']=hashlib.sha256(json.dumps(fingerprint,sort_keys=True).encode()).hexdigest()
    report={'binary_sha256':sha(binary),'build_id':env['MF_RESEARCH_BUILD_ID'],'input_hashes':fingerprint,'steps':a.steps,'clock_seconds':a.clock_seconds,'original_names':names,'runs':[]}
    selected=[next(c for c in CASES if c[0]==name) for name in a.cases] if a.cases else CASES
    for name,count,seed,noise,maker in selected:
        if a.count_override:count=a.count_override
        if a.deadline_unix and time.time()>=a.deadline_unix:break
        recipe,roster=population(name,seed,count,'mixed',ROOT/'docs/strategy-ecosystem-originals.json',
            periods=[10000,30000,60000,300000],ecology=True,noise_fraction=noise,maker_model=maker,original_names=names)
        path=OUT/(name+'.recipe.json');write(path,recipe);write(OUT/(name+'.roster.json'),roster)
        row={'case':name,'count':count,'seed':seed,'noise_fraction':noise,'maker':maker,'recipe_sha256':sha(path),'roles':roster['role_counts']}
        report['runs'].append(row)
        if not a.plan_only:
            existing=OUT/(name+'.receipt.json')
            if a.skip_complete and existing.exists() and existing.stat().st_size:
                try:
                    old=json.loads(existing.read_text(encoding='utf-8'))
                    if old.get('run_complete',True) and old.get('steps')==a.steps and (name!='adaptive-n5-s7' or not a.forks or old.get('forks_complete')):
                        row.update(skipped_complete=True,receipt_sha256=sha(existing));continue
                except json.JSONDecodeError:pass
            command=[str(binary),str(path),str(a.steps),str(a.clock_seconds)]
            if a.forks and name=='adaptive-n5-s7':command.append(str(OUT/f'impact-{count}'))
            if a.checkpoint:
                env['MF_RESEARCH_CHECKPOINT']=str(OUT/(name+'.checkpoint.json'))
                env['MF_RESEARCH_STOP_FILE']=str(a.stop_file.resolve() if a.stop_file else OUT/'STOP')
            if a.deadline_unix:env['MF_RESEARCH_DEADLINE_UNIX']=str(a.deadline_unix)
            start=time.monotonic()
            with (OUT/(name+'.receipt.json')).open('w',encoding='utf-8') as out,(OUT/(name+'.stderr')).open('w',encoding='utf-8')as err:
                result=subprocess.run(command,cwd=ROOT,env=env,stdout=out,stderr=err)
            row.update(exit_code=result.returncode,wall_seconds=time.monotonic()-start)
            if result.returncode==0:
                row['receipt_sha256']=sha(OUT/(name+'.receipt.json'))
                receipt=json.loads((OUT/(name+'.receipt.json')).read_text(encoding='utf-8'))
                row.update(run_complete=receipt.get('run_complete',True),simulation_time_ms=receipt['simulation_time_ms'],forks_complete=receipt.get('forks_complete'))
            else:row['error_tail']=(OUT/(name+'.stderr')).read_text(encoding='utf-8')[-3000:]
            write(OUT/('batch-'+'-'.join(a.cases or ['all'])+'.json'),report)
            if result.returncode:print(json.dumps(row,ensure_ascii=True),flush=True);return 1
            if a.checkpoint and not json.loads((OUT/(name+'.receipt.json')).read_text(encoding='utf-8')).get('run_complete'):
                print(json.dumps(row,ensure_ascii=True),flush=True);break
        print(json.dumps(row,ensure_ascii=True),flush=True)
    write(OUT/('plan.json' if a.plan_only else 'batch-'+'-'.join(a.cases or ['all'])+'.json'),report)
    return 0
if __name__=='__main__':raise SystemExit(main())
