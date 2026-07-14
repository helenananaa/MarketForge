use exchange_core::{
    AccountSnapshots, ActorExecutionResult, AgentRuntime, BookLevel, Command, DcaTrader,
    DcaTraderConfig, Event, GatewayExecution, GridTrader, GridTraderConfig, InstrumentConfig,
    MarketConfig, MarketExecution, NewOrder, OrderGateway, OrderKind, ParticipantConfig,
    ParticipantKind, RoomManager, ScenarioAccount, ScenarioConfig, Side, SpotClearingConfig,
    SpotMarketConfig, SpotRiskConfig, TradingApi,
};

fn main() {
    let mut rooms = RoomManager::new();
    rooms
        .create_room(smoke_scenario())
        .expect("smoke scenario should bootstrap");

    let mut gateway = OrderGateway::new(&mut rooms, 1);
    let mut runtime = AgentRuntime::new();

    runtime.add_participant(GridTrader::new(GridTraderConfig {
        participant: participant("grid-maker", 30),
        center_price_tick: 100,
        grid_spacing_ticks: 2,
        levels: 2,
        qty_per_level: 2,
    }));
    runtime.add_participant(DcaTrader::new(DcaTraderConfig {
        participant: participant("dca-buyer", 20),
        interval_steps: 1,
        order_qty: 1,
        use_market_order: false,
        limit_offset_ticks: 3,
        fallback_price_tick: 100,
        side: Side::Buy,
    }));

    println!("MarketForge smoke simulation");
    println!("room=demo-spot symbol=V-BTC-SPOT agents=grid-maker,dca-buyer");

    for _ in 0..5 {
        let step = runtime.run_step(&mut gateway);
        println!(
            "\n== step {} market_time={}ms ==",
            step.step, step.market_time_ms
        );

        for participant in step.participant_results {
            match participant.result {
                Ok(executions) if executions.is_empty() => {
                    println!("{}: no action", participant.participant_id);
                }
                Ok(executions) => {
                    for execution in executions {
                        print_execution(&execution);
                    }
                }
                Err(error) => {
                    println!("{}: gateway error: {error:?}", participant.participant_id);
                }
            }
        }

        let view = gateway
            .market_view("demo-spot")
            .expect("market view should be available");
        println!(
            "book: bids [{}] asks [{}]",
            format_levels(&view.book.bids),
            format_levels(&view.book.asks)
        );
        println!("accounts: {}", format_accounts(&view.accounts));
    }
}

fn smoke_scenario() -> ScenarioConfig {
    ScenarioConfig {
        room_id: "demo-spot".to_string(),
        venue_preset: None,
        venue_rules: exchange_core::VenueRuleConfig::default(),
        venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
        assets: Vec::new(),
        market: MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig {
                allow_short: true,
                ..SpotRiskConfig::default()
            },
        }),
        extra_markets: Vec::new(),
        initial_portfolios: Vec::new(),
        initial_allocations: Vec::new(),
        routed_initial_allocations: Vec::new(),
        accounts: vec![
            ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 10_000,
            },
            ScenarioAccount::Spot {
                account_id: 30,
                cash_balance: 10_000,
                position_qty: 100,
            },
        ],
        seed_orders: vec![Command::NewOrder(NewOrder {
            order_id: 10_000,
            account_id: 30,
            side: Side::Sell,
            kind: OrderKind::Limit { price_tick: 105 },
            qty: 3,
            reduce_only: false,
        })],
        routed_seed_orders: Vec::new(),
    }
}

fn participant(id: &str, account_id: u64) -> ParticipantConfig {
    ParticipantConfig {
        participant_id: id.to_string(),
        kind: ParticipantKind::RuleAgent,
        room_id: "demo-spot".to_string(),
        account_id,
    }
}

fn print_execution(execution: &GatewayExecution) {
    println!(
        "{} account={} action={:?}",
        execution.participant_id, execution.account_id, execution.action
    );

    match &execution.execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
            for record in &result.events {
                match &record.event {
                    Event::TradePrinted(trade) => {
                        println!(
                            "  trade id={} price={} qty={} taker={:?}",
                            trade.trade_id, trade.price_tick, trade.qty, trade.taker_side
                        );
                    }
                    Event::OrderRested {
                        order_id,
                        price_tick,
                        remaining_qty,
                    } => {
                        println!(
                            "  rested order={} price={} qty={}",
                            order_id, price_tick, remaining_qty
                        );
                    }
                    Event::RiskRejected { order_id, reason } => {
                        println!("  risk rejected order={} reason={reason:?}", order_id);
                    }
                    Event::OrderFilled { order_id } => {
                        println!("  filled order={order_id}");
                    }
                    _ => {}
                }
            }
        }
        ActorExecutionResult::Accepted(_) => {
            println!("  accepted non-spot execution");
        }
        ActorExecutionResult::Rejected(reason) => {
            println!("  rejected: {reason:?}");
        }
    }
}

fn format_levels(levels: &[BookLevel]) -> String {
    levels
        .iter()
        .map(|level| format!("{}@{}", level.qty, level.price_tick))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_accounts(accounts: &AccountSnapshots) -> String {
    match accounts {
        AccountSnapshots::Spot(accounts) => accounts
            .iter()
            .map(|account| {
                format!(
                    "acct={} cash={} pos={}",
                    account.account_id, account.cash_balance, account.position_qty
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
        AccountSnapshots::Perp(accounts) => accounts
            .iter()
            .map(|account| {
                format!(
                    "acct={} cash={} pos={} equity={}",
                    account.account_id, account.cash_balance, account.position_qty, account.equity
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}
