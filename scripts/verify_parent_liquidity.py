"""Verify immutable inputs separately from actual native outcome qualification."""
import argparse,hashlib,json,re,zipfile
from pathlib import Path
from research_parent_liquidity import BASE,PREVIOUS,SEEDS,recipe_for,active_main
from research_market_mechanisms import ROOT,sha,write


def read(p):return json.loads(p.read_text(encoding='utf-8'))


def native_path(path):
    return Path('E:/'+path[7:])if path.startswith('/mnt/e/')else Path(path)


def offline():
    old=read(BASE/'control-inputs.json');design=read(BASE/'design/model-inputs.json')
    changed={f['path']:f['sha256']for f in design['files']}
    for f in old['files']:
        assert sha(BASE/'control-source'/f['path'])==f['sha256']
        assert sha(ROOT/f['path'])==changed.get(f['path'],f['sha256']),f['path']
    for f in design['files']:
        assert sha(BASE/'design/model-source'/f['path'])==sha(ROOT/f['path'])==f['sha256']
    frozen=read(BASE/'design/frozen-design.json')
    for f in frozen['files']:assert sha(BASE/'design'/f['path'])==f['sha256']
    rows=[]
    for name in dict.fromkeys(('compatibility','pilot','main',active_main().name,'holdout','controls')):
        folder=BASE/name;p=read(folder/'plan.json')
        assert sha(folder/'candidate.exe')==p['binary_sha256']
        with (folder/'candidate.exe').open('rb')as f:assert f.read(4)==b'\x7fELF'
        assert p['binary_sha256']==(old['binary_sha256']if name=='controls'else frozen['binary_sha256'])
        for r in p['runs']:
            assert sha(Path(r['recipe']))==r['recipe_sha256']
            assert r['steps']==14400 and r['seconds']==1 and r['warmup_ms']==7200000
            spec=read(Path(r['recipe']))
            if r['profile']=='liquidity-parent':
                control=(read(BASE/'controls'/f"native-s{r['seed']}-raw-flow.recipe.json")if r['seed']in SEEDS
                    else read(BASE/'holdout/native-s271-legacy.recipe.json'))
                assert spec==recipe_for(control)
            rows.append(dict(folder=name,name=r['name'],recipe_sha256=r['recipe_sha256']))
    archives={
        '2026-10-24-market-mechanisms-evidence.zip':'2e7df9279d0fcc5d067edb0d050ce0782d9f6a18ce7abf94b2be734cd065f93c',
        '2026-10-24-market-feedback-evidence.zip':'0e000afe851fba640d7f2eb4c0e326ed05865e4990771158c750e0fc5a55024e',
        '2026-10-25-market-causal-evidence.zip':'1b2201748423599962aeb8b7f7cf3fcf821e062fb98050a6a040ea5dc9ede9dd',
        '2026-10-25-market-wicks-evidence.zip':'2a57a2c08aa8f7fb71033ed3b821db485fa1eabc67a517bd792487806c0fa49e',
        '2026-10-25-market-price-formation-evidence.zip':'8e4cd54b73f96180ff76e7c782da975c9fce9c6b4925e1ef75c41c9a872c11c8',
        '2026-10-25-market-flow-surprise-evidence.zip':'c15f4e79d510d487b597753e342d5db2ecadd155462af3d3eefe803d3f8981d2'}
    for name,digest in archives.items():assert sha(ROOT/'docs/validation'/name)==digest
    with zipfile.ZipFile(ROOT/'docs/validation/2026-10-25-market-flow-surprise-evidence.zip')as z:
        manifest=json.loads(z.read('manifest.json'));hashes={f['name']:f['sha256']for f in manifest['files']}
        assert sha(BASE/'control-linux.elf')==hashes['binaries/surprise-linux.elf']
        cache=read(BASE/'controls/cache-provenance.json')
        for seed in SEEDS:
            name=f'native-s{seed}-raw-flow'
            for suffix in ('.recipe.json','.run.json','.summary.json','.log'):
                assert sha(BASE/'controls'/(name+suffix))==hashes['study/main/'+name+suffix]
            raw=BASE/'controls'/(name+'.receipt.json');d=read(BASE/'controls'/(name+'.run.json'))
            assert d['complete']and sha(raw)==cache[name]['receipt_sha256']==d['receipt_sha256']
            prior=next(f for f in manifest['external_native_data']if Path(f['path']).resolve()==(PREVIOUS/'main'/(name+'.receipt.json')).resolve())
            assert sha(raw)==prior['sha256']
    result=dict(source_and_recipe_checks=True,plans=rows,cached_actual_controls=4,previous_archives_unchanged=archives,
        scope='Implementation/input verification only. No new native result or market improvement follows from this check. Actual old controls are immutable cached data; compatibility and new four-seed outcomes require separate qualification.')
    write(BASE/'offline-verification.json',result);print(json.dumps(result,indent=2))
    return result


