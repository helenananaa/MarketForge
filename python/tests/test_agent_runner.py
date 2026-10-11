import json
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer
from unittest.mock import patch

from marketforge.agents.__main__ import handler
from marketforge.agents.connectors import CodexSession, OpenCodeSession
from marketforge.agents.mcp_server import ServiceClient, validate_service_url
from marketforge.agents.program_sdk import Client
from marketforge.agents.runner import ContainerRunner
from python.tests import test_agent_external as external_fixture


class GatewayTests(unittest.TestCase):
    def setUp(self):
        external_fixture.ExternalTests.setUp(self)
        self.gateway = ThreadingHTTPServer(('127.0.0.1', 0), handler(self.service, self.operator, set(), gateway=True))
        self.gateway.daemon_threads = True
        self.gateway_thread = threading.Thread(target=self.gateway.serve_forever)
        self.gateway_thread.start()
        self.url = f'http://127.0.0.1:{self.gateway.server_port}'
        self.api = Client(service_url=self.url, trader='alice', token=self.token)

    def tearDown(self):
        self.gateway.shutdown(); self.gateway.server_close(); self.gateway_thread.join(2)
        external_fixture.ExternalTests.tearDown(self)

    def fetch(self, path, token=None, body=None):
        request = urllib.request.Request(self.url + path, data=json.dumps(body).encode() if body is not None else None,
            headers={'Authorization': 'Bearer ' + (token or self.token), 'Content-Type': 'application/json'})
        return urllib.request.urlopen(request, timeout=5)

    def test_operator_and_connector_controls_never_cross_gateway(self):
        for path in ('/traders', '/status', '/tools/alice/connection', '/tools/alice/model', '/tools/alice/runner'):
            for token in (self.operator, self.token):
                with self.assertRaises(urllib.error.HTTPError) as error:
                    self.fetch(path, token, {'action': 'start'})
                self.assertEqual(error.exception.code, 404)
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.fetch('/tools/alice/schema', self.operator)
        self.assertEqual(error.exception.code, 401)
        self.assertEqual(self.api.context()['trader'], 'alice')
        self.service.create({'id': 'bob', 'room': 'room', 'account_id': 30, 'instruments': ['SPOT']})
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.fetch('/tools/bob/schema')
        self.assertEqual(error.exception.code, 401)

    def test_program_http_same_intent_scope_and_shared_budget(self):
        first = self.api.trade('SPOT', 'limit', 'once', side='Buy', price_tick=100, qty=2)
        self.assertEqual(first, self.api.trade('SPOT', 'limit', 'once', side='Buy', price_tick=100, qty=2))
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(self.exchange.orders[0]['account_id'], 20)
        with self.assertRaisesRegex(ValueError, 'different'):
            self.api.trade('SPOT', 'limit', 'once', side='Buy', price_tick=100, qty=3)
        with self.assertRaises(ValueError):
            self.api.trade('SPOT', 'limit', 'spoof', side='Buy', price_tick=100, qty=1, account_id=30)
        self.assertIn('error', self.api.observe('OTHER'))
        policy = self.service.config('alice'); policy['orders_per_minute'] = 1
        self.service.store.put('trader', 'alice', policy)
        self.assertIn('error', self.api.trade('SPOT', 'limit', 'budget', side='Buy', price_tick=100, qty=1))
        self.assertEqual(len(self.exchange.orders), 1)
        with self.assertRaisesRegex(ValueError, 'unavailable'):
            self.api.call('workspace_exec', {'command': 'echo escape'})

    def test_unknown_response_preserves_intent_on_http_retry(self):
        self.exchange.lose_response = True
        with self.assertRaisesRegex(TimeoutError, 'unknown'):
            self.api.trade('SPOT', 'limit', 'unknown', side='Buy', price_tick=100, qty=1)
        self.assertTrue(self.api.trade('SPOT', 'limit', 'unknown', side='Buy', price_tick=100, qty=1)['accepted'])
        self.assertEqual(len(self.exchange.orders), 1)

    def test_pause_offline_and_urgent_alert_reject_program_admission(self):
        for patch_target, value, expected in (('connection_live', False, 'offline'), ('pending', True, 'urgent')):
            target = patch.object(self.service, patch_target, return_value=value) if patch_target != 'pending' else patch.object(self.service.alerts, 'state', return_value={'pending': [{}]})
            with target, self.assertRaisesRegex(ValueError, expected):
                self.api.trade('SPOT', 'limit', expected, side='Buy', price_tick=100, qty=1)
        self.service.stop('alice')
        with self.assertRaisesRegex(ValueError, 'paused'):
            self.api.trade('SPOT', 'limit', 'paused', side='Buy', price_tick=100, qty=1)
        self.assertFalse(self.exchange.orders)

    def test_gateway_mcp_preserves_epoch_and_decision_fences(self):
        gateway = ServiceClient(self.url, 'alice', self.token)
        old = gateway.call('decision_begin', {'generation': 0})
        fresh = gateway.call('decision_begin', {'generation': 0})
        args = {'instrument': 'SPOT', 'action': 'limit', 'side': 'Buy', 'price_tick': 100, 'qty': 1,
            'generation': old['generation'], 'decision_id': old['decision_id'], 'request_id': 'stale'}
        with self.assertRaisesRegex(ValueError, 'stale'):
            gateway.call('trade', args)
        args.update(decision_id=fresh['decision_id'], request_id='fresh')
        self.assertTrue(gateway.call('trade', args)['accepted'])

    def test_runner_migration_requires_operator_pause_and_disconnect(self):
        with self.assertRaisesRegex(ValueError, 'pause'):
            self.service.runner_migrate('alice', {'action': 'migrate'})
        self.service.stop('alice')
        self.client.request('connection', {'action': 'attach', 'owner_id': 'owner', 'connection_id': 'epoch',
            'backend': 'codex', 'session_id': 'thread', 'state': 'idle'})
        with self.assertRaisesRegex(ValueError, 'disconnect'):
            self.service.runner_migrate('alice', {'action': 'migrate'})
        with self.assertRaises(urllib.error.HTTPError) as error:
            request = urllib.request.Request(self.client.url + '/traders/alice/runner', data=b'{"action":"migrate"}',
                headers={'Authorization': 'Bearer ' + self.token})
            urllib.request.urlopen(request, timeout=5)
        self.assertEqual(error.exception.code, 401)


