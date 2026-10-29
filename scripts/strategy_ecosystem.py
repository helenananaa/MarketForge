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
               ecology=False,noise_fraction=.05,maker_model='adaptive',original_names=None,value_model='static',value_fraction=.1,flow_mix='default',switching_fraction=0.,price_scale=1,fast_flow=False,maker_depth_scale=None,perp_maker_depth_scale=None,maker_fee_ppm=None,pine_library=0,mark_interval_ms=None,maker_quote_mode='touch',requote_on_cancel_ack=False,basis_execution='midpoint',reservation_innovations=False,market_depth_flow=False,touch_excursion_calibration=False,closed_candle_flow=False,reactive_execution='sweep',style_execution='sweep',persistent_index_anchor=False,book_slope_flow=False,flow_surprise=False,parent_execution='sweep'):
    if not 100<=count<=10000:raise ValueError('population must be within 100..10000')
    if runtime not in ('native','pine','mixed'):raise ValueError('unknown runtime')
    if maker_quote_mode not in ('touch','model'):raise ValueError('unknown maker quote mode')
    if reactive_execution not in ('sweep','sliced'):raise ValueError('unknown reactive execution policy')
    if parent_execution not in ('sweep','touch-budget'):raise ValueError('unknown parent execution policy')
    if parent_execution!='sweep' and not fast_flow:raise ValueError('liquidity-aware parents require fast flow')
    if reactive_execution=='sliced' and not fast_flow:raise ValueError('sliced reactive execution requires fast flow')
    if style_execution not in ('sweep','touch'):raise ValueError('unknown style execution policy')
    if style_execution=='touch' and (not fast_flow or switching_fraction<=0):raise ValueError('touch style execution requires fast flow and a nonempty switching cohort')
    if basis_execution not in ('midpoint','executable','touch-hedge'):raise ValueError('unknown basis execution policy')
    if (maker_quote_mode!='touch' or requote_on_cancel_ack or reservation_innovations or market_depth_flow or touch_excursion_calibration or closed_candle_flow or persistent_index_anchor or book_slope_flow or flow_surprise) and (not fast_flow or maker_model=='legacy'):
        raise ValueError('experimental maker mechanisms require fast flow and adaptive makers')
    if persistent_index_anchor and not reservation_innovations:raise ValueError('persistent index anchor requires reservation innovations')
    if book_slope_flow and not market_depth_flow:raise ValueError('book slope flow requires market depth flow')
    if flow_surprise and not book_slope_flow:raise ValueError('flow surprise requires book slope flow')
    if value_model not in ('static','endogenous'):raise ValueError('unknown value model')
    if not 0<value_fraction<.5:raise ValueError('value fraction must be within (0, 0.5)')
    periods=periods or PERIODS
    if flow_mix not in ('default','rebalanced'):raise ValueError('unknown flow mix')
    if switching_fraction and(flow_mix!='rebalanced' or not 0<switching_fraction<.5):
        raise ValueError('style switching requires --flow-mix rebalanced and a fraction within (0, 0.5)')
    if flow_mix=='rebalanced':
        if not ecology or value_model!='endogenous' or maker_model=='legacy':
            raise ValueError('rebalanced flow requires --ecology, --value-model endogenous and adaptive makers')
        # Sub-10-second technical churn is removed; slower horizons remain.
        periods=[p for p in periods if p>=10000]
        if not periods:raise ValueError('rebalanced flow needs at least one period of 10 s or longer')
    if not periods or any(not isinstance(p,int)or p<1000 or p>86400000 or p%1000 for p in periods):raise ValueError('periods must be whole simulated seconds within the host range')
    spec=json.loads((ROOT/'scripts/fixtures/microstructure_market.json').read_text(encoding='utf-8'))
    scenario=spec['scenario'];scenario['room_id']=room;scenario['accounts']=[];scenario['seed_orders']=[];scenario['routed_seed_orders']=[];scenario['market_events']=[]
    scenario['initial_portfolios']=[];scenario['initial_allocations']=[];scenario['routed_initial_allocations']=[]
    spot=scenario['market']['Spot'];perp=scenario['extra_markets'][0]['Perp']
    # price_scale>1 prices the same asset in finer ticks (tick = 1/price_scale
    # basis points at the anchor). Cash scales with price so lot counts match.
    if price_scale<1 or int(price_scale)!=price_scale:raise ValueError('price scale must be a positive integer')
    if fast_flow and flow_mix!='rebalanced':raise ValueError('fast flow requires --flow-mix rebalanced')
    price_scale=int(price_scale);capital_scale*=price_scale
    # Fast flow defaults: fast-refreshing makers quote thin, closely spaced
    # levels (perp deeper, as it carries more volume), so net taker flow moves
    # the price while the touch stays one or two ticks wide.
    if maker_depth_scale is None:maker_depth_scale=.05 if fast_flow else 1.0
    if perp_maker_depth_scale is None:perp_maker_depth_scale=.12 if fast_flow else maker_depth_scale
    if not(0<maker_depth_scale<=1 and 0<perp_maker_depth_scale<=1):raise ValueError('maker depth scales must be within (0, 1]')
    # Fast flow quotes one-tick spreads; like liquid venues, resting liquidity
    # pays no fee so the quoted spread is not below the makers' own costs.
    if maker_fee_ppm is None:maker_fee_ppm=0 if fast_flow else 100
    if not 0<=maker_fee_ppm<=300:raise ValueError('maker fee must be within [0, taker fee]')
    anchor=10000*price_scale
    for market in (spot,perp):
        market['clearing'].update(maker_fee_ppm=maker_fee_ppm,taker_fee_ppm=300)
        market['risk']={'allow_short':False}if market is spot else{}
    perp.update(initial_mark_price_tick=anchor)
    perp['clearing']['leverage']=5
    perp['price_link']['max_age_ms']=120000
    if mark_interval_ms is not None:
        if mark_interval_ms<=0:raise ValueError('mark interval must be positive')
        # Sample the mark from the spot book on the clock, as venues publish it.
        perp['price_link']['mark_interval_ms']=mark_interval_ms
    perp['funding'].update(interval_ms=28800000,base_rate_ppm=100)
    spec.update(agents=[],agent_interval_ms=1000,autostart_agents=True)
    # Library Pine bots are added on top; the other roles keep their shares
    # of the remaining count.
    base=count-pine_library
    makers=max(4,base//10);execution=max(4,round(base*(.15 if ecology else .1)));value=max(4,round(base*value_fraction))
    noise=round(base*(.1 if fast_flow else noise_fraction)) if ecology else 0
    switching=round(base*switching_fraction)
    # Two-sided basis pairs (two bots, one account) tie the perpetual to spot.
    basis_pairs=max(2,base//60) if flow_mix=='rebalanced' else 0
    technical=count-makers-execution-value-noise-switching-2*basis_pairs-pine_library
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
    if value_model=='endogenous':
        # Valuation disagreement becomes a primary source of directional demand;
        # capital moves from the technical pool, the total stays fixed.
        pools['value']+=int(10_000_000*capital_scale);pools['technical']-=int(10_000_000*capital_scale)
        if pools['technical']<=0:raise ValueError('technical cash pool exhausted')
    if flow_mix=='rebalanced':
        # Directional demand comes mainly from valuation and parent orders;
        # technical capital becomes an overlay. Total cash is unchanged.
        pools.update(maker=int(30_000_000*capital_scale),execution=int(25_000_000*capital_scale),value=int(35_000_000*capital_scale))
        pools['value']-=int(8_000_000*capital_scale);pools['basis']=int(8_000_000*capital_scale)
        if switching:
            # Style switchers take cash from every directional role.
            # Switcher cash scales with their head count (10% at a 0.2 fraction);
            # the remainder stays with valuation traders.
            switching_cash=int(min(10_000_000,50_000_000*switching_fraction)*capital_scale)
            pools.update(execution=int(20_000_000*capital_scale),value=int(35_000_000*capital_scale)-switching_cash,switching=switching_cash)
            role_counts['switching']=switching
        # Premium-based funding settles every 15 minutes without a constant
        # bias, so a persistent perp/spot gap is actually charged.
        perp['funding'].update(interval_ms=900000,base_rate_ppm=0)
        if fast_flow:
            # Independent small-taker flow carries diffusive one-minute moves.
            pools['noise']=int(8_000_000*capital_scale);pools['value']-=int(3_000_000*capital_scale)
        pools['technical']=int(100_000_000*capital_scale)-sum(v for k,v in pools.items()if k!='technical')
        if pools['technical']<=0:raise ValueError('technical cash pool exhausted')
    if pine_library:
        # Library Pine originals share the technical capital by head count.
        pools['pine']=pools['technical']*pine_library//(technical+pine_library);pools['technical']-=pools['pine']
        role_counts['pine']=pine_library
    originals=[]
    if original_manifest:
        originals=json.loads(Path(original_manifest).read_text(encoding='utf-8')).get('qualified',[])
        if original_names is not None: originals=[x for x in originals if x['name'] in original_names]
    library=[]
    if pine_library:
        # One bot per distinct original, drawn round-robin across behavior
        # families so trend scripts do not crowd out the rest.
        if pine_library>len(originals):raise ValueError(f'--pine-library {pine_library} exceeds {len(originals)} qualified originals')
        lr=random.Random(child_seed(seed,'pine-library'));groups={}
        for x in originals:groups.setdefault(x.get('family_hint','other'),[]).append(x)
        for k in sorted(groups):lr.shuffle(groups[k])
        while len(library)<pine_library:
            for k in sorted(groups):
                if groups[k] and len(library)<pine_library:library.append(groups[k].pop())
    def add(role,index,plugin,version,config,leg,cash,position=0,family=None,origin=None):
        account=100+len(spec['agents']);name=f'{role}-{leg}-{index}';instrument=f'V-BTC-{leg.upper()}'
        scenario['accounts'].append({'Spot'if position else'Basic':{'account_id':account,'cash_balance':cash,**({'position_qty':position}if position else{})}})
        spec['agents'].append({'Plugin':{'participant':{'participant_id':name,'kind':'MarketMaker'if role=='maker'else'Strategy'if role in('technical','pine')else'RuleAgent',
            'room_id':room,'account_id':account,'instrument_id':instrument},'plugin_id':plugin,'plugin_version':version,'state_version':1,'config_version':1,
            'seed':max(1,child_seed(seed,name)&(2**53-1)),'config':config}})
        metadata.append({'participant_id':name,'account_id':account,'role':role,'family':family,'instrument':instrument,'cash':cash,'initial_spot_inventory':position,'config':config,'origin':origin})
    def cashes(role,n):
        # Endogenous valuation accounts draw heavy-tailed (Pareto 1.5, capped)
        # wealth so equity-scaled parent orders are heavy-tailed as well.
        if role=='value'and value_model=='endogenous':vr=random.Random(child_seed(seed,'endogenous-value-cash'));weights=[round(min(50.,vr.paretovariate(1.5))*1000)for _ in range(n)]
        else:weights=[rng.choice([1,1,1,2,3,5,10])for _ in range(n)]
        total=sum(weights);pool=pools[role]
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
                    version='1.0.0'
                    if flow_mix=='rebalanced':
                        # Own stream so the 1.0.0 parameter draws above are unchanged.
                        fr=random.Random(child_seed(seed,f'maker-flow-{i}'))
                        config.update(flow_skew_ppm=fr.choice([300000,600000,1000000]),flow_widen_ppm=fr.choice([0,500000,1000000]),
                            flow_half_life_ms=fr.choice([5000,15000,30000]));version='1.1.0'
                        # Perpetual quotes lean on the spot index so the book cannot drift freely.
                        if fast_flow:
                            # Liquidity at the touch is replaced within a step or
                            # two, as competing market makers do in liquid markets.
                            config.update(refresh_ms=fr.choice([1000,1000,2000]),jitter_ms=fr.randrange(0,1001),
                                quote_ttl_ms=fr.choice([10000,20000,30000]))
                            # Price off other participants' quotes: a maker's own
                            # skewed quotes must not become its next reference.
                            config['external_reference']=True
                        if leg=='perp':
                            # Perp quotes follow perp trading; basis pairs keep the
                            # link to spot through actual trades.
                            config['index_weight_ppm']=fr.choice([0,50000,100000])
                            # Perp flow is persistently one-sided (levered switchers);
                            # strong widening left a bounce-dominated perp spread.
                            config['flow_widen_ppm']=fr.choice([0,250000])
                    # Quoted size relative to directional flow sets how far a
                    # minute's net flow moves the price; liquid markets refill a
                    # book that flow actually consumes.
                    scale=perp_maker_depth_scale if leg=='perp' else maker_depth_scale
                    if scale<1:config['max_qty']=max(1,round(config['max_qty']*scale))
                    add(role,i,'AdaptiveLiquidityMaker',version,config,leg,cash,inventory,model,'source_algorithm_adaptation')
                else:add(role,i,'DynamicMarketMaker','1',config,leg,cash,inventory)
            elif role=='execution':
                direction='Buy'if(i//2)%2==0 else'Sell'
                target=max(5,min(4000 if ecology else 800,cash//anchor//(2 if ecology else 3)));position=target if leg=='spot'and direction=='Sell'else 0
                # Long-duration asynchronous parent tasks supply genuine directional
                # demand. Their horizons/size are finite, not perpetual noise.
                add(role,i,'ExecutionTrader','1',{'side':direction,'target_qty':target,'horizon_ms':[1200000,3600000,7200000][(i//4)%3],
                    'start_after_ms':rng.randrange(0,36000000) if ecology else (i//4)%12*15000,'max_qty':[1,2,4][i%3],'max_slippage_ticks':30,'decision_interval_ms':[1000,3000,7000][i%3],
                    'jitter_ms':i%4*500,'inventory_cap':max(100,target*2),'fee_buffer_ppm':1000,'fallback_price_tick':anchor},leg,cash,position)
            elif role=='switching':
                # Own stream; two thirds trade the perpetual, where leverage,
                # exchange-side stops and venue liquidation can cascade.
                sr=random.Random(child_seed(seed,f'style-switching-{i}'))
                leg='spot'if i%2==0 else'perp'
                leverage=sr.choice([1,2,3])if leg=='perp'else 1
                fast=sr.choice([10000,20000,60000,120000] if fast_flow else [30000,60000,120000,300000])
                add(role,i,'StyleSwitchingTrader','1.0.0',{'inventory_cap':max(100,cash*leverage//anchor),
                    'equity_use_ppm':sr.choice([200000,400000,600000]),'target_leverage':leverage,
                    'decision_interval_ms':sr.choice([2000,5000,10000,20000] if fast_flow else [5000,10000,20000,40000]),'fast_half_life_ms':fast,
                    'slow_half_life_ms':fast*sr.choice([4,8]),'anchor_half_life_ms':sr.choice([1800000,3600000,10800000,21600000]),
                    'saturation_ppm':sr.choice([1000,2000,4000]),'fitness_half_life_ms':sr.choice([300000,900000,1800000]),
                    'intensity_ppm':sr.choice([1000000,3000000,10000000]),'switch_ppm':sr.choice([50000,100000,200000]),
                    'herding_ppm':sr.choice([0,500000,1000000,2000000]),'flow_half_life_ms':sr.choice([30000,60000,120000]),
                    'child_ppm':sr.choice([100000,200000,300000]),'min_trade_ppm':150000,'max_slippage_ppm':sr.choice([2000,3000,5000]),
                    'stop_lookback_ms':sr.choice([900000,1800000,3600000]),'stop_buffer_ppm':sr.choice([2000,4000,8000]),
                    'stop_cooldown_ms':sr.choice([300000,600000,1200000]),'warmup_ms':sr.choice([600000,900000]),
                    # Internal stops: exchange-side protection is currently ~6.5x
                    # slower in long runs; exits still go through real orders.
                    'stop_mode':'Internal','fee_buffer_ppm':1000,'fallback_price_tick':anchor},leg,cash,0,None,'style_switching')
            elif role=='value'and value_model=='endogenous':
                # Beliefs come only from each bot's own view of market history:
                # an EWMA anchor over its own horizon, a private bias and a
                # private seeded drift. No shared value path is provided.
                # Own stream: other roles' draws are unchanged by this mode.
                vr=random.Random(child_seed(seed,f'endogenous-value-{i}'))
                leverage=vr.choice([1,2,3])if leg=='perp'else 1
                position=cash//anchor//2 if leg=='spot'else 0
                cap=max(100,cash*leverage//anchor)
                add(role,i,'EndogenousValueTrader','1.0.0',{'neutral_position':min(position,cap),'inventory_cap':cap,
                    'equity_use_ppm':vr.choice([200000,400000,600000]),'target_leverage':leverage,
                    'anchor_half_life_ms':vr.choice([600000,1800000,3600000,7200000,21600000,43200000]),
                    'warmup_ms':vr.choice([300000,600000,900000]),'bias_ppm':max(-30000,min(30000,round(vr.gauss(0,4000)))),
                    'belief_drift_ppm':vr.choice([0,2000,5000,10000]),'belief_drift_cap_ppm':20000,
                    'edge_ppm':vr.choice([1000,2000,4000]),'saturation_ppm':vr.choice([5000,10000,20000]),
                    'min_confidence_ppm':vr.choice([100000,250000,500000]),'confidence_drawdown_ppm':vr.choice([50000,100000,200000]),
                    'rebalance_ppm':vr.choice([100000,200000,300000]),'decision_interval_ms':vr.choice([15000,30000,60000,120000]),
                    'child_interval_ms':vr.choice([3000,7000,15000]),'execution_horizon_ms':vr.choice([600000,1800000,3600000,7200000]),
                    'passive_ppm':vr.choice([200000,500000,800000]),'max_slippage_ppm':vr.choice([2000,3000,5000]),
                    'order_ttl_ms':vr.choice([15000,30000,60000]),'vol_activity_ppm':vr.choice([0,500000,1000000]),
                    'fee_buffer_ppm':1000,'fallback_price_tick':anchor},leg,cash,position,None,'endogenous_valuation')
            elif role=='value':
                belief=anchor+[-300,-180,-100,-40,40,100,180,300][(i//2)%8]
                position=max(5,min(100,cash//anchor//4))if leg=='spot'else 0
                add(role,i,'ValueTrader','1',{'fair_price_tick':belief,'edge_ticks':[8,16,30][i%3],
                    'value_shift_ticks':0,'value_shift_at_ms':0,'decision_interval_ms':[5000,15000,60000][i%3],
                    'jitter_ms':5000,'max_qty':[1,2,3][i%3],'inventory_cap':max(100,cash//anchor//2),'fee_buffer_ppm':1000,'fallback_price_tick':anchor},leg,cash,position)
            elif role=='noise'and fast_flow:
                # Reactive small takers: arrival rate rises with observed trade
                # activity (bursts), direction chases or fades the observed move,
                # sizes are heavy-tailed. Own seeded stream per bot.
                nr=random.Random(child_seed(seed,f'fast-flow-{i}'))
                position=max(5,min(600,cash//anchor//4)) if leg=='spot' else 0
                cap=max(300,cash//anchor,2*position)
                # Weak reaction, tilted contrarian (about a third chase, two
                # thirds fade the recent move), so persistent flow does not
                # turn into predictable one-minute trends.
                chase=nr.choice([200000,400000,600000])*(1 if nr.random()<.35 else -1)
                add(role,i,'ReactiveFlowTrader','1.0.0',{'base_interval_ms':nr.choice([4000,8000,15000,30000]),
                    # Attention: arrivals rise with the observed move, so large
                    # candles carry large volume and quiet minutes stay quiet.
                    'attention_ppm':nr.choice([4000000,8000000,12000000]),
                    'short_half_life_ms':nr.choice([10000,20000,40000]),'long_half_life_ms':nr.choice([600000,1200000]),
                    # Moderate excitation: activity clusters, but quiet periods keep
                    # a baseline flow instead of going dead.
                    'excitation_ppm':nr.choice([600000,1000000,1300000]),'move_half_life_ms':nr.choice([30000,60000,180000]),
                    'move_scale_ppm':nr.choice([1000,2000,5000]),'chase_ppm':chase,'herd_ppm':nr.choice([0,150000,300000]),
                    'inventory_target':position,'inventory_pull_ppm':nr.choice([300000,800000]),'min_qty':2,
                    'max_qty':nr.choice([60,150,300]),'size_alpha_ppm':nr.choice([1300000,1500000,1800000]),
                    'aggressive_ppm':nr.choice([600000,800000,950000]),'max_slippage_ppm':nr.choice([500,1000,2000]),
                    'order_ttl_ms':nr.choice([10000,20000,40000]),'inventory_cap':cap,
                    'target_leverage':nr.choice([1,2]) if leg=='perp' else 1,'fee_buffer_ppm':1000,'fallback_price_tick':anchor},leg,cash,position)
            elif role=='noise':
                position=max(5,min(50,cash//anchor//4)) if leg=='spot' else 0
                add(role,i,'AdaptiveNoiseTrader','1',{'activity_ppm':100000,'market_order_ratio_ppm':500000,'side_persistence_ppm':550000,
                    'max_qty':2,'inventory_cap':100,'price_radius_ticks':12,'order_ttl_ms':45000,
                    'decision_interval_ms':rng.choice([3000,7000,15000]),'jitter_ms':9000,'fallback_price_tick':anchor},leg,cash,position)
            elif role=='pine':
                # Unchanged original source and default inputs. Spot has no
                # shorting, exchange exits or stop entries, so scripts using any
                # of them trade the perpetual; the rest alternate.
                original=library[i];pr=random.Random(child_seed(seed,f'pine-{i}'))
                source=(ROOT/'bot-plugins/pine-strategy/scripts'/original['installed_script']).read_text(encoding='utf-8')
                if original.get('has_short') or re.search(r'\bstrategy\.exit\b|\bstop\s*=',source):leg='perp'
                size=max(2,min(200,cash//anchor//2))
                add(role,i,'pine.strategy','2.0.0',{'script':original['installed_script'],'execution_mode':'broker','inputs':{},
                    'bar_interval_ms':60000*pr.choice([m for m in (1,1,1,3,5) if m in original.get('timeframes_minutes',[1])] or original['timeframes_minutes']),'history_limit':2048,'quantize_strategy_orders':True,
                    'max_qty':pr.choice([1,2,4,8]),'inventory_cap':max(100,size),'slippage_ticks':30,'fee_buffer_ppm':1000},
                    leg,cash,0,'Original: '+original['name'],original['source_sha256'])
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
    if basis_pairs:
        br=random.Random(child_seed(seed,'basis-link'))
        for j in range(basis_pairs):
            account=100+len(spec['agents']);cash=pools['basis']//basis_pairs+(pools['basis']%basis_pairs if j==basis_pairs-1 else 0)
            inventory=cash//anchor//4
            scenario['accounts'].append({'Spot':{'account_id':account,'cash_balance':cash,'position_qty':inventory}})
            common={'neutral_spot':inventory,'max_hedge':inventory,'step_qty':max(2,inventory//5),
                # Both legs cross the spread at the taker fee on entry and exit
                # (12 bp a round trip); fast flow only enters wider than that.
                'entry_ppm':br.choice([2000,2500,3000] if fast_flow else [1500,2000,3000]),'exit_ppm':br.choice([300,500]),'max_slippage_ppm':2000,
                'decision_interval_ms':br.choice([1000,1000,2000]),'jitter_ms':500,'target_leverage':2,'fee_buffer_ppm':1000}
            for leg,instrument,peer in (('Perp','V-BTC-PERP','V-BTC-SPOT'),('Spot','V-BTC-SPOT','V-BTC-PERP')):
                name=f'basis-{leg.lower()}-{j}'
                config={**common,'leg':leg,'hedge_instrument_id':peer}
                spec['agents'].append({'Plugin':{'participant':{'participant_id':name,'kind':'RuleAgent','room_id':room,'account_id':account,'instrument_id':instrument},
                    'plugin_id':'BasisLinkTrader','plugin_version':{'midpoint':'1.0.0','executable':'1.1.0','touch-hedge':'1.2.0'}[basis_execution],'state_version':1,'config_version':1,
                    'seed':max(1,child_seed(seed,name)&(2**53-1)),'config':config}})
                metadata.append({'participant_id':name,'account_id':account,'role':'basis','family':None,'instrument':instrument,'cash':cash if leg=='Perp' else 0,
                    'initial_spot_inventory':inventory if leg=='Perp' else 0,'config':config,'origin':'basis_link'})
        role_counts['basis']=2*basis_pairs
    # Account 10 owns a small finite initial quote, independent of the bot roster.
    scenario['accounts'].append({'Spot':{'account_id':10,'cash_balance':1000000*price_scale,'position_qty':100}})
    for account in (20,30):
        scenario['accounts'].append({'Spot':{'account_id':account,'cash_balance':(50000000 if ecology and account==20 else 1000000)*price_scale,'position_qty':2000 if ecology and account==20 else 100}})
    scenario['seed_orders']=[{'NewOrder':{'order_id':1+j,'account_id':10,'side':side,'kind':{'Limit':{'price_tick':anchor+offset*price_scale}},'qty':10,'reduce_only':False}}for j,(side,offset)in enumerate([('Buy',-4),('Sell',4)])]
    roster={'scope':'finite-capital strategy ecosystem','count':count,'flow_mix':flow_mix,'price_scale':price_scale,'fast_flow':fast_flow,'maker_fee_ppm':maker_fee_ppm,'mark_interval_ms':mark_interval_ms,'maker_depth_scale':maker_depth_scale,'perp_maker_depth_scale':perp_maker_depth_scale,'runtime':runtime,'seed':seed,'role_counts':role_counts,'maker_model':maker_model if ecology else 'legacy','value_model':value_model,
        'role_cash_pools':pools,'total_cash_pool':sum(pools.values()),'initial_spot_inventory':sum(m['initial_spot_inventory']for m in metadata),
        'families':families,'periods_ms':sorted(set(m['config']['bar_interval_ms']for m in metadata if m['role']=='technical')),
        'participants':metadata,'qualification':'candidate population; inspect actual warmup, activity, fills, account budgets and market outcomes'}
    if price_scale>1:
        # Tick-denominated parameters keep their basis-point meaning; maker
        # spread floors drop to a few fine ticks so the quoted spread stays
        # small next to one-minute moves, as in liquid markets.
        for agent in spec['agents']:
            c=agent['Plugin']['config']
            for k in ('max_half_spread_ticks','level_spacing_ticks','max_slippage_ticks','price_radius_ticks','slippage_ticks',
                      'half_spread_ticks','inventory_skew_ticks','withdraw_volatility_ticks','size_volatility_ticks'):
                if k in c:c[k]*=price_scale
            if 'min_half_spread_ticks' in c:c['min_half_spread_ticks']=max(1,c['min_half_spread_ticks']*price_scale//10)
            if fast_flow and agent['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
                # Competing makers: a one-tick floor, half of them capped at a
                # few ticks, and closely spaced levels, so the touch is tight
                # and depth is continuous instead of lumpy.
                c['min_half_spread_ticks']=1
                c['level_spacing_ticks']=max(1,c['level_spacing_ticks']//5)
                if maker_quote_mode=='touch' and child_seed(seed,'tight-'+agent['Plugin']['participant']['participant_id'])%2==0:
                    # Touch makers: one tick either side of their center,
                    # re-quoting every step so a swept touch is restored at once.
                    c.update(max_half_spread_ticks=1,refresh_ms=1000,jitter_ms=0)
    if requote_on_cancel_ack:
        for agent in spec['agents']:
            if agent['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
                agent['Plugin']['config']['requote_on_cancel_ack']=True
    if reservation_innovations:
        for agent in spec['agents']:
            if agent['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
                agent['Plugin']['config']['reservation_innovations']=True
    if market_depth_flow:
        for agent in spec['agents']:
            if agent['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
                agent['Plugin']['config']['market_depth_flow']=True
        roster['market_depth_flow']={'enabled':True,'qualification':'experimental; normalize fresh order flow by observed external liquidity within existing quote reach'}
    if parent_execution=='touch-budget':
        for agent in spec['agents']:
            plugin=agent['Plugin']
            if plugin['plugin_id']=='ExecutionTrader':
                plugin['plugin_version']='1.1.0'
                plugin['config']['liquidity_aware']=True
        roster['parent_execution']={'policy':'touch-budget','qualification':'experimental finite business parents; unchanged targets and deadlines, external touch children, acknowledgement-driven debt retry on a new clock, authoritative fill/expiry ledger'}
    for enabled,key in ((touch_excursion_calibration,'touch_excursion_calibration'),(closed_candle_flow,'closed_candle_flow'),(persistent_index_anchor,'persistent_index_anchor'),(book_slope_flow,'book_slope_flow'),(flow_surprise,'flow_surprise')):
        if enabled:
            for agent in spec['agents']:
                if agent['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
                    agent['Plugin']['config'][key]=True
            roster[key]={'enabled':True,'qualification':'experimental observation mechanism; requires paired wick/market validation'}
    if maker_quote_mode!='touch' or requote_on_cancel_ack:
        roster['maker_mechanisms']={'quote_mode':maker_quote_mode,'requote_on_cancel_ack':requote_on_cancel_ack,
            'qualification':'experimental, not a real-market calibration or a promoted default'}
    if basis_execution!='midpoint':
        roster['basis_execution']={'policy':basis_execution,'qualification':'experimental; executable pairs and actual hedge capacity, not atomic cross-venue fills'}
    if reservation_innovations:
        roster['reservation_innovations']={'enabled':True,'qualification':'experimental closed-loop reservation-bias policy; requires paired market evaluation'}
    if reactive_execution=='sliced':
        for agent in spec['agents']:
            if agent['Plugin']['plugin_id']=='ReactiveFlowTrader':agent['Plugin']['plugin_version']='1.1.0'
        roster['reactive_execution']={'policy':'sliced','qualification':'experimental; original aggressive demand is retained until real fills or its original finite TTL, children execute at observed external touch only'}
    if style_execution=='touch':
        for agent in spec['agents']:
            if agent['Plugin']['plugin_id']=='StyleSwitchingTrader':agent['Plugin']['plugin_version']='1.1.0'
        roster['style_execution']={'policy':'touch','qualification':'experimental; unchanged target position/signals/stops and existing child caps, further constrained by observed external touch liquidity'}
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
    p.add_argument('--value-model',choices=['static','endogenous'],default='static');p.add_argument('--value-fraction',type=float,default=.1)
    p.add_argument('--flow-mix',choices=['default','rebalanced'],default='default');p.add_argument('--switching-fraction',type=float,default=0.)
    p.add_argument('--price-scale',type=int,default=1);p.add_argument('--fast-flow',action='store_true')
    p.add_argument('--maker-depth-scale',type=float);p.add_argument('--perp-maker-depth-scale',type=float)
    p.add_argument('--maker-fee-ppm',type=int,help='resting-order fee; default 0 with --fast-flow, else 100')
    p.add_argument('--mark-interval-ms',type=int,help='sample the perp mark once per this much market time at the clock; default: after every spot order that moves it')
    p.add_argument('--pine-library',type=int,default=0,help='bots running distinct originals from --original-manifest (part of --count)')
    p.add_argument('--maker-quote-mode',choices=['touch','model'],default='touch',help='experimental model mode preserves calculated spreads and independently drawn refresh times; touch preserves the old recipe')
    p.add_argument('--requote-on-cancel-ack',action='store_true',help='re-evaluate cancelled maker quotes on the next authoritative observation without waiting another refresh interval')
    p.add_argument('--basis-execution',choices=['midpoint','executable','touch-hedge'],default='midpoint',help='experimental executable basis pairs, depth/capital checks and waiting for the actual hedge')
    p.add_argument('--reservation-innovations',action='store_true',help='experimental maker policy: apply changes in reservation bias after authoritative quote confirmation')
    p.add_argument('--market-depth-flow',action='store_true',help='experimental maker policy: normalize public flow by visible external liquidity rather than one bot quote size')
    p.add_argument('--touch-excursion-calibration',action='store_true',help='experimental maker intensity: measure candle penetration beyond the book visible at interval start')
    p.add_argument('--closed-candle-flow',action='store_true',help='experimental maker flow: complete aggressor volume from closed public one-second bars')
    p.add_argument('--persistent-index-anchor',action='store_true',help='experimental maker fix: retain current live index restoring force while differencing local inventory reservation bias')
    p.add_argument('--book-slope-flow',action='store_true',help='experimental maker response: inverse visible external supply curve instead of half-spread-scaled flow')
    p.add_argument('--flow-surprise',action='store_true',help='experimental maker response: use signed-flow innovations predicted from the existing public-flow half-life, not repeated predictable flow')
    p.add_argument('--parent-execution',choices=['sweep','touch-budget'],default='sweep',help='experimental finite business parents: same target/deadline, one outstanding child at external touch, authoritative fill ledger')
    p.add_argument('--reactive-execution',choices=['sweep','sliced'],default='sweep',help='experimental aggressive-demand execution: retain the original quantity/price boundary/TTL and submit touch-sized IOC children')
    p.add_argument('--style-execution',choices=['sweep','touch'],default='sweep',help='experimental style execution: keep the target and stop policy, cap each IOC child to observed external touch liquidity')
    a=p.parse_args();names=json.loads(a.original_runtime_manifest.read_text(encoding='utf-8'))['qualified_names'] if a.original_runtime_manifest else None
    spec,roster=population(a.room,a.seed,a.count,a.runtime,a.original_manifest,a.capital_scale,a.periods_ms,
        ecology=a.ecology,noise_fraction=a.noise_fraction,maker_model=a.maker_model,original_names=names,
        value_model=a.value_model,value_fraction=a.value_fraction,flow_mix=a.flow_mix,switching_fraction=a.switching_fraction,price_scale=a.price_scale,fast_flow=a.fast_flow,maker_depth_scale=a.maker_depth_scale,perp_maker_depth_scale=a.perp_maker_depth_scale,maker_fee_ppm=a.maker_fee_ppm,pine_library=a.pine_library,mark_interval_ms=a.mark_interval_ms,maker_quote_mode=a.maker_quote_mode,requote_on_cancel_ack=a.requote_on_cancel_ack,basis_execution=a.basis_execution,reservation_innovations=a.reservation_innovations,market_depth_flow=a.market_depth_flow,touch_excursion_calibration=a.touch_excursion_calibration,closed_candle_flow=a.closed_candle_flow,reactive_execution=a.reactive_execution,style_execution=a.style_execution,persistent_index_anchor=a.persistent_index_anchor,book_slope_flow=a.book_slope_flow,flow_surprise=a.flow_surprise,parent_execution=a.parent_execution)
    if a.export:
        a.export.parent.mkdir(parents=True,exist_ok=True);a.export.write_text(json.dumps(spec,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        a.export.with_suffix('.roster.json').write_text(json.dumps(roster,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(json.dumps({'recipe':str(a.export),'count':a.count,'roles':roster['role_counts'],'periods':roster['periods_ms']}))
    elif a.base_url:
        print(json.dumps(Client(a.base_url,bearer=os.environ.get('MARKETFORGE_AGENT_ADMIN_TOKEN'))._request('POST','/rooms',spec)))
    else:print(json.dumps(roster,ensure_ascii=False,indent=2))

if __name__=='__main__':main()
