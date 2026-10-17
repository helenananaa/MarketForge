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

/// Checks one bot's actions, then records its requested wake and action count.
#[allow(clippy::too_many_arguments)]
fn accept_actions(
    bot: &dyn ScheduledBot,
    t: &AgentTemplate,
    instrument: &str,
    related: &[String],
    actions: &[OrderAction],
    polled: bool,
    screen: &mut (Vec<u8>, Option<u64>),
    actions_per_bot: &mut BTreeMap<String, u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    if actions.len() > 64 {
        return Err("strategy exceeded action limit".into());
    }
    let snapshot = bot.snapshot();
    bots::validate_bot_orders(instrument, related, actions, &snapshot)?;
    if polled {
        screen.1 = match &snapshot {
            PersistedAgentKindState::Plugin { data, .. } => {
                data.get("_next_wake_ms").and_then(serde_json::Value::as_u64)
            }
            _ => None,
        };
    }
    *actions_per_bot
        .entry(t.participant_id().into())
        .or_default() += actions.len() as u64;
    Ok(())
}

/// Worker calls of the due event-driven plugins. Each bot decides only from
/// its own observation and state, so the call order cannot change a result;
/// results come back in bot order.
fn decide_concurrently(
    bots: &mut [Box<dyn ScheduledBot>],
    due: Vec<(usize, ParticipantObservation)>,
    threads: usize,
) -> Vec<(usize, Result<Vec<OrderAction>, BotError>)> {
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
                    loop {
                        let next = queue.lock().unwrap().next();
                        let Some((i, bot, observation)) = next else {
                            break done;
                        };
                        done.push((i, bot.decide(&observation)));
                    }
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("plugin decision thread panicked"))
            .collect::<Vec<_>>()
    });
    results.sort_by_key(|(i, _)| *i);
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
    let polled = templates
        .iter()
        .map(|t| poll_ms.is_some() && registry.event_filter(t).is_some())
        .collect::<Vec<_>>();
    // Per polled plugin: last wake key and its own requested wake time.
    let mut screens = vec![(Vec::<u8>::new(), None::<u64>); templates.len()];
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
    for step in 0..steps {
        rooms
            .advance_clock(room, seconds)
            .map_err(|e| format!("{e:?}"))?;
        let mut decisions = vec![];
        let mut due = vec![];
        {
            let snapshot = rooms.clone();
            let wave = match &previous_wave {
                Some(previous) => {
                    snapshot.into_native_observation_wave_after(room.to_string(), previous)
                }
                None => snapshot.into_native_observation_wave(room.to_string()),
            };
            let mut batch = wave.batch();
            for (i, bot) in bots.iter_mut().enumerate() {
                let t = &templates[i];
                let cfg = t.config();
                let instrument = cfg.instrument_id.as_ref().ok_or("instrument required")?;
                if let (true, Some(poll)) = (polled[i], poll_ms) {
                    let view = batch
                        .shared_bot_observation(instrument, cfg.account_id, None, &[])
                        .map_err(|e| format!("{e:?}"))?;
                    let time = view.public.market_time_ms;
                    let key = serde_json::to_vec(&(
                        requests[i].map(|r| time / r.interval_ms),
                        time / poll,
                        &view.own_orders,
                        &view.position_protections,
                        settled_account(&view.own_account),
                    ))?;
                    if screens[i].0 == key && !screens[i].1.is_some_and(|w| w <= time) {
                        actions_per_bot
                            .entry(t.participant_id().into())
                            .or_default();
                        decisions.push(vec![]);
                        continue;
                    }
                    screens[i].0 = key;
                }
                let obs = batch
                    .shared_bot_observation(instrument, cfg.account_id, requests[i], &related[i])
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
                decisions.push(actions);
            }
            drop(batch);
            previous_wave = Some(wave);
        }
        for (i, result) in decide_concurrently(bots, due, plugin_threads) {
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
            decisions[i] = actions;
        }
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
