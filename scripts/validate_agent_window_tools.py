"""Independent exchange + real HTTP/stdio MCP compound-window acceptance, no LLM."""
import argparse
import asyncio
import json
import os
from pathlib import Path
import tempfile
import threading
from http.server import ThreadingHTTPServer

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from marketforge.agents.__main__ import handler
from marketforge.agents.replay import isolated_exchange
from marketforge.agents.runtime import TradingService
from validate_agent_event_tools import scenarios, ROOT


async def validate(executable):
    report={"provider_inference":False,"checks":[]}
    with isolated_exchange(executable) as admin, tempfile.TemporaryDirectory(prefix="marketforge-window-tools-") as folder:
        liquidation, funding=scenarios()
        for scenario in (liquidation,funding):
            admin._request('POST','/rooms',{'scenario':scenario,'autostart_agents':False})
        room=liquidation['room_id']; instrument='V-BTC-PERP'
        def order(account, side, qty, price=70):
            result=admin._request('POST',f'/rooms/{room}/instruments/{instrument}/orders',
                {'participant_id':'window-controller','account_id':account,'action':{'PlaceLimit':{'side':side,'qty':qty,'price_tick':price}}})
            assert result['accepted'],result
        order(10,'Sell',10,100); order(20,'Buy',10,100); order(30,'Buy',4)
        trader='window'
        admin.add_member(room,'agent-'+trader,'trader'); admin.assign_account(room,10,'agent-'+trader)
        service=TradingService(folder,None,admin.base_url)
        service.create({'id':trader,'room':room,'account_id':10,'instruments':[instrument],'backend':'external'})
        token=service.issue_access(trader)['token']; service.start(trader)
        server=ThreadingHTTPServer(('127.0.0.1',0),handler(service,'window-validation-operator-token',set()))
        server.daemon_threads=True; thread=threading.Thread(target=server.serve_forever); thread.start()
        try:
            params=StdioServerParameters(command=os.sys.executable,
                args=['-m','marketforge.agents.mcp_server','--service-url',f'http://127.0.0.1:{server.server_port}','--trader',trader],
                env={'PYTHONPATH':str(ROOT/'python'),'MARKETFORGE_TOOL_TOKEN':token})
            async with stdio_client(params) as (read,write):
                async with ClientSession(read,write) as session:
                    await session.initialize()
                    async def call(name,args):
                        result=await session.call_tool(name,args)
                        assert not result.isError,result.structuredContent
                        return result.structuredContent
                    schemas=await session.list_tools()
                    assert {'combined_alert_set','window_stats','order_flow'} <= {t.name for t in schemas.tools}
                    context=await call('context',{})
                    decision=await call('decision_begin',{'generation':context['generation'],'plan':'watch cumulative long liquidations with depressed mark'})
                    lease={k:decision[k] for k in ('decision_id','generation')}
                    args={**lease,'request_id':'compound-001','name':'cumulative-liquidation','rules':[
                        {'left':{'source':'window','instrument':instrument,'window_ms':30000,'metric':'long_liquidation_notional'},'op':'gte','right':'600'},
                        {'left':{'source':'state','instrument':instrument,'metric':'mark_price'},'op':'lt','right':80}], 'reason':'30s long liquidation total >=600 and mark<80'}
                    await call('combined_alert_set',args)
                    admin._request('POST',f'/rooms/{room}/instruments/{instrument}/mark-price',{'price_tick':70})
                    await asyncio.sleep(1.3)
                    first=await call('window_stats',{'instrument':instrument})
                    assert first['complete'] and first['metrics']['liquidation_notional']=='280',first
                    context=await call('context',{})
                    assert not any(t['name']=='cumulative-liquidation' for t in context['interrupts']),context
                    order(30,'Buy',6)
                    for _ in range(100):
                        context=await call('context',{})
                        if any(t['name']=='cumulative-liquidation' for t in context['interrupts']): break
                        await asyncio.sleep(.05)
                    trigger=next(t for t in context['interrupts'] if t['name']=='cumulative-liquidation')
                    assert trigger['conditions'][0]['left']['value']=='700',trigger
                    assert trigger['conditions'][1]['left']['value']==70,trigger
                    assert context['generation']>lease['generation']
                    flow=await call('order_flow',{'instrument':instrument,'bucket_ms':1000,'large_trade_min_notional':'600'})
                    assert flow['complete'] and flow['metrics']['trade_count']==3,flow
                    assert flow['metrics']['liquidation_count']==2 and flow['metrics']['long_liquidation_notional']=='700',flow
                    assert flow['metrics']['delta_qty']=='0' and flow['buckets'][0]['cvd_qty']=='0',flow
                    assert flow['metrics']['quote_volume']=='1700',flow
                    assert len(flow['volume_profile'])==2 and not flow['profile_truncated'],flow
                    assert 'account_id' not in json.dumps(flow)
                    capped=await call('window_stats',{'instrument':instrument,'scan_limit':1})
                    assert not capped['complete'] and capped['truncated'],capped
                    stale=await session.call_tool('combined_alert_set',{**args,'request_id':'stale-001','name':'stale-plan'})
                    assert stale.isError and 'stale' in stale.structuredContent['error']
                    report['checks'].append({'case':'cumulative-not-per-fill-compound-alert','first_partial_notional':'280','second_partial_notional':'420',
                        'trigger':trigger,'flow':flow,'stale_decision_rejected':True,'tool_count':len(schemas.tools)})
                    admin.advance_clock(room,31)
                    expired=await call('window_stats',{'instrument':instrument,'window_ms':30000})
                    assert expired['metrics']['liquidation_notional']=='0',expired
                    historical=await call('order_flow',{'instrument':instrument,'window_ms':30000,'end_market_time_ms':0})
                    assert historical['metrics']['liquidation_notional']=='700' and historical['book'] is None,historical
                    report['checks'].append({'case':'window-eviction-and-historical-flow','expired':expired['metrics']['liquidation_notional'],
                        'historical':historical['metrics']['liquidation_notional'],'historical_book_unavailable':historical['book'] is None})
            # Funding accounting uses the same native aggregate API, with actual receipts.
            for account, side, price, qty in [(10,'Sell',10000,1),(20,'Buy',10000,1),(10,'Buy',9999,10),(10,'Sell',10001,10)]:
                admin._request('POST',f'/rooms/event-funding/instruments/{instrument}/orders',
                    {'participant_id':'window-controller','account_id':account,'action':{'PlaceLimit':{'side':side,'qty':qty,'price_tick':price}}})
            admin.advance_clock('event-funding',2)
            stats=admin.window_stats('event-funding',instrument,window_ms=1)
            assert stats['complete'] and stats['metrics']['funding_settled_count']==1,stats
            assert stats['metrics']['funding_total_transfer']=='100' and stats['metrics']['last_funding_rate_ppm']==-10000,stats
            report['checks'].append({'case':'native-sdk-funding-window','stats':stats})
        finally:
            service.stop(trader)
            for _,threads in service.workers.values():
                for worker in threads: worker.join(3)
            server.shutdown(); server.server_close(); thread.join(2); service.store.db.close()
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exchange-server',default=str(ROOT/'target/debug/exchange-server.exe'))
    parser.add_argument('--report',required=True)
    args=parser.parse_args(); result=asyncio.run(validate(args.exchange_server))
    Path(args.report).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps({'passed':len(result['checks']),'provider_inference':False}))
