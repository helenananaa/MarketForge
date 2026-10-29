"""Short fixed-input output/memory regression only; never runs a full horizon."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from research_market_mechanisms import ROOT, sha, write
from research_resources import windows_memory
from research_receipts import finish_result


def run_one(out, binary, recipe, steps, summary_expected):
    out.mkdir(exist_ok=False)
    env=dict(os.environ,RESEARCH_PLUGIN_WAKES='events',MARKETFORGE_PINE_IN_PROCESS='1',
             MARKETFORGE_PINE_REPLAY_STATE='1',MARKETFORGE_BOT_WORKERS='8',RAYON_NUM_THREADS='8')
    for key in ('MF_RESEARCH_CHECKPOINT','MF_RESEARCH_STOP_FILE','MF_RESEARCH_DEADLINE_UNIX',
                'RESEARCH_PLUGIN_POLL_MS','MF_RESEARCH_TRACE_PATH','MF_RESEARCH_TRACE_WINDOWS',
                'RESEARCH_PROFILE','RESEARCH_SCRIPT_PROFILE','RESEARCH_AUDIT','MF_RESEARCH_OUTPUT'):
        env.pop(key,None)
    env['PATH']=str(ROOT.parent/'MarketForge/.venv/Scripts')+os.pathsep+env['PATH']
    summary=out/'summary.json';receipt=out/'receipt.json'
    env.update(MF_RESEARCH_SUMMARY_PATH=str(summary),MF_RESEARCH_SPOOL_DIR=str(out/'spool'))
    first=windows_memory()
    if first['free_commit_kib']<6*1024*1024:
        raise RuntimeError('Short validation needs 6 GiB host commitment headroom')
    result=dict(complete=False,binary_sha256=sha(binary),recipe_sha256=sha(recipe),steps=steps,
                baseline=first,samples=[],scope='Short diagnostic only; no long-run qualification')
    start=time.monotonic();reason=None
    print('START',out.name,'steps',steps,flush=True)
    with receipt.open('wb')as stdout,(out/'stderr.log').open('wb')as stderr:
        child=subprocess.Popen([str(binary),str(recipe),str(steps),'1'],cwd=ROOT,env=env,stdout=stdout,stderr=stderr)
        try:
            while child.poll()is None:
                sample=dict(time=time.time(),**windows_memory(child.pid));result['samples'].append(sample)
                if sample['free_commit_kib']<4*1024*1024:reason='diagnostic_host_commit_below4GiB'
                elif sample.get('process_private_bytes',0)>4*1024**3:reason='diagnostic_private_above4GiB'
                elif time.monotonic()-start>300:reason='diagnostic_timeout'
                if reason:child.kill();result['failure']=reason;break
                time.sleep(.25)
            result['exit_code']=child.wait()
        finally:
            if child.poll()is None:child.kill();child.wait()
    result.update(wall_seconds=time.monotonic()-start,receipt_bytes=receipt.stat().st_size,
        receipt_sha256=sha(receipt),peak_private_bytes=max((s.get('process_private_bytes',0)for s in result['samples']),default=0),
        peak_reported_private_bytes=max((s.get('process_peak_private_bytes',0)for s in result['samples']),default=0))
    if summary_expected:
        finish_result(result,receipt,dict(steps=steps,seconds=1),summary)
    else:
        # Reference runs are trusted only for paired comparison after clean native exit.
        result['complete']=result['exit_code']==0 and not reason and receipt.stat().st_size>0
    write(out/'run.json',result)
    print('DONE',out.name,'complete',result['complete'],'private MiB',round(result['peak_private_bytes']/1024**2,2),flush=True)
    if not result['complete']:raise RuntimeError('Short validation failed; partial evidence preserved')
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--old',required=True,type=Path);parser.add_argument('--new',required=True,type=Path)
    parser.add_argument('--recipe',required=True,type=Path);parser.add_argument('--out',required=True,type=Path)
    parser.add_argument('--steps',nargs='+',type=int,default=[1800,3600])
    parser.add_argument('--reference',type=Path,help='Reuse verified frozen short reference receipts; never rerun a full horizon')
    args=parser.parse_args()
    if not args.steps or any(not 1<=n<=3600 for n in args.steps):
        raise ValueError('This verifier allows only short runs of at most 3600 steps')
    out=args.out.resolve();out.mkdir(exist_ok=False)
    old=out/'old.exe';new=out/'new.exe';recipe=out/'recipe.json'
    shutil.copyfile(args.old,old);shutil.copyfile(args.new,new);shutil.copyfile(args.recipe,recipe)
    pairs=[]
    for steps in args.steps:
        if args.reference:
            source=args.reference/f'old-{steps}'
            reference=json.loads((source/'run.json').read_text(encoding='utf-8'))
            assert reference['complete']and reference['exit_code']==0 and reference['steps']==steps
            assert reference['binary_sha256']==sha(old)and reference['recipe_sha256']==sha(recipe)
            assert reference['receipt_sha256']==sha(source/'receipt.json')
            write(out/f'old-{steps}-reference.json',dict(source=str(source.resolve()),run_sha256=sha(source/'run.json'),**reference))
        else:
            reference=run_one(out/f'old-{steps}',old,recipe,steps,False)
        updated=run_one(out/f'new-{steps}',new,recipe,steps,True)
        identical=reference['receipt_sha256']==updated['receipt_sha256']
        pair=dict(steps=steps,byte_identical=identical,old_peak_private_bytes=reference['peak_private_bytes'],
                  new_peak_private_bytes=updated['peak_private_bytes'],receipt_sha256=updated['receipt_sha256'],reference_cached=bool(args.reference))
        pairs.append(pair);write(out/'comparison.json',dict(full_horizon_run=False,pairs=pairs))
        if not identical:raise AssertionError('Native outputs differ; inspect retained receipts before accepting')
    print(json.dumps(pairs,indent=2),flush=True)


if __name__=='__main__':main()