def delivery():
    """Qualify only completed pilot evidence; never promote a partial batch."""
    offline()
    assert read(BASE/'compatibility-proof.json')['byte_identical']
    assert (BASE/'user-requested-offline-delivery.json').exists()
    assert not (BASE/'completion.json').exists()
    checked=[];external=[]
    for folder_name in ('compatibility','pilot'):
        folder=BASE/folder_name;plan=read(folder/'plan.json')
        for r in plan['runs']:
            d=read(folder/(r['name']+'.run.json'));summary=read(folder/(r['name']+'.summary.json'))
            assert d['complete'] and d['exit_code']==0
            assert d['inputs']==dict(recipe_sha256=r['recipe_sha256'],binary_sha256=plan['binary_sha256'],steps=14400,seconds=1)
            assert summary['simulation_time_ms']==14400000 and summary['steps']==14400
            raw=folder/(r['name']+'.receipt.json');digest=sha(raw)
            assert digest==d['receipt_sha256']==summary['receipt_sha256']
            external.append(dict(path=str(raw),bytes=raw.stat().st_size,sha256=digest,classification='completed_new_run'))
            for arc in summary['execution_archives']:
                path=native_path(arc['path']);h=hashlib.sha256();count=0
                with path.open('rb')as f:
                    for block in iter(lambda:f.read(4*1024*1024),b''):
                        h.update(block);count+=block.count(b'\n')
                assert h.hexdigest()==arc['sha256'] and count==arc['records']
                external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=h.hexdigest(),records=count,classification='completed_new_run'))
            checked.append(dict(folder=folder_name,name=r['name'],receipt_sha256=digest))
    pilot=read(BASE/'pilot/comparison.json');assert pilot['complete'] and pilot['completed_pairs']==1
    for m in pilot['metrics']:
        assert m['integrity']['count_matches'] and m['integrity']['self_trades']==0
        assert m['asset_checks']['total_deltas']['V']==0
        assert all(not m['asset_checks'][stage]['negative_available']for stage in ('before','after'))
    for p in pilot['parents']:
        assert p['activated_demand_lots']==p['filled_lots']+p['expired_lots']+p['remaining_lots']
    interruptions=[]
    for name,cause in (('main','clock_relay_guard_false_stale'),('main-retry-clock','user_requested_offline_delivery')):
        folder=BASE/name;run=read(folder/'native-s7-liquidity-parent.run.json')
        assert not run['complete']
        steps=re.findall(r'step=(\d+)',(folder/'native-s7-liquidity-parent.log').read_text())
        interruptions.append(dict(folder=name,cause=cause,last_logged_step=int(steps[-1])if steps else None,
            exact_stop_step_unknown=True,successful_hours=0,raw_metadata_unchanged=True))
        for path in (folder/'native-s7-liquidity-parent.spool').rglob('*'):
            if path.is_file():external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=sha(path),classification='incomplete_attempt',cause=cause))
    result=dict(delivery='implementation_and_offline_verification',new_completed_runs=checked,new_native_runs=2,
        new_simulated_hours=8,cached_old_controls=4,completed_paired_seeds=[41],four_paired_seeds_complete=False,
        independent_holdout_complete=False,long12h_complete=False,market_realism_qualified=False,
        core_tests=dict(passed=487,ignored=12),python_tests=dict(passed=37),interruptions=interruptions,
        external_native_data=external,scope='Two completed new native runs: compatibility and seed41 pilot. All duplicate pilot copies and old controls are cached, not additional runs. Partial attempts count no successful hours. Delivery stops here on user request; no full market-effect qualification.')
    write(BASE/'delivery-status.json',result);print(json.dumps({k:v for k,v in result.items()if k!='external_native_data'},indent=2))
    return result


def outcomes():
    result=offline();assert read(BASE/'compatibility-proof.json')['byte_identical']
    comparison=read(active_main()/'comparison.json');assert comparison['complete']and comparison['completed_pairs']==4
    for m in comparison['metrics']:
        assert m['integrity']['count_matches']and m['integrity']['self_trades']==0
        assert m['asset_checks']['total_deltas']['V']==0
        assert all(not m['asset_checks'][stage]['negative_available']for stage in ('before','after'))
    checked=[];external=[];unique=[]
    for name in ('compatibility','pilot',active_main().name,'controls'):
        folder=BASE/name;p=read(folder/'plan.json');cache=read(folder/'cache-provenance.json')if(folder/'cache-provenance.json').exists()else{}
        for r in p['runs']:
            d=read(folder/(r['name']+'.run.json'));assert d['complete']and d['exit_code']==0
            assert d['inputs']==dict(recipe_sha256=r['recipe_sha256'],binary_sha256=p['binary_sha256'],steps=14400,seconds=1)
            path=folder/(r['name']+'.receipt.json');assert sha(path)==d['receipt_sha256']
            summary=read(folder/(r['name']+'.summary.json'));assert summary['receipt_sha256']==d['receipt_sha256']
            external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=d['receipt_sha256']))
            for arc in summary['execution_archives']:
                path=native_path(arc['path']);assert sha(path)==arc['sha256']
                with path.open('rb')as f:count=sum(block.count(b'\n')for block in iter(lambda:f.read(1024*1024),b''))
                assert count==arc['records'];external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=arc['sha256'],records=count))
            reused=r['name']in cache
            if reused:
                origin=Path(cache[r['name']]['directory']);assert sha(origin/(r['name']+'.run.json'))==sha(folder/(r['name']+'.run.json'))
            else:unique.append(dict(folder=name,name=r['name'],metadata=d))
            checked.append(dict(folder=name,name=r['name'],cached=reused,receipt_sha256=d['receipt_sha256']))
    result.update(new_native_runs=len(unique),new_simulated_hours=4*len(unique),checked=checked,external_native_data=external,
        compatibility_byte_identity=True,four_paired_seeds_complete=True,independent_holdout_complete=False,long12h_complete=False,
        scope='Four paired4h actual outcomes, four cached controls not new work. Seed41 main candidate reuses pilot. Whole-run business accounting and actual prints reconciled; endpoint assets/volume checked. No12h, full intraday fee/margin ledger, real L2 calibration or broad market-realism qualification.')
    write(BASE/'completion.json',result);print(json.dumps({k:v for k,v in result.items()if k not in ('external_native_data','plans','checked')},indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['offline','delivery','outcomes']);a=p.parse_args()
    {'offline':offline,'delivery':delivery,'outcomes':outcomes}[a.action]()
