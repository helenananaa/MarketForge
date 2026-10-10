"""Account-bound API for programs inside a MarketForge Docker workspace.

The host relays requests through the workspace volume. No operator or exchange
credential is installed in the container. Request IDs are durable trade intents.
"""
import json
import hashlib
import os
from pathlib import Path
import time
import uuid


class Client:
    def __init__(self, timeout=60):
        self.root = Path('/work/.marketforge')
        self.timeout = timeout

    def call(self, name, arguments=None, request_id=None):
        request_id = request_id or uuid.uuid4().hex
        if not request_id or len(request_id) > 128 or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-' for c in request_id):
            raise ValueError('request_id must be 1-128 letters, digits, underscore or dash')
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

    def context(self):
        return self.call('context')

    def observe(self, instrument):
        return self.call('market_read', {'instrument': instrument, 'kind': 'observe'})

    def candles(self, instrument, **query):
        return self.call('market_history', {'instrument': instrument, **query})

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
