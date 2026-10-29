"""Package verified small evidence, leaving immutable large native data local."""
import hashlib
import json
import zipfile
from research_market_mechanisms import ROOT, sha, write
from research_flow_surprise import BASE


def pack():
    target = ROOT/'docs/validation/2026-10-25-market-flow-surprise-evidence.zip'
    assert not target.exists()
    completion = json.loads((BASE/'completion.json').read_text(encoding='utf-8'))
    report = ROOT/'docs/validation/2026-10-25-market-flow-surprise.md'
    portable=BASE/'PORTABLE_REPORT.md'
    portable.write_text(report.read_text(encoding='utf-8').replace('../../.local/strategy-research-flow-surprise-20261025/','study/').replace('../MARKET_FLOW_SURPRISE_EXPERIMENTS.md','workspace/docs/MARKET_FLOW_SURPRISE_EXPERIMENTS.md'),encoding='utf-8')
    write(BASE/'external-native-data.json', dict(files=completion['external_native_data'],
        scope='Complete raw native receipts and full execution archives are retained at these local paths and excluded from this ZIP. Hashes and execution record counts were independently checked. This archive is evidence, not a standalone runtime distribution.'))
    files = {}
    for path in BASE.rglob('*'):
        if not path.is_file() or path.name == 'candidate.exe' or any(p.endswith('.spool') for p in path.parts):
            continue
        if path.name.endswith('.receipt.json') or path.name in ('pack.log', 'archive-verification.json', 'host-memory.json', 'host-memory-monitor.stop', 'PORTABLE_REPORT.md'):
            continue
        files['study/'+path.relative_to(BASE).as_posix()] = path
    files['binaries/control-linux.elf'] = BASE/'current-control/candidate.exe'
    files['binaries/surprise-linux.elf'] = BASE/'candidate-design/candidate.exe'
    files['REPORT.md']=portable
    for path in (ROOT/'scripts').glob('*flow_surprise*.py'):
        files['workspace/'+path.relative_to(ROOT).as_posix()] = path
    required = ['scripts/analyze_market_mechanisms.py', 'scripts/analyze_basis_execution.py',
        'scripts/analyze_wick_full.py', 'scripts/analyze_wick_anatomy.py', 'scripts/analyze_wick_basis_capacity.py',
        'scripts/analyze_wick_demand.py', 'scripts/analyze_price_formation_links.py', 'scripts/summarize_market_feedback.py',
        'scripts/research_market_mechanisms.py', 'scripts/research_wick_observations.py', 'scripts/research_receipts.py',
        'scripts/research_numeric.py', 'scripts/research_json.py', 'scripts/strategy_ecosystem.py',
        'scripts/research_resources.py', 'scripts/run_price_formation_linux.py',
        'scripts/background_market.py', 'scripts/technical_strategy_catalog.py',
        'python/tests/test_flow_surprise_recipe.py', 'python/tests/test_market_mechanism_recipe.py',
        'python/tests/test_price_formation_runner.py', 'python/tests/test_research_memory_output.py', 'python/tests/test_research_json.py',
        'docs/MARKET_FLOW_SURPRISE_EXPERIMENTS.md', 'docs/validation/2026-10-25-market-flow-surprise.md']
    for name in required:
        files['workspace/'+name] = ROOT/name
    files['reference/cached-calendar-reference.json'] = ROOT/'.local/strategy-research-reservation-20261024/reference-calendar/calendar-reference.json'
    files['reference/previous-reference-short-context.json'] = ROOT/'.local/strategy-research-price-formation-20261025/reference-short-context.json'
    members = []
    with zipfile.ZipFile(target, 'x', compression=zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        for name, path in sorted(files.items()):
            z.write(path, name)
            members.append(dict(name=name, bytes=path.stat().st_size, sha256=sha(path)))
        z.writestr('manifest.json', json.dumps(dict(files=members, external_native_data=completion['external_native_data']), ensure_ascii=False, indent=2))
    with zipfile.ZipFile(target) as z:
        assert z.testzip() is None
        for entry in members:
            assert hashlib.sha256(z.read(entry['name'])).hexdigest() == entry['sha256']
            assert sha(files[entry['name']]) == entry['sha256']
    digest = sha(target)
    target.with_suffix('.zip.sha256').write_text(digest+'  '+target.name+'\n', encoding='utf-8')
    result = dict(path=str(target), sha256=digest, bytes=target.stat().st_size,
        members=len(members)+1, crc_and_all_payload_hashes_verified=True)
    write(BASE/'archive-verification.json', result)
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    pack()
