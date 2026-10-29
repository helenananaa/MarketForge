"""Resume frozen paired4h trials, then all four paired12h trials, serially."""
import argparse,copy,json,shutil,subprocess,sys,threading,time
from datetime import datetime
from pathlib import Path
from research_parent_liquidity import BASE,SEEDS
from research_market_mechanisms import ROOT,sha,write
from research_resources import windows_memory
from parent_memory_relay import tick
from run_parent_liquidity_study import run_main,native

MAIN=BASE/'main-resume-20261026'
CONTROL=BASE/'long12h-controls-20261026'
CANDIDATE=BASE/'long12h-candidates-20261026'


def read(p):return json.loads(p.read_text(encoding='utf-8'))


def prepare():
    assert not (BASE/'followup-20261026.json').exists()
    original=read(BASE/'main/plan.json')
    for folder,profile,hours in ((MAIN,'liquidity-parent',4),(CONTROL,'legacy',12),(CANDIDATE,'liquidity-parent',12)):
        folder.mkdir(exist_ok=False)
        p=copy.deepcopy(original);p['runs']=[]
        binary=BASE/('control-linux.elf'if profile=='legacy'else'design/candidate-linux.elf')
        shutil.copyfile(binary,folder/'candidate.exe');p['binary_sha256']=sha(binary)
        for seed in SEEDS:
            source=BASE/('controls'if profile=='legacy'else'main')
            original_row=next(r for r in read(source/'plan.json')['runs']if r['seed']==seed)
            row=copy.deepcopy(original_row);source_recipe=Path(row['recipe'])
            recipe=folder/source_recipe.name;shutil.copyfile(source_recipe,recipe)
            assert sha(recipe)==row['recipe_sha256']
            row.update(recipe=str(recipe),steps=hours*3600)
            p['runs'].append(row)
        p.update(prepared_at=datetime.now().astimezone().isoformat(),source_plan_sha256=sha(BASE/'main/plan.json'),
            scope=f'User-authorized frozen economic recipes and exact binaries;{hours}h,1s,300bots. No coefficients, parent horizons, funding, fees, initial capital, maker or hedge policy changed. Serial native and offline analysis. Same6GiB host/8GiB guest launch and3GiB stop guards.')
        write(folder/'plan.json',p)
        shutil.copyfile(BASE/'main/runtime-inputs.json',folder/'runtime-inputs.json')
        unix='/mnt/e/'+(folder/'candidate.exe').relative_to('E:/').as_posix()
        subprocess.run(['wsl','--exec','chmod','755',unix],check=True)
    name='native-s41-liquidity-parent'
    for suffix in ('.run.json','.summary.json','.receipt.json','.log'):
        shutil.copyfile(BASE/'pilot'/(name+suffix),MAIN/(name+suffix))
    write(MAIN/'cache-provenance.json',{name:dict(directory=str(BASE/'pilot'),
        run_sha256=sha(BASE/'pilot'/(name+'.run.json')),receipt_sha256=sha(BASE/'pilot'/(name+'.receipt.json')),
        qualification='Exact complete pilot cache. Not additional native hours.')})
    write(BASE/'active-main.json',dict(directory=str(MAIN),reason='User resumed all four frozen paired4h trials and paired12h validation. Failed and user-stopped directories preserved.'))
    write(BASE/'followup-20261026.json',dict(prepared_at=datetime.now().astimezone().isoformat(),
        seeds=list(SEEDS),main=str(MAIN),long_control=str(CONTROL),long_candidate=str(CANDIDATE),
        long_primary_ms=[7200000,43200000],long_terminal_ms=[36000000,43200000],
        long_hours=list(range(2,12)),four_hour_primary_ms=[7200000,14400000],
        all_long_pairs_required=True,independent_seed271_authorized=False,
        policy='Run all four long pairs regardless of4h outcomes. No outcome-based seed or interval selection, numeric tuning or default promotion. Long source recipes equal existing frozen4h recipes byte-for-byte. First4h trade prefix/candles will be checked against4h outcomes; same endogenous RNG path is expected, not independent evidence.',
        input_plans=[dict(path=str(f/'plan.json'),sha256=sha(f/'plan.json'))for f in (MAIN,CONTROL,CANDIDATE)],
        harness=[dict(path=str(p.relative_to(ROOT)),sha256=sha(p))for p in [Path(__file__),ROOT/'scripts/analyze_parent_liquidity_long.py',ROOT/'scripts/run_parent_liquidity_linux.py',ROOT/'scripts/parent_resource_guard.py']]))
    print('Frozen followup plans prepared',flush=True)


def relay(stop):
    while not stop.is_set():
        result=tick(BASE)
        if not result['published']:
            with (BASE/'relay-publication-errors.jsonl').open('a')as f:f.write(json.dumps(result)+'\n')
        # Retry a transient sharing collision using a freshly measured pulse.
        # Persistent failure still leaves the target stale and stops natives.
        stop.wait(5 if result['published']else .25)


def run():
    for f in read(BASE/'followup-20261026.json')['input_plans']:assert sha(Path(f['path']))==f['sha256']
    stop=threading.Event();guardian=threading.Thread(target=relay,args=(stop,),daemon=True);guardian.start()
    try:
        run_main(MAIN)
        for seed in SEEDS:
            for folder in (CONTROL,CANDIDATE):
                done=folder/f"native-s{seed}-{'raw-flow'if folder==CONTROL else'liquidity-parent'}.run.json"
                if done.exists():assert read(done)['complete'];continue
                native(folder,seed);print('LONG native '+folder.name+' seed'+str(seed)+' complete',flush=True)
            log=BASE/f'long12h-analysis-s{seed}.log'
            with log.open('wb')as f:
                subprocess.run([sys.executable,str(ROOT/'scripts/analyze_parent_liquidity_long.py'),
                    '--seed',str(seed)],cwd=ROOT,stdout=f,stderr=subprocess.STDOUT,check=True)
            print('LONG pair seed'+str(seed)+' analyzed',flush=True)
        with (BASE/'long12h-combine.log').open('wb')as f:
            subprocess.run([sys.executable,str(ROOT/'scripts/analyze_parent_liquidity_long.py')],cwd=ROOT,
                stdout=f,stderr=subprocess.STDOUT,check=True)
        write(BASE/'long12h-complete.json',dict(complete=True,seeds=list(SEEDS),
            comparison_sha256=sha(CANDIDATE/'comparison.json'),new_native_runs=8,new_simulated_hours=96))
    finally:
        stop.set();guardian.join(10)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['prepare','run']);a=p.parse_args()
    prepare()if a.action=='prepare'else run()
