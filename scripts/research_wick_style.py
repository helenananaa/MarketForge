"""Factorial execution check after remaining perp extrema identify style sweeps."""
import argparse,copy,json,shutil,sys,os,subprocess
from datetime import datetime
from research_market_mechanisms import ROOT,sha,write,ensure_runtime_inputs,run_one
from research_wick_observations import BASE,SOURCE

PROFILES={'control':(False,False),'sliced':(True,False),'styled':(False,True),'dual-execution':(True,True)}


def recipe_for(control,profile):
    reactive,style=PROFILES[profile];result=copy.deepcopy(control)
    for a in result['agents']:
        p=a['Plugin']
        if p['plugin_id']=='ReactiveFlowTrader' and reactive:
            assert p['plugin_version']=='1.0.0';p['plugin_version']='1.1.0'
        if p['plugin_id']=='StyleSwitchingTrader' and style:
            assert p['plugin_version']=='1.0.0';p['plugin_version']='1.1.0'
    original=copy.deepcopy(result)
    for a in original['agents']:
        if a['Plugin']['plugin_id']in ('ReactiveFlowTrader','StyleSwitchingTrader'):a['Plugin']['plugin_version']='1.0.0'
    assert original==control
    return result


def plan():
    out=BASE/'pilot-style';assert not out.exists();out.mkdir();shutil.copyfile(ROOT/'target/release/examples/strategy_ecosystem_market.exe',out/'candidate.exe')
    compiled=[]
    for name in ('Cargo.lock','exchange-core/src/reactive_flow.rs','exchange-core/src/reactive_flow_tests.rs','exchange-core/src/style_switching.rs','exchange-core/src/style_switching_tests.rs','exchange-core/src/adaptive_liquidity.rs','exchange-core/src/basis_link.rs','exchange-core/examples/strategy_market.rs','exchange-core/examples/support/research_trace.rs','exchange-server/examples/strategy_ecosystem_market.rs','scripts/strategy_ecosystem.py'):
        src=ROOT/name;dst=out/'compiled-source'/name;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,dst);compiled.append(dict(path=name,sha256=sha(src)))
    write(out/'compiled-inputs.json',dict(files=compiled,binary_sha256=sha(out/'candidate.exe')))
    designs=[];runs=[]
    for seed in (41,7,19,73):
        ctrl=json.loads((SOURCE/f'native-s{seed}-depth.recipe.json').read_text(encoding='utf-8'))
        for profile in PROFILES:
            path=out/f'native-s{seed}-{profile}.recipe.json';write(path,recipe_for(ctrl,profile))
            row=dict(name=path.name.removesuffix('.recipe.json'),suite='native',seed=seed,profile=profile,bots=300,steps=43200,seconds=1,warmup_ms=7200000,recipe=str(path.resolve()),recipe_sha256=sha(path));designs.append(row)
            if seed==41:runs.append({**row,'steps':14400})
    ensure_runtime_inputs(out,runs)
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=runs,all_seed_full_designs=designs,profiles=PROFILES,
        reason='After the full12h sliced seed41 outcome, style traders produced113/240 perp5m extrema and all113 traded across multiple prices. This is a subsequent exploratory mechanism iteration, not a predeclared original hypothesis or independent holdout.',
        diagnosis_sha256=sha(BASE/'sliced-seed41-diagnosis/anatomy-summary.json'),
        fixed='Only ReactiveFlowTrader and/or StyleSwitchingTrader plugin_version1.0->1.1 differ. Every numerical config, target policy, signal/stop rules, cash/inventory/fees, maker observation, other agents and1s wave clock remain fixed. Realized fills, paths and subsequent signals remain endogenous.',
        mechanism='Style1.1 keeps the computed desired position and existing capacity child cap, but further caps its IOC child to observed external touch depth and prices it at that touch, only if within the unchanged original slippage boundary. Actual observed position, not submitted size, determines the next target residual. Stop-outs remain actual-position exits and can take several observations. No new sizing coefficient or spread/volatility cap.',
        pilot='Four seed41 profiles4h, fixed primary hours2–4. Control must byte-match the old control; sliced must byte-match the prior execution binary sliced control. All16 four-seed12h recipes frozen before style-candidate outcomes. All outcomes retained.',
        analysis_scope='Subsequent factorial execution-only seed41 four-hour screen, fixed hours2–4. Numeric inputs and target/signal/stop policies held equal. This follow-up was motivated by remaining extrema in the full seed41 sliced outcome; not independent validation. All outcomes retained; volume reduction alone is insufficient.',
        full='Promising follow-up completes12h on all four original seeds, fixed primary hours2–12. Finish all original sliced full runs before launching follow-up full runs; one full native process at a time. No numerical retuning.'))
    print('Frozen four style pilot runs and16 full-design recipes',flush=True)


def run():
    out=BASE/'pilot-style';p=json.loads((out/'plan.json').read_text(encoding='utf-8'));ensure_runtime_inputs(out,p['runs']);binary=out/'candidate.exe'
    assert sha(binary)==json.loads((out/'compiled-inputs.json').read_text(encoding='utf-8'))['binary_sha256']
    for row in p['runs']:
        r=run_one(out,row,binary,10800);assert r['complete']
        if row['profile']in ('control','sliced'):
            name='native-s41-old' if row['profile']=='control' else 'native-s41-sliced'
            folder=BASE/('compatibility-old' if row['profile']=='control' else 'pilot-execution')
            old=json.loads((folder/(name+'.run.json')).read_text(encoding='utf-8'));assert old['complete'] and old['receipt_sha256']==r['receipt_sha256']
            write(out/(row['profile']+'-compatibility.json'),dict(byte_identical=True,steps=14400,receipt_sha256=r['receipt_sha256']))
        env=dict(os.environ,PYTHONPATH=str(ROOT/'.local/strategy-research-reservation-20261024/analysis-deps'))
        with (out/(row['profile']+'-analysis.log')).open('w',encoding='utf-8') as log:
            checked=subprocess.run([sys.executable,str(ROOT/'scripts/analyze_wick_pilot.py'),'--output',str(out)],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
        assert checked.returncode==0
    ensure_runtime_inputs(out,p['runs'])


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['plan','run']);a=p.parse_args();(plan if a.action=='plan' else run)()
