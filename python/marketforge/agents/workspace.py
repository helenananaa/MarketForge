"""Persistent Docker workspaces and a credential-free account API relay.

The mature external agent keeps its own planning/tool loop. This module only
provides container files, shell processes and the existing trading service.
"""
import hashlib
import json
from pathlib import Path
import re
import threading
import time

from .sandbox import DockerSandbox

HELPER = r"""
import json, os, pathlib, signal, subprocess, sys, time
root = pathlib.Path('/work')
request = json.load(sys.stdin)
operation = request['op']
jobs = root / '.marketforge/jobs'
jobs.mkdir(parents=True, exist_ok=True)
def path(value):
    p = (root / value).resolve()
    if not p.is_relative_to(root): raise ValueError('path must stay within /work')
    return p
def identity(pid):
    try: return pathlib.Path('/proc/'+str(pid)+'/stat').read_text().rsplit(')',1)[1].split()[19]
    except FileNotFoundError: return None
def status(job, offset=0):
    p = jobs / job
    info = json.loads((p / 'info.json').read_text())
    done = p / 'exit.json'
    data = (p / 'output.log').read_bytes() if (p / 'output.log').exists() else b''
    offset = min(max(0, offset), len(data))
    end = min(len(data), offset + 32768)
    state = 'exited' if done.exists() else 'running'
    if not done.exists() and (not info.get('pid') or info.get('boot')!=identity(1) or info.get('process_identity')!=identity(info['pid'])):
        state = 'interrupted'
    return dict(job_id=job, status=state, output=data[offset:end].decode(errors='replace'),
        next_offset=end, has_more=end < len(data), output_truncated=(p/'truncated').exists(),
        exit_code=json.loads(done.read_text())['exit_code'] if done.exists() else None)
if operation == 'init':
    for name in ('requests','responses','jobs'):
        (root / '.marketforge' / name).mkdir(parents=True, exist_ok=True)
    (root / 'marketforge_program.py').write_text(request['sdk'])
    print(json.dumps({'ready':True,'cwd':'/work'}))
elif operation == 'write':
    p=path(request['path']); p.parent.mkdir(parents=True,exist_ok=True)
    p.write_text(request['text'],encoding='utf-8')
    print(json.dumps({'path':str(p),'bytes':p.stat().st_size}))
elif operation == 'read':
    p=path(request['path'])
    if p.is_dir(): value={'entries':[x.name for x in sorted(p.iterdir())][:500]}
    else:
        if p.stat().st_size > 262144: raise ValueError('file exceeds 256 KiB; use shell to select a range')
        value={'text':p.read_text(encoding='utf-8')}
    print(json.dumps({'path':str(p),**value}))
elif operation == 'exec':
    job=request['job_id']; p=jobs/job
    if not p.exists():
        p.mkdir(); (p/'info.json').write_text(json.dumps({'status':'launching'}))
        # The supervisor limits output without stopping the user's process.
        wrapper=r'''import json,os,pathlib,subprocess,sys
p=pathlib.Path(sys.argv[1]); cmd=sys.argv[2]; cwd=sys.argv[3]
proc=subprocess.Popen(['/bin/sh','-lc',cmd],cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
with (p/'output.log').open('wb') as out:
 while True:
  chunk=proc.stdout.read1(8192)
  if not chunk: break
  room=max(0,4194304-out.tell()); out.write(chunk[:room]); out.flush()
  if len(chunk)>room: (p/'truncated').touch()
(p/'exit.json').write_text(json.dumps({'exit_code':proc.wait()}))
'''
        proc=subprocess.Popen(['python','-u','-c',wrapper,str(p),request['command'],str(path(request.get('cwd','.')))],
            start_new_session=True,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        (p/'info.json').write_text(json.dumps({'pid':proc.pid,'started_at':time.time(),'boot':identity(1),'process_identity':identity(proc.pid)}))
    print(json.dumps(status(job)))
elif operation == 'process':
    job=request['job_id']; p=jobs/job
    if request.get('stop'):
        info=json.loads((p/'info.json').read_text())
        if not (p/'exit.json').exists() and info.get('pid') and info.get('boot')==identity(1) and info.get('process_identity')==identity(info['pid']):
            try: os.killpg(info['pid'],signal.SIGTERM)
            except ProcessLookupError: pass
            time.sleep(.1)
            try: os.killpg(info['pid'],signal.SIGKILL)
            except ProcessLookupError: pass
            (p/'exit.json').write_text(json.dumps({'exit_code':-15}))
    print(json.dumps(status(job,request.get('offset',0))))
elif operation == 'requests':
    result=[]
    for p in sorted((root/'.marketforge/requests').glob('*.json'))[:8]:
        if p.stat().st_size > 262144:
            p.unlink(); continue
        try:
            q=json.loads(p.read_text()); q['_filename']=p.stem; result.append(q)
        except (ValueError,UnicodeError): p.unlink()
    print(json.dumps(result))
elif operation == 'reply':
    ident=request['id']; p=root/'.marketforge/responses'/ (ident+'.json')
    temporary=p.with_suffix('.tmp'); temporary.write_text(json.dumps(request['reply'],allow_nan=False)); os.replace(temporary,p)
    (root/'.marketforge/requests'/ (ident+'.json')).unlink(missing_ok=True)
    print('{}')
"""


