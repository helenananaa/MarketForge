"""Package small inspectable evidence; preserve large immutable raw files in place."""
import argparse,hashlib,json,zipfile
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write
from research_price_formation import BASE,PREVIOUS


def pack(full):
    target=ROOT/'docs/validation/2026-10-25-market-price-formation-evidence.zip';assert not target.exists()
    files={}
    for p in BASE.rglob('*'):
        if not p.is_file()or p.name.endswith(('.receipt.json','.trace.jsonl'))or p.name=='candidate.exe'or p.name in ('pack.log','archive-verification.json'):continue
        files['study/'+p.relative_to(BASE).as_posix()]=p
    for p in (ROOT/'docs/validation').glob('2026-10-25-market-price-formation*'):
        if p.is_file()and p.suffix not in ('.zip','.sha256'):files['docs/validation/'+p.name]=p
    paths=['docs/MARKET_PRICE_FORMATION_EXPERIMENTS.md','python/tests/test_price_formation_recipe.py','python/tests/test_price_formation_runner.py','python/tests/test_price_formation_links.py',
        'scripts/analyze_market_mechanisms.py','scripts/analyze_basis_execution.py','scripts/analyze_wick_full.py',
        'scripts/analyze_wick_anatomy.py','scripts/analyze_wick_basis_capacity.py','scripts/analyze_wick_demand.py',
        'scripts/summarize_market_feedback.py','scripts/research_market_mechanisms.py','scripts/research_wick_observations.py',
        'scripts/background_market.py','scripts/technical_strategy_catalog.py']
    for p in (ROOT/'scripts').glob('*price_formation*.py'):paths.append(str(p.relative_to(ROOT)))
    for name in paths:files['workspace/'+Path(name).as_posix()]=ROOT/name
    files['binaries/previous-windows-896e.exe']=BASE/'diagnostic-control/candidate.exe';files['binaries/candidate-windows-1977.exe']=BASE/'pilot/candidate.exe'
    if (BASE/'linux-compatibility/candidate.exe').exists():files['binaries/candidate-linux-dd93.elf']=full/'candidate.exe'
    files['reference/previous-full-comparison.json']=PREVIOUS/'full-comparison.json'
    files['reference/previous-basis-capacity-audit-v2.json']=PREVIOUS/'basis-capacity-audit-v2.json'
    files['reference/cached-calendar-reference.json']=ROOT/'.local/strategy-research-reservation-20261024/reference-calendar/calendar-reference.json'
    external=[]
    for plan in BASE.rglob('plan.json'):
        folder=plan.parent
        for r in json.loads(plan.read_text(encoding='utf-8')).get('runs',[]):
            metadata=folder/(r['name']+'.run.json')
            if not metadata.exists():metadata=folder/(r['name']+'.failure-audit.json')
            if not metadata.exists():continue
            done=json.loads(metadata.read_text(encoding='utf-8'));raw=folder/(r['name']+'.receipt.json')
            digest=sha(raw)
            if done['complete']:assert digest==done['receipt_sha256']
            external.append(dict(path=str(raw),bytes=raw.stat().st_size,sha256=digest,complete=done['complete']))
            trace=folder/(r['name']+'.trace.jsonl')
            if trace.exists():external.append(dict(path=str(trace),bytes=trace.stat().st_size,sha256=sha(trace)))
    inventory=BASE/'external-native-data.json';write(inventory,dict(files=external,scope='Large raw receipts and trace remain at these immutable local paths and are excluded from this ZIP. Original Python/plugin runtimes are not bundled; this is evidence, not a standalone runtime distribution.'))
    files['study/external-native-data.json']=inventory
    members=[]
    with zipfile.ZipFile(target,'x',compression=zipfile.ZIP_DEFLATED,compresslevel=6) as z:
        for name,p in sorted(files.items()):
            z.write(p,name);members.append(dict(name=name,bytes=p.stat().st_size,sha256=sha(p)))
        z.writestr('manifest.json',json.dumps(dict(files=members,external_raw_data=external),ensure_ascii=False,indent=2))
    with zipfile.ZipFile(target) as z:
        assert z.testzip()is None
        for r in members:assert hashlib.sha256(z.read(r['name'])).hexdigest()==r['sha256']
    digest=sha(target);target.with_suffix('.zip.sha256').write_text(digest+'  '+target.name+'\n',encoding='utf-8')
    result=dict(path=str(target),sha256=digest,bytes=target.stat().st_size,members=len(members)+1,crc_and_all_payload_hashes_verified=True)
    write(BASE/'archive-verification.json',result);print(json.dumps(result,indent=2))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory',type=Path);pack(p.parse_args().directory)
