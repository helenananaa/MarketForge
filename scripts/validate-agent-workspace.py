"""Real MCP -> persistent Docker -> scoped exchange acceptance, without an LLM provider."""
import argparse
import asyncio
import json
import os
from pathlib import Path
import secrets
import sys
import threading
import time
from http.server import ThreadingHTTPServer
from marketforge import Client, MarketForgeError
from marketforge.agents.runtime import TradingService
from marketforge.agents.__main__ import handler

PROGRAM = """import json,os,time
from marketforge_program import Client
api=Client()
assert os.getuid()==65534
assert not any('TOKEN' in k or 'API_KEY' in k for k in os.environ)
view=api.observe('V-BTC-PERP')
assert view['own_account']['Perp']['account_id']==40
rules=api.call('market_rules',{'instrument':'V-BTC-PERP'})
assert 'fok' in rules['order_actions']
receipt=api.trade('V-BTC-PERP','fok','persistent-fok',side='Buy',price_tick=101,qty=1)
assert receipt['accepted'],receipt
assert receipt==api.trade('V-BTC-PERP','fok','persistent-fok',side='Buy',price_tick=101,qty=1)
try: api.trade('V-BTC-PERP','fok','persistent-fok',side='Buy',price_tick=101,qty=2)
except ValueError: pass
else: raise AssertionError('changed intent was accepted')
pages=list(api.iter_history('V-BTC-PERP',limit=2))
assert len(pages)>2
fills=[f for p in pages for a in p['activities'] for f in a['fills']]
assert fills and fills[-1]['settlements'][0]['fee_paid']=='0'
watch=api.watch('V-BTC-PERP',after_command_seq=pages[-1]['next_after_command_seq'])
first=next(watch)
Path=__import__('pathlib').Path
Path('proof.json').write_text(json.dumps({'uid':os.getuid(),'receipt':receipt,'pages':len(pages),'fills':fills,'cursor':first['next_after_command_seq']}))
print('PROGRAM_OK',flush=True)
"""

