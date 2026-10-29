#[test]
fn archived_research_history_preserves_observations_candles_and_future_sequences() {
    let mut live = RoomManager::new();
    let mut scenario = spot_scenario("archive");
    scenario.accounts = vec![
        ScenarioAccount::Spot {
            account_id: 10,
            cash_balance: 1_000_000,
            position_qty: 10_000,
        },
        ScenarioAccount::Basic {
            account_id: 20,
            cash_balance: 1_000_000,
        },
    ];
    live.create_room(scenario).unwrap();
    let mut reference = live.clone();
    for n in 0..80u64 {
        for manager in [&mut live, &mut reference] {
            manager.advance_clock("archive", 1).unwrap();
        }
        let command = if n % 2 == 0 {
            limit(1000 + n, 10, Side::Sell, 100, 1)
        } else {
            limit(1000 + n, 20, Side::Buy, 100, 1)
        };
        let actual = live.apply("archive", command.clone()).unwrap();
        let expected = reference.apply("archive", command).unwrap();
        assert_eq!(actual, expected);
        if n % 7 == 0 {
            live.compact_archived_research_history("archive").unwrap();
            assert_eq!(live.execution_history_len("archive").unwrap(), 0);
        }
        let request = Some(crate::bots::BotMarketDataRequest {
            interval_ms: 1000,
            max_bars: 8,
        });
        for account in [10, 20] {
            let actual = live
                .bot_observation("archive", "V-BTC-SPOT", account, request)
                .unwrap();
            let expected = reference
                .bot_observation("archive", "V-BTC-SPOT", account, request)
                .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(
                live.bot_account_events("archive", "V-BTC-SPOT", account)
                    .unwrap(),
                reference
                    .bot_account_events("archive", "V-BTC-SPOT", account)
                    .unwrap()
            );
        }
        assert_eq!(
            live.ticker("archive", "V-BTC-SPOT").unwrap(),
            reference.ticker("archive", "V-BTC-SPOT").unwrap()
        );
        assert_eq!(
            live.candles("archive", "V-BTC-SPOT", 1000).unwrap(),
            reference.candles("archive", "V-BTC-SPOT", 1000).unwrap()
        );
        assert_eq!(
            live.net_worth_snapshot("archive").unwrap(),
            reference.net_worth_snapshot("archive").unwrap()
        );
    }
}

#[test]
fn archived_research_history_preserves_last_price_conditional_and_stop_exits() {
    let mut scenario = pending_perp_liquidation_scenario("archive-perp");
    scenario.accounts = (10..=30)
        .step_by(10)
        .map(|account_id| ScenarioAccount::Basic {
            account_id,
            cash_balance: 1_000_000,
        })
        .collect();
    let mut live = RoomManager::new();
    live.create_room(scenario).unwrap();
    let mut reference = live.clone();
    let commands = vec![
        limit(100, 10, Side::Sell, 100, 1),
        limit(101, 20, Side::Buy, 100, 1),
        Command::SetPositionProtection {
            account_id: 20,
            position_side: crate::PositionSide::Both,
            protection: Some(Box::new(crate::PositionProtectionSpec {
                stop_loss_tick: Some(99),
                trigger: crate::ProtectionTrigger::Last,
                ..Default::default()
            })),
        },
        Command::SetConditionalOrder {
            account_id: 30,
            key: "entry".into(),
            spec: Some(Box::new(crate::conditional_orders::ConditionalOrderSpec {
                side: Side::Buy,
                position_side: crate::PositionSide::Both,
                qty: 1,
                trigger_price_tick: 105,
                above: true,
                trigger: crate::ProtectionTrigger::Last,
                limit_price_tick: Some(106),
                protection: None,
            })),
        },
        limit(102, 10, Side::Sell, 106, 2),
        limit(103, 20, Side::Buy, 106, 1),
        limit(104, 30, Side::Buy, 98, 3),
        limit(105, 10, Side::Sell, 98, 1),
    ];
    for command in commands {
        live.compact_archived_research_history("archive-perp")
            .unwrap();
        assert_eq!(
            live.apply("archive-perp", command.clone()).unwrap(),
            reference.apply("archive-perp", command).unwrap()
        );
        for account in [10, 20, 30] {
            let request = Some(crate::bots::BotMarketDataRequest {
                interval_ms: 1000,
                max_bars: 8,
            });
            assert_eq!(
                live.bot_observation("archive-perp", "V-BTC-PERP", account, request)
                    .unwrap(),
                reference
                    .bot_observation("archive-perp", "V-BTC-PERP", account, request)
                    .unwrap()
            );
            assert_eq!(
                live.simulation_room("archive-perp")
                    .unwrap()
                    .primary_exchange()
                    .position_protections("V-BTC-PERP", account),
                reference
                    .simulation_room("archive-perp")
                    .unwrap()
                    .primary_exchange()
                    .position_protections("V-BTC-PERP", account)
            );
        }
    }
    let account = live.account_snapshot("archive-perp", 20).unwrap().unwrap();
    assert!(matches!(account,crate::AccountSnapshot::Perp(a) if a.position_qty==0));
    assert!(live.simulation_room("archive-perp").unwrap().primary_exchange()
        .conditional_orders("V-BTC-PERP",30).iter().any(|order|order.submitted_order_id.is_some()));
}
