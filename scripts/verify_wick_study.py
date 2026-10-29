"""Verify frozen native inputs, demand accounting and archived evidence identity."""
import json,gc
from pathlib import Path
from analyze_market_mechanisms import ROOT,sha,write
from research_wick_observations import BASE,SOURCE


def verify():
    runs=[];sources=[];unstarted=[]
    for folder in ('pilot-observations','pilot-execution','pilot-style','full-sliced','full-dual-retry'):
        out=BASE/folder;p=json.loads((out/'plan.json').read_text(encoding='utf-8'))
        inputs=json.loads((out/'compiled-inputs.json').read_text(encoding='utf-8'));assert sha(out/'candidate.exe')==inputs['binary_sha256']
        for f in inputs['files']:
            assert sha(out/'compiled-source'/f['path'])==f['sha256']
            if folder=='full-dual-retry':assert sha(ROOT/f['path'])==f['sha256'],'Current compiled source drift: '+f['path']
        sources.append(dict(folder=folder,binary_sha256=inputs['binary_sha256'],compiled_files_verified=len(inputs['files'])))
        for row in p['runs']:
            done=out/(row['name']+'.run.json')
            if not done.exists() and folder=='full-sliced' and row['seed']in (19,73):
                assert not (out/(row['name']+'.receipt.json')).exists()
                unstarted.append(dict(folder=folder,name=row['name'],seed=row['seed'],reason='Entire reactive-only candidate rejected before this run started.'));continue
            r=json.loads(done.read_text(encoding='utf-8'));assert r['complete'] and r['exit_code']==0
            assert r['inputs']['recipe_sha256']==row['recipe_sha256']==sha(Path(row['recipe']))
            assert r['inputs']['binary_sha256']==inputs['binary_sha256']
            assert r['inputs']['steps']==row['steps'] and r['inputs']['seconds']==1
            assert sha(out/(row['name']+'.receipt.json'))==r['receipt_sha256']
            runs.append(dict(folder=folder,name=row['name'],steps=row['steps'],receipt_sha256=r['receipt_sha256']))
    old=json.loads((BASE/'compatibility-old/native-s41-old.run.json').read_text(encoding='utf-8'));assert old['complete']
    assert old['receipt_sha256']==sha(BASE/'compatibility-old/native-s41-old.receipt.json')
    for folder in ('pilot-observations','pilot-execution','pilot-style'):
        r=json.loads((BASE/folder/'native-s41-control.run.json').read_text(encoding='utf-8'));assert r['receipt_sha256']==old['receipt_sha256']
    a=json.loads((BASE/'pilot-execution/native-s41-sliced.run.json').read_text(encoding='utf-8'));b=json.loads((BASE/'pilot-style/native-s41-sliced.run.json').read_text(encoding='utf-8'));assert a['receipt_sha256']==b['receipt_sha256']
    stopped=json.loads((BASE/'phase1-launcher-stop-result.json').read_text(encoding='utf-8-sig'));assert stopped['native_processes_cancelled']==0 and len(unstarted)==2
    failed=BASE/'full-dual-execution';interrupted=json.loads((failed/'native-s41-dual-execution.run.json').read_text(encoding='utf-8'))
    failed_inputs=json.loads((failed/'compiled-inputs.json').read_text(encoding='utf-8'))
    for f in failed_inputs['files']:assert sha(failed/'compiled-source'/f['path'])==f['sha256']
    assert not interrupted['complete'] and interrupted['exit_code']==4294967295
    assert (failed/'native-s41-dual-execution.receipt.json').stat().st_size==0
    oldplan=json.loads((failed/'plan.json').read_text(encoding='utf-8'));retryplan=json.loads((BASE/'full-dual-retry/plan.json').read_text(encoding='utf-8'))
    assert sha(failed/'candidate.exe')==sha(BASE/'full-dual-retry/candidate.exe')==interrupted['inputs']['binary_sha256']
    for a,b in zip(oldplan['runs'],retryplan['runs']):
        assert {k:v for k,v in a.items()if k!='recipe'}=={k:v for k,v in b.items()if k!='recipe'}
        assert sha(Path(a['recipe']))==sha(Path(b['recipe']))==a['recipe_sha256']
    for seed in (7,19,73):assert not (failed/f'native-s{seed}-dual-execution.run.json').exists()
    # Every original account/market/config remains identical after removing only
    # the explicit version/observation changes; compare to the archived control.
    from research_wick_style import recipe_for
    p=json.loads((BASE/'full-dual-retry/plan.json').read_text(encoding='utf-8'))
    for row in p['runs']:
        control=json.loads((SOURCE/f"native-s{row['seed']}-depth.recipe.json").read_text(encoding='utf-8'))
        candidate=json.loads(Path(row['recipe']).read_text(encoding='utf-8'));assert candidate==recipe_for(control,row['profile'])
    # Exact4h candidate prefixes, including every trade and book sample.
    prefixes=[]
    for pilot,full,name in [('pilot-execution','full-sliced','native-s41-sliced'),('pilot-style','full-dual-retry','native-s41-dual-execution')]:
        short_path=BASE/pilot/(name+'.receipt.json');full_path=BASE/full/(name+'.receipt.json')
        short=json.loads(short_path.read_text(encoding='utf-8'));long=json.loads(full_path.read_text(encoding='utf-8'));end=short['simulation_time_ms'];checks={}
        for key in ('trade_receipts','book_snapshots'):
            prefix=[r for r in long[key] if r['time_ms']<=end];assert prefix==short[key];checks[key]=dict(equal=True,rows=len(prefix))
        assert short['assets_before']==long['assets_before'];del short,long;gc.collect()
        item=dict(short_receipt_sha256=sha(short_path),full_receipt_sha256=sha(full_path),end_ms=end,checks=checks,assets_before_equal=True);prefixes.append(dict(folder=full,**item))
        write(BASE/full/'seed41-pilot-prefix.json',item)
    ledger=json.loads((BASE/'full-dual-retry/demand-reconciliation.json').read_text(encoding='utf-8'))
    assert len(ledger['records'])==8
    for r in ledger['records']:
        assert r['actual_minus_child_fills']==0
        assert r['demand_lots']==r['filled_lots']+r['expired_lots']+r['pending_lots']
        for a in r['details']:assert a['actual_minus_child_fills']==0
    metrics=json.loads((BASE/'full-dual-retry/full-comparison.json').read_text(encoding='utf-8'));assert metrics['complete']
    checked=[]
    for m in metrics['metrics']:
        if m['profile']!='dual-execution':continue
        assert m['integrity']['count_matches'] and m['integrity']['self_trades']==0
        for stage in ('before','after'):assert not m['asset_checks'][stage]['negative_available']
        assert m['asset_checks']['total_deltas']['V']==0
        checked.append(dict(seed=m['seed'],unique_trades=m['integrity']['unique_trades'],self_trades=0,negative_available_at_endpoints=False,spot_base_asset_delta=0))
    assert len(checked)==4
    archives={
        '2026-10-24-market-mechanisms-evidence.zip':'2e7df9279d0fcc5d067edb0d050ce0782d9f6a18ce7abf94b2be734cd065f93c',
        '2026-10-24-market-feedback-evidence.zip':'0e000afe851fba640d7f2eb4c0e326ed05865e4990771158c750e0fc5a55024e',
        '2026-10-25-market-causal-evidence.zip':'1b2201748423599962aeb8b7f7cf3fcf821e062fb98050a6a040ea5dc9ede9dd'}
    for name,digest in archives.items():assert sha(ROOT/'docs/validation'/name)==digest,name
    report=dict(native_runs_verified=len(runs)+1,simulated_hours=sum(r['steps']/3600 for r in runs)+4,
        native_attempts_verified=len(runs)+2,resource_interrupted_attempts=1,interrupted_attempt=dict(directory=str(failed),metadata=interrupted,last_logged_step=16400,exact_stop_step_unknown=True,empty_stdout_sha256=sha(failed/'native-s41-dual-execution.receipt.json'),identical_input_retry_verified=True),
        runs=runs,compiled_sources=sources,unstarted_rejected_candidate_runs=unstarted,old_and_three_new_control4h_byte_identical=True,reactive_only4h_across_style_binary_byte_identical=True,candidate4h_12h_prefix=prefixes,
        full_candidate_account_demand_ledgers_reconcile=True,old_archives_sha256=archives,
        endpoint_accounting_and_trade_checks=checked,
        verifier_sha256=sha(Path(__file__)),scope='Raw native receipts/metadata and frozen compiled inputs verified without mutation.21 native attempts:20 complete, one resource interruption. Successful runs comprise56h of four-hour controls/screens plus72h of six full candidates,128h total: two rejected reactive-only seeds and all four final dual-execution seeds. The interrupted fragment is not counted in successful hours. Two unstarted reactive-only designs are explicitly rejected, not successes. The interrupted dual run and its three unstarted designs are superseded by an identical-input four-seed retry in a fresh directory. Archived four12h depth controls are reused and not counted as newly simulated. New-binary legacy-control equality is only proven through4h. All final outcomes and demand ledgers retained.')
    write(BASE/'verification.json',report);print(json.dumps({k:v for k,v in report.items() if k not in ('runs','compiled_sources')},indent=2))


if __name__=='__main__':verify()
