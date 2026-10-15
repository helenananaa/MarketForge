"""Generate a strategy-led spot/perpetual population with finite allocations.

No noise traders or prewritten prices. Formula families, actual Pine originals,
parameters, candle periods, capital and risk rules are separate population axes.
"""
from __future__ import annotations
import argparse,copy,hashlib,json,os,random,re
from pathlib import Path
from background_market import Client,child_seed
from technical_strategy_catalog import FORMULAS,defaults

ROOT=Path(__file__).resolve().parents[1]
PERIODS=[1000,5000,10000,30000,60000,300000]

def varied_parameters(family,index):
    p=defaults(family);keys=FORMULAS[family][-1]
    factor=[.6,.8,1.,1.3,1.7][index%5]
    for key in keys:
        if key in ('fast','slow','signal','length','exit_length','smooth','displacement'):p[key]=max(1,min(512,round(p[key]*factor)))
        elif key in ('factor','channel_factor'):p[key]*=[.7,1.,1.3][index%3]
    if p['fast']>=p['slow']:p['slow']=min(512,p['fast']+1)
    if family in ('RsiReversion','MfiReversion','Stochastic','StochRsi'):
        p['low']=[20.,25.,30.,35.][index%4];p['high']=[60.,65.,70.,80.][index%4]
    return {k:p[k]for k in keys}

