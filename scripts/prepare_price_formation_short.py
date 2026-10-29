"""User-authorized four-hour paired study after two resource interruptions."""
import json,shutil
from datetime import datetime
from research_market_mechanisms import sha,write
from research_price_formation import BASE


def prepare():
    for kind,source in (('controls','linux-controls'),('candidate','linux-full'),('holdout','linux-holdout')):
        old=BASE/source;out=BASE/('short-'+kind);out.mkdir(exist_ok=False)
        for name in ('candidate.exe','compiled-inputs.json','runtime-inputs.json'):shutil.copyfile(old/name,out/name)
        shutil.copytree(old/'compiled-source',out/'compiled-source')
        p=json.loads((old/'plan.json').read_text(encoding='utf-8'));rows=[];cached={}
        for r in p['runs']:
            path=out/(r['name']+'.recipe.json');shutil.copyfile(r['recipe'],path);assert sha(path)==r['recipe_sha256']
            rows.append({**r,'recipe':str(path),'steps':14400})
            if r['seed']==41:
                origin=BASE/'linux-compatibility';done=origin/(r['name']+'.run.json')
                assert json.loads(done.read_text(encoding='utf-8'))['complete']
                for suffix in ('.run.json','.receipt.json','.log'):shutil.copyfile(origin/(r['name']+suffix),out/(r['name']+suffix))
                cached[r['name']]=dict(directory=str(origin),run_sha256=sha(done),receipt_sha256=sha(origin/(r['name']+'.receipt.json')))
        p.update(frozen_at=datetime.now().astimezone().isoformat(),runs=rows,cached_runs=cached,source_plan_sha256=sha(old/'plan.json'),
            minimum_linux_available_gib=16,minimum_host_commit_gib=8,task_cgroup=dict(MemoryHigh='4G',MemoryMax='8G',MemorySwapMax='8G',User='helenanana'),
            scope='Human explicitly chose four-hour paired comparisons with long-duration gap retained, after Windows and Linux resource guard interruptions. Seeds41,7,19,73:4h primary[2h,4h). Seed101 also4h, not used for selection. Same original frozen recipe bytes/binary and1s clock; no numerical fitting. Existing completed seed41 Linux compatibility outcomes are cached by exact raw hashes, not counted as new executions. Other processes/global WSL settings unchanged; only owned transient memory limits and3GiB guards.')
        write(out/'plan.json',p)
        print(out)


if __name__=='__main__':prepare()
