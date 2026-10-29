"""Freeze touch-sliced aggressive-demand studies without numerical retuning."""
import argparse,copy,json,shutil,sys,os,subprocess
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write,ensure_runtime_inputs,run_one
from research_wick_observations import BASE,PROFILES as OBSERVATIONS,SOURCE

PROFILES={'control':None,'sliced':'control','sliced-penetration':'penetration',
          'sliced-candleflow':'candleflow','sliced-both':'both'}


def recipe_for(control,profile):
    candidate=copy.deepcopy(control)
    for a in candidate['agents']:
        p=a['Plugin']
        if p['plugin_id']=='ReactiveFlowTrader' and profile!='control':
            assert p['plugin_version']=='1.0.0';p['plugin_version']='1.1.0'
        if p['plugin_id']=='AdaptiveLiquidityMaker':
            for key in OBSERVATIONS[PROFILES[profile] or 'control']:p['config'][key]=True
    restored=copy.deepcopy(candidate)
    for a in restored['agents']:
        p=a['Plugin']
        if p['plugin_id']=='ReactiveFlowTrader':p['plugin_version']='1.0.0'
        if p['plugin_id']=='AdaptiveLiquidityMaker':
            for key in OBSERVATIONS[PROFILES[profile] or 'control']:p['config'].pop(key)
    assert restored==control
    return candidate


def plan():
    out=BASE/'pilot-execution';assert not out.exists();out.mkdir()
    shutil.copyfile(ROOT/'target/release/examples/strategy_ecosystem_market.exe',out/'candidate.exe')
    compiled=[]
    for name in ('Cargo.lock','exchange-core/src/reactive_flow.rs','exchange-core/src/reactive_flow_tests.rs','exchange-core/src/adaptive_liquidity.rs','exchange-core/src/basis_link.rs','exchange-core/examples/strategy_market.rs','exchange-core/examples/support/research_trace.rs','exchange-server/examples/strategy_ecosystem_market.rs','scripts/strategy_ecosystem.py'):
        src=ROOT/name;target=out/'compiled-source'/name;target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,target);compiled.append(dict(path=name,sha256=sha(src)))
    write(out/'compiled-inputs.json',dict(files=compiled,binary_sha256=sha(out/'candidate.exe')))
    designs=[];runs=[]
    for seed in (41,7,19,73):
        control=json.loads((SOURCE/f'native-s{seed}-depth.recipe.json').read_text(encoding='utf-8'))
        for profile in PROFILES:
            path=out/f'native-s{seed}-{profile}.recipe.json';write(path,recipe_for(control,profile))
            row=dict(name=path.name.removesuffix('.recipe.json'),suite='native',seed=seed,profile=profile,bots=300,steps=43200,seconds=1,warmup_ms=7200000,recipe=str(path.resolve()),recipe_sha256=sha(path))
            designs.append(row)
            if seed==41:runs.append({**row,'steps':14400})
    ensure_runtime_inputs(out,runs)
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=runs,all_seed_full_designs=designs,profiles=PROFILES,
        fixed='All cash/inventory, role counts, numeric coefficients, original intended-order size distributions, slippage bounds, TTL and all non-reactive/non-maker agents unchanged. Only ReactiveFlowTrader1.0->1.1 execution policy and the separately declared maker observation flags differ.',
        mechanism='Aggressive demand becomes a finite parent with unchanged sampled quantity, side and original price boundary. Each IOC child is capped to current displayed external touch liquidity and priced at that touch. Residual demand is retried on the next observation, without resampling. Only authoritative own taker fills reduce it; unfilled demand expires at the existing TTL. Passive demands retain legacy execution. Subsequent arrivals are deferred while a parent is live, so exact realized arrival counts and volumes are endogenous outcomes, not held equal.',
        ledger='Saved slicing demand_lots = filled_lots + expired_lots + current parent remaining. No submitted quantity is treated as filled. Real account budgets/STP/lot size remain binding.',
        pilot='All five seed41 profiles run4h; primary fixed hours2–4, exploratory only. New-binary legacy control must byte-match the independently executed old control before candidate claims.',
        full='All20 recipes across four seeds frozen before any execution-policy pilot result. A promising mechanism must complete12h on every seed, fixed primary hours2–12. No numerical retuning or suppressing bad seeds.',
        analysis_scope='Fixed seed41 four-hour mechanism pilot, primary hours2–4. Observation/execution ablations with all numerical recipe inputs held fixed. Actual arrival counts, fills and prices can change endogenously. All frozen outcomes retained; a smaller range, less activity or stronger trend alone is not a wick correction.',
        evaluation=['Actual1m/5m wick fractions and upper/lower lengths, both-long incidence, coverage','Return ACF, tails and sampled quote-mid movement; no smoothing or trade filtering','Actual volume, active accounts, two-sided quotes, original demand filled/expired/residual ledger','Spot/perp basis tails and linkage; no acceptance on volatility reduction alone'],
        limits='Seeds previously examined; no independent holdout. One-second clock and wave observations retained. Touch IOC can still fill better prices posted before its turn, and no inter-order-book atomicity is introduced.'))
    print('Frozen20 full-design recipes and five execution pilot runs',flush=True)


def run():
    out=BASE/'pilot-execution';p=json.loads((out/'plan.json').read_text(encoding='utf-8'));ensure_runtime_inputs(out,p['runs'])
    binary=out/'candidate.exe';assert sha(binary)==json.loads((out/'compiled-inputs.json').read_text(encoding='utf-8'))['binary_sha256']
    for row in p['runs']:
        r=run_one(out,row,binary,10800);assert r['complete']
        if row['profile']=='control':
            old=json.loads((BASE/'compatibility-old/native-s41-old.run.json').read_text(encoding='utf-8'));assert old['complete'] and r['receipt_sha256']==old['receipt_sha256']
            write(out/'compatibility.json',dict(old_and_new_control_byte_identical=True,steps=14400,receipt_sha256=r['receipt_sha256']))
        env=dict(os.environ,PYTHONPATH=str(ROOT/'.local/strategy-research-reservation-20261024/analysis-deps'))
        with (out/(row['profile']+'-analysis.log')).open('w',encoding='utf-8') as log:
            checked=subprocess.run([sys.executable,str(ROOT/'scripts/analyze_wick_pilot.py'),'--output',str(out)],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
        assert checked.returncode==0
    ensure_runtime_inputs(out,p['runs'])


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['plan','run']);a=p.parse_args();(plan if a.action=='plan' else run)()
