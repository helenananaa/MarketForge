"""Scoped journal/metadata and shared human-workbench indicator APIs."""
import json
import os
import urllib.parse
import urllib.request


class AdvancedTools:
    def advanced_execute(self, config, name, args):
        from .runtime import integer, text_value
        client=self.client(config); room=urllib.parse.quote(config['room'],safe='')
        if name=='portfolio':
            return client._request('GET',f'/rooms/{room}/accounts/{config["account_id"]}/portfolio')
        if name in ('conditional_orders','account_history','ledger','market_rules'):
            instrument=args['instrument']; self.check_instrument(config,instrument)
            market=urllib.parse.quote(instrument,safe='')
            if name=='market_rules': return client._request('GET',f'/rooms/{room}/instruments/{market}/rules')
            if name=='conditional_orders': return client._request('GET',f'/rooms/{room}/instruments/{market}/conditionals',query={'account_id':config['account_id']})
            query={'account_id':config['account_id'],'limit':integer(args.get('limit',100),1,500)}
            for field in ('after_command_seq','from_start','include_market','order_id'):
                if field in args: query[field]=args[field]
            return client._request('GET',f'/rooms/{room}/instruments/{market}/account-history',query=query)
        base=os.environ.get('MARKETFORGE_ANALYSIS_URL','http://127.0.0.1:18086/api/v1').rstrip('/')
        if name=='indicator_catalog':
            return {'registry':self.analysis_request(base+'/indicators/registry'),
                'runtimes':self.analysis_request(base+'/indicators/runtimes')}
        data=self.candle_data(config,args)
        return self.compute_indicator(data,args,base)

    def compute_indicator(self,data,args,base=None):
        from .runtime import text_value
        base=base or os.environ.get('MARKETFORGE_ANALYSIS_URL','http://127.0.0.1:18086/api/v1').rstrip('/')
        spec={'exchange':'marketforge','symbol':args['instrument'],'market_type':'simulation',
            'interval':f'{data["interval_ms"]/1000:g}s','securityMode':'safe','params':args.get('params',{}),
            'ohlcv':[{'time':1704067200+b['open_time_ms']/1000,'open':b['open_tick'],
                'high':b['high_tick'],'low':b['low_tick'],'close':b['close_tick'],'volume':b['volume'],
                'is_closed':b['is_final']} for b in data['candles']]}
        if 'script' in args:
            spec.update(mode='script',language=args.get('language','pine'),script=text_value(args['script'],100000))
        elif 'name' in args: spec.update(mode='builtin',name=text_value(args['name'],100))
        else: raise ValueError('name or script is required')
        result=self.analysis_request(base+'/indicators/compute',spec)
        return {'source':data,'indicator':result,'time_origin_unix_seconds':1704067200,
            'clock_advanced':False,'engine':'CandleScope provided-bar engine'}

    @staticmethod
    def analysis_request(url, body=None):
        request=urllib.request.Request(url,data=None if body is None else json.dumps(body,allow_nan=False).encode(),
            headers={'Content-Type':'application/json'})
        from .mcp_server import NoRedirect
        with urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect()).open(request,timeout=30) as response:
            raw=response.read(2_000_001)
            if len(raw)>2_000_000: raise ValueError('indicator output exceeds 2 MiB; narrow history window')
            return json.loads(raw)
