"""Host-owned Docker transport for native framework sessions.

Only the trader capability and explicitly selected model credentials enter the
owned volume. Framework RPC, lifecycle and event supervision stay on the host.
"""
import json
import os
from pathlib import Path
import re
import time
import uuid

from .workspace import DockerWorkspace

PROVIDER_ENV = {'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'GOOGLE_GENERATIVE_AI_API_KEY',
    'GEMINI_API_KEY', 'OPENROUTER_API_KEY', 'AZURE_OPENAI_API_KEY', 'AZURE_OPENAI_ENDPOINT'}

LAUNCH = """import json,os,sys
from pathlib import Path
os.umask(0o077)
os.environ.update(json.loads(Path('/work/.marketforge/provider-env.json').read_text()))
os.environ.update(HOME='/work',CODEX_HOME='/work/.codex',XDG_CONFIG_HOME='/work/.config',XDG_DATA_HOME='/work/.local/share',XDG_STATE_HOME='/work/.local/state',XDG_CACHE_HOME='/work/.cache')
for key in ('CODEX_HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME'):
 Path(os.environ[key]).mkdir(parents=True,exist_ok=True)
os.chdir('/work')
os.execvp(sys.argv[1],sys.argv[1:])
"""

HTTP = """import json,sys,urllib.request
request=json.load(sys.stdin)
class NoRedirect(urllib.request.HTTPRedirectHandler):
 def redirect_request(self,*args,**kwargs): raise ValueError('redirect disabled')
req=urllib.request.Request('http://127.0.0.1:4096'+request['path'],data=json.dumps(request['body']).encode() if request['body'] is not None else None,headers=request['headers'])
with urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect()).open(req,timeout=request['timeout']) as response:
 raw=response.read(1048577)
 if len(raw)>1048576: raise ValueError('response exceeds 1 MiB')
 print(json.dumps(json.loads(raw) if raw else None))
"""


class ContainerRunner:
    def __init__(self, client, *, model_env=(), auth_file=None, backend='codex'):
        reference = client.request('runner', {'action': 'start'})
        identity = reference['identity']
        if not re.fullmatch('[a-f0-9]{24}', identity) or reference['container'] != 'mf-workspace-' + identity:
            raise ValueError('invalid owned runner identity')
        self.workspace = DockerWorkspace('', reference['image'], runner=True)
        self.workspace.identity = identity
        self.workspace.name = reference['container']
        self.workspace.volume = self.workspace.name + '-data'
        data = self.workspace.inspect()
        if data['Image'] != reference['image'] or data['Config'].get('Labels', {}).get('marketforge.runner') != '1':
            raise ValueError('runner image/ownership mismatch')
        self.url, self.trader = reference['gateway_url'], client.trader
        from .mcp_server import validate_service_url
        validate_service_url(self.url, allow_container_gateway=True)
        values = {}
        for key in model_env:
            if key not in PROVIDER_ENV: raise ValueError('unsupported provider environment variable: ' + key)
            if not os.environ.get(key): raise ValueError('selected provider environment variable is empty: ' + key)
            values[key] = os.environ[key]
        # The existing per-trader credentials survive reconnects unless the
        # operator explicitly supplies replacement credentials.
        if values:
            self.write('.marketforge/provider-env.json', json.dumps(values))
        else:
            self.workspace._command('exec', self.workspace.name, 'python', '-I', '-c',
                "from pathlib import Path; p=Path('/work/.marketforge/provider-env.json'); p.touch(mode=0o600,exist_ok=True); p.write_text('{}') if p.stat().st_size==0 else None")
        self.token_file = '/work/.marketforge/tool.token'
        self.write('.marketforge/tool.token', client.token)
        self.write('.marketforge/api.json', json.dumps({'service_url': self.url, 'trader': self.trader, 'token_file': self.token_file}))
        if auth_file:
            value = Path(auth_file).read_text(encoding='utf-8')
            if len(value.encode()) > 128000: raise ValueError('model auth file exceeds 128 KiB')
            json.loads(value)
            self.write('.codex/auth.json' if backend == 'codex' else '.local/share/opencode/auth.json', value)
        self.backend = backend
        # A container has a full workbench; interactive permission questions
        # cannot be serviced by the unattended connector.
        if backend == 'opencode':
            self.write('.config/opencode/opencode.json', json.dumps({'$schema': 'https://opencode.ai/config.json', 'permission': 'allow'}))

    def write(self, path, text):
        return self.workspace.operation({'op': 'write', 'path': path, 'text': text})

    def command(self, *args):
        self.ensure_running()
        return [*self.workspace.docker, 'exec', '-i', '-w', '/work', self.workspace.name, 'python', '-I', '-c', LAUNCH, *args]

    def mcp_command(self, connection_id=None):
        return ['python', '-m', 'marketforge.agents.mcp_server', '--service-url', self.url,
            '--trader', self.trader, '--token-file', self.token_file, '--allow-container-gateway',
            *(['--connection-id', connection_id] if connection_id else [])]

    def ensure_running(self):
        if not self.workspace.inspect()['State']['Running']:
            self.workspace.start()

    def opencode_request(self, path, body, headers, timeout=30):
        if not path.startswith('/') or path.startswith('//') or '#' in path:
            raise ValueError('invalid framework request path')
        raw, _ = self.workspace._capture([*self.workspace.docker, 'exec', '-i', self.workspace.name, 'python', '-I', '-c', HTTP],
            json.dumps({'path': path, 'body': body, 'headers': headers, 'timeout': timeout}).encode(), timeout=timeout + 5)
        return json.loads(raw)

    def start_opencode(self):
        # Reuse the native server/database after a host connector restart.
        self.ensure_running()
        try:
            if self.opencode_request('/global/health', None, {}, 3)['healthy']: return
        except (ValueError, OSError):
            pass
        import shlex
        command = ' '.join(shlex.quote(x) for x in ['python', '-I', '-c', LAUNCH, 'opencode', 'serve', '--hostname', '127.0.0.1', '--port', '4096'])
        self.workspace.operation({'op': 'exec', 'job_id': uuid.uuid4().hex, 'command': command})
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                if self.opencode_request('/global/health', None, {}, 2)['healthy']: return
            except (ValueError, OSError):
                pass
            time.sleep(.25)
        raise ValueError('container OpenCode server did not become healthy')
