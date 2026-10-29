"""Save a separate immutable followup bundle, preserving the earlier delivery."""
import hashlib,json,zipfile
from research_market_mechanisms import ROOT,sha,write
from research_parent_liquidity import BASE


def pack():
    qualification=json.loads((BASE/'followup-completion.json').read_text());assert qualification['complete']
    path=ROOT/'docs/validation/2026-10-26-market-parent-liquidity-evidence.zip';assert not path.exists()
    report=ROOT/'docs/validation/2026-10-26-market-parent-liquidity-results.md'
    portable=BASE/'PORTABLE_FOLLOWUP.md'
    portable.write_text(report.read_text(encoding='utf-8').replace('../../.local/strategy-research-parent-liquidity-20261025/','study/'),encoding='utf-8')
    files={}
    for p in BASE.rglob('*'):
        if not p.is_file()or any(s.endswith('.spool')for s in p.parts):continue
        if p.name.endswith(('.receipt.json','.elf','.exe'))or p.name.startswith('PORTABLE_'):continue
        if p.name in ('host-memory.json','host-memory.tmp','pack-followup.log','followup-archive-verification.json'):continue
        files['study/'+p.relative_to(BASE).as_posix()]=p
    files['REPORT.md']=portable;files['workspace/'+report.relative_to(ROOT).as_posix()]=report
    files['binaries/control-linux.elf']=BASE/'control-linux.elf'
    files['binaries/candidate-linux.elf']=BASE/'design/candidate-linux.elf'
    # Include the previous exact helper set plus every new followup helper.
    for pattern in ('scripts/*parent_liquidity*.py','python/tests/test_parent_liquidity*.py'):
        for p in ROOT.glob(pattern):files['workspace/'+p.relative_to(ROOT).as_posix()]=p
    for name in ('scripts/parent_memory_relay.py','python/tests/test_parent_memory_relay.py'):
        files['workspace/'+name]=ROOT/name
    prior=ROOT/'docs/validation/2026-10-25-market-parent-liquidity-implementation-evidence.zip'
    assert sha(prior)=='4dec2484b9c36c772a124874dd5a4f650ea212e4f36accbd3c0026dab89c21c6'
    extras={}
    with zipfile.ZipFile(prior)as old:
        for info in old.infolist():
            if info.filename.startswith('workspace/scripts/')or info.filename.startswith('workspace/python/tests/')or info.filename=='workspace/docs/MARKET_PARENT_LIQUIDITY_EXPERIMENTS.md':
                current=ROOT/info.filename.removeprefix('workspace/')
                if current.exists():files[info.filename]=current
                else:extras[info.filename]=old.read(info.filename)
    external=qualification['external_native_data'];members=[]
    with zipfile.ZipFile(path,'x',zipfile.ZIP_DEFLATED,compresslevel=6)as z:
        for name,p in sorted(files.items()):
            z.write(p,name);members.append(dict(name=name,bytes=p.stat().st_size,sha256=sha(p)))
        for name,blob in sorted(extras.items()):
            z.writestr(name,blob);members.append(dict(name=name,bytes=len(blob),sha256=hashlib.sha256(blob).hexdigest()))
        z.writestr('manifest.json',json.dumps(dict(stage='four-seed4h-and12h-results',files=members,
            external_native_data=external,scope='Full source, binaries, plans, logs, diagnostics and unfiltered-window charts. Large raw receipts/archives remain local with hashes. Earlier implementation evidence ZIP is preserved, not rewritten.'),ensure_ascii=False,indent=2))
    with zipfile.ZipFile(path)as z:
        assert z.testzip()is None
        for f in members:assert hashlib.sha256(z.read(f['name'])).hexdigest()==f['sha256']
    digest=sha(path);path.with_suffix('.zip.sha256').write_text(digest+'  '+path.name+'\n',encoding='utf-8')
    result=dict(path=str(path),sha256=digest,bytes=path.stat().st_size,members=len(members)+1,
        crc_and_all_payload_hashes_verified=True,previous_implementation_zip_sha256=sha(prior))
    write(BASE/'followup-archive-verification.json',result);print(json.dumps(result,indent=2))


if __name__=='__main__':pack()
