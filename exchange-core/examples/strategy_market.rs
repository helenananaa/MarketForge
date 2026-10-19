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
fn settled_account(account: &Option<AccountSnapshot>) -> serde_json::Value {
    let mut value = serde_json::to_value(account).unwrap_or_default();
    if let Some(perp) = value.get_mut("Perp").and_then(serde_json::Value::as_object_mut) {
        perp.retain(|k, _| {
            matches!(
                k.as_str(),
                "cash_balance"
                    | "position_qty"
                    | "avg_entry_price_tick"
                    | "realized_pnl"
                    | "fees_paid"
                    | "funding_pnl"
                    | "margin_status"
            )
        });
    }
    value
}

/// What decides whether a screened plugin is due: its last wake key, the wake
/// time and last-trade levels it requested, and a receipt it has not read.
#[derive(Clone, Default)]
struct Screen {
    key: Vec<u8>,
    wake_ms: Option<u64>,
    prices: Vec<PriceWake>,
    receipt: bool,
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

/// Private inputs that only the account's own activity changes: resting
/// orders, protections and the settled account, here and on related markets.
fn private_inputs(view: &observation::SharedParticipantObservation) -> serde_json::Value {
    serde_json::json!([
        view.public.instrument_id,
        view.own_orders,
        view.position_protections,
        settled_account(&view.own_account),
        view.related_markets.iter().map(private_inputs).collect::<Vec<_>>(),
    ])
}

/// True when the last trade on a reported market reached a requested level.
fn price_reached(
    view: &observation::SharedParticipantObservation,
    wakes: &[PriceWake],
) -> Result<bool, Box<dyn std::error::Error>> {
    for wake in wakes {
        let market = std::iter::once(view)
            .chain(&view.related_markets)
            .find(|v| v.public.instrument_id == wake.instrument_id)
            .ok_or("price wake on a market the plugin does not observe")?;
        if let Some(last) = market.public.public_trades.last().map(|t| t.price_tick)
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

/// Hash of one serialized wake-key component, to tell which component changed.
fn part_hash(value: &impl serde::Serialize) -> Result<u64, serde_json::Error> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(value)?.hash(&mut hasher);
    Ok(hasher.finish())
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
    let mut profile = std::env::var("RESEARCH_PROFILE")
        .is_ok_and(|v| v == "1")
        .then(Profile::default);
    // Per polled plugin: hashes of the last key's bar, poll, order and account
    // parts, and the reason the current call is due (profiling only).
    let mut parts = vec![None::<[u64; 4]>; templates.len()];
    let mut reasons = vec!["unpolled"; templates.len()];
    // Each wave extends the previous wave's receipts, candles and account
    // indexes by the new execution suffix instead of replaying the whole log.
    // History that cannot be proven append-only is rebuilt cold, as before.
    let mut previous_wave: Option<exchange_core::room::NativeObservationWave> = None;
    // Worker calls of due event-driven plugins (process workers) run on
    // several threads; everything else stays on this thread in bot order.
    // RESEARCH_PLUGIN_THREADS=1 makes the calls one at a time.
    let plugin_threads = std::env::var("RESEARCH_PLUGIN_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8)
        .max(1);
    let concurrent = templates
        .iter()
        .map(|t| registry.event_filter(t).is_some())
        .collect::<Vec<_>>();
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
            for (i, bot) in bots.iter_mut().enumerate() {
                let t = &templates[i];
                let cfg = t.config();
                let instrument = cfg.instrument_id.as_ref().ok_or("instrument required")?;
                let started = std::time::Instant::now();
                if polled[i] {
                    let view = batch
                        .shared_bot_observation(
                            instrument,
                            cfg.account_id,
                            None,
                            if event_wakes { &related[i] } else { &[] },
                        )
                        .map_err(|e| format!("{e:?}"))?;
                    let time = view.public.market_time_ms;
                    let bar = requests[i].map(|r| time / r.interval_ms);
                    let settled = settled_account(&view.own_account);
                    let (key, private) = match poll_ms {
                        Some(poll) => (
                            serde_json::to_vec(&(
                                bar,
                                time / poll,
                                &view.own_orders,
                                &view.position_protections,
                                &settled,
                            ))?,
                            serde_json::Value::Null,
                        ),
                        None => {
                            let simulation =
                                rooms.simulation_room(room).map_err(|e| format!("{e:?}"))?;
                            let mut conditions = vec![];
                            for market in std::iter::once(instrument).chain(&related[i]) {
                                let venue = simulation
                                    .venue_id_for_instrument(market)
                                    .ok_or("unknown related market")?;
                                // Without the exchange's own scan cursor, which
                                // moves every step while an order is armed.
                                for c in simulation
                                    .exchange(&venue)
                                    .map_err(|e| format!("{e:?}"))?
                                    .conditional_orders(market, cfg.account_id)
                                {
                                    conditions.push(serde_json::json!([
                                        c.key,
                                        c.spec,
                                        c.status,
                                        c.submitted_order_id
                                    ]));
                                }
                            }
                            let private = serde_json::json!([private_inputs(&view), conditions]);
                            let bars = (
                                bar,
                                intervals[i].iter().map(|ms| time / ms).collect::<Vec<_>>(),
                            );
                            (serde_json::to_vec(&(bars, &private))?, private)
                        }
                    };
                    let woken = screens[i].wake_ms.is_some_and(|w| w <= time);
                    let priced = event_wakes && price_reached(&view, &screens[i].prices)?;
                    let receipt = event_wakes && screens[i].receipt;
                    if screens[i].key == key && !woken && !priced && !receipt {
                        actions_per_bot
                            .entry(t.participant_id().into())
                            .or_default();
                        decisions.push(vec![]);
                        if let Some(p) = &mut profile {
                            p.add("screen", started);
                        }
                        continue;
                    }
                    screens[i].key = key;
                    screens[i].receipt = false;
                    if profile.is_some() {
                        let bars = (bar, intervals[i].iter().map(|ms| time / ms).collect::<Vec<_>>());
                        let now = [
                            part_hash(&bars)?,
                            poll_ms.map_or(0, |poll| time / poll),
                            part_hash(&(&view.own_orders, &view.position_protections, &private))?,
                            part_hash(&settled)?,
                        ];
                        reasons[i] = match parts[i] {
                            None => "first",
                            Some(old) if old[0] != now[0] => "bar",
                            Some(old) if old[2] != now[2] || old[3] != now[3] => "account",
                            _ if woken => "wake",
                            _ if receipt => "receipt",
                            _ if priced => "price",
                            _ => "poll",
                        };
                        parts[i] = Some(now);
                    }
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
                    let mut state = bots[i].snapshot();
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
