fn canonical_risk_room(manager: &RoomManager) -> serde_json::Value {
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "seen_order_ids" {
                        value.as_array_mut().unwrap().sort_by_key(|v| v.as_u64());
                    } else {
                        normalize(value);
                    }
                }
            }
            serde_json::Value::Array(values) => values.iter_mut().for_each(normalize),
            _ => {}
        }
    }
    let histories: BTreeMap<_, _> = manager
        .executions
        .iter()
        .map(|(id, events)| (id, events.iter().collect::<Vec<_>>()))
        .collect();
    let mut value = serde_json::json!({"rooms": manager.rooms, "executions": format!("{histories:?}"),
        "pending": format!("{:?}", manager.pending_liquidations), "last_trades": format!("{:?}", manager.recovered_last_trades),
        "bot_accounts": format!("{:?}", manager.restored_bot_accounts), "bot_history": format!("{:?}", manager.restored_bot_history)});
    normalize(&mut value);
    value
}

#[test]
fn adaptive_risk_room_liquidation_partial_fill_freeze_and_restore_match_scan() {
    for hedge in [false, true] {
        let room_id = "adaptive-liquidation";
        let mut scenario = pending_cross_margin_liquidation_scenario(room_id);
        scenario
            .accounts
            .extend((30..=158).map(|account_id| ScenarioAccount::Basic {
                account_id,
                cash_balance: 1_000_000,
            }));
        if hedge {
            for market in
                std::iter::once(&mut scenario.market).chain(scenario.extra_markets.iter_mut())
            {
                let MarketConfig::Perp(perp) = market else {
                    unreachable!()
                };
                perp.clearing.position_mode = crate::PositionMode::Hedge;
            }
        }
        let mut optimized = RoomManager::new();
        optimized.create_room(scenario).unwrap();
        let mut reference = optimized.clone();
        let mut saw_pending = false;
        let mut saw_partial = false;
        for n in 0..12 {
            let operation = |rooms: &mut RoomManager| {
                if n == 4 || n == 7 {
                    return format!("{:?}", rooms.advance_clock(room_id, 1));
                }
                if n == 5 {
                    return format!("{:?}", rooms.submit_deposit(room_id, None, 20, "USDT", 20));
                }
                let (instrument, mut command) = match n {
                    0 => ("V-BTC-PERP", limit(1, 10, Side::Sell, 100, 10)),
                    1 => ("V-BTC-PERP", limit(2, 20, Side::Buy, 100, 10)),
                    2 => ("V-ETH-PERP", limit(3, 20, Side::Buy, 90, 1)),
                    3 => (
                        "V-BTC-PERP",
                        Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
                    ),
                    6 => ("V-ETH-PERP", limit(4, 20, Side::Buy, 90, 1)),
                    8 => ("V-BTC-PERP", limit(5, 30, Side::Buy, 80, 3)),
                    9 => (
                        "V-BTC-PERP",
                        Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
                    ),
                    10 => ("V-BTC-PERP", limit(6, 30, Side::Buy, 80, 7)),
                    _ => (
                        "V-ETH-PERP",
                        Command::SetMarkPrice(SetMarkPrice { price_tick: 100 }),
                    ),
                };
                if hedge && let Command::NewOrder(order) = &mut command {
                    order.position_side = if order.side == Side::Buy {
                        crate::PositionSide::Long
                    } else {
                        crate::PositionSide::Short
                    };
                }
                format!(
                    "{:?}",
                    rooms.apply_to_instrument(room_id, instrument, command)
                )
            };
            let a = operation(&mut optimized);
            let b = crate::risk_scope::reference(|| operation(&mut reference));
            assert_eq!(a, b, "n={n} hedge={hedge}");
            assert_eq!(
                canonical_risk_room(&optimized),
                canonical_risk_room(&reference),
                "state n={n} hedge={hedge}"
            );
            saw_pending |= optimized.pending_liquidation_count(room_id).unwrap() > 0;
            if n == 8 {
                let Some(crate::AccountSnapshot::Perp(account)) = optimized
                    .account_snapshot_for(room_id, "V-BTC-PERP", 20)
                    .unwrap()
                else {
                    unreachable!()
                };
                saw_partial = if let Some(legs) = account.hedge_positions {
                    legs.long.qty == 7
                } else {
                    account.position_qty == 7
                };
            }
            if n == 4 || n == 8 {
                let restore = |old: &RoomManager| {
                    let room: SimulationRoom = serde_json::from_value(
                        serde_json::to_value(old.simulation_room(room_id).unwrap()).unwrap(),
                    )
                    .unwrap();
                    let history = old.execution_history(room_id).unwrap().to_vec();
                    let mut restored = RoomManager::new();
                    restored.restore_simulation_room(room, history).unwrap();
                    restored
                };
                optimized = restore(&optimized);
                reference = crate::risk_scope::reference(|| restore(&reference));
                assert_eq!(
                    canonical_risk_room(&optimized),
                    canonical_risk_room(&reference)
                );
            }
        }
        assert!(
            saw_pending && saw_partial,
            "must exercise pending and actual partial liquidation"
        );
        assert_eq!(optimized.pending_liquidation_count(room_id).unwrap(), 0);
    }
}
