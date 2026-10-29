"""Linux per-file cache hints for completed task artifacts only; no global reclaim."""
import json
import os
import time
from pathlib import Path

BASE=Path('/mnt/e/projects/MarketForge-tuning/.local/strategy-research-flow-surprise-20261025').resolve()


def memory():
    fields={line.split(':')[0]:int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith(('MemFree:','MemAvailable:','Cached:','Buffers:'))}
    fields['host_relay']=json.loads((BASE/'host-memory.json').read_text(encoding='utf-8-sig'))
    return fields


def run():
    target=BASE/'completed-file-cache-hints.json'
    assert not target.exists()
    entries={}
    for done in BASE.rglob('*.run.json'):
        record=json.loads(done.read_text())
        if not record.get('complete'):continue
        summary=done.with_name(done.name.replace('.run.json','.summary.json'))
        if not summary.exists():continue
        data=json.loads(summary.read_text())
        raw=done.with_name(done.name.replace('.run.json','.receipt.json'))
        entries[raw.resolve()]=record['receipt_sha256']
        for arc in data['execution_archives']:
            entries[Path(arc['path']).resolve()]=arc['sha256']
    before=memory();actions=[]
    for path,digest in sorted(entries.items()):
        assert path.is_relative_to(BASE) and path.is_file(),str(path)
        # No file removal, truncation, byte rewrite, global drop_caches or
        # memory.reclaim. Never include an artifact whose native run is active.
        entry=dict(path=str(path),bytes=path.stat().st_size,expected_sha256=digest)
        try:
            fd=os.open(path,os.O_RDONLY)
            try:
                os.fsync(fd)
                os.posix_fadvise(fd,0,0,os.POSIX_FADV_DONTNEED)
            finally:os.close(fd)
            entry['hint_completed']=True
        except OSError as error:
            entry.update(hint_completed=False,error=str(error))
        actions.append(entry)
    time.sleep(6)
    result=dict(time_unix=time.time(),before=before,after=memory(),files=actions,
        scope='Per-file fsync and POSIX_FADV_DONTNEED on completed native artifacts under this task root only. No global cache reclaim, WSL configuration, other project path or process changes. Frozen plans and8GiB host start gate unchanged. Expected hashes derive from native validated completion summaries; final evidence verification independently checks bytes after these hints.')
    target.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result,indent=2))


if __name__=='__main__':run()
