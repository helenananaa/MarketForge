"""Dispatch at most two independent frozen native tasks; never retry in place."""
import json
import subprocess
import time
from pathlib import Path
from research_market_mechanisms import write, sha
from research_flow_surprise import BASE


def run():
    assert (BASE/'pilot-selection.json').exists()
    target = BASE/'matrix-dispatch.json'
    assert not target.exists()
    pending = []
    for folder in (BASE/'main', BASE/'holdout'):
        plan = json.loads((folder/'plan.json').read_text(encoding='utf-8'))
        for row in plan['runs']:
            done = folder/(row['name']+'.run.json')
            if done.exists():
                assert json.loads(done.read_text(encoding='utf-8'))['complete']
                continue
            pending.append((folder, row))
    running, records, failed = [], [], False
    write(target, dict(max_concurrency=2, tasks=records, complete=False))
    while pending or running:
        while pending and len(running)<2 and not failed:
            folder, row = pending.pop(0)
            log = BASE/(folder.name+'-'+row['name']+'-launcher.log')
            assert not log.exists()
            stream = log.open('wb')
            command = ['wsl', '--user', 'root', '--exec', 'bash', '-lc',
                'cd /mnt/e/projects/MarketForge-tuning && python3 scripts/run_flow_surprise_linux.py '+
                folder.as_posix()+' --seed '+str(row['seed'])+' --profile '+row['profile']]
            child = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT)
            record = dict(directory=str(folder), name=row['name'], seed=row['seed'], profile=row['profile'],
                recipe_sha256=row['recipe_sha256'], plan_sha256=sha(folder/'plan.json'),
                pid=child.pid, started_unix=time.time(), command=command)
            records.append(record)
            running.append((child, stream, record))
            print('START '+folder.name+'/'+row['name'], flush=True)
            write(target, dict(max_concurrency=2, tasks=records, complete=False))
        for child, stream, record in list(running):
            code = child.poll()
            if code is None:
                continue
            stream.close()
            record.update(exit_code=code, finished_unix=time.time())
            done = Path(record['directory'])/(record['name']+'.run.json')
            success = code==0 and done.exists() and json.loads(done.read_text(encoding='utf-8'))['complete']
            record['complete'] = success
            failed |= not success
            running.remove((child, stream, record))
            print(('DONE ' if success else 'FAILED ')+record['name'], flush=True)
            write(target, dict(max_concurrency=2, tasks=records, complete=False))
        if failed and not running:
            write(target, dict(max_concurrency=2, tasks=records, complete=False, unstarted=[r['name'] for _,r in pending]))
            raise RuntimeError('Native task failed; preserve the attempt and do not dispatch new tasks')
        if running:
            time.sleep(10)
    write(target, dict(max_concurrency=2, tasks=records, complete=True))


if __name__ == '__main__':
    run()
