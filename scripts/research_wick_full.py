"""Qualify a pilot-selected mechanism on every predeclared fixed seed."""
import argparse,json,shutil,sys,subprocess,os,time
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write,ensure_runtime_inputs,run_one
from research_wick_observations import BASE,SOURCE


def plan(profile,pilot_kind='execution',output=None):
    pilot=BASE/('pilot-'+pilot_kind);p=json.loads((pilot/'plan.json').read_text(encoding='utf-8'))
    assert profile in p['profiles'] and profile!='control'
    out=output.resolve() if output else BASE/('full-'+profile);assert not out.exists();out.mkdir()
    shutil.copyfile(pilot/'candidate.exe',out/'candidate.exe')
    shutil.copytree(pilot/'compiled-source',out/'compiled-source');shutil.copyfile(pilot/'compiled-inputs.json',out/'compiled-inputs.json')
    shutil.copyfile(pilot/'pilot-comparison.json',out/'selection-pilot-snapshot.json')
    shutil.copyfile(BASE/'pilot-execution/execution-scope-clarification.json',out/'execution-scope-clarification.json')
    selected=json.loads((out/'selection-pilot-snapshot.json').read_text(encoding='utf-8'))
    runs=[];controls=[]
    for r in p['all_seed_full_designs']:
        if r['profile']!=profile:continue
        src=ROOT/r['recipe'] if not os.path.isabs(r['recipe']) else r['recipe']
        dest=out/(r['name']+'.recipe.json');shutil.copyfile(src,dest);assert sha(dest)==r['recipe_sha256']
        runs.append({**r,'recipe':str(dest.resolve())})
        old=SOURCE/f"native-s{r['seed']}-depth.run.json";done=json.loads(old.read_text(encoding='utf-8'));assert done['complete']
        controls.append(dict(seed=r['seed'],directory=str(SOURCE),name=f"native-s{r['seed']}-depth",run_sha256=sha(old),receipt_sha256=done['receipt_sha256']))
    assert [r['seed'] for r in runs]==[41,7,19,73]
    ensure_runtime_inputs(out,runs)
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),profile=profile,runs=runs,controls=controls,
        pilot_directory=str(pilot),
        pilot_plan_sha256=sha(pilot/'plan.json'),selection_result_sha256=sha(pilot/'pilot-comparison.json'),
        selection_snapshot_sha256=sha(out/'selection-pilot-snapshot.json'),execution_scope_clarification_sha256=sha(out/'execution-scope-clarification.json'),
        pilot_completed_at_selection=selected['completed_runs'],pilot_total_planned=len(p['runs']),
        protocol=f"Same predeclared four seeds,300bots,12h,1s clock. All{len(p['all_seed_full_designs'])} full recipes of this pilot were frozen before its candidate execution. Candidate recipe bytes are unchanged. Primary fixed[2h,12h); retain every seed. Serial full native execution. Old-depth controls are the archived identical-input legacy version. New-binary4h control byte identity and exact old4h/12h trade-book prefix proof are separate evidence; no claim of full12h new-binary control identity until separately run.",
        selection='Least-complex promising mechanism advanced after the available exploratory seed41 outcomes, using normalized wick shape and activity/ACF/tails/basis jointly; this is not a claim that every other pilot profile was already finished or that this is the final winner. All remaining frozen pilots continue and are retained. These seeds have been examined previously; no independent holdout or real-market qualification.',
        fixed=p['fixed'],mechanism=p['mechanism'],ledger=p.get('ledger','Style target remains a position goal; actual account position determines the next residual. Reactive parent-demand ledger remains unchanged.'),limits=p.get('limits','Subsequent exploratory iteration on previously examined seeds; no independent holdout or full market-realism qualification.')))
    print('Frozen12h full study for '+profile,flush=True)


def resource_ready(out):
    """Gate only new child launches; never change market time or trading inputs."""
    threshold=20*1024*1024
    while True:
        raw=subprocess.check_output(['powershell','-NoProfile','-Command','(Get-CimInstance Win32_OperatingSystem).FreeVirtualMemory'],text=True)
        available=int(raw.strip())
        with (out/'resource-start-gates.jsonl').open('a',encoding='utf-8') as f:
            f.write(json.dumps(dict(time=datetime.now().astimezone().isoformat(),free_commit_kib=available,minimum_kib=threshold))+'\n')
        if available>=threshold:return
        time.sleep(30)


def run(profile,output=None):
    out=output.resolve() if output else BASE/('full-'+profile);p=json.loads((out/'plan.json').read_text(encoding='utf-8'));ensure_runtime_inputs(out,p['runs'])
    binary=out/'candidate.exe';assert sha(binary)==json.loads((out/'compiled-inputs.json').read_text(encoding='utf-8'))['binary_sha256']
    for row in p['runs']:
        if not (out/(row['name']+'.run.json')).exists():resource_ready(out)
        result=run_one(out,row,binary,10800);assert result['complete']
        if row['seed']==41:
            pilot=Path(p.get('pilot_directory',str(BASE/'pilot-execution')))/(row['name']+'.receipt.json')
            full=out/(row['name']+'.receipt.json')
            short=json.loads(pilot.read_text(encoding='utf-8'));long=json.loads(full.read_text(encoding='utf-8'))
            end=short['simulation_time_ms'];checks={}
            for key in ('trade_receipts','book_snapshots'):
                prefix=[x for x in long[key] if x['time_ms']<=end];assert prefix==short[key],key
                checks[key]=dict(equal=True,rows=len(prefix))
            assert short['assets_before']==long['assets_before']
            write(out/'seed41-pilot-prefix.json',dict(short_receipt_sha256=sha(pilot),full_receipt_sha256=sha(full),end_ms=end,checks=checks,assets_before_equal=True))
            del short,long
        env=dict(os.environ,PYTHONPATH=str(ROOT/'.local/strategy-research-reservation-20261024/analysis-deps'))
        with (out/(row['name']+'-analysis.log')).open('w',encoding='utf-8') as log:
            checked=subprocess.run([sys.executable,str(ROOT/'scripts/analyze_wick_full.py'),str(out)],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
        assert checked.returncode==0
    ensure_runtime_inputs(out,p['runs'])


if __name__=='__main__':
    from pathlib import Path
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['plan','run']);p.add_argument('profile');p.add_argument('--pilot',choices=['execution','style'],default='execution');p.add_argument('--output',type=Path);a=p.parse_args();plan(a.profile,a.pilot,a.output) if a.action=='plan' else run(a.profile,a.output)
