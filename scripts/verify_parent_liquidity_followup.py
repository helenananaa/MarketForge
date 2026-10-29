"""Verify frozen source, completed short/long receipts and complete archives."""
import hashlib,json,re
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write
from research_parent_liquidity import BASE,SEEDS,active_main
from verify_parent_liquidity import outcomes,native_path


def read(p):return json.loads(p.read_text(encoding='utf-8'))


def verify():
    assert read(BASE/'long12h-complete.json')['complete']
    short=outcomes();long=read(BASE/'long12h-candidates-20261026/comparison.json')
    assert long['complete']and long['completed_pairs']==4
    protocol=read(BASE/'followup-20261026.json')
    amendments=[read(BASE/'followup-analysis-amendment-1.json'),read(BASE/'followup-relay-amendment-2.json')]
    for f in protocol['input_plans']:assert sha(Path(f['path']))==f['sha256']
    for f in protocol['harness']:
        amendment=next((a for a in amendments if a['path']==f['path']),None)
        expected=amendment['after_sha256']if amendment else f['sha256']
        assert sha(ROOT/f['path'])==expected,f['path']
    for amendment in amendments:
        assert amendment['before_sha256']==next(f['sha256']for f in protocol['harness']if f['path']==amendment['path'])
        if 'added_helper'in amendment:
            f=amendment['added_helper'];assert sha(ROOT/f['path'])==f['sha256']
    for m in long['metrics']:
        assert m['integrity']['count_matches']and m['integrity']['self_trades']==0
        assert m['asset_checks']['total_deltas']['V']==0
        assert all(not m['asset_checks'][stage]['negative_available']for stage in ('before','after'))
    for p in long['parents']:
        assert p['activated_demand_lots']==p['filled_lots']+p['expired_lots']+p['remaining_lots']
    assert len(long['prefixes'])==8 and all(p['byte_value_identical']for p in long['prefixes'])
    external=list(short['external_native_data']);checked=[]
    for folder_name in ('long12h-controls-20261026','long12h-candidates-20261026'):
        folder=BASE/folder_name;plan=read(folder/'plan.json')
        original=BASE/('controls'if 'controls'in folder_name else'main')
        previous=read(original/'plan.json')
        for r in plan['runs']:
            old=next(p for p in previous['runs']if p['seed']==r['seed'])
            assert r['recipe_sha256']==old['recipe_sha256']==sha(Path(r['recipe']))
            assert r['steps']==43200 and r['seconds']==1 and r['warmup_ms']==7200000
            d=read(folder/(r['name']+'.run.json'));summary=read(folder/(r['name']+'.summary.json'))
            assert d['complete']and d['exit_code']==0
            assert d['inputs']==dict(recipe_sha256=r['recipe_sha256'],binary_sha256=plan['binary_sha256'],steps=43200,seconds=1)
            assert summary['steps']==43200 and summary['simulation_time_ms']==43200000
            path=folder/(r['name']+'.receipt.json');assert sha(path)==d['receipt_sha256']==summary['receipt_sha256']
            external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=d['receipt_sha256']))
            for arc in summary['execution_archives']:
                path=native_path(arc['path']);h=hashlib.sha256();count=0
                with path.open('rb')as f:
                    for block in iter(lambda:f.read(4*1024*1024),b''):h.update(block);count+=block.count(b'\n')
                assert h.hexdigest()==arc['sha256']and count==arc['records']
                external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=h.hexdigest(),records=count))
            checked.append(dict(folder=folder_name,name=r['name'],receipt_sha256=d['receipt_sha256'],
                sampled_peak_memory_kib=d['peak_memory_kib'],sampled_peak_swap_kib=d['peak_swap_kib'],wall_seconds=d['wall_seconds']))
    interruptions=[]
    for p in BASE.rglob('*.run.json'):
        d=read(p)
        if d.get('complete'):continue
        name=p.name.removesuffix('.run.json');log=p.parent/(name+'.log')
        steps=re.findall(r'step=(\d+)',log.read_text(encoding='utf-8'))if log.exists()else[]
        interruptions.append(dict(path=str(p),metadata_sha256=sha(p),name=name,
            recorded_reason=d.get('stop_reason',d.get('failure')),last_logged_step=int(steps[-1])if steps else None,
            exact_stop_step_unknown=True,successful_hours=0))
        spool=p.parent/(name+'.spool')
        for path in spool.rglob('*'):
            if path.is_file():external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=sha(path),classification='incomplete_attempt'))
    result=dict(complete=True,four_paired4h_complete=True,four_paired12h_complete=True,
        seeds=list(SEEDS),new_runs_this_followup=11,new_simulated_hours_this_followup=108,
        unique_new_runs_whole_study=short['new_native_runs']+8,unique_new_simulated_hours_whole_study=short['new_simulated_hours']+96,
        short_summary_sha256=sha(active_main()/'comparison.json'),long_summary_sha256=sha(BASE/'long12h-candidates-20261026/comparison.json'),
        exact_economic_recipes_preserved=True,source_and_binary_frozen=True,long_prefixes_identical=True,
        long_runs=checked,external_native_data=external,interrupted_attempts=interruptions,independent_seed271_complete=False,
        broad_market_realism_qualified=False,
        scope='Complete paired4h and paired12h results for all four previously examined seeds. Historical controls and duplicate seed41 pilot caches not new work; previous incomplete attempts count no successful hours.12h temporal extension, no independent holdout/real L2/millisecond asynchronous qualification. Archive hashes and newline record counts independently verified; endpoint assets and demand balances checked, not a complete intraday fee/insurance audit.')
    write(BASE/'followup-completion.json',result)
    print(json.dumps({k:v for k,v in result.items()if k!='external_native_data'},indent=2))


if __name__=='__main__':verify()
