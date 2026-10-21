//! Research a strategy population using the existing frozen native observation
//! wave and the real gateway. This changes no production scheduler or data path.
use exchange_core::*;
use std::collections::BTreeMap;
#[path = "support/research_checkpoint.rs"]
mod checkpoint;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run(BotRegistry::with_builtins())
}

pub fn run(registry: BotRegistry) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let recipe = args.next().ok_or("recipe path required")?;
    let steps: u64 = args.next().unwrap_or_else(|| "600".into()).parse()?;
    if !(1..=100000).contains(&steps) {
        return Err("steps must be within 1..100000".into());
    }
    let seconds: u64 = args.next().unwrap_or_else(|| "1".into()).parse()?;
    if !(1..=60).contains(&seconds) {
        return Err("clock step must be 1..60 seconds".into());
    }
    let recipe_source = std::fs::read_to_string(&recipe)?;
    let spec: population::BackgroundMarket = serde_json::from_str(&recipe_source)?;
    let room = spec.scenario.room_id.clone();
    let templates = spec.agents;
    let mut bots = templates
        .iter()
        .map(|t| registry.create(t, &t.initial_state()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut rooms = RoomManager::new();
    rooms
        .create_room(spec.scenario)
        .map_err(|e| format!("{e:?}"))?;
    let mut order_id = 100000;
    let mut result = if let Ok(path) = std::env::var("MF_RESEARCH_CHECKPOINT") {
        checkpoint::run(
            &registry,
            &templates,
            &room,
            &mut rooms,
            &mut bots,
            &mut order_id,
            steps,
            seconds,
            &recipe_source,
            std::path::Path::new(&path),
        )?
    } else {
        run_waves(
            &registry,
            &templates,
            &room,
            &mut rooms,
            &mut bots,
            &mut order_id,
            steps,
            seconds,
        )?
    };
    if let Some(directory) = args
        .next()
        .filter(|_| result["steps"].as_u64() == Some(steps))
    {
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory)?;
        std::fs::write(directory.join("parent.json"), serde_json::to_vec(&result)?)?;
        let frozen = fingerprint(&rooms, &room)?;
        let states = bots.iter().map(|b| b.snapshot()).collect::<Vec<_>>();
        let mut branches = vec![];
        for instrument in ["V-BTC-SPOT", "V-BTC-PERP"] {
            for (side, qty) in [
                (Side::Buy, 0),
                (Side::Buy, 10),
                (Side::Sell, 10),
                (Side::Buy, 100),
                (Side::Sell, 100),
                (Side::Buy, 1000),
                (Side::Sell, 1000),
            ] {
                let name = format!("{}-{:?}-{}", instrument, side, qty);
                if checkpoint::stopped() {
                    result["fork_receipts"] = serde_json::json!(branches);
                    result["fork_parent_unchanged"] = serde_json::json!(true);
                    result["forks_complete"] = serde_json::json!(false);
                    println!("{}", serde_json::to_string(&result)?);
                    return Ok(());
                }
                eprintln!("fork={name}");
                let mut branch_rooms = rooms.clone();
                let mut branch_bots = templates
                    .iter()
                    .zip(&states)
                    .map(|(t, s)| registry.create(t, s))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut branch_order_id = order_id;
                let before = branch_rooms
                    .book_snapshot_for(&room, instrument)
                    .map_err(|e| format!("{e:?}"))?;
                let mut intervention = serde_json::Value::Null;
                if qty > 0 {
                    let bid = before.bids.first().ok_or("probe requires bid")?.price_tick;
                    let ask = before.asks.first().ok_or("probe requires ask")?.price_tick;
                    let mid = bid + (ask - bid) / 2;
                    let action = OrderAction::PlaceImmediateOrCancel {
                        side,
                        price_tick: Some(if side == Side::Buy {
                            mid + 250
                        } else {
                            (mid - 250).max(1)
                        }),
                        qty,
                    };
                    let mut gateway =
                        OrderGateway::new_scheduler(&mut branch_rooms, branch_order_id);
                    let execution = gateway
                        .submit_action(GatewayRequest {
                            participant_id: "research-player".into(),
                            room_id: room.clone(),
                            instrument_id: Some(instrument.into()),
                            account_id: 20,
                            action,
                        })
                        .map_err(|e| format!("{e:?}"))?;
                    branch_order_id = gateway.next_order_id();
                    let mut filled = 0;
                    let mut notional = 0_i128;
                    let mut prints = vec![];
                    if let ActorExecutionResult::Accepted(result) = &execution.execution.result {
                        let events = match result {
                            MarketExecution::Spot(r) => &r.events,
                            MarketExecution::Perp(r) => &r.events,
                        };
                        for event in events {
                            if let Event::TradePrinted(t) = &event.event {
                                filled += t.qty;
                                notional += i128::from(t.qty) * i128::from(t.price_tick);
                                prints.push(serde_json::json!({"price_tick":t.price_tick,"qty":t.qty,"maker_account_id":t.maker_account_id,"taker_order_id":t.taker_order_id}));
                            }
                        }
                    }
                    intervention = serde_json::json!({"instrument_id":instrument,"side":side,"requested_qty":qty,"filled_qty":filled,"notional":notional.to_string(),"result":format!("{:?}",execution.execution.result),"time_ms":execution.execution.market_time_ms,"fills":prints,"command":execution.command});
                }
                let immediate = branch_rooms
                    .book_snapshot_for(&room, instrument)
                    .map_err(|e| format!("{e:?}"))?;
                let mut branch = run_waves(
                    &registry,
                    &templates,
                    &room,
                    &mut branch_rooms,
                    &mut branch_bots,
                    &mut branch_order_id,
                    (600 / seconds).max(1),
                    seconds,
                )?;
                branch["intervention"] = intervention;
                branch["before_book"] = serde_json::to_value(before)?;
                branch["immediate_book"] = serde_json::to_value(immediate)?;
                let path = directory.join(format!("{name}.json"));
                std::fs::write(&path, serde_json::to_vec(&branch)?)?;
                if fingerprint(&rooms, &room)? != frozen {
                    return Err("research fork mutated the parent".into());
                }
                branches.push(path.to_string_lossy().to_string());
            }
        }
        result["fork_receipts"] = serde_json::json!(branches);
        result["fork_parent_unchanged"] = serde_json::json!(true);
        result["forks_complete"] = serde_json::json!(true);
    }
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn fingerprint(
    rooms: &RoomManager,
    room: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(
        serde_json::json!({"time":rooms.clock(room).map_err(|e|format!("{e:?}"))?.market_time_ms(),"assets":rooms.net_worth_snapshot(room).map_err(|e|format!("{e:?}"))?,"spot":rooms.book_snapshot_for(room,"V-BTC-SPOT").map_err(|e|format!("{e:?}"))?,"perp":rooms.book_snapshot_for(room,"V-BTC-PERP").map_err(|e|format!("{e:?}"))?}),
    )
}

/// Account fields that change only through the account's own activity (fills,
/// fees, funding, margin state); mark price moves leave them unchanged.
#[derive(Clone, PartialEq, Eq)]
enum Settled {
    Spot(exchange_core::spot::SpotAccountSnapshot),
    Perp {
        cash_balance: i128,
        position_qty: i128,
        avg_entry_price_tick: i64,
        realized_pnl: i128,
        fees_paid: i128,
        funding_pnl: i128,
        margin_status: exchange_core::perp::PerpMarginStatus,
    },
}

fn settled_account(account: Option<AccountSnapshot>) -> Option<Settled> {
    account.map(|account| match account {
        AccountSnapshot::Spot(a) => Settled::Spot(a),
        AccountSnapshot::Perp(a) => Settled::Perp {
            cash_balance: a.cash_balance,
            position_qty: a.position_qty,
            avg_entry_price_tick: a.avg_entry_price_tick,
            realized_pnl: a.realized_pnl,
            fees_paid: a.fees_paid,
            funding_pnl: a.funding_pnl,
            margin_status: a.margin_status,
        },
    })
}

/// A screened plugin's private inputs on one market it observes: resting
/// orders, protections and the settled account.
#[derive(Clone, PartialEq, Eq)]
struct MarketInputs {
    orders: Vec<exchange_core::model::Order>,
    protections: Vec<PositionProtection>,
    settled: Option<Settled>,
}

/// Inputs that only the account's own activity changes: per observed market
/// (its own first) the market inputs, and its conditional orders there without
/// the exchange's own scan cursor, which moves every step while one is armed.
#[derive(Clone, PartialEq, Eq)]
struct PrivateInputs {
    markets: Vec<MarketInputs>,
    conditions: Vec<(
        String,
        exchange_core::conditional_orders::ConditionalOrderSpec,
        String,
        Option<u64>,
    )>,
}

/// A screened plugin is due when this changes: the bar index on each interval
/// it reads, the poll period (polling only) and its private inputs. Typed
/// equality of these values is equality of their JSON: none holds a float.
#[derive(Clone, PartialEq, Eq)]
struct WakeKey {
    bars: (Option<u64>, Vec<u64>),
    poll: Option<u64>,
    private: PrivateInputs,
}

/// What decides whether a screened plugin is due: its last wake key, the wake
/// time and last-trade levels it requested, and a receipt it has not read.
#[derive(Clone, Default)]
struct Screen {
    key: Option<WakeKey>,
    wake_ms: Option<u64>,
    prices: Vec<PriceWake>,
    receipt: bool,
}

/// A plugin screened for wakes: its index, account and the markets whose
/// private inputs it compares, its own first.
struct Screened {
    bot: usize,
    account: u64,
    markets: Vec<String>,
}

/// A plugin's `_wake_prices` entry: call it once the last trade on the market
/// is at or above / at or below a level that its decisions compare against.
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PriceWake {
    instrument_id: String,
    at_or_above: Option<i64>,
    at_or_below: Option<i64>,
}

/// The private inputs of every screened plugin that differ from its last wake
/// key (None: unchanged), read from the committed state: the values its
/// observation would show, without building the observation. Protections and
/// conditional orders are grouped by market and account once, instead of
/// filtering every entry on the venue for each plugin. Plugins are split
/// across up to `threads` threads; results come back in plugin order.
fn changed_private_inputs(
    rooms: &RoomManager,
    room: &str,
    screened: &[Screened],
    screens: &[Screen],
    conditions: bool,
    threads: usize,
) -> Result<Vec<Option<PrivateInputs>>, Box<dyn std::error::Error>> {
    type Entries<'a> = (
        Vec<&'a PositionProtection>,
        Vec<&'a exchange_core::conditional_orders::ConditionalOrder>,
    );
    let simulation = rooms.simulation_room(room).map_err(|e| format!("{e:?}"))?;
    let mut venues = BTreeMap::<_, std::collections::BTreeSet<&str>>::new();
    for s in screened {
        for market in &s.markets {
            let venue = simulation
                .venue_id_for_instrument(market)
                .ok_or("unknown related market")?;
            venues.entry(venue).or_default().insert(market);
        }
    }
    // Per (market, account), in the venue's key order, as the per-account
    // accessors return them.
    let mut entries = BTreeMap::<(&str, u64), Entries<'_>>::new();
    for (venue, markets) in &venues {
        let exchange = simulation.exchange(venue).map_err(|e| format!("{e:?}"))?;
        for p in exchange.all_position_protections() {
            if let Some(market) = markets.get(p.instrument_id.as_str()) {
                entries.entry((*market, p.account_id)).or_default().0.push(p);
            }
        }
        for c in exchange.all_conditional_orders() {
            if let Some(market) = markets.get(c.instrument_id.as_str()) {
                entries.entry((*market, c.account_id)).or_default().1.push(c);
            }
        }
    }
    let read = |s: &Screened| -> Result<Option<PrivateInputs>, String> {
        let mut inputs = PrivateInputs {
            markets: vec![],
            conditions: vec![],
        };
        for (n, market) in s.markets.iter().enumerate() {
            let found = entries.get(&(market.as_str(), s.account));
            // A related market listed twice, or listed as the plugin's own,
            // is observed once but has its conditional orders compared again.
            if !s.markets[..n].contains(market) {
                let account = rooms
                    .account_snapshot_for(room, market, s.account)
                    .map_err(|e| format!("{e:?}"))?
                    .filter(|a| match a {
                        AccountSnapshot::Spot(a) => a.account_id == s.account,
                        AccountSnapshot::Perp(a) => a.account_id == s.account,
                    });
                inputs.markets.push(MarketInputs {
                    orders: rooms
                        .resting_orders_for_account(room, market, s.account)
                        .map_err(|e| format!("{e:?}"))?,
                    protections: found.map_or_else(Vec::new, |e| {
                        e.0.iter().map(|p| (*p).clone()).collect()
                    }),
                    settled: settled_account(account),
                });
            }
            if conditions {
                for c in found.map_or(&[][..], |e| &e.1[..]) {
                    inputs.conditions.push((
                        c.key.clone(),
                        c.spec.clone(),
                        c.status.clone(),
                        c.submitted_order_id,
                    ));
                }
            }
        }
        let last = screens[s.bot].key.as_ref().map(|key| &key.private);
        Ok((last != Some(&inputs)).then_some(inputs))
    };
    // Small populations are not worth a thread.
    let chunk = screened.len().div_ceil(threads.max(1)).max(64);
    if screened.len() <= chunk {
        return Ok(screened.iter().map(&read).collect::<Result<_, _>>()?);
    }
    let results = std::thread::scope(|scope| {
        let handles = screened
            .chunks(chunk)
            .map(|part| scope.spawn(|| part.iter().map(&read).collect::<Result<Vec<_>, _>>()))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|h| h.join().expect("screen thread panicked"))
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(results.into_iter().flatten().collect())
}

/// True when the last trade on a reported market reached a requested level.
fn price_reached(
    markets: &[std::sync::Arc<ParticipantObservation>],
    wakes: &[PriceWake],
) -> Result<bool, Box<dyn std::error::Error>> {
    for wake in wakes {
        let market = markets
            .iter()
            .find(|v| v.instrument_id == wake.instrument_id)
            .ok_or("price wake on a market the plugin does not observe")?;
        if let Some(last) = market.public_trades.last().map(|t| t.price_tick)
            && (wake.at_or_above.is_some_and(|p| last >= p)
                || wake.at_or_below.is_some_and(|p| last <= p))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Stands in for a state that validation does not read (no tracked orders).
const UNREAD_STATE: PersistedAgentKindState = PersistedAgentKindState::Noise { rng_state: 0 };

/// Opt-in (RESEARCH_PROFILE=1) wall time per phase and the reason each plugin
/// call was due, printed every 600 steps. It never changes a decision.
#[derive(Default)]
struct Profile {
    phases: BTreeMap<&'static str, std::time::Duration>,
    // Per reason: calls, calls returning actions, actions.
    wakes: BTreeMap<&'static str, (u64, u64, u64)>,
    // Action kinds returned by calls that only the poll interval made due.
    poll_actions: BTreeMap<String, u64>,
}

impl Profile {
    fn add(&mut self, phase: &'static str, started: std::time::Instant) {
        *self.phases.entry(phase).or_default() += started.elapsed();
    }
    fn wake(&mut self, reason: &'static str, actions: &[OrderAction]) {
        let entry = self.wakes.entry(reason).or_default();
        entry.0 += 1;
        entry.1 += u64::from(!actions.is_empty());
        entry.2 += actions.len() as u64;
        if reason == "poll" {
            for action in actions {
                *self.poll_actions.entry(action_kind(action)).or_default() += 1;
            }
        }
    }
    fn report(&self, step: u64) {
        let phases = self
            .phases
            .iter()
            .map(|(k, v)| format!("{k}={:.1}s", v.as_secs_f64()))
            .collect::<Vec<_>>()
            .join(" ");
        let wakes = self
            .wakes
            .iter()
            .map(|(k, (calls, acting, actions))| format!("{k}={calls}/{acting}/{actions}"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "profile step={step} {phases} wakes(calls/acting/actions): {wakes} poll_actions={:?}",
            self.poll_actions
        );
    }
}

/// Variant name of an action, with the inner action for tracked bot orders.
fn action_kind(action: &OrderAction) -> String {
    let value = serde_json::to_value(action).unwrap_or_default();
    let name = |v: &serde_json::Value| match v {
        serde_json::Value::Object(map) => map.keys().next().cloned().unwrap_or_default(),
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    };
    match value.get("BotOrder").and_then(|o| o.get("action")) {
        Some(inner) => format!("BotOrder.{}", name(inner)),
        None => name(&value),
    }
}

/// Checks one bot's actions, then records its requested wake and action count.
#[allow(clippy::too_many_arguments)]
fn accept_actions(
    bot: &dyn ScheduledBot,
    t: &AgentTemplate,
    instrument: &str,
    related: &[String],
    actions: &[OrderAction],
    polled: bool,
    screen: &mut Screen,
    actions_per_bot: &mut BTreeMap<String, u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    if actions.len() > 64 {
        return Err("strategy exceeded action limit".into());
    }
    // Validation reads the state only for tracked bot orders; serializing
    // every bot's state each step just to skip it dominated long runs.
    let tracked = actions
        .iter()
        .any(|a| matches!(a, OrderAction::BotOrder { .. }));
    let snapshot = (tracked || polled).then(|| bot.snapshot());
    bots::validate_bot_orders(
        instrument,
        related,
        actions,
        snapshot.as_ref().unwrap_or(&UNREAD_STATE),
    )?;
    if let (true, Some(PersistedAgentKindState::Plugin { data, .. })) = (polled, &snapshot) {
        screen.wake_ms = data.get("_next_wake_ms").and_then(serde_json::Value::as_u64);
        screen.prices = match data.get("_wake_prices") {
            Some(value) => serde_json::from_value(value.clone())?,
            None => vec![],
        };
    }
    *actions_per_bot
        .entry(t.participant_id().into())
        .or_default() += actions.len() as u64;
    Ok(())
}

/// Worker calls of the due event-driven plugins. Each bot decides only from
/// its own observation and state, so the call order cannot change a result;
/// results come back in bot order, with the summed duration of the calls.
#[allow(clippy::type_complexity)]
fn decide_concurrently(
    bots: &mut [Box<dyn ScheduledBot>],
    due: Vec<(usize, ParticipantObservation)>,
    threads: usize,
) -> (
    Vec<(usize, Result<Vec<OrderAction>, BotError>)>,
    std::time::Duration,
) {
    let mut observations = due.into_iter().peekable();
    let mut work = vec![];
    for (i, bot) in bots.iter_mut().enumerate() {
        if observations.peek().is_some_and(|(j, _)| *j == i) {
            let (_, observation) = observations.next().unwrap();
            work.push((i, bot, observation));
        }
    }
    let workers = threads.min(work.len());
    let queue = std::sync::Mutex::new(work.into_iter());
    let mut results = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = vec![];
                    let mut busy = std::time::Duration::ZERO;
                    loop {
                        let next = queue.lock().unwrap().next();
                        let Some((i, bot, observation)) = next else {
                            break (done, busy);
                        };
                        let started = std::time::Instant::now();
                        done.push((i, bot.decide(&observation)));
                        busy += started.elapsed();
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut busy = std::time::Duration::ZERO;
        let mut results = vec![];
        for handle in handles {
            let (done, time) = handle.join().expect("plugin decision thread panicked");
            results.extend(done);
            busy += time;
        }
        (results, busy)
    });
    results.0.sort_by_key(|(i, _)| *i);
    results
}

#[allow(clippy::too_many_arguments)]
fn run_waves(
    registry: &BotRegistry,
    templates: &[AgentTemplate],
    room: &str,
    rooms: &mut RoomManager,
    bots: &mut [Box<dyn ScheduledBot>],
    order_id: &mut u64,
    steps: u64,
    seconds: u64,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let requests = templates
        .iter()
        .map(|t| registry.market_data_request(t))
        .collect::<Result<Vec<_>, _>>()?;
    let related = templates
        .iter()
        .map(|t| registry.related_instruments(t))
        .collect::<Result<Vec<_>, _>>()?;
    let intervals = templates
        .iter()
        .map(|t| registry.additional_intervals(t))
        .collect::<Result<Vec<_>, _>>()?;
    let before = rooms
        .net_worth_snapshot(room)
        .map_err(|e| format!("{e:?}"))?;
    let owners: BTreeMap<_, _> = templates
        .iter()
        .map(|t| {
            (
                (
                    t.config().account_id,
                    t.config().instrument_id.clone().unwrap(),
                ),
                t.participant_id().to_owned(),
            )
        })
        .collect();
    let mut fills = BTreeMap::<String, u64>::new();
    let mut rejections = BTreeMap::<String, u64>::new();
    let mut trades = BTreeMap::<String, u64>::new();
    let mut receipts = vec![];
    let instruments = templates
        .iter()
        .map(|t| t.config().instrument_id.clone().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    let mut book_snapshots = vec![];
    let mut actions_per_bot = BTreeMap::<String, u64>::new();
    // Every bot is listed, also one that never acts.
    for t in templates {
        actions_per_bot.insert(t.participant_id().into(), 0);
    }
    let mut history_cursor = rooms
        .execution_history_len(room)
        .map_err(|e| format!("{e:?}"))?;
    // Opt-in for populations with many process plugins. An event-driven
    // plugin still wakes at once on a closed bar, a change in its own orders,
    // protections, position or cash, or a wake it requested; changes that come
    // only from the mark price reach it once per poll interval instead of every
    // step. Unset keeps the live trigger, where every such change wakes it.
    let poll_ms = std::env::var("RESEARCH_PLUGIN_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0);
    // RESEARCH_PLUGIN_WAKES=events replaces polling: an event-driven plugin
    // wakes only for what its decisions read between bars. That is a closed
    // bar on any requested interval; a change in its own orders, protections,
    // conditional orders or settled account, here or on a related market; a
    // receipt for an order it placed; a wake time it requested; or the last
    // trade reaching a level it reported in `_wake_prices`. Mark-price-only
    // changes never wake it, so a plugin must report every price it compares.
    let event_wakes = match std::env::var("RESEARCH_PLUGIN_WAKES").ok().as_deref() {
        None => false,
        Some("events") if poll_ms.is_none() => true,
        Some("events") => {
            return Err("RESEARCH_PLUGIN_WAKES=events replaces RESEARCH_PLUGIN_POLL_MS".into());
        }
        Some(other) => return Err(format!("unknown RESEARCH_PLUGIN_WAKES={other}").into()),
    };
    let polled = templates
        .iter()
        .map(|t| (poll_ms.is_some() || event_wakes) && registry.event_filter(t).is_some())
        .collect::<Vec<_>>();
    let mut screens = vec![Screen::default(); templates.len()];
    // Polling compares only the plugin's own market; event wakes compare its
    // related markets and conditional orders as well.
    let screened = (0..templates.len())
        .filter(|&i| polled[i])
        .map(|i| {
            let cfg = templates[i].config();
            let own = cfg.instrument_id.clone().ok_or("instrument required")?;
            let related = if event_wakes { &related[i][..] } else { &[] };
            Ok(Screened {
                bot: i,
                account: cfg.account_id,
                markets: std::iter::once(own).chain(related.iter().cloned()).collect(),
            })
        })
        .collect::<Result<Vec<_>, &str>>()?;
    let mut profile = std::env::var("RESEARCH_PROFILE")
        .is_ok_and(|v| v == "1")
        .then(Profile::default);
    // The reason the current call of each plugin is due (profiling only).
    let mut reasons = vec!["unpolled"; templates.len()];
    // Each wave extends the previous wave's receipts, candles and account
    // indexes by the new execution suffix instead of replaying the whole log.
    // History that cannot be proven append-only is rebuilt cold, as before.
    let mut previous_wave: Option<exchange_core::room::NativeObservationWave> = None;
    // Worker calls of due event-driven plugins (process workers) run on
    // several threads, by default one per worker of the largest pool;
    // everything else stays on this thread in bot order.
    // RESEARCH_PLUGIN_THREADS=1 makes the calls one at a time.
    let concurrent = templates
        .iter()
        .map(|t| registry.event_filter(t).is_some())
        .collect::<Vec<_>>();
    let plugin_threads = std::env::var("RESEARCH_PLUGIN_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .or_else(|| {
            templates
                .iter()
                .zip(&concurrent)
                .filter(|(_, concurrent)| **concurrent)
                .filter_map(|(t, _)| registry.parallel_calls(t))
                .max()
        })
        .unwrap_or(8)
        .max(1);
    // Bots that never read their fill history get observations without it.
    let own_fills = templates
        .iter()
        .map(|t| registry.reads_own_fills(t))
        .collect::<Vec<_>>();
    for step in 0..steps {
        let started = std::time::Instant::now();
        rooms
            .advance_clock(room, seconds)
            .map_err(|e| format!("{e:?}"))?;
        if let Some(p) = &mut profile {
            p.add("clock", started);
        }
        let mut decisions = vec![];
        let mut due = vec![];
        {
            let started = std::time::Instant::now();
            let snapshot = rooms.clone();
            let wave = match &previous_wave {
                Some(previous) => {
                    snapshot.into_native_observation_wave_after(room.to_string(), previous)
                }
                None => snapshot.into_native_observation_wave(room.to_string()),
            };
            let mut batch = wave.batch();
            if let Some(p) = &mut profile {
                p.add("wave", started);
            }
            // The private inputs of each screened plugin that changed since
            // its last wake key, read before any plugin decides: the committed
            // state does not change during the wave.
            let started = std::time::Instant::now();
            let mut changed = vec![None; templates.len()];
            if !screened.is_empty() {
                let inputs = changed_private_inputs(
                    rooms,
                    room,
                    &screened,
                    &screens,
                    event_wakes,
                    plugin_threads,
                )?;
                for (s, inputs) in screened.iter().zip(inputs) {
                    changed[s.bot] = inputs;
                }
            }
            // Public views by market; the wave builds each for its first caller.
            let mut public = Vec::<(&str, std::sync::Arc<ParticipantObservation>)>::new();
            if let Some(p) = &mut profile {
                p.add("screen", started);
            }
            for (i, bot) in bots.iter_mut().enumerate() {
                let t = &templates[i];
                let cfg = t.config();
                let instrument = cfg.instrument_id.as_ref().ok_or("instrument required")?;
                let started = std::time::Instant::now();
                if polled[i] {
                    // Public views in the order its observation builds them.
                    let mut markets = vec![];
                    let related = related[i].iter().filter(|m| event_wakes && *m != instrument);
                    for market in std::iter::once(instrument).chain(related) {
                        let view = match public.iter().find(|(m, _)| *m == market) {
                            Some((_, view)) => view.clone(),
                            None => {
                                let view = batch
                                    .shared_public_observation(market, cfg.account_id)
                                    .map_err(|e| format!("{e:?}"))?;
                                public.push((market, view.clone()));
                                view
                            }
                        };
                        markets.push(view);
                    }
                    let time = markets[0].market_time_ms;
                    let extra = intervals[i].iter().filter(|_| event_wakes);
                    let bars = (
                        requests[i].map(|r| time / r.interval_ms),
                        extra.map(|ms| time / ms).collect::<Vec<_>>(),
                    );
                    let poll = poll_ms.map(|poll| time / poll);
                    let changed = changed[i].take();
                    let same = changed.is_none()
                        && screens[i]
                            .key
                            .as_ref()
                            .is_some_and(|key| key.bars == bars && key.poll == poll);
                    let woken = screens[i].wake_ms.is_some_and(|w| w <= time);
                    let priced = event_wakes && price_reached(&markets, &screens[i].prices)?;
                    let receipt = event_wakes && screens[i].receipt;
                    if same && !woken && !priced && !receipt {
                        decisions.push(vec![]);
                        if let Some(p) = &mut profile {
                            p.add("screen", started);
                        }
                        continue;
                    }
                    if profile.is_some() {
                        reasons[i] = match &screens[i].key {
                            None => "first",
                            Some(old) if old.bars != bars => "bar",
                            Some(_) if changed.is_some() => "account",
                            _ if woken => "wake",
                            _ if receipt => "receipt",
                            _ if priced => "price",
                            _ => "poll",
                        };
                    }
                    let private = match changed {
                        Some(private) => private,
                        None => screens[i].key.take().ok_or("plugin was not screened")?.private,
                    };
                    screens[i].key = Some(WakeKey {
                        bars,
                        poll,
                        private,
                    });
                    screens[i].receipt = false;
                }
                if let Some(p) = &mut profile {
                    p.add("screen", started);
                }
                let started = std::time::Instant::now();
                let obs = batch
                    .shared_bot_observation_with(
                        instrument,
                        cfg.account_id,
                        requests[i],
                        &related[i],
                        own_fills[i],
                    )
                    .map_err(|e| format!("{e:?}"))?;
                let actions = if registry.account_events(t) || !intervals[i].is_empty() {
                    let mut owned = obs.to_owned_observation();
                    batch
                        .enrich_data(
                            &mut owned,
                            requests[i],
                            &related[i],
                            &intervals[i],
                            registry.account_events(t),
                            cfg.account_id,
                        )
                        .map_err(|e| format!("{e:?}"))?;
                    if concurrent[i] {
                        due.push((i, owned));
                        decisions.push(vec![]);
                        if let Some(p) = &mut profile {
                            p.add("observe_plugin", started);
                        }
                        continue;
                    }
                    bot.decide(&owned).map_err(|e| {
                        format!(
                            "participant={} step={} error={e}",
                            t.participant_id(),
                            step + 1
                        )
                    })?
                } else {
                    bot.decide_shared(&obs).map_err(|e| {
                        format!(
                            "participant={} step={} error={e}",
                            t.participant_id(),
                            step + 1
                        )
                    })?
                };
                accept_actions(
                    bot.as_ref(),
                    t,
                    instrument,
                    &related[i],
                    &actions,
                    polled[i],
                    &mut screens[i],
                    &mut actions_per_bot,
                )?;
                if let Some(p) = &mut profile {
                    p.add("builtin", started);
                    if polled[i] {
                        p.wake(reasons[i], &actions);
                    }
                }
                decisions.push(actions);
            }
            let started = std::time::Instant::now();
            drop(batch);
            previous_wave = Some(wave);
            if let Some(p) = &mut profile {
                p.add("wave", started);
            }
        }
        let started = std::time::Instant::now();
        let (results, busy) = decide_concurrently(bots, due, plugin_threads);
        if let Some(p) = &mut profile {
            p.add("plugins_wall", started);
            *p.phases.entry("plugins_busy").or_default() += busy;
        }
        let started = std::time::Instant::now();
        for (i, result) in results {
            let t = &templates[i];
            let actions = result.map_err(|e| {
                format!(
                    "participant={} step={} error={e}",
                    t.participant_id(),
                    step + 1
                )
            })?;
            let instrument = t.config().instrument_id.as_ref().unwrap();
            accept_actions(
                bots[i].as_ref(),
                t,
                instrument,
                &related[i],
                &actions,
                polled[i],
                &mut screens[i],
                &mut actions_per_bot,
            )?;
            if let Some(p) = &mut profile {
                p.wake(reasons[i], &actions);
            }
            decisions[i] = actions;
        }
        if let Some(p) = &mut profile {
            p.add("accept", started);
        }
        let started = std::time::Instant::now();
        // All agents observe the same committed wave. Matching/risk still run
        // on the current authoritative state for each ordered submission.
        for (i, actions) in decisions.into_iter().enumerate() {
            let t = &templates[i];
            let cfg = t.config();
            let instrument = cfg.instrument_id.as_ref().unwrap();
            for action in actions {
                let (instrument, resolved) = bots::resolve_bot_order(instrument, &action)?;
                let mut gateway = OrderGateway::new_scheduler(rooms, *order_id);
                let execution = gateway
                    .submit_action(GatewayRequest {
                        participant_id: t.participant_id().into(),
                        room_id: room.to_string(),
                        instrument_id: Some(instrument.to_string()),
                        account_id: cfg.account_id,
                        action: resolved.clone(),
                    })
                    .map_err(|e| format!("{e:?}"))?;
                *order_id = gateway.next_order_id();
                bots[i].execution_feedback(&action, &execution);
                if matches!(action, OrderAction::BotOrder { .. }) {
                    // Taken, not shared, so recording does not copy the state.
                    let mut state = bots[i].take_snapshot();
                    bots::record_bot_execution(&mut state, &action, &execution);
                    bots[i].prepare_reused_call(&state)?;
                    screens[i].receipt = true;
                }
                let ActorExecutionResult::Accepted(result) = &execution.execution.result else {
                    *rejections
                        .entry(format!("{:?}", execution.execution.result))
                        .or_default() += 1;
                    continue;
                };
                let events = match result {
                    MarketExecution::Spot(r) => &r.events,
                    MarketExecution::Perp(r) => &r.events,
                };
                for event in events {
                    match &event.event {
                        Event::OrderRejected { .. }
                        | Event::RiskRejected { .. }
                        | Event::CancelRejected { .. } => {
                            *rejections.entry(format!("{:?}", event.event)).or_default() += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(p) = &mut profile {
            p.add("submit", started);
        }
        let started = std::time::Instant::now();
        // The authoritative suffix also includes clock-triggered protection,
        // funding/liquidation and cascaded executions, not just direct actions.
        for execution in rooms
            .execution_history_from(room, history_cursor)
            .map_err(|e| format!("{e:?}"))?
        {
            if let ActorExecutionResult::Accepted(result) = &execution.result {
                let events = match result {
                    MarketExecution::Spot(r) => &r.events,
                    MarketExecution::Perp(r) => &r.events,
                };
                for event in events {
                    if let Event::TradePrinted(t) = &event.event {
                        if t.maker_account_id == t.taker_account_id {
                            return Err("self trade".into());
                        }
                        let instrument = &execution.instrument_id;
                        *trades.entry(instrument.to_string()).or_default() += 1;
                        receipts.push(serde_json::json!({"instrument_id":instrument,"time_ms":execution.market_time_ms,"price_tick":t.price_tick,"qty":t.qty,"taker_side":t.taker_side,"trade_id":t.trade_id,"taker_order_id":t.taker_order_id,"maker_account_id":t.maker_account_id,"taker_account_id":t.taker_account_id}));
                        for account in [t.maker_account_id, t.taker_account_id] {
                            if let Some(owner) = owners.get(&(account, instrument.to_string())) {
                                *fills.entry(owner.clone()).or_default() += t.qty;
                            }
                        }
                    }
                }
            }
        }
        history_cursor = rooms
            .execution_history_len(room)
            .map_err(|e| format!("{e:?}"))?;
        // Read-only, after the complete action wave. These snapshots cannot
        // reveal intra-wave depletion or cancellations between samples.
        for instrument in &instruments {
            let book = rooms
                .book_snapshot_for(room, instrument)
                .map_err(|e| format!("{e:?}"))?;
            book_snapshots.push(serde_json::json!({
                "instrument_id": instrument,
                "time_ms": rooms.clock(room).map_err(|e|format!("{e:?}"))?.market_time_ms(),
                "phase": "after_all_actions", "book": book
            }));
        }
        if let Some(p) = &mut profile {
            p.add("receipts", started);
            if (step + 1) % 600 == 0 || step + 1 == steps {
                p.report(step + 1);
            }
        }
        if (step + 1) % 100 == 0 {
            eprintln!(
                "step={} trades={} active_bots={}",
                step + 1,
                trades.values().sum::<u64>(),
                fills.len()
            );
        }
    }
    Ok(
        serde_json::json!({"runner":"existing_native_observation_wave","bots":templates.len(),"steps":steps,"simulation_time_ms":rooms.clock(room).map_err(|e|format!("{e:?}"))?.market_time_ms(),
        "trades":trades,"fills":fills,"trade_receipts":receipts,"book_snapshots":book_snapshots,"rejections":rejections,"assets_before":before,"assets_after":rooms.net_worth_snapshot(room).map_err(|e|format!("{e:?}"))?,"actions_per_bot":actions_per_bot,"clock_step_seconds":seconds,
        "states":templates.iter().zip(bots.iter()).map(|(t,b)|serde_json::json!({"id":t.participant_id(),"state":b.snapshot()})).collect::<Vec<_>>(),
        "qualification":"real finite-capital synthetic matching via the existing native delivery API; not live-market or durable-server qualification"}),
    )
}