def population(room='strategy-ecosystem',seed=7,count=1000,runtime='native',original_manifest=None,capital_scale=10.0,periods=None,
               ecology=False,noise_fraction=.05,maker_model='adaptive',original_names=None):
    if not 100<=count<=10000:raise ValueError('population must be within 100..10000')
    if runtime not in ('native','pine','mixed'):raise ValueError('unknown runtime')
    periods=periods or PERIODS
    if not periods or any(not isinstance(p,int)or p<1000 or p>86400000 or p%1000 for p in periods):raise ValueError('periods must be whole simulated seconds within the host range')
    spec=json.loads((ROOT/'scripts/fixtures/microstructure_market.json').read_text(encoding='utf-8'))
    scenario=spec['scenario'];scenario['room_id']=room;scenario['accounts']=[];scenario['seed_orders']=[];scenario['routed_seed_orders']=[];scenario['market_events']=[]
    scenario['initial_portfolios']=[];scenario['initial_allocations']=[];scenario['routed_initial_allocations']=[]
    spot=scenario['market']['Spot'];perp=scenario['extra_markets'][0]['Perp']
    anchor=10000
    for market in (spot,perp):
        market['clearing'].update(maker_fee_ppm=100,taker_fee_ppm=300)
        market['risk']={'allow_short':False}if market is spot else{}
    perp.update(initial_mark_price_tick=anchor)
    perp['clearing']['leverage']=5
    perp['price_link']['max_age_ms']=120000
    perp['funding'].update(interval_ms=28800000,base_rate_ppm=100)
    spec.update(agents=[],agent_interval_ms=1000,autostart_agents=True)
    makers=max(4,count//10);execution=max(4,round(count*(.15 if ecology else .1)));value=max(4,count//10)
    noise=round(count*noise_fraction) if ecology else 0
    technical=count-makers-execution-value-noise
    families=list(FORMULAS)
    if technical//2<len(families):raise ValueError('population does not cover each family on both instruments')
    rng=random.Random(seed)
    metadata=[]
    # Exact cash pool per role, independent of population size. Initial spot
    # supply is explicit and separately reported; no runtime mint/refill occurs.
    pools={'maker':int(30_000_000*capital_scale),'execution':int(15_000_000*capital_scale),'value':int(20_000_000*capital_scale),'technical':int(35_000_000*capital_scale)}
    role_counts={'maker':makers,'execution':execution,'value':value,'technical':technical}
    if ecology:
        pools['execution']=int(25_000_000*capital_scale)
        pools['technical']-=int(10_000_000*capital_scale)
        if noise:
            pools['noise']=int(100_000_000*capital_scale*.02)
            pools['technical']-=pools['noise'];role_counts['noise']=noise
    originals=[]
    if original_manifest:
        originals=json.loads(Path(original_manifest).read_text(encoding='utf-8')).get('qualified',[])
        if original_names is not None: originals=[x for x in originals if x['name'] in original_names]
    def add(role,index,plugin,version,config,leg,cash,position=0,family=None,origin=None):
        account=100+len(spec['agents']);name=f'{role}-{leg}-{index}';instrument=f'V-BTC-{leg.upper()}'
        scenario['accounts'].append({'Spot'if position else'Basic':{'account_id':account,'cash_balance':cash,**({'position_qty':position}if position else{})}})
        spec['agents'].append({'Plugin':{'participant':{'participant_id':name,'kind':'MarketMaker'if role=='maker'else'Strategy'if role=='technical'else'RuleAgent',
            'room_id':room,'account_id':account,'instrument_id':instrument},'plugin_id':plugin,'plugin_version':version,'state_version':1,'config_version':1,
            'seed':max(1,child_seed(seed,name)&(2**53-1)),'config':config}})
        metadata.append({'participant_id':name,'account_id':account,'role':role,'family':family,'instrument':instrument,'cash':cash,'initial_spot_inventory':position,'config':config,'origin':origin})
    def cashes(role,n):
        weights=[rng.choice([1,1,1,2,3,5,10])for _ in range(n)];total=sum(weights);pool=pools[role]
        values=[pool*w//total for w in weights]
        values[-1]+=pool-sum(values);return values
    for role,n in role_counts.items():
        if ecology:rng=random.Random(child_seed(seed,'ecology-'+role))
        for i,cash in enumerate(cashes(role,n)):
            leg='spot'if i%2==0 else'perp'
            if role=='maker':
                depth=max(2,round(800/n));cap=max(100,depth*16)
                inventory=cap//2 if leg=='spot'else 0
                config={'inventory_target':inventory,'inventory_cap':cap,'max_qty':depth,'levels':4,
                    'half_spread_ticks':[2,4,6,10][i%4],'level_spacing_ticks':[2,3,5][i%3],
                    'inventory_skew_ticks':[4,10,20][i%3],'volatility_spread_multiplier':1,'withdraw_volatility_ticks':200,
                    'decision_interval_ms':[1000,2000,4000][i%3],'jitter_ms':i%5*200,'size_volatility_ticks':40,
                    'replenish_depth_ppm':800000,'fee_buffer_ppm':1000,'fallback_price_tick':anchor}
                if ecology and maker_model!='legacy':
                    model='GLFT' if maker_model=='GLFT' or (maker_model=='adaptive' and (i//2)%2==0) else 'Avellaneda'
                    config={'model':model,'inventory_target':inventory,'inventory_cap':cap,'max_qty':depth,'levels':4,
                        'gamma_ppm':rng.choice([25000,50000,100000,200000] if model=='GLFT' else [1000000,2000000,5000000]),'eta_ppm':rng.choice([200000,500000,1000000]),
                        'min_half_spread_ticks':rng.choice([2,3,4]),'max_half_spread_ticks':40,
                        'level_spacing_ticks':rng.choice([2,3,5]),'refresh_ms':rng.choice([3000,7000,11000]),
                        'jitter_ms':rng.randrange(1000,9000),'quote_ttl_ms':rng.choice([30000,60000,90000]),
                        'window':rng.choice([60,120,240]),'min_samples':24,'skew_ppm':rng.choice([20000,50000,100000]),
                        'microprice_ppm':rng.choice([0,300000,700000,1000000]),'index_weight_ppm':rng.choice([0,0,200000]),
                        'fee_buffer_ppm':1000,'fallback_price_tick':anchor}
                    add(role,i,'AdaptiveLiquidityMaker','1.0.0',config,leg,cash,inventory,model,'source_algorithm_adaptation')
                else:add(role,i,'DynamicMarketMaker','1',config,leg,cash,inventory)
            elif role=='execution':
                direction='Buy'if(i//2)%2==0 else'Sell'
                target=max(5,min(4000 if ecology else 800,cash//anchor//(2 if ecology else 3)));position=target if leg=='spot'and direction=='Sell'else 0
                # Long-duration asynchronous parent tasks supply genuine directional
                # demand. Their horizons/size are finite, not perpetual noise.
                add(role,i,'ExecutionTrader','1',{'side':direction,'target_qty':target,'horizon_ms':[1200000,3600000,7200000][(i//4)%3],
                    'start_after_ms':rng.randrange(0,36000000) if ecology else (i//4)%12*15000,'max_qty':[1,2,4][i%3],'max_slippage_ticks':30,'decision_interval_ms':[1000,3000,7000][i%3],
                    'jitter_ms':i%4*500,'inventory_cap':max(100,target*2),'fee_buffer_ppm':1000,'fallback_price_tick':anchor},leg,cash,position)
            elif role=='value':
                belief=anchor+[-300,-180,-100,-40,40,100,180,300][(i//2)%8]
                position=max(5,min(100,cash//anchor//4))if leg=='spot'else 0
                add(role,i,'ValueTrader','1',{'fair_price_tick':belief,'edge_ticks':[8,16,30][i%3],
                    'value_shift_ticks':0,'value_shift_at_ms':0,'decision_interval_ms':[5000,15000,60000][i%3],
                    'jitter_ms':5000,'max_qty':[1,2,3][i%3],'inventory_cap':max(100,cash//anchor//2),'fee_buffer_ppm':1000,'fallback_price_tick':anchor},leg,cash,position)
            elif role=='noise':
                position=max(5,min(50,cash//anchor//4)) if leg=='spot' else 0
                add(role,i,'AdaptiveNoiseTrader','1',{'activity_ppm':100000,'market_order_ratio_ppm':500000,'side_persistence_ppm':550000,
                    'max_qty':2,'inventory_cap':100,'price_radius_ticks':12,'order_ttl_ms':45000,
                    'decision_interval_ms':rng.choice([3000,7000,15000]),'jitter_ms':9000,'fallback_price_tick':anchor},leg,cash,position)
            else:
                family=families[(i//2)%len(families)];variant_index=i//(2*len(families))
                # Short through long horizons coexist. First wave covers every
                # family at short periods; later waves supply the long horizons.
                period=periods[(i//2+variant_index)%len(periods)] if ecology else periods[variant_index%len(periods)]
                params=varied_parameters(family,variant_index)
                max_qty=[1,2,4,8][variant_index%4]
                size=max(2,min(200,cash//anchor//2))
                leverage=[1,2,3][variant_index%3]if leg=='perp'else 1
                config={'family':family,'parameters':params,'bar_interval_ms':period,'history_limit':1024,'position_mode':'LongShort',
                    'position_size':size,'inventory_cap':max(100,size),'capital_fraction_ppm':[100000,200000,400000][variant_index%3],
                    'target_leverage':leverage,'max_qty':max_qty,'slippage_ticks':[8,16,30][variant_index%3],'fee_buffer_ppm':1000,
                    'stop_loss_ppm':rng.choice([8000,15000,30000,60000]) if ecology else [10000,30000,60000][variant_index%3],'take_profit_ppm':rng.choice([15000,30000,60000,120000]) if ecology else [30000,80000,150000][variant_index%3],
                    'max_holding_ms':period*rng.choice([20,40,90,180]) if ecology else period*[20,60,120][variant_index%3],'cooldown_ms':period*rng.choice([1,2,5,11]) if ecology else period*[1,3,8][variant_index%3],
                    'signal_delay_ms':child_seed(seed,f'latency-{i}')%min(period,5000),'risk':{'max_drawdown_ppm':200000,'trailing_stop_ppm':50000,'cooldown_ms':period}}
                use_pine=runtime=='pine'or(runtime=='mixed'and i%10 in(0,1))
                origin='configurable_formula_adaptation'
                if use_pine:
                    config={'script':f'ecosystem/{family}.pine','execution_mode':'broker','inputs':{**params,'qty':max_qty,'allow_short':leg=='perp'},
                        'bar_interval_ms':period,'history_limit':1024,'max_qty':max_qty,'inventory_cap':max(100,size),'slippage_ticks':30,'fee_buffer_ppm':1000}
                    if originals and i%10 in(0,1):
                        original=originals[(i//10)%len(originals)];config['script']=original['installed_script'];config['inputs']={};origin=original['source_sha256'];family='Original: '+original['name']
                        if ecology:
                            source=(ROOT/'bot-plugins/pine-strategy/scripts'/original['installed_script']).read_text(encoding='utf-8')
                            if re.search(r'\bstrategy\.short\b',source):leg='perp'
                            # Original sources remain unchanged. One-minute and
                            # five-minute feeds retain the full 12-hour history.
                            config['bar_interval_ms']=300000 if original['name'] in ('D-Bot Alpha RSI Breakout Strategy','Williams -R Strategy','Highest High and Lowest Low Channel Strategy') and i%2==0 else 60000
                            config['history_limit']=2048
                            config['quantize_strategy_orders']=True
                            config['inventory_cap']=max(100,size);config['max_qty']=max_qty
                        # Change genuine source inputs using their public titles;
                        # leave dates, symbols, switches and risk rules intact.
                        unique={x.get('title')for x in original.get('inputs',[])if x.get('title')and sum(v.get('title')==x.get('title')for v in original['inputs'])==1}
                        for inp in original.get('inputs',[]):
                            title=inp.get('title');default=inp.get('default')
                            if title in unique and inp.get('name')=='input.int'and isinstance(default,int)and not isinstance(default,bool)and re.search(r'length|period|ema|sma|rsi|lookback',title,re.I):
                                number=max(inp.get('min')or 1,round(default*[.8,1.,1.2][variant_index%3]));number=min(inp.get('max')or number,number);config['inputs'][title]=number
                    plugin,version='pine.strategy','2.0.0'
                else:plugin,version='TechnicalStrategyTrader','1.0.0'
                add(role,i,plugin,version,config,leg,cash,0,family,origin)
    # Account 10 owns a small finite initial quote, independent of the bot roster.
    scenario['accounts'].append({'Spot':{'account_id':10,'cash_balance':1000000,'position_qty':100}})
    for account in (20,30):
        scenario['accounts'].append({'Spot':{'account_id':account,'cash_balance':50000000 if ecology and account==20 else 1000000,'position_qty':2000 if ecology and account==20 else 100}})
    scenario['seed_orders']=[{'NewOrder':{'order_id':1+j,'account_id':10,'side':side,'kind':{'Limit':{'price_tick':anchor+offset}},'qty':10,'reduce_only':False}}for j,(side,offset)in enumerate([('Buy',-4),('Sell',4)])]
    roster={'scope':'finite-capital strategy ecosystem','count':count,'runtime':runtime,'seed':seed,'role_counts':role_counts,'maker_model':maker_model if ecology else 'legacy',
        'role_cash_pools':pools,'total_cash_pool':sum(pools.values()),'initial_spot_inventory':sum(m['initial_spot_inventory']for m in metadata),
        'families':families,'periods_ms':sorted(set(m['config']['bar_interval_ms']for m in metadata if m['role']=='technical')),
        'participants':metadata,'qualification':'candidate population; inspect actual warmup, activity, fills, account budgets and market outcomes'}
    assert len(spec['agents'])==count
    assert sum(pools.values())==int(100_000_000*capital_scale)
    return spec,roster

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--room',default='strategy-ecosystem');p.add_argument('--seed',type=int,default=7)
    p.add_argument('--count',type=int,default=1000);p.add_argument('--runtime',choices=['native','pine','mixed'],default='native')
    p.add_argument('--original-manifest',type=Path);p.add_argument('--export',type=Path);p.add_argument('--base-url');p.add_argument('--capital-scale',type=float,default=10.);p.add_argument('--periods-ms',type=int,nargs='+')
    p.add_argument('--ecology',action='store_true');p.add_argument('--noise-fraction',type=float,default=.05)
    p.add_argument('--maker-model',choices=['legacy','adaptive','GLFT','Avellaneda'],default='adaptive')
    p.add_argument('--original-runtime-manifest',type=Path)
    a=p.parse_args();names=json.loads(a.original_runtime_manifest.read_text(encoding='utf-8'))['qualified_names'] if a.original_runtime_manifest else None
    spec,roster=population(a.room,a.seed,a.count,a.runtime,a.original_manifest,a.capital_scale,a.periods_ms,
        ecology=a.ecology,noise_fraction=a.noise_fraction,maker_model=a.maker_model,original_names=names)
    if a.export:
        a.export.parent.mkdir(parents=True,exist_ok=True);a.export.write_text(json.dumps(spec,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        a.export.with_suffix('.roster.json').write_text(json.dumps(roster,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(json.dumps({'recipe':str(a.export),'count':a.count,'roles':roster['role_counts'],'periods':roster['periods_ms']}))
    elif a.base_url:
        print(json.dumps(Client(a.base_url,bearer=os.environ.get('MARKETFORGE_AGENT_ADMIN_TOKEN'))._request('POST','/rooms',spec)))
    else:print(json.dumps(roster,ensure_ascii=False,indent=2))

if __name__=='__main__':main()
