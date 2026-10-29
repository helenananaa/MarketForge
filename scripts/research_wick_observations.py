"""Frozen observation-mechanism ablations for systematic candle wicks."""
import argparse,copy,json,shutil,sys,os,subprocess
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write,ensure_runtime_inputs,run_one

BASE=ROOT/'.local/strategy-research-wicks-20261025'
SOURCE=ROOT/'.local/strategy-research-causal-20261025/full-study'
PROFILES={'control':(), 'penetration':('touch_excursion_calibration',),
          'candleflow':('closed_candle_flow',),'both':('touch_excursion_calibration','closed_candle_flow')}


def recipe_for(control,profile):
    assert profile in PROFILES
    candidate=copy.deepcopy(control)
    for a in candidate['agents']:
        p=a['Plugin']
        if p['plugin_id']=='AdaptiveLiquidityMaker':
            assert p['plugin_version']=='1.1.0' and p['config']['market_depth_flow'] and p['config']['reservation_innovations']
            assert all(k not in p['config'] for k in ('touch_excursion_calibration','closed_candle_flow'))
            for key in PROFILES[profile]:p['config'][key]=True
    restored=copy.deepcopy(candidate)
    for a in restored['agents']:
        if a['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
            for key in PROFILES[profile]:a['Plugin']['config'].pop(key)
    assert restored==control
    return candidate


def plan():
    out=BASE/'pilot-observations';assert not out.exists();out.mkdir()
    binary=ROOT/'target/release/examples/strategy_ecosystem_market.exe'
    shutil.copyfile(binary,BASE/'candidate-observations.exe');shutil.copyfile(binary,out/'candidate.exe')
    designs=[];runs=[]
    for seed in (41,7,19,73):
        source=SOURCE/f'native-s{seed}-depth.recipe.json';control=json.loads(source.read_text(encoding='utf-8'))
        for profile in PROFILES:
            path=out/f'native-s{seed}-{profile}.recipe.json';write(path,recipe_for(control,profile))
            if profile=='control':assert sha(path)==sha(source)
            item=dict(name=f'native-s{seed}-{profile}',suite='native',seed=seed,profile=profile,bots=300,steps=43200,seconds=1,warmup_ms=7200000,recipe=str(path.resolve()),recipe_sha256=sha(path))
            designs.append(item)
            if seed==41:runs.append({**item,'steps':14400})
    compiled=[];snapshot=out/'compiled-source'
    for name in ('Cargo.lock','exchange-core/src/adaptive_liquidity.rs','exchange-core/src/basis_link.rs','exchange-core/examples/strategy_market.rs','exchange-core/examples/support/research_trace.rs','exchange-server/examples/strategy_ecosystem_market.rs','scripts/strategy_ecosystem.py'):
        src=ROOT/name;target=snapshot/name;target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,target);compiled.append(dict(path=name,sha256=sha(src)))
    write(out/'compiled-inputs.json',dict(files=compiled,binary_sha256=sha(out/'candidate.exe')))
    ensure_runtime_inputs(out,runs)
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),runs=runs,all_seed_full_designs=designs,
        old_control=dict(directory=str(BASE/'compatibility-old'),name='native-s41-old',binary_sha256=sha(BASE/'control.exe'),recipe_sha256=sha(SOURCE/'native-s41-depth.recipe.json'),steps=14400),
        profiles=PROFILES,
        fixed='All numerical coefficients, spread limits, fees, initial cash/inventory, quantity/depth, schedules and all non-maker agents unchanged; only the two explicit observation booleans differ.',
        calibration='Penetration changes the estimator coordinate from distance to previous reference mid to excursion beyond the previous actual bid/ask. It is an experimental competitive-arrival model, not claimed algebraically equivalent to the old GLFT calibration. Midpoint volatility, risk coefficients and quote formulas are not changed.',
        flow='Closed-candle mode consumes complete public one-second aggressor quantities once, with close<=observation time, instead of the last32 tape. It deliberately waits for closure, skips unknown direction, and does not read a future/current partial candle or private fills.',
        pilot='All four seed41 profiles run4h; fixed primary hours2–4. The old and new binary control receipts must match all bytes before interpreting candidates. This is an exploratory screen, not final qualification.',
        full='All four seeds and all four profile recipes frozen now, before pilot candidate results. A promising mechanism must subsequently complete12h on all four seeds, primary hours2–12; failed hypotheses and runs are retained. No numerical retuning or window shifting.',
        evaluation=['1m/5m upper/lower wick lengths and ratios, both-wick frequencies, body/range and nonempty coverage','Sampled quote-mid shape and return ACF; true trade OHLCV, not rendered modification','Return std/tails, directional persistence, activity, actual volume and two-sided availability','Basis tails and spot/perp linkage; a smaller price range or increased trend alone is insufficient'],
        scope='Previously examined seeds; no independent holdout. Historical eight10h windows per market descriptive, not confidence bands. Existing failed inventory-level hypothesis remains off; depth policy is the control.'))
    print('Frozen 4 pilot profiles and all 16 four-seed full recipes',flush=True)


def run():
    out=BASE/'pilot-observations';plan=json.loads((out/'plan.json').read_text(encoding='utf-8'));ensure_runtime_inputs(out,plan['runs'])
    binary=out/'candidate.exe';assert sha(binary)==json.loads((out/'compiled-inputs.json').read_text(encoding='utf-8'))['binary_sha256']
    for row in plan['runs']:
        result=run_one(out,row,binary,10800);assert result['complete']
        if row['profile']=='control':
            old_path=BASE/'compatibility-old/native-s41-old.run.json';assert old_path.is_file(),'Complete the independently frozen old-binary control first'
            old=json.loads(old_path.read_text(encoding='utf-8'));assert old['complete']
            assert result['receipt_sha256']==old['receipt_sha256'],'Legacy behavior changed; stop before candidate claims'
            write(out/'compatibility.json',dict(old_and_new_control_byte_identical=True,receipt_sha256=result['receipt_sha256'],steps=14400))
        env=dict(os.environ,PYTHONPATH=str(ROOT/'.local/strategy-research-reservation-20261024/analysis-deps'))
        with (out/(row['profile']+'-analysis.log')).open('w',encoding='utf-8') as log:
            checked=subprocess.run([sys.executable,str(ROOT/'scripts/analyze_wick_pilot.py'),'--output',str(out)],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
        assert checked.returncode==0,'Inspect '+row['profile']+'-analysis.log'
    ensure_runtime_inputs(out,plan['runs'])


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['plan','run']);a=p.parse_args();(plan if a.action=='plan' else run)()
