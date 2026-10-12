use pine_runtime::{Bar, run_historical};
use pine_runtime::{
    ChartContext, RequestEnvironment, RequestTimeframe, run_historical_with_request_environment,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bars() -> [Bar; 3] {
    [
        Bar {
            time: 1,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 2,
            open: 10.0,
            high: 12.0,
            low: 8.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 3,
            open: 10.0,
            high: 12.0,
            low: 8.0,
            close: 10.0,
            volume: 1.0,
        },
    ]
}

#[test]
fn price_based_entry_slippage_depends_on_order_kind() {
    for (command, direction, price_argument, expected) in [
        ("entry", "strategy.long", "limit=9", 9.0),
        ("entry", "strategy.short", "limit=11", 11.0),
        ("entry", "strategy.long", "stop=11", 12.0),
        ("entry", "strategy.short", "stop=9", 8.0),
        ("order", "strategy.long", "limit=9", 9.0),
        ("order", "strategy.short", "limit=11", 11.0),
        ("order", "strategy.long", "stop=11", 12.0),
        ("order", "strategy.short", "stop=9", 8.0),
        ("entry", "strategy.long", "stop=11,limit=10", 10.0),
        ("entry", "strategy.short", "stop=9,limit=10", 10.0),
        ("order", "strategy.long", "stop=11,limit=10", 10.0),
        ("order", "strategy.short", "stop=9,limit=10", 10.0),
    ] {
        let source = SourceFile::new(
            "orders.pine",
            format!(
                "//@version=6\nstrategy(\"orders\",slippage=100)\nif bar_index==0\n    strategy.{command}(\"E\",{direction},qty=1,{price_argument})\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(&analysis.hir.unwrap(), &bars()).unwrap();
        let strategy = result.strategy.unwrap();
        let order = strategy
            .orders
            .first()
            .unwrap_or_else(|| panic!("{command} {direction} {price_argument} did not fill"));
        assert_eq!(
            order.price, expected,
            "{command} {direction} {price_argument}"
        );
    }
}

#[test]
fn limit_reversal_uses_limit_price_for_both_sides() {
    for commission in [
        "",
        ",commission_type=strategy.commission.cash_per_order,commission_value=1",
    ] {
        let source = SourceFile::new(
            "reversal.pine",
            format!(
                "//@version=6\nstrategy(\"reversal\",slippage=100{commission})\nif bar_index==0\n    strategy.entry(\"L\",strategy.long,qty=1)\nif bar_index==1\n    strategy.entry(\"S\",strategy.short,qty=1,limit=12)\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(&analysis.hir.unwrap(), &bars()).unwrap();
        let strategy = result.strategy.unwrap();
        assert_eq!(strategy.trades[0].exit_price, 12.0, "{commission}");
        assert_eq!(strategy.position.last().unwrap().size, -1.0, "{commission}");
    }
}

#[test]
fn limit_at_close_fills_on_signal_bar_and_close_all_clears_all_legs() {
    let source = SourceFile::new(
        "close.pine",
        "//@version=6\nstrategy(\"close\", process_orders_on_close=true, pyramiding=10, commission_type=strategy.commission.percent, commission_value=0.07)\nif bar_index==0\n    strategy.entry(\"base\", strategy.long, qty=1.9559848638067299, limit=10)\n    strategy.entry(\"s1\", strategy.long, qty=0.0019707656058506095, limit=10)\n    strategy.entry(\"s2\", strategy.long, qty=0.004001554529645908, limit=10)\n    strategy.entry(\"s3\", strategy.long, qty=0.006140492884367122, limit=10)\n    strategy.entry(\"s4\", strategy.long, qty=0.008440540047240031, limit=10)\nif bar_index==1\n    strategy.close_all()\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(&analysis.hir.unwrap(), &bars())
        .unwrap()
        .strategy
        .unwrap();
    assert_eq!(strategy.trades.len(), 5);
    assert!(
        strategy
            .trades
            .iter()
            .all(|trade| trade.entry_bar_index == 0)
    );
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn account_currency_requires_matching_host_chart_currency() {
    let source = SourceFile::new(
        "currency.pine",
        "//@version=6\nstrategy(\"usdt\", currency=currency.USDT)\nplot(syminfo.currency == strategy.account_currency ? 1 : 0)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.unwrap();
    let error = run_historical(&program, &bars()).unwrap_err();
    assert!(
        error
            .message
            .contains("USDT differs from chart currency USD")
    );
    let chart = ChartContext::new("BINANCE:BTCUSDT", RequestTimeframe::default())
        .with_currency("USDT")
        .unwrap();
    let environment = RequestEnvironment::default().for_chart(chart);
    let result = run_historical_with_request_environment(&program, &bars(), environment).unwrap();
    assert_eq!(result.plots[0].values.len(), 3);
}
