"""Fresh platform plans; never relabel the interrupted Windows attempt."""
import argparse,json,shutil,subprocess
from datetime import datetime
from research_market_mechanisms import ROOT,write,sha,ensure_runtime_inputs
from research_price_formation import BASE


def prepare(kind):
    pilot=BASE/'pilot';out=BASE/('linux-'+kind);out.mkdir(exist_ok=False)
    if kind=='compatibility':
        src='/home/helenanana/.cache/marketforge-price-formation-20261025/release/examples/strategy_ecosystem_market'
        unix='/mnt/e/'+str((out/'candidate.exe').relative_to('E:/')).replace('\\','/')
        subprocess.run(['wsl','--exec','cp',src,unix],check=True)
        subprocess.run(['wsl','--exec','chmod','755',unix],check=True)
    else:
        shutil.copyfile(BASE/'linux-compatibility/candidate.exe',out/'candidate.exe')
        unix='/mnt/e/'+str((out/'candidate.exe').relative_to('E:/')).replace('\\','/')
        subprocess.run(['wsl','--exec','chmod','755',unix],check=True)
    shutil.copytree(pilot/'compiled-source',out/'compiled-source')
    inputs=json.loads((pilot/'compiled-inputs.json').read_text(encoding='utf-8'));inputs['binary_sha256']=sha(out/'candidate.exe')
    write(out/'compiled-inputs.json',inputs)
    parent=json.loads((pilot/'plan.json').read_text(encoding='utf-8'))
    if kind=='compatibility':designs=[r for r in parent['all_seed_full_designs']if r['seed']==41 and r['profile']in ('control','anchor-slope')]
    elif kind in ('controls','full'):designs=[r for r in parent['all_seed_full_designs']if r['profile']==('control'if kind=='controls'else'anchor-slope')]
    else:designs=[r for r in json.loads((BASE/'holdout-design/plan.json').read_text(encoding='utf-8'))['designs']if r['profile']in ('control','anchor-slope')]
    runs=[]
    for r in designs:
        path=out/(r['name']+'.recipe.json');shutil.copyfile(r['recipe'],path);assert sha(path)==r['recipe_sha256']
        runs.append({**r,'recipe':str(path),'steps':14400 if kind in ('compatibility','holdout') else 43200})
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=runs,binary_sha256=sha(out/'candidate.exe'),
        platform='WSL Ubuntu x86_64; rustc1.97.1 matching Windows compiler version; fresh Linux controls paired with candidate',
        minimum_linux_available_gib=16 if kind in ('compatibility','holdout') else 32,minimum_host_commit_gib=12 if kind in ('compatibility','holdout') else 16,
        task_cgroup=None if kind=='compatibility'else dict(MemoryHigh='10G',MemoryMax='14G',MemorySwapMax='16G',User='helenanana'),
        source_plan_sha256=sha(pilot/'plan.json'),scope='Same pre-frozen numerical recipes, flags, bot count, seeds,1s clock and primary windows. Separate new platform after Windows memory guard interruption; do not merge platform baselines. Serial native processes; only owned child stopped below3GiB Linux available or Windows commit. No system changes or other project processes touched. Compatibility tests compare both full4h receipts and every trade/book against frozen Windows screens before qualification.'))
    ensure_runtime_inputs(out,runs)
    print(out)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('kind',choices=['compatibility','controls','full','holdout']);prepare(p.parse_args().kind)
