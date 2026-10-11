"""Real Linux frameworks, container MCP and HTTP orders in an isolated market.

No provider credentials or model inference are used. This proves transports,
shared files, account boundaries and native session storage, not LLM reasoning.
"""
import argparse
import asyncio
import base64
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import threading
import time
from http.server import ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'python'))
from marketforge.agents.__main__ import handler
from marketforge.agents.connectors import CodexSession, OpenCodeSession
from marketforge.agents.mcp_server import ServiceClient
from marketforge.agents.replay import isolated_exchange
from marketforge.agents.runner import ContainerRunner
from marketforge.agents.runtime import TradingService
from marketforge.agents.workspace import DockerWorkspace

PROGRAM = '''import json,os
from pathlib import Path
from marketforge_program import Client
api=Client()
assert api.http is not None
assert os.getuid()==65534
assert not any(k.startswith('MARKETFORGE_') or k.endswith('API_KEY') for k in os.environ)
assert not Path('/var/run/docker.sock').exists()
context=api.context(); assert context['account_id']==20
args=dict(side='Buy',price_tick=101,qty=1)
receipt=api.trade('V-BTC-SPOT','fok','runner-once',**args)
assert receipt['accepted'],receipt
assert api.trade('V-BTC-SPOT','fok','runner-once',**args)==receipt
try: api.trade('V-BTC-SPOT','fok','runner-once',**{**args,'qty':2})
except ValueError: pass
else: raise AssertionError('changed intent accepted')
assert 'error' in api.observe('OTHER')
pages=list(api.iter_history('V-BTC-SPOT',limit=2))
assert any(a['fills'] for p in pages for a in p['activities'])
Path('runner-proof.json').write_text(json.dumps({'uid':os.getuid(),'receipt':receipt,'history_pages':len(pages),'http':True}))
print('RUNNER_PROGRAM_OK',flush=True)
'''


async def mcp_probe(runner, service, host, state):
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client
    command = [*runner.workspace.docker, 'exec', '-i', runner.workspace.name, *runner.mcp_command(state['connection_id'])]
    params = StdioServerParameters(command=command[0], args=command[1:])
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            names = {tool.name for tool in (await session.list_tools()).tools}
            assert {'workspace_exec', 'trade', 'market_events', 'window_stats', 'order_flow', 'combined_alert_set'} <= names
            async def call(name, args):
                result = await session.call_tool(name, args)
                assert not result.isError, (name, result.content)
                return result.structuredContent
            context = await call('context', {})
            d = await call('decision_begin', {'generation': context['generation'], 'plan': 'Docker transport acceptance'})
            fence = {'generation': d['generation'], 'decision_id': d['decision_id']}
            work = await call('workspace_start', {**fence, 'request_id': 'shared-work'})
            assert work['container'] == runner.workspace.name
            await call('workspace_write', {**fence, 'request_id': 'write-program', 'path': 'runner-program.py', 'text': PROGRAM})
            job = await call('workspace_exec', {**fence, 'request_id': 'run-program', 'command': 'python -u runner-program.py', 'wait_seconds': 2})
            deadline = time.monotonic() + 45
            while job['status'] == 'running' and time.monotonic() < deadline:
                await asyncio.sleep(.25)
                job = await call('workspace_process', {**fence, 'request_id': secrets.token_hex(8), 'job_id': job['job_id']})
            assert job['exit_code'] == 0, job
            proof = json.loads((await call('workspace_read', {'path': 'runner-proof.json'}))['text'])
            assert proof['uid'] == 65534 and proof['http']
            view = await call('market_read', {'instrument': 'V-BTC-SPOT', 'kind': 'observe'})
            assert view['own_account']['Spot']['position_qty'] == 1
            # A reminder travels through the real durable event stream and
            # invalidates the same epoch's old execution lease.
            await call('alert_set', {**fence, 'request_id': 'alert', 'name': 'clock', 'conditions': [
                {'instrument': 'V-BTC-SPOT', 'metric': 'market_time_ms', 'op': 'gte', 'value': 1}]})
            host.advance_clock(context['room'], 1)
            service.alerts.poll('runnerqa')
            assert any(e['kind'] == 'alert_triggered' for e in service.store.events('runnerqa', 0))
            blocked = await session.call_tool('trade', {**fence, 'request_id': 'stale', 'instrument': 'V-BTC-SPOT', 'action': 'fok', 'side': 'Buy', 'price_tick': 101, 'qty': 1})
            assert blocked.isError
            return {'tools': len(names), 'program': proof, 'job': job, 'shared_container': True, 'alert_fences_lease': True}