class DockerWorkspace(DockerSandbox):
    def __init__(self, identity, image='marketforge-strategy:1'):
        super().__init__(image)
        self.identity = hashlib.sha256(identity.encode()).hexdigest()[:24]
        self.name = 'mf-workspace-' + self.identity
        self.volume = self.name + '-data'
        self.command_lock=threading.RLock()

    def inspect(self):
        value = self._command('inspect', self.name)
        data = json.loads(value)[0]
        if data.get('Config', {}).get('Labels', {}).get('marketforge.workspace') != self.identity:
            raise ValueError('workspace ownership mismatch')
        return data

    def start(self):
        found = self._command('ps', '-a', '--filter', 'name=^/' + self.name + '$', '--format', '{{.Names}}')
        if found:
            self.inspect()
        else:
            self._command('volume', 'create', '--label', 'marketforge.workspace=' + self.identity, self.volume)
            volume=json.loads(self._command('volume','inspect',self.volume))[0]
            if volume.get('Labels',{}).get('marketforge.workspace')!=self.identity:
                raise ValueError('workspace volume ownership mismatch')
            self._command('create', '--name', self.name, '--label', 'marketforge.workspace=' + self.identity,
                '--network=bridge', '--cap-drop=ALL', '--security-opt=no-new-privileges',
                '--user=65534:65534', '--read-only', '--tmpfs', '/tmp:rw,nosuid,size=256m',
                '--pids-limit=256', '--memory=2g', '--memory-swap=2g', '--cpus=2',
                '--log-driver=none', '-v', self.volume + ':/work', '-w', '/work',
                '-e', 'HOME=/work', '-e', 'PYTHONPATH=/work',
                self.image, 'python', '-c', 'import time; time.sleep(10**9)')
        volume=json.loads(self._command('volume','inspect',self.volume))[0]
        if volume.get('Labels',{}).get('marketforge.workspace')!=self.identity:
            raise ValueError('workspace volume ownership mismatch')
        self._command('run', '--rm', '--network=none', '--cap-drop=ALL', '--cap-add=CHOWN', '--cap-add=FOWNER',
            '--security-opt=no-new-privileges', '--user=0', '-v', self.volume + ':/work',
            self.image, 'python', '-I', '-c', 'import os; os.chmod("/work",0o700); os.chown("/work",65534,65534)')
        self._command('start', self.name)
        return self.operation({'op':'init', 'sdk':Path(__file__).with_name('program_sdk.py').read_text(encoding='utf-8')})

    def operation(self, request):
        with self.command_lock:
            return self._operation(request)

    def _operation(self, request):
        self.inspect()
        raw, _ = self._capture([*self.docker, 'exec', '-i', self.name, 'python', '-I', '-c', HELPER],
            json.dumps(request, allow_nan=False).encode(), timeout=20)
        return json.loads(raw)

    def stop(self):
        self.inspect()
        self._command('stop', '-t', '2', self.name)
        return {'status':'stopped','files_retained':True,'container':self.name}


PROGRAM_TOOLS = {'market_read','market_history','market_indicators','chart_export','risk_events',
    'orders','fills','account_history','market_rules','portfolio','ledger','trade','order_cancel_all','policy_status',
    'conditional_orders','indicator_compute','indicator_catalog'}


