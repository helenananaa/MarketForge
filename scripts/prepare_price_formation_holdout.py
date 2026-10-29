"""Freeze one additional short seed before viewing any of its outcomes."""
import argparse,json,shutil
from datetime import datetime
from research_market_mechanisms import ROOT,options,write,sha,ensure_runtime_inputs
from research_price_formation import BASE,PROFILES,recipe_for
from strategy_ecosystem import population


def design():
    out=BASE/'holdout-design';out.mkdir(exist_ok=False)
    control,_=population(**options(101),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',
        reservation_innovations=True,market_depth_flow=True,reactive_execution='sliced',style_execution='touch')
    runs=[]
    for profile in PROFILES:
        path=out/f'native-s101-{profile}.recipe.json';write(path,recipe_for(control,profile))
        runs.append(dict(name=path.name.removesuffix('.recipe.json'),seed=101,profile=profile,bots=300,
            steps=14400,seconds=1,warmup_ms=7200000,recipe=str(path),recipe_sha256=sha(path)))
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),designs=runs,
        binary_sha256=sha(BASE/'pilot/candidate.exe'),scope='Additional seed101,4h,primary[2h,4h),not used in this round of candidate selection. Freeze all four profiles before any seed101 outcome. Only control and the independently selected candidate will execute, after the original four full seeds. This is one short holdout, not long-duration or broad generalization qualification.'))


def select(profile):
    source=BASE/'holdout-design';p=json.loads((source/'plan.json').read_text(encoding='utf-8'))
    out=BASE/'holdout';out.mkdir(exist_ok=False);shutil.copyfile(BASE/'pilot/candidate.exe',out/'candidate.exe')
    rows=[]
    for r in p['designs']:
        if r['profile']not in ('control',profile):continue
        path=out/(r['name']+'.recipe.json');shutil.copyfile(r['recipe'],path);assert sha(path)==r['recipe_sha256']
        rows.append({**r,'recipe':str(path)})
    assert len(rows)==2
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=rows,binary_sha256=p['binary_sha256'],minimum_launch_commit_gib=12,
        design_plan_sha256=sha(source/'plan.json'),scope=p['scope'],selection_sha256=sha(BASE/'selection-decision.json')))
    ensure_runtime_inputs(out,rows)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['design','select']);p.add_argument('--profile',choices=list(PROFILES));a=p.parse_args()
    design()if a.action=='design'else select(a.profile)