def validate(args):
    output = Path(args.output); output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='mf-runner-qa-') as folder, isolated_exchange(args.exchange_executable) as host:
        service = TradingService(Path(folder) / 'service', None, host.base_url)
        gateway = ThreadingHTTPServer(('0.0.0.0', 0), handler(service, secrets.token_urlsafe(32), set(), gateway=True))
        local = ThreadingHTTPServer(('127.0.0.1', 0), handler(service, secrets.token_urlsafe(32), set()))
        for server in (gateway, local):
            server.daemon_threads = True
            threading.Thread(target=server.serve_forever, daemon=True).start()
        service.container_gateway_url = f'http://{args.gateway_host}:{gateway.server_port}'
        service.runner_image = args.image
        runner, codex, opencode = None, None, None
        try:
            scenario = json.loads((ROOT / 'scripts/fixtures/f6_batch_spec.json').read_text())['scenario']
            scenario['room_id'] = 'agent-runner-qa'
            host._request('POST', '/rooms', {'scenario': scenario, 'autostart_agents': False})
            host.add_member(scenario['room_id'], 'agent-runnerqa', 'trader')
            host.assign_account(scenario['room_id'], 20, 'agent-runnerqa')
            service.create({'id': 'runnerqa', 'backend': 'external', 'room': scenario['room_id'], 'account_id': 20,
                'instruments': ['V-BTC-SPOT'], 'orders_per_minute': 10})
            client = ServiceClient(f'http://127.0.0.1:{local.server_port}', 'runnerqa', service.issue_access('runnerqa')['token'])
            # Exercise the legacy topology's migration before the first native
            # session. Use the same image to avoid a second build dependency.
            legacy = DockerWorkspace(str(service.store.path.resolve()) + ':runnerqa', args.image)
            service.workspace_instances['runnerqa'] = legacy
            legacy.start()
            legacy.operation({'op': 'write', 'path': 'legacy-retained.txt', 'text': 'retained across migration'})
            migration = service.runner_migrate('runnerqa', {'action': 'migrate'})
            assert migration['files_retained']
            runner = ContainerRunner(client)
            assert runner.workspace.operation({'op': 'read', 'path': 'legacy-retained.txt'})['text'] == 'retained across migration'
            state = {'connection_id': secrets.token_hex(16)}
            print('Starting real container Codex', flush=True)
            codex = CodexSession(client, state, Path(folder), Path(folder) / 'unused.token', runner=runner)
            assert codex.health()['state'] == 'idle'
            native_check = codex.check()
            native_context = codex.rpc.request('mcpServer/tool/call', {'threadId': codex.id, 'server': 'marketforge', 'tool': 'context', 'arguments': {}})
            assert not native_context.get('isError'), native_context
            codex.rpc.request('fs/writeFile', {'path': '/work/native-codex.txt', 'dataBase64': base64.b64encode(b'native filesystem in Docker').decode()})
            assert runner.workspace.operation({'op': 'read', 'path': 'native-codex.txt'})['text'] == 'native filesystem in Docker'
            state['session_id'] = codex.id
            print('Codex thread ' + codex.id, flush=True)
            service.start('runnerqa')
            owner = secrets.token_hex(16)
            def attach(backend, session_id):
                client.request('connection', {'action': 'attach', 'owner_id': owner, 'connection_id': state['connection_id'],
                    'backend': backend, 'session_id': session_id, 'state': 'idle'})
            attach('codex', codex.id)
            # Test workbench under actual native RPC and account gateway.
            result = asyncio.run(mcp_probe(runner, service, host, state))
            result['codex'] = {'session_id': codex.id, **native_check, 'native_mcp_context': True, 'native_files_in_work': True}
            print('MCP and HTTP program passed', flush=True)
            codex.close(); codex = None
            runner.workspace.stop()
            resumed = CodexSession(client, state, Path(folder), Path(folder) / 'unused.token', runner=runner)
            assert resumed.id == state['session_id']
            result['codex']['resumed_same_thread'] = True
            result['codex']['resumed_after_container_stop'] = True
            resumed.close()
            print('Starting real container OpenCode', flush=True)
            opencode = OpenCodeSession(client, {}, Path(folder), Path(folder) / 'unused.token', 'http://127.0.0.1:4096', runner=runner)
            assert opencode.health()['state'] == 'idle'
            mcp = opencode.request('/mcp'); assert mcp['marketforge']['status'] == 'connected', mcp
            identifier = opencode.id
            opencode.close()
            runner.workspace.stop(); runner.workspace.start()
            opencode = OpenCodeSession(client, {'session_id': identifier}, Path(folder), Path(folder) / 'unused.token', 'http://127.0.0.1:4096', runner=runner)
            assert opencode.id == identifier
            result['opencode'] = {'session_id': identifier, 'resumed_after_container_restart': True, 'mcp_connected': True}
            assert json.loads(runner.workspace.operation({'op': 'read', 'path': 'runner-proof.json'})['text']) == result['program']
            # Exercise the documented CLI, including state locks, default
            # Docker selection, --check and a second invocation's resume.
            cli = {}
            for backend in ('codex', 'opencode'):
                command = [sys.executable, '-m', 'marketforge.agents.connectors', '--backend', backend,
                    '--trader', 'runnerqa', '--service-url', client.url, '--state-file', str(Path(folder) / (backend + '-check.json')), '--check']
                env = {**os.environ, 'PYTHONPATH': str(ROOT / 'python'), 'MARKETFORGE_TOOL_TOKEN': client.token}
                checks = []
                for _ in range(2):
                    completed = subprocess.run(command, env=env, capture_output=True, text=True, timeout=90, check=True)
                    checks.append(json.loads(completed.stdout))
                assert checks[0]['session_id'] == checks[1]['session_id']
                assert checks[1]['environment'] == 'docker' and not checks[1]['model_turn_started']
                cli[backend] = {**checks[1], 'second_invocation_resumed': True}
            result['cli_check'] = cli
            # Host expiry and pause gate script orders after native disconnect.
            service.stop('runnerqa')
            request = {'id': 'paused', 'name': 'trade', 'arguments': {'instrument': 'V-BTC-SPOT', 'action': 'fok', 'side': 'Buy', 'price_tick': 101, 'qty': 1}}
            try: service.program_call('runnerqa', request)
            except ValueError as exc: assert 'paused' in str(exc)
            else: raise AssertionError('paused trade accepted')
            inspect = runner.workspace.inspect()
            assert inspect['HostConfig']['ReadonlyRootfs']
            assert inspect['HostConfig']['CapDrop'] == ['ALL']
            assert inspect['Config']['User'] == '65534:65534'
            assert all(m['Type'] == 'volume' and m['Destination'] == '/work' for m in inspect['Mounts'])
            assert not inspect['HostConfig'].get('PortBindings')
            result.update(status='passed', model_inference=False, files_retained=True, paused_trade_rejected=True,
                legacy_migration_retains_files=True,
                image_id=inspect['Image'], mounts=inspect['Mounts'], native_versions=runner.workspace._command('exec', runner.workspace.name, 'sh', '-c', 'codex --version; opencode --version'))
            (output / 'runner-acceptance.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
            print(json.dumps({'status': 'passed', 'evidence': str(output / 'runner-acceptance.json')}))
            return result
        finally:
            for adapter in (codex, opencode):
                if adapter:
                    try: adapter.close()
                    except Exception: pass
            if service.store.get('trader', 'runnerqa'): service.stop('runnerqa')
            service.close_workspaces()
            if runner:
                runner.workspace._command('rm', '-f', runner.workspace.name)
                runner.workspace._command('volume', 'rm', runner.workspace.volume)
            for server in (local, gateway): server.shutdown(); server.server_close()
            for _, threads in service.workers.values():
                for thread in threads: thread.join(3)
            service.store.db.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--exchange-executable', default=str(ROOT / 'target/debug/exchange-server.exe'))
    parser.add_argument('--gateway-host', default='host.docker.internal', help='Windows + WSL Docker: Windows address from WSL default route')
    parser.add_argument('--image', default='marketforge-agent:1')
    parser.add_argument('--output', default=str(ROOT / '.local/agents/runner-validation'))
    validate(parser.parse_args())
