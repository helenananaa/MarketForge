"""Build and verify a compact, explicitly non-standalone wick evidence archive."""
import hashlib,json,zipfile
from pathlib import Path
from analyze_market_mechanisms import ROOT,sha,write
from research_wick_observations import BASE,SOURCE


def pack():
    report=ROOT/'docs/validation/2026-10-25-market-wicks.md';assert report.is_file()
    verified=json.loads((BASE/'verification.json').read_text(encoding='utf-8'));assert verified['native_runs_verified']==20
    out=report.with_name('2026-10-25-market-wicks-evidence.zip');assert not out.exists(),'Preserve old archives'
    files={}
    def add(path,name):
        assert path.is_file();assert name not in files;files[name]=path
    for p in sorted(BASE.rglob('*')):
        if not p.is_file() or '__pycache__' in p.parts:continue
        if p.name.endswith(('.receipt.json','.exe','.trace.jsonl')):continue
        add(p,'study/'+p.relative_to(BASE).as_posix())
    # Keep each executable exactly once; interpreted plugin runtimes remain an
    # explicit external dependency, as documented by runtime-input manifests.
    for name,p in [('control-2ba.exe',BASE/'control.exe'),('observations-301.exe',BASE/'pilot-observations/candidate.exe'),('execution-1f52.exe',BASE/'pilot-execution/candidate.exe'),('style-896e.exe',BASE/'pilot-style/candidate.exe')]:add(p,'binaries/'+name)
    for seed in (41,7,19,73):
        name=f'native-s{seed}-depth'
        for suffix in ('.recipe.json','.run.json','.metrics.json','.log','-V-BTC-SPOT.candles.csv','-V-BTC-PERP.candles.csv'):
            add(SOURCE/(name+suffix),'baseline/'+name+suffix)
    add(SOURCE/'plan.json','baseline/plan.json')
    for p in sorted((ROOT/'.local/strategy-research-reservation-20261024/reference-calendar').glob('*.json')):add(p,'historical-reference/'+p.name)
    names=['strategy_ecosystem.py','research_market_mechanisms.py','analyze_market_mechanisms.py','analyze_basis_execution.py','summarize_market_feedback.py','research_causal_trace.py']
    names+= [p.name for p in (ROOT/'scripts').glob('*wick*.py')]
    for name in sorted(set(names)):add(ROOT/'scripts'/name,'repository/scripts/'+name)
    for name in ('test_wick_observation_recipe.py','test_market_mechanism_analysis.py','test_basis_execution_analysis.py','test_wick_basis_capacity_analysis.py'):
        add(ROOT/'python/tests'/name,'repository/python/tests/'+name)
    for p in (ROOT/'docs/validation').glob('2026-10-25-market-wicks*'):
        if p.is_file() and p.suffix!='.zip':add(p,'validation/'+p.name)
    add(ROOT/'docs/MARKET_WICK_EXPERIMENTS.md','repository/docs/MARKET_WICK_EXPERIMENTS.md')
    inventory=[dict(path=name,bytes=p.stat().st_size,sha256=sha(p)) for name,p in sorted(files.items())]
    manifest=dict(entries=inventory,scope='Compact review evidence: frozen binaries, compiled sources, plans, recipes, run metadata, tests/failures, actual OHLCV exports, derived metrics and historical reference. Huge raw native receipt JSON and trace JSONL are omitted; their original workspace paths/hashes remain in run metadata and verification. External interpreted plugin/Python runtimes are not bundled. This is not a standalone runtime or a substitute for raw-receipt audit.',verifier_sha256=sha(ROOT/'scripts/verify_wick_study.py'))
    with zipfile.ZipFile(out,'x',compression=zipfile.ZIP_DEFLATED,compresslevel=6) as z:
        for name,p in sorted(files.items()):z.write(p,name)
        z.writestr('MANIFEST.json',json.dumps(manifest,ensure_ascii=False,indent=2)+'\n')
    with zipfile.ZipFile(out) as z:
        assert z.testzip() is None
        assert len(z.infolist())==len(inventory)+1
        for r in inventory:assert hashlib.sha256(z.read(r['path'])).hexdigest()==r['sha256'],r['path']
    result=dict(archive=str(out),sha256=sha(out),bytes=out.stat().st_size,members=len(inventory)+1,crc_and_all_member_sha256_verified=True,excluded_raw_receipts=True,external_runtimes_not_bundled=True)
    write(BASE/'evidence-archive-verification.json',result)
    out.with_suffix('.zip.sha256').write_text(result['sha256']+'  '+out.name+'\n',encoding='utf-8')
    print(json.dumps(result,indent=2))


if __name__=='__main__':pack()
