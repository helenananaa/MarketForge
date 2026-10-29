"""Hint away only the completed Cargo artifacts owned by these two chat studies."""
import json,os,subprocess,time
from pathlib import Path

BASE=Path('/mnt/e/projects/MarketForge-tuning/.local/strategy-research-parent-liquidity-20261025')
ROOTS=[Path('/home/helenanana/.cache')/name for name in ('marketforge-parent-liquidity-20261025','marketforge-flow-surprise-20261025')]


def resident(files):
    total=0
    for i in range(0,len(files),128):
        r=subprocess.run(['fincore','--bytes','--noheadings','--output','RES',*map(str,files[i:i+128])],capture_output=True,text=True,check=True)
        total+=sum(int(x.strip())for x in r.stdout.splitlines())
    return total


if __name__=='__main__':
    target=BASE/'owned-build-cache-hints.json';assert not target.exists()
    # Both release builds have ended; native executables are independent frozen
    # copies on E:, not these Cargo output inodes. Never touch shared toolchains,
    # package registries, /proc/sys/vm/drop_caches or cgroup memory.reclaim.
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():continue
        try:
            if (p/'comm').read_text().strip()in('cargo','rustc'):
                args=(p/'cmdline').read_bytes().decode(errors='replace')
                assert not any(str(root)in args for root in ROOTS),'Owned compiler still running'
        except FileNotFoundError:pass
    before=json.loads((BASE/'host-memory.json').read_text());records=[]
    for root in ROOTS:
        assert root.resolve()==root and root.name.startswith('marketforge-')
        files=[p for p in root.rglob('*')if p.is_file()]
        initial=resident(files);failures=[]
        for path in files:
            assert path.resolve().is_relative_to(root)
            try:
                fd=os.open(path,os.O_RDONLY)
                try:os.fsync(fd);os.posix_fadvise(fd,0,0,os.POSIX_FADV_DONTNEED)
                finally:os.close(fd)
            except OSError as error:failures.append(dict(path=str(path),error=str(error)))
        records.append(dict(path=str(root),files=len(files),bytes=sum(p.stat().st_size for p in files),resident_before=initial,resident_after=resident(files),failures=failures))
    time.sleep(6)
    result=dict(time_unix=time.time(),before=before,after=json.loads((BASE/'host-memory.json').read_text()),roots=records,
        scope='Per-file cache hints on completed task-owned Cargo output directories only, current and immediately preceding study. No byte rewrites, deletions, shared registry/toolchain/cache reclaim, global reclaim, WSL settings or other process stops. Frozen executables unchanged and checked by offline provenance.6GiB start/3GiB stop gates unchanged.')
    target.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