async def probe(args,service,service_url,token_path,output,owner):
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client
    params=StdioServerParameters(command=sys.executable,args=['-m','marketforge.agents.mcp_server','--service-url',service_url,'--trader','workqa','--token-file',str(token_path)],env={**os.environ,'PYTHONPATH':'python'})
    async with stdio_client(params) as (read,write):
      async with ClientSession(read,write) as session:
        await session.initialize()
        names=[t.name for t in (await session.list_tools()).tools]
        assert {'workspace_start','workspace_exec','account_history','indicator_compute','conditional_orders'}<=set(names)
        async def call(name,arguments):
            result=await session.call_tool(name,arguments)
            assert not result.isError,(name,result.content)
            return result.structuredContent
        async def decision():
            context=await call('context',{})
            d=await call('decision_begin',{'generation':context['generation'],'plan':'Disposable workspace acceptance'})
            return {'generation':d['generation'],'decision_id':d['decision_id']}
        fence=await decision()
        work=await call('workspace_start',{**fence,'request_id':'start-workbench'})
        written=await call('workspace_write',{**fence,'request_id':'write-program','path':'strategy.py','text':PROGRAM})
        job=await call('workspace_exec',{**fence,'request_id':'execute-program','command':'python -u strategy.py','wait_seconds':1})
        deadline=time.monotonic()+90
        while job['status']=='running' and time.monotonic()<deadline:
            await asyncio.sleep(.3)
            job=await call('workspace_process',{**await decision(),'request_id':secrets.token_hex(8),'job_id':job['job_id']})
        assert job['exit_code']==0,job
        proof=await call('workspace_read',{'path':'proof.json'})
        proof=json.loads(proof['text']);assert proof['uid']==65534
        repeat=await call('workspace_exec',{**await decision(),'request_id':'execute-program','command':'python -u strategy.py','wait_seconds':1})
        assert repeat['job_id']==job['job_id']
        view=await call('market_read',{'instrument':'V-BTC-PERP','kind':'observe'})
        assert view['own_account']['Perp']['position_qty']==1,view
        # Validate shared indicator engine and line chart using the same real bars.
        builtin=await call('indicator_compute',{'instrument':'V-BTC-PERP','name':'MA','params':{'period':3}})
        assert builtin['indicator']['ok'],builtin
        pine=await call('indicator_compute',{'instrument':'V-BTC-PERP','language':'pine','script':'//@version=6\nindicator("Workspace")\nplot(ta.sma(close,3))'})
        assert pine['indicator']['ok'],pine
        chart=await session.call_tool('chart_export',{'instrument':'V-BTC-PERP','indicator':{'name':'MA','params':{'period':3}}})
        assert not chart.isError,chart.content
        import base64
        (output/'shared-indicator.png').write_bytes(base64.b64decode(chart.content[0].data))
        assert chart.structuredContent['indicator_lines']>0,chart.structuredContent
        # An old order remains directly queryable after more than the recent window.
        order_id=proof['receipt']['events'][0]['order_id']
        old=await call('orders',{'instrument':'V-BTC-PERP','order_id':str(order_id),'limit':1})
        assert old['orders'][0]['order_id']==order_id,old
        conditional=await call('trade',{**await decision(),'request_id':'native-entry','instrument':'V-BTC-PERP','action':'conditional','conditional_key':'once','conditional_spec':{'side':'Buy','qty':1,'trigger_price_tick':110,'above':True,'limit_price_tick':98}})
        assert conditional['accepted'],conditional
        owner._request('POST',f'/rooms/{args.room}/instruments/V-BTC-PERP/mark-price',{'price_tick':110})
        native=await call('conditional_orders',{'instrument':'V-BTC-PERP'})
        child=native['conditionals'][0]['submitted_order_id'];assert child>=7_000_000_000_000_000_000,native
        direct=await call('orders',{'instrument':'V-BTC-PERP','order_id':str(child),'limit':1})
        assert direct['orders'][0]['status']=='open',direct
        canceled=await call('trade',{**await decision(),'request_id':'cancel-native','instrument':'V-BTC-PERP','action':'conditional','conditional_key':'once','conditional_spec':None})
        assert canceled['accepted'],canceled
        # Internet + arbitrary Shell + installed Python dependency, inside /work only.
        installed=await call('workspace_exec',{**await decision(),'request_id':'install-package','command':'python -m pip install --user --disable-pip-version-check packaging==24.2 && python -c "import packaging; print(packaging.__version__)"','wait_seconds':1})
        deadline=time.monotonic()+60
        while installed['status']=='running' and time.monotonic()<deadline:
            await asyncio.sleep(.5)
            installed=await call('workspace_process',{**await decision(),'request_id':secrets.token_hex(8),'job_id':installed['job_id']})
        assert installed['exit_code']==0 and '24.2' in installed['output'],installed
        background=await call('workspace_exec',{**await decision(),'request_id':'long-process','command':'python -u -c "import time; print(123); time.sleep(3600)"','wait_seconds':1})
        assert background['status']=='running',background
        stopped=await call('workspace_process',{**await decision(),'request_id':'stop-process','job_id':background['job_id'],'stop':True})
        assert stopped['status']=='exited',stopped
        # Pause is checked before new program admissions.
        service.stop('workqa')
        try:service.program_call('workqa',{'id':'paused-buy','name':'trade','arguments':{'instrument':'V-BTC-PERP','action':'fok','side':'Buy','qty':1,'price_tick':101}})
        except ValueError as exc: assert 'paused' in str(exc)
        else:raise AssertionError('paused program could trade')
        workspace=service.workspace('workqa');inspect=workspace.inspect()
        assert inspect['HostConfig']['NetworkMode']=='bridge'
        assert inspect['Config']['User']=='65534:65534'
        assert all(m['Type']=='volume' and m['Destination']=='/work' for m in inspect['Mounts'])
        workspace.stop();workspace.start()
        retained=workspace.operation({'op':'read','path':'proof.json'})
        assert json.loads(retained['text'])==proof
        return {'tools':len(names),'workspace':work,'program':proof,'job':job,'dependency':installed,'background_stop':stopped,'native_child_id':str(child),'builtin':builtin,'pine':pine,'chart':{k:v for k,v in chart.structuredContent.items() if k!='image_base64'},'files_survive_restart':True,'paused_trade_rejected':True,'docker_mounts':inspect['Mounts']}

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--base-url',required=True);parser.add_argument('--room',required=True);args=parser.parse_args()
    assert args.base_url.startswith('http://127.0.0.1:') and args.room.startswith('capability-qa')
    output=Path('output/trading-capabilities')/args.room;output.mkdir(parents=True,exist_ok=True)
    owner=Client(args.base_url,user_id='local-user')
    owner._request('POST','/rooms',{'room_id':args.room,'market':{'Perp':{'instrument':{'symbol':'V-BTC-PERP','base_asset':'V','quote_asset':'BTC','tick_size':1,'lot_size':1},'clearing':{'leverage':10,'maker_fee_ppm':0,'taker_fee_ppm':0,'maintenance_margin_ppm':50000,'liquidation_fee_ppm':10000,'initial_insurance_fund':0},'risk':{},'initial_mark_price_tick':100}},'accounts':[{'Basic':{'account_id':a,'cash_balance':10000}} for a in (10,30,40)],'seed_orders':[]})
    owner.add_member(args.room,'agent-workqa','trader');owner.assign_account(args.room,40,'agent-workqa')
    owner.place(args.room,10,'Sell',101,100);owner.place(args.room,30,'Buy',99,100)
    for i in range(8):owner.place(args.room,30 if i%2==0 else 10,'Buy' if i%2==0 else 'Sell',101 if i%2==0 else 99,1);owner.advance_clock(args.room,1)
    service=TradingService(output/'service',None,args.base_url)
    server=ThreadingHTTPServer(('127.0.0.1',0),handler(service,secrets.token_urlsafe(32),set()));server.daemon_threads=True
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        service.create({'id':'workqa','room':args.room,'account_id':40,'instruments':['V-BTC-PERP'],'orders_per_minute':100})
        token_path=output/'tool.token';token_path.write_text(service.issue_access('workqa')['token']);service.start('workqa')
        result=asyncio.run(probe(args,service,f'http://127.0.0.1:{server.server_port}',token_path,output,owner))
        for path in (f'/rooms/{args.room}/instruments/V-BTC-PERP/account-history?account_id=30',f'/rooms/{args.room}/accounts/30/portfolio'):
            try:Client(args.base_url,user_id='agent-workqa')._request('GET',path)
            except MarketForgeError as exc:assert exc.status==403
            else:raise AssertionError('account scope escaped')
        result['other_account_forbidden']=True
        (output/'workspace-acceptance.json').write_text(json.dumps(result,indent=2),encoding='utf-8')
        print(json.dumps({'status':'passed','tools':result['tools'],'room':args.room,'evidence':str(output/'workspace-acceptance.json')}))
    finally:
        service.stop('workqa');service.close_workspaces();server.shutdown();server.server_close();service.store.db.close();(output/'tool.token').unlink(missing_ok=True)
if __name__=='__main__':main()
