//! Controlled quote-feedback diagnostic, not a matching/market-realism test.
//! Reuses the existing lone-maker regression fixture's inventory/skew settings.
//! After a common warmup, no trades or inventory changes occur. Only posted
//! quotes are fed back; post-only crossing attempts are rejected locally.
use exchange_core::*;
use serde_json::json;
use std::collections::BTreeMap;

fn template(id: u64, innovations: bool) -> AgentTemplate {
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: format!("probe-{id}"),
            kind: ParticipantKind::MarketMaker,
            room_id: "probe".into(),
            account_id: id,
            instrument_id: Some("TEST".into()),
        },
        plugin_id: "AdaptiveLiquidityMaker".into(),
        plugin_version: "1.1.0".into(),
        state_version: 1,
        config_version: 1,
        seed: 7,
        config: json!({"inventory_target":0,"inventory_cap":100,"skew_ppm":1000000,
            "max_half_spread_ticks":5,"flow_half_life_ms":10000,"jitter_ms":0,
            "refresh_ms":1000,"min_samples":4,"window":16,"external_reference":true,
            "reservation_innovations":innovations}),
    })
}

fn book(orders: &[Order]) -> BookSnapshot {
    let mut bids = BTreeMap::new();
    let mut asks = BTreeMap::new();
    for o in orders {
        let side = if o.side == Side::Buy {
            &mut bids
        } else {
            &mut asks
        };
        *side.entry(o.price_tick).or_insert(0) += o.remaining_qty;
    }
    BookSnapshot {
        bids: bids
            .into_iter()
            .rev()
            .map(|(price_tick, qty)| BookLevel { price_tick, qty })
            .collect(),
        asks: asks
            .into_iter()
            .map(|(price_tick, qty)| BookLevel { price_tick, qty })
            .collect(),
    }
}

fn view(id: u64, second: u64, orders: &[Order], position: i128) -> ParticipantObservation {
    let own: Vec<_> = orders
        .iter()
        .filter(|o| o.account_id == id)
        .cloned()
        .collect();
    ParticipantObservation {
        version: 1,
        room_id: "probe".into(),
        venue_id: "test".into(),
        instrument_id: "TEST".into(),
        instrument: None,
        status: MarketStatus::Running,
        step: second,
        market_time_ms: second * 1000,
        book: book(orders),
        public_trades: vec![],
        own_orders: own.clone(),
        related_markets: vec![],
        market_events: vec![],
        own_account: Some(AccountSnapshot::Spot(
            SpotAccount {
                account_id: id,
                cash_balance: 1_000_000_000,
                position_qty: position,
                reserved_cash: own
                    .iter()
                    .filter(|o| o.side == Side::Buy)
                    .map(|o| o.price_tick as i128 * o.remaining_qty as i128)
                    .sum(),
                reserved_position: own
                    .iter()
                    .filter(|o| o.side == Side::Sell)
                    .map(|o| o.remaining_qty as i128)
                    .sum(),
                fees_paid: 0,
            }
            .snapshot(),
        )),
        bot_market_data: None,
        position_protections: vec![],
        risk: None,
        perp_price: None,
    }
}

fn center(bot: &dyn ScheduledBot) -> f64 {
    match bot.snapshot() {
        PersistedAgentKindState::Plugin { data, .. } => data["center"].as_f64().unwrap(),
        _ => unreachable!(),
    }
}

fn run(count: usize, innovations: bool) -> serde_json::Value {
    run_with_templates(count, innovations, None)
}

