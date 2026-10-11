"""Account-bound API for programs inside a MarketForge Docker workspace.

Runner containers use the scoped HTTP gateway. Older script-only workspaces
retain the volume relay. Request IDs are durable trade intents in both modes.
"""
import json
import hashlib
import os
from pathlib import Path
import time
import uuid
import urllib.error
import urllib.request
from urllib.parse import urlsplit
import ipaddress


class Client:
    def __init__(self, timeout=60, *, service_url=None, trader=None, token=None):
        self.root = Path('/work/.marketforge')
        self.timeout = timeout
        config = self.root / 'api.json'
        self.http = None
        if service_url is None and config.exists():
            settings = json.loads(config.read_text())
            service_url, trader = settings['service_url'], settings['trader']
            token = Path(settings['token_file']).read_text().strip()
        if service_url is not None:
            parsed = urlsplit(service_url)
            private = parsed.hostname in ('localhost', 'host.docker.internal')
            try:
                address = ipaddress.ip_address(parsed.hostname or '')
                private = address.is_private and not address.is_unspecified and not address.is_multicast
            except ValueError:
                pass
            if parsed.scheme != 'http' or not private or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in ('', '/'):
                raise ValueError('use a private scoped HTTP gateway base URL')
            if not isinstance(trader, str) or not trader or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-' for c in trader):
                raise ValueError('invalid trader identity')
            if not token or len(token) < 24:
                raise ValueError('scoped trader token required')
            self.http = (service_url.rstrip('/') + '/tools/' + trader + '/program', token)

    def call(self, name, arguments=None, request_id=None):
        request_id = request_id or uuid.uuid4().hex
        if not request_id or len(request_id) > 128 or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-' for c in request_id):
            raise ValueError('request_id must be 1-128 letters, digits, underscore or dash')
        if self.http:
            return self._http_call(name, arguments or {}, request_id)
        pending = self.root / 'requests' / (request_id + '.json')
        reply = self.root / 'responses' / (request_id + '.json')
        pending.parent.mkdir(parents=True, exist_ok=True)
        payload = json.dumps({'id': request_id, 'name': name, 'arguments': arguments or {}}, allow_nan=False)
        digest=hashlib.sha256(json.dumps({'name':name,'arguments':arguments or {}},sort_keys=True,separators=(',',':'),allow_nan=False).encode()).hexdigest()
        if reply.exists():
            previous=json.loads(reply.read_text())
            if previous.get('input_hash')!=digest:
                raise ValueError('request_id already used for different arguments')
            if 'result' in previous: return previous['result']
            reply.unlink()
        temporary = pending.with_suffix('.' + uuid.uuid4().hex + '.tmp')
        temporary.write_text(payload)
        try:
            os.link(temporary,pending)
        except FileExistsError:
            existing=json.loads(pending.read_text())
            prior=hashlib.sha256(json.dumps({'name':existing['name'],'arguments':existing.get('arguments',{})},sort_keys=True,separators=(',',':'),allow_nan=False).encode()).hexdigest()
            if prior!=digest: raise ValueError('request_id already pending with different arguments')
        finally:
            temporary.unlink(missing_ok=True)
        deadline = time.monotonic() + self.timeout
        while not reply.exists():
            if time.monotonic() >= deadline:
                raise TimeoutError('Result unknown; retry the same request_id and arguments')
            time.sleep(.05)
        result = json.loads(reply.read_text())
        if result.get('input_hash')!=digest: raise ValueError('response does not match request intent')
        if 'error' in result:
            raise RuntimeError(result['error'])
        return result['result']

    def _http_call(self, name, arguments, request_id):
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *args, **kwargs):
                raise ValueError('gateway redirects are disabled')
        url, token = self.http
        payload = json.dumps({'id': request_id, 'name': name, 'arguments': arguments}, allow_nan=False).encode()
        if len(payload) > 262144:
            raise ValueError('request exceeds 256 KiB')
        request = urllib.request.Request(url, data=payload, headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
        try:
            with urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect()).open(request, timeout=self.timeout) as response:
                raw = response.read(4194305)
        except urllib.error.HTTPError as exc:
            value = json.loads(exc.read(8192))
            if exc.code >= 500:
                raise TimeoutError('Result unknown; retry the same request_id and arguments') from None
            if exc.code == 400: raise ValueError(value.get('error', 'invalid request')) from None
            raise RuntimeError(value.get('error', 'gateway request failed; retry the same request_id')) from None
        except (OSError, urllib.error.URLError):
            raise TimeoutError('Result unknown; retry the same request_id and arguments') from None
        if len(raw) > 4194304: raise ValueError('response exceeds 4 MiB; narrow the query')
        return json.loads(raw)

    def context(self):
        return self.call('context')

    def observe(self, instrument):
        return self.call('market_read', {'instrument': instrument, 'kind': 'observe'})

    def candles(self, instrument, **query):
        return self.call('market_history', {'instrument': instrument, **query})

    def window_stats(self, instrument, **query):
        return self.call('window_stats', {'instrument': instrument, **query})

    def order_flow(self, instrument, **query):
        return self.call('order_flow', {'instrument': instrument, **query})

    def market_events(self, instrument, **query):
        return self.call('market_events', {'instrument': instrument, **query})

    def iter_market_events(self, instrument, **query):
        query = {"from_start": True, "limit": 500, **query}
        if "after_command_seq" in query: query.pop("from_start", None)
        while True:
            page = self.market_events(instrument, **query)
            yield page
            if not page["has_more"]: break
            query.pop("from_start", None)
            query["after_command_seq"] = page["next_after_command_seq"]

    def trade(self, instrument, action, request_id, **fields):
        return self.call('trade', {'instrument': instrument, 'action': action, **fields}, request_id)

    def history(self, instrument, **query):
        return self.call('account_history', {'instrument': instrument, **query})

    def iter_history(self, instrument, **query):
        query={"from_start":True,"limit":500,**query}
        while True:
            page=self.history(instrument,**query)
            yield page
            if not page["has_more"]: break
            query.pop("from_start",None)
            query["after_command_seq"]=page["next_after_command_seq"]

    def order_activity(self,instrument,order_id):
        return [a for page in self.iter_history(instrument,order_id=str(order_id)) for a in page["activities"]]

    def watch(self, instrument, after_command_seq=None, poll_seconds=.25):
        """Yield ordered, resumable public/own-account activity pages.

        Persist next_after_command_seq only after processing each yielded page.
        Empty filtered pages still advance the cursor. The first call starts at
        the tail unless an explicit cursor is supplied.
        """
        while True:
            args = {'instrument': instrument, 'include_market': True}
            if after_command_seq is not None:
                args['after_command_seq'] = after_command_seq
            ident=uuid.uuid4().hex
            while True:
                try:
                    page=self.call('account_history',args,ident)
                    break
                except TimeoutError:
                    time.sleep(1)
                except RuntimeError as exc:
                    if 'result uncertain' not in str(exc): raise
                    time.sleep(1)
            if 'error' in page: raise RuntimeError(page['error'])
            yield page
            after_command_seq = page.get('next_after_command_seq', after_command_seq)
            if not page.get('has_more'):
                time.sleep(max(.05, poll_seconds))
