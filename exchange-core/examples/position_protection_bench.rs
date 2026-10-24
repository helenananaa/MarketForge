//! Isolated armed-protection scan benchmark. stdout is a deterministic receipt;
//! stderr reports only the timed scans. No clock advancement or order execution
//! occurs inside the timed loop. Use the ecosystem runner for end-to-end claims.
use exchange_core::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(
        args.next().ok_or("configuration JSON required")?,
    )?)?;
    let steps: u64 = args.next().ok_or("scan count required")?.parse()?;
    let seconds: u64 = args.next().ok_or("receipt clock field required")?.parse()?;
    let accounts = config["accounts"].as_u64().ok_or("accounts required")?;
    let depth = config["depth"].as_u64().ok_or("depth required")?;
    assert!((1..=1000).contains(&accounts) && (1..=900).contains(&depth));
    let market = MarketConfig::Perp(PerpMarketConfig {
        instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).map_err(|e| format!("{e:?}"))?,
        clearing: PerpClearingConfig::default(),
        risk: PerpRiskConfig {
            max_order_notional: config["notional_cap"].as_i64().map(i128::from),
            ..PerpRiskConfig::default()
        },
        initial_mark_price_tick: 1000,
        price_link: None,
        funding: None,
    });
    let config = ExchangeConfig::new_single(market).map_err(|e| format!("{e:?}"))?;
    let mut actor = ExchangeActor::new("protection-bench", config).map_err(|e| format!("{e:?}"))?;
    for id in 1..=accounts + 1 {
        actor.create_account(id, 1_000_000_000_000);
    }
    let mut order_id = 0;
    let mut order = |actor: &mut ExchangeActor, account_id, side, qty, price_tick| {
        order_id += 1;
        let result = actor.apply(Command::NewOrder(NewOrder {
            position_side: PositionSide::Both,
            order_id,
            account_id,
            side,
            qty,
            reduce_only: false,
            kind: OrderKind::Limit { price_tick },
        }));
        assert!(matches!(result.result, ActorExecutionResult::Accepted(_)));
    };
    order(&mut actor, accounts + 1, Side::Sell, accounts * depth, 1000);
    for id in 1..=accounts {
        order(&mut actor, id, Side::Buy, depth, 1000);
        let result = actor.apply(Command::SetPositionProtection {
            account_id: id,
            position_side: PositionSide::Both,
            protection: Some(Box::new(PositionProtectionSpec {
                take_profit_tick: Some(2000),
                stop_loss_tick: Some(1),
                trigger: ProtectionTrigger::Mark,
                ..PositionProtectionSpec::default()
            })),
        });
        assert!(matches!(result.result, ActorExecutionResult::Accepted(_)));
    }
    for level in 0..depth {
        order(&mut actor, accounts + 1, Side::Buy, 1, 999 - level as i64);
    }
    assert_eq!(actor.all_position_protections().count(), accounts as usize);
    assert!(actor.prepare_position_exits().is_empty());
    assert!(
        actor
            .all_position_protections()
            .all(|p| p.status == "armed")
    );
    let before = serde_json::to_value(&actor)?;
    let started = std::time::Instant::now();
    for _ in 0..steps {
        assert!(std::hint::black_box(actor.prepare_position_exits()).is_empty());
    }
    let elapsed = started.elapsed().as_secs_f64();
    let after = serde_json::to_value(&actor)?;
    assert_eq!(before, after);
    eprintln!("profile step={steps} protections={elapsed:.6}s");
    println!(
        "{}",
        serde_json::json!({
            "steps": steps, "clock_step_seconds": seconds, "trades": accounts,
            "qualification": "isolated armed scans; no clock or execution in timed loop",
            "accounts": accounts, "depth": depth, "actor": after
        })
    );
    Ok(())
}
