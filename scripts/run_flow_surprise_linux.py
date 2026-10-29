"""Run immutable Windows-path plans inside WSL using the existing guest memory."""
import argparse,json,os,subprocess,time
from pathlib import Path
from research_market_mechanisms import ROOT,sha,write
from research_receipts import finish_result


def local_path(p):
    p=str(p).replace('\\','/')
    return Path('/mnt/'+p[0].lower()+p[2:] if len(p)>2 and p[1]==':'else p)


def memory():
    fields={r.split(':')[0]:int(r.split()[1])for r in Path('/proc/meminfo').read_text().splitlines()if r.startswith(('MemAvailable:','SwapFree:'))}
    # This host disables WSL Windows-executable interop. A read-only Windows
    # guardian relays commit headroom atomically; stale/missing relay fails shut.
    pulse=ROOT/'.local/strategy-research-flow-surprise-20261025/host-memory.json'
    try:
        seen=json.loads(pulse.read_text(encoding='utf-8-sig'));age=time.time()-seen['unix_seconds']
        host=int(seen['free_commit_kib'])if 0<=age<30 else 0
    except (OSError,ValueError,KeyError):host=0;age=-1
    return dict(linux_available_kib=fields['MemAvailable'],linux_swap_free_kib=fields['SwapFree'],host_free_commit_kib=host,host_heartbeat_age_seconds=age)


def run(out,seed=None,profile=None):
    out=local_path(out);p=json.loads((out/'plan.json').read_text(encoding='utf-8'));binary=out/'candidate.exe'
    assert sha(binary)==p['binary_sha256']
    runtime=json.loads((out/'runtime-inputs.json').read_text(encoding='utf-8'))
    for f in runtime['files']:assert sha(ROOT/f['path'].replace('\\','/'))==f['sha256']
    for row in p['runs']:
        if seed is not None and row['seed']!=seed:continue
        if profile is not None and row['profile']!=profile:continue
        done=out/(row['name']+'.run.json');assert not done.exists(),'Preserve attempts in a fresh directory'
        while True:
            m=memory()
            with (out/'resource-gates.jsonl').open('a') as f:f.write(json.dumps(dict(time=time.time(),**m))+'\n')
            if m['linux_available_kib']>=p['minimum_linux_available_gib']*1024*1024 and m['host_free_commit_kib']>=p['minimum_host_commit_gib']*1024*1024:break
            time.sleep(20)
        recipe=local_path(row['recipe']);assert sha(recipe)==row['recipe_sha256']
        env=dict(os.environ,RESEARCH_PLUGIN_WAKES='events',MARKETFORGE_PINE_IN_PROCESS='1',MARKETFORGE_PINE_REPLAY_STATE='1',MARKETFORGE_BOT_WORKERS='8',RAYON_NUM_THREADS='8')
        for key in ('MF_RESEARCH_CHECKPOINT','MF_RESEARCH_STOP_FILE','MF_RESEARCH_DEADLINE_UNIX','RESEARCH_PLUGIN_POLL_MS','RESEARCH_AUDIT','RESEARCH_PROFILE','RESEARCH_SCRIPT_PROFILE','MF_RESEARCH_TRACE_PATH','MF_RESEARCH_TRACE_WINDOWS'):env.pop(key,None)
        start=time.monotonic();receipt=out/(row['name']+'.receipt.json')
        summary=out/(row['name']+'.summary.json')
        assert not summary.exists(),'Preserve completed summaries; choose a fresh attempt directory'
        env.update(MF_RESEARCH_SUMMARY_PATH=str(summary),MF_RESEARCH_SPOOL_DIR=str(out/(row['name']+'.spool')),MF_RESEARCH_OUTPUT='disk')
        result=dict(name=row['name'],complete=False,platform='linux-wsl',inputs=dict(recipe_sha256=row['recipe_sha256'],binary_sha256=p['binary_sha256'],steps=row['steps'],seconds=row['seconds']))
        print('START '+row['name'],flush=True)
        native=[str(binary),str(recipe),str(row['steps']),str(row['seconds'])]
        unit=None;command=native
        if p.get('task_cgroup'):
            unit=f"marketforge-surprise-{out.name}-s{row['seed']}-{row['profile']}-{os.getpid()}"
            command=['systemd-run','--quiet','--wait','--pipe','--collect','--unit='+unit,
                '--property=WorkingDirectory='+str(ROOT),'--property=TimeoutStopSec=5s','--property=KillMode=control-group']
            command+=['--property='+k+'='+v for k,v in p['task_cgroup'].items()]
            command+=['--setenv='+k+'='+env[k]for k in ('RESEARCH_PLUGIN_WAKES','MARKETFORGE_PINE_IN_PROCESS','MARKETFORGE_PINE_REPLAY_STATE','MARKETFORGE_BOT_WORKERS','RAYON_NUM_THREADS','MF_RESEARCH_SUMMARY_PATH','MF_RESEARCH_SPOOL_DIR','MF_RESEARCH_OUTPUT')]
            command+=native
        with receipt.open('wb')as stdout,(out/(row['name']+'.log')).open('wb')as stderr:
            child=subprocess.Popen(command,cwd=ROOT,env=env,stdout=stdout,stderr=stderr)
            result.update(pid=child.pid,command=command,task_cgroup=p.get('task_cgroup'),unit=unit);peak=0;swap_peak=0
            while child.poll()is None:
                m=memory();status=Path(f'/proc/{child.pid}/status')
                group=Path('/sys/fs/cgroup/system.slice')/(unit+'.service')if unit else None
                if group and group.exists():
                    try:
                        peak=max(peak,int((group/'memory.peak').read_text())//1024)
                        swap_peak=max(swap_peak,int((group/'memory.swap.current').read_text())//1024)
                        m['group_events']=(group/'memory.events').read_text()
                        stats=dict(line.split() for line in (group/'memory.stat').read_text().splitlines())
                        m.update(group_anon_bytes=int(stats['anon']),group_file_cache_bytes=int(stats['file']),
                                 group_memory_bytes=int((group/'memory.current').read_text()),
                                 group_swap_bytes=int((group/'memory.swap.current').read_text()))
                    except FileNotFoundError:pass
                elif status.exists():
                    rss=next((int(r.split()[1])for r in status.read_text().splitlines()if r.startswith('VmRSS:')),0);peak=max(peak,rss)
                with (out/'resource-samples.jsonl').open('a')as f:f.write(json.dumps(dict(time=time.time(),pid=child.pid,peak_memory_kib=peak,peak_swap_kib=swap_peak,**m))+'\n')
                reason=('host_commit_relay_stale'if not 0<=m['host_heartbeat_age_seconds']<30 else
                        'host_commit_headroom_below_3GiB'if m['host_free_commit_kib']<3*1024*1024 else
                        'guest_available_memory_below_3GiB'if m['linux_available_kib']<3*1024*1024 else
                        'wall_timeout'if time.monotonic()-start>10800 else None)
                if reason:
                    if unit:subprocess.run(['systemctl','stop',unit],check=False)
                    else:child.kill()
                    result.update(failure='Owned Linux child stopped: '+reason,stop_reason=reason);break
                time.sleep(10)
            result.update(exit_code=child.wait(),peak_memory_kib=peak,peak_swap_kib=swap_peak)
        result['wall_seconds']=time.monotonic()-start
        finish_result(result,receipt,row,summary)
        write(done,result);print(json.dumps(result),flush=True);assert result['complete']


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directory');p.add_argument('--seed',type=int);p.add_argument('--profile',choices=['raw-flow','surprise']);a=p.parse_args();run(a.directory,a.seed,a.profile)