class RunnerConfigTests(unittest.TestCase):
    def test_only_explicit_private_gateway_allowed(self):
        validate_service_url('http://127.0.0.1:57306')
        for url in ('http://host.docker.internal:57307', 'http://172.23.16.1:57307'):
            with self.assertRaises(ValueError): validate_service_url(url)
            validate_service_url(url, True)
        for url in ('http://example.com:80', 'http://8.8.8.8:80', 'http://0.0.0.0:80', 'http://127.0.0.1:80/x', 'http://user:pass@127.0.0.1:80', 'http://127.0.0.1:80?q=1'):
            with self.assertRaises(ValueError): validate_service_url(url, True)

    def test_codex_native_and_mcp_paths_are_in_same_container(self):
        class Runner:
            def command(self, *args): return ['docker', 'exec', '-i', 'owned', *args]
            def mcp_command(self, epoch): return ['python', '-m', 'marketforge.agents.mcp_server', '--connection-id', epoch]
        class RPC:
            def __init__(self, command, env): self.command = command; self.calls = []
            def send(self, message): pass
            def request(self, name, params):
                self.calls.append((name, params))
                return {'thread': {'id': 'native'}}
            def close(self): pass
        with patch('marketforge.agents.connectors.CodexRPC', RPC):
            session = CodexSession(None, {'connection_id': 'epoch'}, Path('host'), Path('host.token'), runner=Runner())
        params = next(params for method, params in session.rpc.calls if method == 'thread/start')
        self.assertEqual(params['cwd'], '/work')
        self.assertEqual(params['sandbox'], 'danger-full-access')
        self.assertEqual(session.rpc.command, ['docker', 'exec', '-i', 'owned', 'codex', 'app-server'])
        self.assertEqual(params['config']['mcp_servers']['marketforge']['env']['PYTHONPATH'], '/work:/opt/marketforge/python')
        self.assertEqual(session.rpc.calls[-1][0], 'thread/inject_items')
        with patch('marketforge.agents.connectors.CodexRPC', RPC):
            resumed = CodexSession(None, {'session_id': 'native', 'connection_id': 'epoch'}, Path('host'), Path('host.token'), runner=Runner())
        self.assertEqual(resumed.rpc.calls[-1][0], 'thread/resume')


if __name__ == '__main__': unittest.main()
