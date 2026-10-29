"""Verify this study without rewriting any previous experiment evidence."""
import argparse,json,itertools,zipfile
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write
from research_price_formation import BASE,PREVIOUS,recipe_for
from analyze_wick_basis_capacity import records,one
from analyze_wick_demand import analyze as demand


def verify(full):
    checked=[]
    prior_controls=[]
    with zipfile.ZipFile(ROOT/'docs/validation/2026-10-25-market-wicks-evidence.zip') as z:
        manifest=json.loads(z.read('MANIFEST.json'))
        hashes={r['path']:r['sha256']for r in manifest['entries']}
        for seed in (41,7,19,73):
            name=f'native-s{seed}-dual-execution'
            d=json.loads((PREVIOUS/(name+'.run.json')).read_text(encoding='utf-8'))
            assert d['complete']and sha(PREVIOUS/(name+'.receipt.json'))==d['receipt_sha256']
            for suffix in ('.run.json','.metrics.json','.recipe.json','-V-BTC-SPOT.candles.csv','-V-BTC-PERP.candles.csv'):
                assert sha(PREVIOUS/(name+suffix))==hashes['study/full-dual-retry/'+name+suffix]
            prior_controls.append(dict(seed=seed,receipt_sha256=d['receipt_sha256'],archived_exports_unchanged=True))
        for name in ('full-comparison.json','basis-capacity-audit-v2.json'):
            assert sha(PREVIOUS/name)==hashes['study/full-dual-retry/'+name]
    short=full.name.startswith('short-');linux=full.name.startswith('linux-')or short
    holdout=BASE/('short-holdout'if short else'linux-holdout'if linux else'holdout')
    controls=BASE/('short-controls'if short else'linux-controls-bounded')if linux else None
    folders=[BASE/'diagnostic-control',BASE/'pilot']+([BASE/'linux-compatibility',controls]if linux else[])+[full]+([holdout]if holdout.exists()else[])
    for out in folders:
        p=json.loads((out/'plan.json').read_text(encoding='utf-8'));assert sha(out/'candidate.exe')==p['binary_sha256']
        if (out/'compiled-inputs.json').exists():
            inputs=json.loads((out/'compiled-inputs.json').read_text(encoding='utf-8'))
            for f in inputs['files']:
                assert sha(out/'compiled-source'/f['path'])==f['sha256']
                assert sha(ROOT/f['path'])==f['sha256'],'Current frozen source changed: '+f['path']
        for r in p['runs']:
            d=json.loads((out/(r['name']+'.run.json')).read_text(encoding='utf-8'))
            assert d['complete'] and d['exit_code']==0
            assert d['inputs']==dict(recipe_sha256=r['recipe_sha256'],binary_sha256=p['binary_sha256'],steps=r['steps'],seconds=1)
            assert sha(Path(r['recipe']))==r['recipe_sha256']
            assert sha(out/(r['name']+'.receipt.json'))==d['receipt_sha256']
            if r['seed']==101:
                from strategy_ecosystem import population
                from research_market_mechanisms import options
                control,_=population(**options(101),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',
                    reservation_innovations=True,market_depth_flow=True,reactive_execution='sliced',style_execution='touch')
            else:control=json.loads((PREVIOUS/f"native-s{r['seed']}-dual-execution.recipe.json").read_text(encoding='utf-8'))
            expected=control if out.name=='diagnostic-control'else recipe_for(control,r['profile'])
            assert json.loads(Path(r['recipe']).read_text(encoding='utf-8'))==expected
            cached=p.get('cached_runs',{}).get(r['name'])
            if cached:
                origin=Path(cached['directory']);assert sha(origin/(r['name']+'.run.json'))==cached['run_sha256']==sha(out/(r['name']+'.run.json'))
                assert sha(origin/(r['name']+'.receipt.json'))==cached['receipt_sha256']==d['receipt_sha256']
            checked.append(dict(directory=str(out),name=r['name'],steps=r['steps'],receipt_sha256=d['receipt_sha256'],cached_reuse=bool(cached)))
    for key in ('observation-identity.json',):assert json.loads((BASE/'diagnostic-control'/key).read_text(encoding='utf-8'))['byte_identical']
    assert json.loads((BASE/'pilot/control-compatibility.json').read_text(encoding='utf-8'))['byte_identical']
    p=json.loads((full/'plan.json').read_text(encoding='utf-8'));seed41=next(r for r in p['runs']if r['seed']==41)
    name=seed41['name'];short_path=BASE/('linux-compatibility'if linux else'pilot')/(name+'.receipt.json');long=full/(name+'.receipt.json');prefix={}
    sentinel=object()
    for key in ('trade_receipts','book_snapshots'):
        a=records(short_path,key);b=(r for r in records(long,key)if r['time_ms']<=14400000);n=0
        for x,y in itertools.zip_longest(a,b,fillvalue=sentinel):assert x==y,key;n+=1
        prefix[key]=n
    if linux:
        for key in ('trade_receipts','book_snapshots'):
            a=records(BASE/'linux-compatibility/native-s41-control.receipt.json',key)
            b=(r for r in records(controls/'native-s41-control.receipt.json',key)if r['time_ms']<=14400000)
            n=0
            for x,y in itertools.zip_longest(a,b,fillvalue=sentinel):assert x==y,key;n+=1
            prefix['control_'+key]=n
    for out in ([controls]if controls else[])+[full]+([holdout]if holdout.exists()else[]):
        demand(out)
        ledger=json.loads((out/'demand-reconciliation.json').read_text(encoding='utf-8'))
        for r in ledger['records']:
            assert r['actual_minus_child_fills']==0
            for a in r['details']:assert a['actual_minus_child_fills']==0
        if out==controls:continue # control metrics are in the paired comparison
        metrics=json.loads((out/'comparison.json').read_text(encoding='utf-8'));assert metrics['complete']
        for m in metrics['metrics']+metrics.get('control_metrics',[]):
            assert m['integrity']['count_matches']and m['integrity']['self_trades']==0
            assert m['asset_checks']['total_deltas']['V']==0
            for stage in ('before','after'):assert not m['asset_checks'][stage]['negative_available']
    capacities=[]
    if controls:
        for r in json.loads((controls/'plan.json').read_text(encoding='utf-8'))['runs']:capacities.extend(one(controls,{**r,'profile':'dual-execution'}))
    for r in p['runs']:capacities.extend(one(full,r))
    write(full/'basis-capacity.json',dict(records=capacities,scope=f"Actual ordered fills, primary hours2–{p['runs'][0]['steps']/3600:g}, same existing bounded-memory v2 carry-in reconstruction. Unchanged finite capacity; cap occupancy is not every-missed-opportunity attribution."))
    archives={
        '2026-10-24-market-mechanisms-evidence.zip':'2e7df9279d0fcc5d067edb0d050ce0782d9f6a18ce7abf94b2be734cd065f93c',
        '2026-10-24-market-feedback-evidence.zip':'0e000afe851fba640d7f2eb4c0e326ed05865e4990771158c750e0fc5a55024e',
        '2026-10-25-market-causal-evidence.zip':'1b2201748423599962aeb8b7f7cf3fcf821e062fb98050a6a040ea5dc9ede9dd',
        '2026-10-25-market-wicks-evidence.zip':'2a57a2c08aa8f7fb71033ed3b821db485fa1eabc67a517bd792487806c0fa49e'}
    for name,digest in archives.items():assert sha(ROOT/'docs/validation'/name)==digest
    interrupted=None;linux_interrupted=None
    if linux:
        failed=BASE/'full-anchor-slope';d=json.loads((failed/'native-s41-anchor-slope.run.json').read_text(encoding='utf-8'))
        assert not d['complete']and d['exit_code']==1 and (failed/'native-s41-anchor-slope.receipt.json').stat().st_size==0
        interrupted=dict(directory=str(failed),metadata=d,last_logged_step=33100,exact_stop_step_unknown=True,successful_hours=0)
        for seed in (7,19,73):assert not (failed/f'native-s{seed}-anchor-slope.run.json').exists()
        for directory in ('linux-controls','linux-full','holdout'):assert not list((BASE/directory).glob('*.run.json'))
        if short:
            audit=BASE/'linux-controls-bounded/native-s41-control.failure-audit.json'
            linux_interrupted=json.loads(audit.read_text(encoding='utf-8'));assert not linux_interrupted['complete']
            raw=audit.with_name('native-s41-control.receipt.json');assert raw.stat().st_size==0 and sha(raw)==linux_interrupted['receipt_sha256']
            assert linux_interrupted['resource_stop_sample']['host_free_commit_kib']<3*1024*1024
            assert sha(BASE/'linux-runner-before-guard-repair.py')==linux_interrupted['launcher_before_repair_sha256']
            assert not list((BASE/'linux-full-bounded').glob('*.run.json'))
        platform=json.loads((BASE/'platform-compatibility.json').read_text(encoding='utf-8'))
        for r in platform['records']:assert all(r['non_state_checks'].values())
    unique=[r for r in checked if not r['cached_reuse']]
    if short:
        assert [r['seed']for r in p['runs']]==[41,7,19,73]
        assert len(unique)==15 and sum(r['steps']/3600 for r in unique)==68
        assert len(checked)-len(unique)==2
    write(BASE/'verification.json',dict(native_runs=len(unique),receipt_artifacts_verified=len(checked),cached_artifacts_verified=len(checked)-len(unique),native_attempts=len(unique)+(1 if interrupted else 0)+(1 if linux_interrupted else 0),simulated_hours=sum(r['steps']/3600 for r in unique),runs=checked,
        interrupted_windows_attempt=interrupted,main_control_directory=str(controls)if controls else str(PREVIOUS),main_candidate_directory=str(full),holdout_directory=str(holdout),
        interrupted_linux_attempt=linux_interrupted,short_horizon_only=short,primary_window_ms=[7200000,14400000 if short else 43200000],
        traced_seed7_full_byte_identity=True,new_binary_windows_control4h_byte_identity=True,candidate_seed41_path_check=prefix,candidate_seed41_check_scope='Cached4h artifacts verified, not4h/12h proof'if short else'4h/12h prefix',
        recipes_only_declared_boolean_flags=True,demand_ledgers_actual_prints_equal=True,endpoint_and_trade_integrity=True,
        previous_controls=prior_controls,original_archives=archives,verifier_sha256=sha(Path(__file__)),scope='Exact native receipts and frozen input identities; prior control exports checked against the immutable previous ZIP; finite endpoint assets and actual prints checked. No intraday full-fee/margin ledger, independent holdout or live/scale qualification.'))
    print(json.dumps(dict(native_runs=len(unique),simulated_hours=sum(r['steps']/3600 for r in unique),cached_artifacts=len(checked)-len(unique),prefix=prefix),indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);verify(p.parse_args().directory)
