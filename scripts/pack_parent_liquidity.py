"""Package immutable implementation proof or actual results without conflating them."""
import hashlib,json,zipfile
from research_parent_liquidity import BASE
from research_market_mechanisms import ROOT,sha,write


def pack():
    stage='results'if(BASE/'completion.json').exists()else'implementation'
    target=ROOT/f'docs/validation/2026-10-25-market-parent-liquidity-{stage}-evidence.zip'
    assert not target.exists()
    report=ROOT/f'docs/validation/2026-10-25-market-parent-liquidity-{stage}.md'
    portable=BASE/f'PORTABLE_{stage.upper()}.md'
    portable.write_text(report.read_text(encoding='utf-8').replace('../../.local/strategy-research-parent-liquidity-20261025/','study/').replace('../MARKET_PARENT_LIQUIDITY_EXPERIMENTS.md','workspace/docs/MARKET_PARENT_LIQUIDITY_EXPERIMENTS.md'),encoding='utf-8')
    files={}
    for path in BASE.rglob('*'):
        if not path.is_file()or any(p.endswith('.spool')for p in path.parts):continue
        if path.name.endswith(('.receipt.json','.elf','.exe'))or path.name.startswith('PORTABLE_'):continue
        if path.name in ('pack.log','host-memory.json','host-memory.tmp','host-memory-monitor.stop','archive-verification.json'):continue
        files['study/'+path.relative_to(BASE).as_posix()]=path
    files['REPORT.md']=portable;files['workspace/'+report.relative_to(ROOT).as_posix()]=report
    files['binaries/control-linux.elf']=BASE/'control-linux.elf'
    files['binaries/candidate-linux.elf']=BASE/'design/candidate-linux.elf'
    for pattern in ('scripts/*parent_liquidity*.py','python/tests/test_parent_liquidity*.py'):
        for p in ROOT.glob(pattern):files['workspace/'+p.relative_to(ROOT).as_posix()]=p
    extra=['docs/MARKET_PARENT_LIQUIDITY_EXPERIMENTS.md','scripts/strategy_ecosystem.py','scripts/research_market_mechanisms.py',
        'scripts/research_resources.py','scripts/research_receipts.py','scripts/research_numeric.py','scripts/research_json.py',
        'scripts/analyze_market_mechanisms.py','scripts/analyze_wick_full.py','scripts/analyze_basis_execution.py',
        'scripts/analyze_wick_anatomy.py','scripts/analyze_wick_basis_capacity.py','scripts/analyze_price_formation_links.py',
        'scripts/diagnose_flow_surprise_horizons.py','scripts/summarize_market_feedback.py','scripts/research_wick_observations.py',
        'scripts/analyze_wick_demand.py','scripts/background_market.py','scripts/technical_strategy_catalog.py',
        'scripts/release_parent_build_cache.py','scripts/parent_resource_guard.py',
        'python/tests/test_parent_resource_guard.py']
    for name in extra:files['workspace/'+name]=ROOT/name
    external=[]
    for plan in BASE.glob('*/plan.json'):
        cache_path=plan.parent/'cache-provenance.json'
        cache=json.loads(cache_path.read_text())if cache_path.exists()else{}
        for r in json.loads(plan.read_text())['runs']:
            p=plan.parent/(r['name']+'.receipt.json')
            metadata=plan.parent/(r['name']+'.run.json')
            if p.exists():
                done=json.loads(metadata.read_text())if metadata.exists()else{}
                cached=plan.parent.name=='controls'or r['name']in cache
                external.append(dict(path=str(p),bytes=p.stat().st_size,sha256=sha(p),cached=cached,
                    complete=done.get('complete',False),classification='cached_data'if cached else'completed_new_run'if done.get('complete')else'incomplete_attempt'))
    delivery_path=BASE/'delivery-status.json'
    if stage=='implementation'and delivery_path.exists():
        for f in json.loads(delivery_path.read_text())['external_native_data']:
            if f['path']not in {r['path']for r in external}:external.append(f)
    if stage=='results':external=json.loads((BASE/'completion.json').read_text())['external_native_data']
    inventory=BASE/'external-native-data.json';write(inventory,dict(files=external,stage=stage,
        scope='Large raw data retained local. Cached old controls are not new work. Native qualification requires completed outcome metadata, not implementation tests. Not a standalone runtime distribution.'))
    files['study/external-native-data.json']=inventory
    members=[]
    with zipfile.ZipFile(target,'x',compression=zipfile.ZIP_DEFLATED,compresslevel=6)as z:
        for name,path in sorted(files.items()):
            z.write(path,name);members.append(dict(name=name,bytes=path.stat().st_size,sha256=sha(path)))
        z.writestr('manifest.json',json.dumps(dict(stage=stage,files=members,external_native_data=external),ensure_ascii=False,indent=2))
    with zipfile.ZipFile(target)as z:
        assert z.testzip()is None
        for r in members:
            assert hashlib.sha256(z.read(r['name'])).hexdigest()==r['sha256']==sha(files[r['name']])
    digest=sha(target);target.with_suffix('.zip.sha256').write_text(digest+'  '+target.name+'\n',encoding='utf-8')
    result=dict(stage=stage,path=str(target),sha256=digest,bytes=target.stat().st_size,members=len(members)+1,crc_and_all_payload_hashes_verified=True)
    write(BASE/'archive-verification.json',result);print(json.dumps(result,indent=2))


if __name__=='__main__':pack()