class Workspaces:
    def workspace(self, trader):
        with self.guard:
            if trader not in self.workspace_instances:
                self.workspace_instances[trader] = DockerWorkspace(str(self.store.path.resolve()) + ':' + trader)
            return self.workspace_instances[trader]

    def workspace_execute(self, config, key, name, args, stop):
        from .runtime import integer, text_value
        trader = config['id']; workspace = self.workspace(trader)
        if stop is not None and stop.is_set():
            raise ValueError('decision changed before workspace execution')
        if name == 'workspace_start':
            result = workspace.start()
            self.start_workspace_relay(trader)
            return {**result,'container':workspace.name,'network':'bridge',
                'permissions':['shell','python','persistent-files','background-processes','internet','own-account-api']}
        if name == 'workspace_stop':
            return workspace.stop()
        if name in ('workspace_read','workspace_write'):
            request = {'op':name.removeprefix('workspace_'),'path':text_value(args['path'],512)}
            if name == 'workspace_write': request['text'] = text_value(args['text'],240000)
            return workspace.operation(request)
        if name == 'workspace_exec':
            job = hashlib.sha256((trader+':'+key).encode()).hexdigest()[:32]
            result = workspace.operation({'op':'exec','job_id':job,'command':text_value(args['command'],32000),
                'cwd':text_value(args.get('cwd','.'),512)})
            deadline=time.monotonic()+integer(args.get('wait_seconds',1),0,15)
            while result['status']=='running' and time.monotonic()<deadline:
                time.sleep(.1)
                result=workspace.operation({'op':'process','job_id':job})
            return result
        if name == 'workspace_process':
            job=args['job_id']
            if not isinstance(job,str) or not re.fullmatch('[a-f0-9]{32}',job): raise ValueError('invalid job_id')
            return workspace.operation({'op':'process','job_id':job,
                'offset':integer(args.get('offset',0),0,2**53-1),'stop':bool(args.get('stop',False))})
        raise ValueError('unknown workspace tool')

    def program_call(self, trader, request):
        from .runtime import identifier
        ident=request['id']
        if not isinstance(ident,str) or not re.fullmatch('[A-Za-z0-9_-]{1,128}',ident): raise ValueError('invalid request ID')
        name=request['name']; args=request.get('arguments',{})
        config=self.config(trader)
        if name=='context': return self.context(trader)
        if name not in PROGRAM_TOOLS: raise ValueError('tool unavailable to workspace program')
        # Programs need no model decision lease; account authorization, pause,
        # current connection liveness, optional risk policies and budgets apply.
        if name in ('trade','order_cancel_all'):
            with self.lock(trader):
                if self.config(trader)['status']!='running': raise ValueError('trader paused')
                if not self.connection_live(trader): raise ValueError('trading connection is offline')
                if self.alerts.state(trader)['pending']: raise ValueError('urgent alert requires reassessment')
                return self.call(trader,'program:'+ident,name,args,source='workspace')
        return self.call(trader,'program:'+ident,name,args,source='workspace')

    def start_workspace_relay(self, trader):
        with self.guard:
            old=self.workspace_relays.get(trader)
            if old and old[1].is_alive(): return
            signal=threading.Event()
            thread=threading.Thread(target=self.workspace_relay,args=(trader,signal),daemon=True)
            self.workspace_relays[trader]=(signal,thread)
            thread.start()

    def workspace_relay(self, trader, signal):
        workspace=self.workspace(trader)
        while not signal.wait(.1):
            try:
                requests=workspace.operation({'op':'requests'})
                for request in requests:
                    ident=request.pop('_filename')
                    if not re.fullmatch('[A-Za-z0-9_-]{1,128}',ident): continue
                    try:
                        if request.get('id')!=ident: raise ValueError('request file identity mismatch')
                        reply={'result':self.program_call(trader,request)}
                    except Exception as exc:
                        reply={'error':str(exc)[:1000] if isinstance(exc,(ValueError,KeyError,TypeError)) else type(exc).__name__+': result uncertain; retry original request ID'}
                    reply['input_hash']=hashlib.sha256(json.dumps({'name':request.get('name'),'arguments':request.get('arguments',{})},sort_keys=True,separators=(',',':'),allow_nan=False).encode()).hexdigest()
                    workspace.operation({'op':'reply','id':ident,'reply':reply})
            except (ValueError,OSError):
                if signal.wait(.5): return

    def close_workspaces(self):
        for signal,_ in self.workspace_relays.values(): signal.set()
        for workspace in self.workspace_instances.values():
            try: workspace.stop()
            except (ValueError,OSError): pass
        for _,thread in self.workspace_relays.values(): thread.join(timeout=3)