fn run_with_templates(
    count: usize,
    innovations: bool,
    source: Option<&[AgentTemplate]>,
) -> serde_json::Value {
    let registry = BotRegistry::with_builtins();
    let templates: Vec<_> = (0..count)
        .map(|i| {
            let mut t =
                source.map_or_else(|| template(20 + i as u64, innovations), |s| s[i].clone());
            if let AgentTemplate::Plugin(p) = &mut t {
                p.participant.account_id = 20 + i as u64;
                p.participant.room_id = "probe".into();
                p.participant.instrument_id = Some("TEST".into());
                p.config["reservation_innovations"] = innovations.into();
            }
            t
        })
        .collect();
    let positions: Vec<i128> = templates
        .iter()
        .map(|t| {
            if source.is_some() {
                if let AgentTemplate::Plugin(p) = t {
                    p.config["inventory_target"].as_i64().unwrap() as i128
                        + p.config["inventory_cap"].as_i64().unwrap() as i128 / 4
                } else {
                    unreachable!()
                }
            } else {
                50
            }
        })
        .collect();
    let anchor = if source.is_some() { 100000 } else { 10000 };
    let mut bots: Vec<_> = (0..count)
        .map(|i| {
            let t = &templates[i];
            registry.create(&t, &t.initial_state()).unwrap()
        })
        .collect();
    // Exactly the existing regression test's alternating external warmup.
    for second in 0..20 {
        for (i, b) in bots.iter_mut().enumerate() {
            let mut v = view(20 + i as u64, second, &[], positions[i]);
            let shift = if second % 2 == 0 { 0 } else { 40 };
            v.book = BookSnapshot {
                bids: vec![BookLevel {
                    price_tick: anchor - 10 + shift,
                    qty: 100,
                }],
                asks: vec![BookLevel {
                    price_tick: anchor + 10 + shift,
                    qty: 100,
                }],
            };
            b.decide(&v).unwrap();
        }
    }
    let initial: Vec<_> = bots.iter().map(|b| center(b.as_ref())).collect();
    let mut orders = Vec::<Order>::new();
    let mut next_id = 1;
    let mut rejected = 0;
    let mut samples = vec![];
    for second in 20..500 {
        let actions: Vec<_> = bots
            .iter_mut()
            .enumerate()
            .map(|(i, b)| {
                b.decide(&view(20 + i as u64, second, &orders, positions[i]))
                    .unwrap()
            })
            .collect();
        for (i, actions) in actions.into_iter().enumerate() {
            for action in actions {
                match action {
                    OrderAction::Cancel { order_id } => orders.retain(|o| o.order_id != order_id),
                    OrderAction::PlacePostOnly {
                        side,
                        price_tick,
                        qty,
                    } => {
                        let crossed = orders.iter().any(|o| {
                            o.side != side
                                && match side {
                                    Side::Buy => price_tick >= o.price_tick,
                                    Side::Sell => price_tick <= o.price_tick,
                                }
                        });
                        if crossed {
                            rejected += 1;
                            continue;
                        }
                        orders.push(Order {
                            order_id: next_id,
                            account_id: 20 + i as u64,
                            side,
                            position_side: Default::default(),
                            price_tick,
                            remaining_qty: qty,
                            seq: next_id,
                        });
                        next_id += 1;
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        if second % 10 == 0 {
            samples.push(json!({"second":second,"centers":bots.iter().map(|b|center(b.as_ref())).collect::<Vec<_>>(),"book":book(&orders)}));
        }
    }
    let last: Vec<_> = bots.iter().map(|b| center(b.as_ref())).collect();
    json!({"makers":count,"reservation_innovations":innovations,"initial_centers":initial,"final_centers":last,
        "center_changes":last.iter().zip(&initial).map(|(a,b)|a-b).collect::<Vec<_>>(),
        "trades":0,"inventories":positions,"source_recipe_configs":source.is_some(),"post_only_rejections":rejected,"samples":samples})
}

fn main() {
    let source=std::env::args().nth(1).map(|path| {
        let spec:population::BackgroundMarket=serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        spec.agents.into_iter().filter(|t|matches!(t,AgentTemplate::Plugin(p) if p.plugin_id=="AdaptiveLiquidityMaker" && p.participant.instrument_id.as_deref()==Some("V-BTC-SPOT"))).collect::<Vec<_>>()
    });
    let runs = if let Some(templates) = source {
        vec![
            run_with_templates(templates.len(), false, Some(&templates)),
            run_with_templates(templates.len(), true, Some(&templates)),
        ]
    } else {
        vec![
            run(1, false),
            run(2, false),
            run(8, false),
            run(1, true),
            run(2, true),
            run(8, true),
        ]
    };
    println!("{}",serde_json::to_string_pretty(&json!({
        "scope":"Synthetic observation diagnostic. Fixed positions, zero trades, no fitted parameters. Does not exercise exchange clearing or prove the cause of a particular market episode.",
            "runs":runs
    })).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_only_quote_feedback_does_not_compound_reservation_bias() {
        for count in [1, 2, 8] {
            let result = run(count, true);
            assert_eq!(result["trades"], 0);
            assert!(
                result["center_changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|v| v.as_f64().unwrap().abs() <= 5.),
                "{result}"
            );
        }
    }
}
