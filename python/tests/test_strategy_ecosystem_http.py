"""Real HTTP/scheduler/matching acceptance for strategy-led populations."""
import json,time,uuid
from pathlib import Path
import test_pine_http as http
from test_pine_broker_http import PineBrokerHttpTests
from strategy_ecosystem import population

class StrategyEcosystemHttpTests(PineBrokerHttpTests):
    def action(self,room,instrument,account,action):
        return self.client.submit_action(room,instrument,account,action,'ecosystem-'+uuid.uuid4().hex)
    def technical_room(self,room,primary,maker=False):
        spec=http.recipe(room,7,False,pine=True)
        plugin=spec['agents'][20]['Plugin']
        plugin.update(plugin_id='TechnicalStrategyTrader',plugin_version='1.0.0',config={'family':'EmaCross','parameters':{'fast':2,'slow':5},
            'bar_interval_ms':1000,'position_size':4,'inventory_cap':10,'max_qty':2,'slippage_ticks':3,'capital_fraction_ppm':500000,'cooldown_ms':0})
        plugin['participant']['instrument_id']=primary
        if maker:plugin.update(plugin_id='AdaptiveLiquidityMaker',config={'model':'GLFT','max_qty':2,'inventory_cap':10,
            'window':16,'min_samples':4,'refresh_ms':1000,'jitter_ms':0,'quote_ttl_ms':15000,'min_half_spread_ticks':1})
        spec['scenario']['accounts']=[{'Spot':{'account_id':10,'cash_balance':1000000,'position_qty':10000}},
            {'Spot':{'account_id':20,'cash_balance':1000000,'position_qty':10000}},{'Spot':{'account_id':300,'cash_balance':100000,'position_qty':0}},
            {'Spot':{'account_id':999,'cash_balance':100000000,'position_qty':10000}}]
        spec['scenario']['seed_orders']=[{'NewOrder':{'order_id':1+j,'account_id':10 if side=='Sell'else 20,'side':side,'kind':{'Limit':{'price_tick':price}},'qty':1,'reduce_only':False}}for j,(side,price)in enumerate([('Buy',99),('Sell',101)])]
        peer=json.loads((http.ROOT/'scripts/fixtures/microstructure_market.json').read_text(encoding='utf-8'))['scenario']['extra_markets'][0]
        peer['Perp'].update(price_link=None,funding=None)
        spec['scenario']['extra_markets']=[peer]
        self.client.start_training({'run_id':room,'scenario':spec['scenario'],'agents':[{'Plugin':plugin}],
            'manual_agents':True,'trainee_account_id':999,'target_qty':10000,'horizon_steps':1000})
        self.client.pause_room(room)

    def instrument_tick(self,room,instrument,price,index):
        self.client._request('POST',f'/rooms/{room}/resume',{})
        for account in (10,20):
            obs=self.observe_market(room,instrument,account)
            for order in obs['own_orders']:self.action(room,instrument,account,{'Cancel':{'order_id':order['order_id']}})
        self.action(room,instrument,10,{'PlaceLimit':{'side':'Sell','price_tick':price,'qty':1}})
        self.action(room,instrument,20,{'PlaceImmediateOrCancel':{'side':'Buy','price_tick':price,'qty':1}})
        self.action(room,instrument,10,{'PlaceLimit':{'side':'Sell','price_tick':price+1,'qty':100}})
        self.action(room,instrument,20,{'PlaceLimit':{'side':'Buy','price_tick':price-1,'qty':100}})
        self.client.pause_room(room)
        return self.client.step_bots(room,f'{room}-tick-{index}')

    def test_native_spot_and_perpetual_use_actual_positions_and_recover_scheduler(self):
        self.start();self.assertIn('TechnicalStrategyTrader',{b['id']for b in self.client.list_bots()})
        receipts={}
        for leg in ['SPOT','PERP']:
            instrument=f'V-BTC-{leg}';room=f'technical-{leg}-{time.time_ns()}';self.technical_room(room,instrument)
            positions=[]
            for i,price in enumerate([*range(100,115),*range(114,84,-1),*range(85,111)]):
                try:saved=self.instrument_tick(room,instrument,price,i)
                except Exception as exc:
                    (self.receipts/'strategy-native-http-failure.json').write_text(json.dumps({'leg':leg,'index':i,'error':str(exc)},indent=2),encoding='utf-8');raise
                account=self.observe_market(room,instrument)['own_account'][leg.title()if leg=='SPOT'else'Perp']
                positions.append(int(account['position_qty']))
                self.assertFalse(self.client._request('GET',f'/rooms/{room}/agents')['bot_errors'])
            self.assertGreater(max(positions),0)
            if leg=='PERP':self.assertLess(min(positions),0)
            else:self.assertGreaterEqual(min(positions),0)
            state=self.data(saved);self.assertGreater(state['bars'],0);self.assertGreater(state['emitted_orders'],0)
            receipts[leg]={'minimum_position':min(positions),'maximum_position':max(positions),'state':state}
        (self.receipts/'strategy-ecosystem-native-http.json').write_text(json.dumps(receipts,indent=2),encoding='utf-8')

    def test_adaptive_liquidity_maker_on_spot_and_perpetual_actual_matching(self):
        self.start();receipts={}
        self.assertIn('AdaptiveLiquidityMaker',{b['id']for b in self.client.list_bots()})
        for leg in ['SPOT','PERP']:
            instrument=f'V-BTC-{leg}';room=f'adaptive-{leg}-{time.time_ns()}';self.technical_room(room,instrument,maker=True)
            positions=[]
            for i,price in enumerate(([100]*4+[94]*4+[106]*4)*4):
                saved=self.instrument_tick(room,instrument,price,i)
                positions.append(int(self.observe_market(room,instrument)['own_account']['Spot'if leg=='SPOT'else'Perp']['position_qty']))
                self.assertFalse(self.client._request('GET',f'/rooms/{room}/agents')['bot_errors'])
            state=self.data(saved)
            self.assertGreater(state['quoted'],0);self.assertGreater(state['cancelled'],0)
            self.assertGreater(max(abs(p)for p in positions),0)
            self.assertLessEqual(max(abs(p)for p in positions),10)
            if leg=='SPOT':self.assertGreaterEqual(min(positions),0)
            receipts[leg]={'positions':positions,'state':state}
        (self.receipts/'adaptive-liquidity-http.json').write_text(json.dumps(receipts,indent=2),encoding='utf-8')

    def test_original_pine_scripts_initialize_through_real_broker_on_both_markets(self):
        self.start()
        manifest=json.loads((http.ROOT/'docs/strategy-ecosystem-originals.json').read_text(encoding='utf-8'))
        # This original has genuine public EMA and RSI inputs, and a short leg.
        receipts={}
        for leg in ['SPOT','PERP']:
            name='D-Bot Alpha RSI Breakout Strategy'if leg=='SPOT'else'EMA SCALPEUR + RSi - SHORT'
            candidate=next(c for c in manifest['qualified']if c['name']==name)
            room=f'original-{leg}-{time.time_ns()}';instrument=f'V-BTC-{leg}'
            inputs={'EMA_L':2,'EMA_L2':5,'EMA_S':2,'EMA_S2':5}if leg=='PERP'else{'SMA Length':3,'RSI Length':3}
            self.room(room,candidate['installed_script'],primary=instrument,config={'inputs':inputs,'history_limit':512})
            errors=[];positions=[];saved=None
            for i,price in enumerate([*range(100,120),*range(119,89,-1),*range(90,111)]):
                saved=self.instrument_tick(room,instrument,price,i)
                status=self.client._request('GET',f'/rooms/{room}/agents');errors.extend(status['bot_errors'])
                account=self.observe_market(room,instrument)['own_account']['Spot'if leg=='SPOT'else'Perp'];positions.append(int(account['position_qty']))
            receipts[leg]={'errors':errors,'minimum_position':min(positions),'maximum_position':max(positions),'state':self.data(saved)}
            self.assertFalse(errors,errors)
            self.assertGreater(self.data(saved).get('evaluated_bars',0),0)
            if leg=='PERP':self.assertLess(min(positions),0,'original short strategy must actually fill')
            else:self.assertGreater(max(positions),0,'original long strategy must actually fill')
            self.assertEqual(positions[-1],0,'original entry must close using actual matching')
        (self.receipts/'strategy-ecosystem-original-http.json').write_text(json.dumps(receipts,indent=2),encoding='utf-8')
