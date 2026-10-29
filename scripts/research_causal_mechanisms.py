"""Freeze the anomaly-driven hedge and public-liquidity mechanism ablations."""
from __future__ import annotations
import argparse, copy, json, subprocess
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT, sha, write, ensure_runtime_inputs

DEFAULT_SOURCE=ROOT/'.local/strategy-research-reservation-20261024'
DEFAULT_DIAGNOSTICS=ROOT/'.local/strategy-research-causal-20261025'
DEFAULT_OUT=DEFAULT_DIAGNOSTICS/'full-study'


def recipe_for(control, profile):
    assert profile in ('control','hedged','depth')
    candidate=copy.deepcopy(control)
    if profile=='control':return candidate
    for a in candidate['agents']:
        p=a['Plugin']
        if p['plugin_id']=='BasisLinkTrader':
            assert p['plugin_version']=='1.0.0';p['plugin_version']='1.2.0'
        if p['plugin_id']=='AdaptiveLiquidityMaker':
            assert p['config'].get('reservation_innovations') is True
            if profile=='depth':p['config']['market_depth_flow']=True
    # Assert an exact structural ablation, including all capital/tasks/signals.
    reverted=copy.deepcopy(candidate)
    for a in reverted['agents']:
        p=a['Plugin']
        if p['plugin_id']=='BasisLinkTrader':p['plugin_version']='1.0.0'
        if p['plugin_id']=='AdaptiveLiquidityMaker':p['config'].pop('market_depth_flow',None)
    assert reverted==control
    return candidate


def plan(source,out,diagnostics=DEFAULT_DIAGNOSTICS):
    out.mkdir(parents=True,exist_ok=True)
    assert not (out/'plan.json').exists(),'Plan is frozen; use a fresh directory'
    prior=json.loads((source/'basis-comparison.json').read_text(encoding='utf-8'))
    assert prior['complete'] and len(prior['pairs'])==4
    selection=json.loads((diagnostics/'selection.json').read_text(encoding='utf-8'))
    controls=[];runs=[]
    for seed in (41,7,19,73):
        name=f'native-s{seed}-reservation'
        recipe=source/(name+'.recipe.json');receipt=source/(name+'.receipt.json');metrics=source/(name+'.metrics.json')
        run=json.loads((source/(name+'.run.json')).read_text(encoding='utf-8'))
        assert run['complete'] and sha(recipe)==run['inputs']['recipe_sha256'] and sha(receipt)==run['receipt_sha256']
        control=json.loads(recipe.read_text(encoding='utf-8'))
        controls.append(dict(seed=seed,name=name,recipe=str(recipe),recipe_sha256=sha(recipe),receipt=str(receipt),receipt_sha256=sha(receipt),metrics=str(metrics),metrics_sha256=sha(metrics)))
        for profile in (('control','hedged','depth') if seed==41 else ('hedged','depth')):
            label=f'native-s{seed}-{profile}';path=out/(label+'.recipe.json');write(path,recipe_for(control,profile))
            if profile=='control':assert sha(path)==sha(recipe)
            runs.append(dict(name=label,suite='native',seed=seed,profile=profile,bots=300,steps=43200,seconds=1,warmup_ms=7200000,recipe=str(path),recipe_sha256=sha(path)))
    compiled=[ROOT/p for p in ('Cargo.lock','exchange-core/src/adaptive_liquidity.rs','exchange-core/src/basis_link.rs','exchange-core/examples/strategy_market.rs','exchange-core/examples/support/research_trace.rs','exchange-server/examples/strategy_ecosystem_market.rs')]
    write(out/'compiled-inputs.json',dict(recorded_at=datetime.now().astimezone().isoformat(),files=[dict(path=str(p.relative_to(ROOT)),sha256=sha(p)) for p in compiled],binary_sha256=sha(out/'candidate.exe')))
    ensure_runtime_inputs(out,runs)
    write(out/'plan.json',dict(frozen_at=datetime.now().astimezone().isoformat(),head=subprocess.check_output(['git','-c','safe.directory='+ROOT.as_posix(),'rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        source_plan_sha256=sha(source/'plan.json'),diagnostic_selection_sha256=sha(diagnostics/'selection.json'),diagnostic_trace_sha256=sha(diagnostics/'trace-s41-reservation.trace.jsonl'),
        profiles=dict(control='Prior reservation candidate; midpoint basis 1.0.0',hedged='Only basis -> 1.2.0: executable pair, actual hedge gating, both legs at observed touch and actual funded depth; hedge no longer requires unrelated opposite quotes',depth='Same 1.2.0 basis, plus maker market_depth_flow=true: normalize new flow by visible external liquidity within existing quote reach'),
        controls=controls,runs=runs,compiled_inputs_sha256=sha(out/'compiled-inputs.json'),
        fixed=['All numeric inputs, funded accounts, initial inventory, signals, role allocation and task schedules','300 native bots, seeds 41/7/19/73, 12h, 1s, primary [2h,12h)','Prior reservation-innovation maker policy stays on in both candidate profiles'],
        evaluation=['All seeds and failures retained','Return and quote-mid lag-1 autocorrelation; wick/range morphology; no acceptance from volatility reduction alone','Return tails, two-sided availability, spread/depth, role activity and actual capital identities','Spot/perp aligned return correlation and close-basis excursions','Same selected time window traces: actual entries, residuals, hedges, liquidation, quote decomposition'],
        compatibility='Full 12h seed-41 control under this binary must reproduce its archived reservation receipt byte for byte; trace on/off smoke must match all fields',
        diagnostics=dict(steps=selection['steps'],windows=f"{selection['start_ms']}:{selection['end_ms']}",profiles=['hedged','depth'],scope='Exact same absolute time window and signals, but endogenous paths diverge. No retuning or reselection after seeing these diagnostics.'),
        scope='Anomaly-selected investigation, same previously examined four seeds; not an unseen holdout. Hedged combines the previous maker hypothesis with a corrected basis mechanism; depth is an incremental maker ablation. Numeric max_slippage remains in the frozen legacy config; 1.2 IOC prices use actual observed touch and cannot sweep past it, replacing its midpoint band. This does not guarantee the touch remains available or reserve cross-market capital. No price-path injection or chart smoothing.'))
    print('FROZEN',len(runs),'runs',flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--source',type=Path,default=DEFAULT_SOURCE);p.add_argument('--output',type=Path,default=DEFAULT_OUT);p.add_argument('--diagnostics',type=Path,default=DEFAULT_DIAGNOSTICS)
    a=p.parse_args();plan(a.source.resolve(),a.output.resolve(),a.diagnostics.resolve())
