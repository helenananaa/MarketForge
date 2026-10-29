"""Serial immutable native runs and offline analysis, preserving every interruption."""
import argparse,json,shutil,subprocess,sys,time
from pathlib import Path
from research_parent_liquidity import BASE,PREVIOUS,SEEDS,active_main
from research_market_mechanisms import ROOT,sha,write


def read(p):return json.loads(p.read_text(encoding='utf-8'))


def native(folder,seed=None):
    command=['wsl','--user','root','--exec','bash','-lc',
        'cd /mnt/e/projects/MarketForge-tuning && python3 scripts/run_parent_liquidity_linux.py '+folder.as_posix()+(' --seed '+str(seed)if seed is not None else'')]
    log=BASE/(folder.name+('-s'+str(seed)if seed is not None else'')+'-launcher.log')
    assert not log.exists()
    with log.open('wb')as f:subprocess.run(command,stdout=f,stderr=subprocess.STDOUT,check=True,cwd=ROOT)


def analyze(folder,seed=None):
    log=BASE/(folder.name+('-s'+str(seed)if seed is not None else'')+'-analysis.log')
    command=[sys.executable,str(ROOT/'scripts/analyze_parent_liquidity.py'),str(folder)]+(['--seed',str(seed)]if seed is not None else[])
    with log.open('wb')as f:subprocess.run(command,stdout=f,stderr=subprocess.STDOUT,check=True,cwd=ROOT)


def run_main(target):
    for seed in SEEDS:
        done=target/f'native-s{seed}-liquidity-parent.run.json'
        if done.exists():
            assert read(done)['complete'],'Preserve failed attempts; prepare a fresh directory'
            continue
        native(target,seed);print('NATIVE seed'+str(seed)+' completed',flush=True)
        analyze(target);print('ANALYSIS through seed'+str(seed)+' completed',flush=True)
    assert read(target/'comparison.json')['complete']
    write(BASE/'four-seed-complete.json',dict(complete=True,directory=str(target),comparison_sha256=sha(target/'comparison.json'),
        scope='Four frozen paired4h comparisons complete.271 held until explicit joint-result decision;12h and broad market-realism qualification remain separate.'))


def run():
    # Compatibility was dispatched separately; wait for its durable receipt.
    path=BASE/'compatibility/native-s41-legacy.run.json'
    while not path.exists():time.sleep(10)
    done=read(path);assert done['complete']
    old=PREVIOUS/'main/native-s41-raw-flow.receipt.json';raw=BASE/'compatibility/native-s41-legacy.receipt.json'
    assert sha(old)==sha(raw)==done['receipt_sha256']
    a=read(BASE/'compatibility/native-s41-legacy.summary.json')['execution_archives']
    b=read(PREVIOUS/'main/native-s41-raw-flow.summary.json')['execution_archives']
    assert [(x['records'],x['sha256'])for x in a]==[(x['records'],x['sha256'])for x in b]
    write(BASE/'compatibility-proof.json',dict(byte_identical=True,receipt_sha256=sha(raw),full_execution_archives=[dict(records=x['records'],sha256=x['sha256'])for x in a],
        scope='New binary legacy path reproduces the complete prior Linux market receipt and full authoritative command archive. Economic baseline controls remain immutable actual4h observations, not simulated substitutes or new hours.'))
    print('COMPATIBILITY byte identity passed',flush=True)
    native(BASE/'pilot');analyze(BASE/'pilot',41)
    write(BASE/'pilot-progress.json',dict(pilot_comparison_sha256=sha(BASE/'pilot/comparison.json'),
        decision='Continue all four frozen seeds to assess execution quality and market tradeoffs, without coefficient retuning or promotion.'))
    source,target=BASE/'pilot',BASE/'main';name='native-s41-liquidity-parent'
    for suffix in ('.run.json','.summary.json','.receipt.json','.log'):
        assert not (target/(name+suffix)).exists();shutil.copyfile(source/(name+suffix),target/(name+suffix))
    write(target/'cache-provenance.json',{name:dict(directory=str(source),run_sha256=sha(source/(name+'.run.json')),receipt_sha256=sha(source/(name+'.receipt.json')),qualification='Exact pilot cache; not another native run or hours.')})
    run_main(target)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--resume-main',type=Path);a=p.parse_args()
    if a.resume_main:
        assert a.resume_main.resolve()==active_main().resolve()and a.resume_main.resolve().is_relative_to(BASE.resolve())
        assert read(BASE/'compatibility-proof.json')['byte_identical']
        run_main(a.resume_main)
    else:run()
