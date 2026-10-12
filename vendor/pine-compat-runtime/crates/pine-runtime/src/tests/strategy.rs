use pine_ir::{
    HirCallArg, HirExpr, HirExprKind, HirLiteral, HirProgram, HirStmt, HirStmtKind, PineType,
    Qualifier, ScriptMode, ValueKind,
};
use pine_syntax::SourceFile;

use super::*;

#[test]
fn historical_closing_immediate_exit_does_not_recalculate_a_phantom_entry() {
    for direction in ["long", "short"] {
        let source = SourceFile::new(
            "close-fill-pass.pine",
            format!(
                r#"//@version=6
strategy("close fill passes", calc_on_order_fills=true)
varip int passes = 0
varip int flatPasses = 0
if bar_index == 0
    strategy.entry("E", strategy.{direction}, qty=1)
if bar_index == 2
    passes += 1
    if strategy.position_size == 0
        flatPasses += 1
        strategy.entry("phantom", strategy.long, qty=1)
    else
        strategy.close_all(immediately=true)
plot(passes)
plot(flatPasses)
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(
            &analysis.hir.unwrap(),
            &[bar(10.0), bar(11.0), bar(12.0), bar(13.0)],
        )
        .unwrap();
        assert_eq!(result.plots[0].values[3], PineValue::Int(1));
        assert_eq!(result.plots[1].values[3], PineValue::Int(0));
        let strategy = result.strategy.unwrap();
        assert_eq!(strategy.trades.len(), 1);
        assert_eq!(strategy.trades[0].exit_price, 12.0);
        assert_eq!(strategy.orders.len(), 1);
    }
}

#[test]
fn fill_recalculation_marks_accounts_at_execution_price_and_preserves_historical_close() {
    for direction in ["long", "short"] {
        for immediate in [true, false] {
            let source = SourceFile::new(
                "fill-account-mark.pine",
                format!(
                    r#"//@version=6
strategy("fill account mark", initial_capital=100000, calc_on_order_fills=true,
 commission_type=strategy.commission.cash_per_order, commission_value=0.1)
varip float equityAtFill = na
varip float profitAtFill = na
varip float tradeProfitAtFill = na
varip float closeAtFill = na
if bar_index == 0
    strategy.entry("E", strategy.{direction}, qty=1)
if strategy.position_size != 0 and na(equityAtFill)
    equityAtFill := strategy.equity
    profitAtFill := strategy.openprofit
    tradeProfitAtFill := strategy.opentrades.profit(0)
    closeAtFill := close
    if {immediate}
        strategy.close_all(immediately=true)
plot(equityAtFill)
plot(profitAtFill)
plot(tradeProfitAtFill)
plot(closeAtFill)
plot(strategy.equity)
"#
                ),
            );
            let analysis = analyze_source(&source);
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            let result = run_historical(
                &analysis.hir.unwrap(),
                &[bar(100.0), bar_ohlc(110.0, 120.0, 80.0, 90.0), bar(95.0)],
            )
            .unwrap();
            assert_eq!(result.plots[0].values[1], PineValue::Float(99999.9));
            assert_eq!(result.plots[1].values[1], PineValue::Float(0.0));
            assert_eq!(result.plots[2].values[1], PineValue::Float(-0.2));
            assert_eq!(result.plots[3].values[1], PineValue::Float(90.0));
            if immediate {
                let trades = &result.strategy.as_ref().unwrap().trades;
                assert_eq!(trades.len(), 1);
                assert_eq!(trades[0].entry_price, 110.0);
                assert_eq!(trades[0].exit_price, 110.0);
                assert_eq!(trades[0].profit, -0.2);
            } else {
                let sign = if direction == "long" { 1.0 } else { -1.0 };
                assert_eq!(
                    result.plots[4].values[1],
                    PineValue::Float(99999.9 - 20.0 * sign)
                );
                assert_eq!(
                    result.plots[4].values[2],
                    PineValue::Float(99999.9 - 15.0 * sign)
                );
            }
        }
    }
}

#[test]
fn explicit_entry_and_order_quantities_use_the_configured_contract_grid() {
    for function in ["entry", "order"] {
        for (direction, sign) in [("long", 1.0), ("short", -1.0)] {
            for order_args in ["", ", limit=100", ", stop=100", ", stop=100, limit=100"] {
                for (qty, expected) in [
                    ("0.1234569", 0.123456),
                    ("161253 * syminfo.mincontract", 0.161252),
                    ("0.161253", 0.161253),
                    ("0.123456", 0.123456),
                    ("488565 * syminfo.mincontract", 0.488564),
                    ("0.488565", 0.488565),
                    ("101250 * syminfo.mincontract", 0.101249),
                    ("0.10125", 0.10125),
                    ("129515 * syminfo.mincontract", 0.129515),
                    ("0.129515", 0.129515),
                    ("0.000001", 0.000001),
                    ("0.25 * syminfo.mincontract", 0.0),
                ] {
                    let source = SourceFile::new(
                        "contract-grid.pine",
                        format!(
                            "//@version=6\nstrategy(\"explicit contract grid\", initial_capital=1000000)\nif bar_index == 0\n    strategy.{function}(\"E\", strategy.{direction}, qty={qty}{order_args})\nplot(strategy.position_size)\n"
                        ),
                    );
                    let analysis = analyze_source(&source);
                    assert!(
                        analysis.diagnostics.is_empty(),
                        "{:?}",
                        analysis.diagnostics
                    );
                    let chart = ChartContext::default().with_quantity_precision(6).unwrap();
                    let result = run_historical_with_request_environment(
                        &analysis.hir.expect("HIR"),
                        &[bar(100.0); 3],
                        RequestEnvironment::default().for_chart(chart),
                    )
                    .unwrap();
                    assert_eq!(
                        result.plots[0].values[2],
                        PineValue::Float(sign * expected),
                        "{function} {direction} {qty}{order_args}"
                    );
                }
            }
        }
    }
}

#[test]
fn sub_contract_reversal_closes_exposure_without_opening_a_fractional_entry() {
    for (direction, opposite, sign) in [("long", "short", 1.0), ("short", "long", -1.0)] {
        for order_args in ["", ", limit=100", ", stop=100", ", stop=100, limit=100"] {
            let source = SourceFile::new(
                "sub-contract-reversal.pine",
                format!(
                    r#"//@version=5
strategy("sub-contract reversal", initial_capital=5000, commission_type=strategy.commission.percent, commission_value=0.04)
if bar_index == 0
    strategy.entry("E", strategy.{direction}, qty=2)
if bar_index == 1
    strategy.entry("same", strategy.{direction}, qty=0.75)
if bar_index == 2
    strategy.entry("zero", strategy.{opposite}, qty=0)
if bar_index == 3
    strategy.entry("R", strategy.{opposite}, qty=0.75{order_args}, comment="rounded reversal", alert_message="reversal filled")
plot(strategy.position_size)
plot(strategy.closedtrades.exit_id(0) == "R" ? 1 : 0)
"#
                ),
            );
            let analysis = analyze_source(&source);
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            let chart = ChartContext::default().with_quantity_precision(0).unwrap();
            let result = run_historical_with_request_environment(
                &analysis.hir.unwrap(),
                &[bar_ohlc(100.0, 101.0, 99.0, 100.0); 6],
                RequestEnvironment::default().for_chart(chart),
            )
            .unwrap();
            assert_eq!(result.plots[0].values[3], PineValue::Float(2.0 * sign));
            assert_eq!(
                result.plots[0].values[5],
                PineValue::Float(0.0),
                "{direction}{order_args}"
            );
            assert_eq!(result.plots[1].values[5], PineValue::Int(1));
            let strategy = result.strategy.unwrap();
            assert!(
                strategy.diagnostics.is_empty(),
                "{:?}",
                strategy.diagnostics
            );
            assert_eq!(strategy.trades.len(), 1);
            assert_eq!(strategy.trades[0].qty, 2.0 * sign);
            if order_args == ", stop=100, limit=100" {
                assert!((4..=5).contains(&strategy.trades[0].exit_bar_index));
            } else {
                assert_eq!(strategy.trades[0].exit_bar_index, 4);
            }
            assert!((strategy.trades[0].profit + 0.16).abs() < 1e-10);
            assert_eq!(strategy.orders.len(), 2);
            assert_eq!(strategy.orders[1].id, "R");
            assert_eq!(strategy.orders[1].qty, 2.0);
            assert_eq!(strategy.alerts.last().unwrap().message, "reversal filled");
        }
    }
}

#[test]
fn sub_contract_reversal_preserves_exits_for_other_pending_entries() {
    for (direction, opposite, sign, stop) in
        [("long", "short", 1.0, 101.0), ("short", "long", -1.0, 99.0)]
    {
        let source = SourceFile::new(
            "sub-contract-attached-exits.pine",
            format!(
                r#"//@version=5
strategy("sub-contract attached exits", initial_capital=5000)
if bar_index == 0
    strategy.entry("E", strategy.{direction}, qty=2)
if bar_index == 1
    strategy.entry("R", strategy.{opposite}, qty=0.75)
    strategy.entry("N", strategy.{opposite}, qty=1)
    strategy.exit("X", "N", stop={stop})
plot(strategy.position_size)
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical_with_request_environment(
            &analysis.hir.unwrap(),
            &[bar_ohlc(100.0, 102.0, 98.0, 100.0); 4],
            RequestEnvironment::default()
                .for_chart(ChartContext::default().with_quantity_precision(0).unwrap()),
        )
        .unwrap();
        let strategy = result.strategy.unwrap();
        assert!(
            strategy.diagnostics.is_empty(),
            "{:?}",
            strategy.diagnostics
        );
        assert_eq!(strategy.trades.len(), 2);
        assert_eq!(strategy.trades[0].id, "E");
        assert_eq!(strategy.trades[0].qty, 2.0 * sign);
        assert_eq!(strategy.trades[1].id, "N");
        assert_eq!(strategy.trades[1].qty, -sign);
        assert_eq!(strategy.trades[1].exit_bar_index, 2);
        assert_eq!(strategy.trades[1].exit_price, stop);
        assert!(strategy.orders.iter().any(|order| order.id == "X"));
    }
}

#[test]
fn sub_contract_reversal_oca_reduction_uses_the_closing_transaction_quantity() {
    for (direction, opposite, sign, limit) in
        [("long", "short", 1.0, 150), ("short", "long", -1.0, 50)]
    {
        let source = SourceFile::new(
            "sub-contract-oca.pine",
            format!(
                r#"//@version=5
strategy("sub-contract OCA", initial_capital=5000)
if bar_index == 0
    strategy.entry("E", strategy.{direction}, qty=2)
if bar_index == 1
    strategy.entry("R", strategy.{opposite}, qty=0.75, oca_name="G", oca_type=strategy.oca.reduce)
    strategy.entry("P", strategy.{opposite}, qty=3, limit={limit}, oca_name="G", oca_type=strategy.oca.reduce)
plot(strategy.position_size)
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical_with_request_environment(
            &analysis.hir.unwrap(),
            &[
                bar(100.0),
                bar(100.0),
                bar(100.0),
                bar_ohlc(100.0, 160.0, 40.0, 100.0),
                bar(100.0),
            ],
            RequestEnvironment::default()
                .for_chart(ChartContext::default().with_quantity_precision(0).unwrap()),
        )
        .unwrap();
        assert_eq!(result.plots[0].values[2], PineValue::Float(0.0));
        assert_eq!(result.plots[0].values[4], PineValue::Float(-sign));
        let strategy = result.strategy.unwrap();
        assert!(
            strategy.diagnostics.is_empty(),
            "{:?}",
            strategy.diagnostics
        );
        assert_eq!(strategy.orders.last().unwrap().id, "P");
        assert_eq!(strategy.orders.last().unwrap().qty, 1.0);
    }
}

#[test]
fn explicit_quantity_grid_respects_host_precision_and_unconfigured_profile() {
    for (qty, scale, expected) in [
        (0.1234569, None, 0.1234569),
        (12.99, Some(1), 12.0),
        (0.99, Some(1), 0.0),
        (12.999, Some(100), 12.99),
        (0.0000000019, Some(1_000_000_000), 0.000000001),
        (1e-100, Some(1_000_000_000), 0.0),
        (1e12, Some(1_000_000), 1e12),
    ] {
        assert_eq!(
            crate::strategy::explicit_quantity_on_chart_grid(qty, scale),
            expected,
            "qty={qty}, scale={scale:?}"
        );
    }
}

#[test]
fn explicit_quantity_grid_does_not_mask_invalid_negative_quantities() {
    for function in ["entry", "order"] {
        let source = SourceFile::new(
            "invalid-grid-quantity.pine",
            format!(
                "//@version=6\nstrategy(\"invalid explicit qty\")\nstrategy.{function}(\"E\", strategy.long, qty=-0.0000001)\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default().with_quantity_precision(6).unwrap();
        let error = run_historical_with_request_environment(
            &analysis.hir.expect("HIR"),
            &[bar(100.0)],
            RequestEnvironment::default().for_chart(chart),
        )
        .expect_err("invalid quantity must fail before quantization");
        assert!(error.message.contains("invalid `qty`"), "{error:?}");
    }
}

#[test]
fn legacy_v4_strategy_orders_execute_inside_user_functions() {
    let source = SourceFile::new(
        "v4-udf-orders.pine",
        r#"//@version=4
strategy("v4 UDF orders", initial_capital=1000)
openTrade() => strategy.entry("L", strategy.long)
closeTrade() => strategy.close("L")
if bar_index == 0
    openTrade()
if bar_index == 2
    closeTrade()
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0), bar(13.0)],
    )
    .expect("runtime result");
    let trades = &result.strategy.expect("strategy result").trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].entry_bar_index, 1);
    assert_eq!(trades[0].exit_bar_index, 3);
    assert_eq!(trades[0].entry_price, 11.0);
    assert_eq!(trades[0].exit_price, 13.0);
}

#[test]
fn legacy_v4_boolean_entry_directions_open_and_reverse_positions() {
    let source = SourceFile::new(
        "v4-boolean-entry-direction.pine",
        r#"//@version=4
strategy("legacy boolean directions")
if bar_index == 0
    strategy.entry("L", true)
if bar_index == 2
    strategy.entry("S", false)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("v4 HIR"),
        &[bar(10.0), bar(11.0), bar(12.0), bar(13.0)],
    )
    .expect("runtime result");
    assert_eq!(result.plots[0].values[1], PineValue::Float(1.0));
    assert_eq!(result.plots[0].values[3], PineValue::Float(-1.0));
    let trades = &result.strategy.expect("strategy result").trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].entry_price, 11.0);
    assert_eq!(trades[0].exit_price, 13.0);
}

#[test]
fn legacy_v4_strategy_close_when_skips_false_order() {
    let source = SourceFile::new(
        "v4-close-when.pine",
        r#"//@version=4
strategy("v4 close when", overlay=true, linktoseries=true)
if bar_index == 0
    strategy.entry("L", strategy.long)
if bar_index == 1
    strategy.close("L", when=false)
if bar_index == 2
    strategy.close("L", when=true)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0), bar(13.0)],
    )
    .expect("runtime result");
    let trades = &result.strategy.expect("strategy result").trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].entry_bar_index, 1);
    assert_eq!(trades[0].exit_bar_index, 3);
    assert_eq!(trades[0].exit_price, 13.0);

    let v6 = SourceFile::new(
        "v6-close-when.pine",
        "//@version=6\nstrategy(\"v6\")\nstrategy.close(\"L\", when=true)\n",
    );
    let analysis = analyze_source(&v6);
    assert!(analysis.hir.is_none());
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E_CALL_ARG_NAME" && diagnostic.message.contains("removed in Pine v6")
    }));
}

#[test]
fn legacy_v4_close_all_when_and_position_alert_placeholder() {
    let source = SourceFile::new(
        "v4-close-all-when.pine",
        r#"//@version=4
strategy("v4 close all when")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
strategy.close_all(when=bar_index == 2)
alertcondition(true, "Position", "size={{strategy.position_size}}")
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0), bar(13.0)],
    )
    .expect("runtime result");
    let trades = &result.strategy.as_ref().expect("strategy result").trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].entry_bar_index, 1);
    assert_eq!(trades[0].exit_bar_index, 3);
    assert_eq!(
        result
            .alerts
            .iter()
            .map(|alert| alert.message.as_str())
            .collect::<Vec<_>>(),
        ["size=0", "size=2", "size=2", "size=0"]
    );

    let v6 = SourceFile::new(
        "v6-close-all-when.pine",
        "//@version=6\nstrategy(\"v6\")\nstrategy.close_all(when=true)\n",
    );
    let analysis = analyze_source(&v6);
    assert!(analysis.hir.is_none());
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E_CALL_ARG_NAME" && diagnostic.message.contains("removed in Pine v6")
    }));
}

#[test]
fn close_fill_recalculates_and_reenters_at_same_historical_open() {
    let source = SourceFile::new(
        "close-fill-reentry.pine",
        r#"//@version=4
strategy("close fill reentry", calc_on_order_fills=true)
if bar_index == 0 and strategy.opentrades == 0
    strategy.entry("L", strategy.long)
if bar_index == 2 and strategy.opentrades > 0
    strategy.close("L")
if bar_index == 3 and strategy.opentrades == 0
    strategy.entry("L", strategy.long)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0), bar(13.0), bar(14.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy result");
    assert_eq!(strategy.trades.len(), 1, "{:?}", strategy.trades);
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.orders.len(), 3, "{:?}", strategy.orders);
    assert_eq!(strategy.orders[1].id, "Close entry(s) order L");
    assert_eq!(strategy.orders[1].bar_index, 3);
    assert_eq!(strategy.orders[1].price, 13.0);
    assert_eq!(strategy.orders[2].id, "L");
    assert_eq!(strategy.orders[2].price, 13.0);
}

#[test]
fn var_assignment_rolls_back_between_historical_fill_recalculations() {
    let source = SourceFile::new(
        "var-fill-rollback.pine",
        r#"//@version=4
strategy("var fill rollback", calc_on_order_fills=true)
var stop = 0.0
if bar_index == 0 and strategy.opentrades == 0
    strategy.entry("L", strategy.long)
    stop := close - 1
if bar_index == 2 and strategy.opentrades > 0
    strategy.close("L")
    stop := 0.0
if bar_index == 3 and strategy.opentrades == 0
    strategy.entry("L", strategy.long)
    stop := close - 1
plot(stop)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0), bar(13.0), bar(14.0)],
    )
    .expect("runtime result");
    assert_eq!(result.plots[0].values[3], PineValue::Float(0.0));
}

#[test]
fn varip_assignment_survives_historical_fill_recalculations() {
    let source = SourceFile::new(
        "varip-fill-persistence.pine",
        r#"//@version=6
strategy("varip fill persistence", calc_on_order_fills=true)
var ordinary = 0
varip intrabar = 0
ordinary += 1
intrabar += 1
if bar_index == 0
    strategy.entry("L", strategy.long)
plot(ordinary)
plot(intrabar)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(10.0), bar(11.0)])
        .expect("runtime result");
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(2)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(1), PineValue::Int(3)]
    );
}

#[test]
fn market_close_from_open_entry_fill_recalculation_fills_at_same_open() {
    let source = SourceFile::new(
        "open-fill-close.pine",
        r#"//@version=4
strategy("open fill close", calc_on_order_fills=true)
var stop = 0.0
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    stop := 9.5
if strategy.opentrades > 0 and close <= stop
    strategy.close("L")
plot(stop)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 11.0, 8.0, 9.0),
            bar_ohlc(9.0, 9.0, 9.0, 9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy result");
    assert_eq!(strategy.trades.len(), 1, "{:?}", strategy.trades);
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 1);
    assert_eq!(strategy.trades[0].entry_price, 10.0);
    assert_eq!(strategy.trades[0].exit_price, 10.0);
}

#[test]
fn historical_fill_recalculations_use_high_and_low_before_next_open() {
    let source = SourceFile::new(
        "v4-history-vertices.pine",
        r#"//@version=4
strategy("v4 historical vertices", calc_on_order_fills=true)
if strategy.opentrades == 0
    strategy.entry("L", strategy.long, qty=1)
else
    strategy.close("L")
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 11.0, 8.0, 9.0),
            bar_ohlc(9.0, 9.0, 9.0, 9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy result");
    let fills: Vec<_> = strategy
        .orders
        .iter()
        .filter(|order| order.bar_index == 1)
        .map(|order| order.price)
        .collect();
    assert_eq!(fills, vec![10.0, 10.0, 11.0, 8.0], "{:?}", strategy.orders);
    assert!(
        strategy.trades.iter().any(|trade| {
            trade.entry_bar_index == 1
                && trade.exit_bar_index == 1
                && trade.entry_price == 10.0
                && trade.exit_price == 10.0
        }),
        "{:?}",
        strategy.trades
    );
    assert!(
        strategy.trades.iter().any(|trade| {
            trade.entry_bar_index == 1
                && trade.exit_bar_index == 1
                && trade.entry_price == 11.0
                && trade.exit_price == 8.0
        }),
        "{:?}",
        strategy.trades
    );
    assert!(
        strategy
            .orders
            .iter()
            .any(|order| order.bar_index == 2 && order.price == 9.0),
        "{:?}",
        strategy.orders
    );
}

#[test]
fn same_bar_percent_of_equity_market_entry_uses_fill_price() {
    let source = SourceFile::new(
        "v4-percent-fill-quantity.pine",
        r#"//@version=4
strategy("percent fill quantity", initial_capital=1000, default_qty_type=strategy.percent_of_equity, default_qty_value=100, calc_on_order_fills=true)
if strategy.opentrades == 0
    strategy.entry("L", strategy.long)
else
    strategy.close("L")
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 8.0, 9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy result");
    let opening_entry = strategy
        .orders
        .iter()
        .find(|order| order.bar_index == 1 && order.price == 12.0)
        .expect("entry at next bar open");
    assert!((opening_entry.qty - 1000.0 / 10.0).abs() < 1e-9);
    let high_entry = strategy
        .orders
        .iter()
        .find(|order| order.bar_index == 1 && order.price == 13.0)
        .expect("entry at high after open close");
    assert!(
        (high_entry.qty - 1000.0 / 13.0).abs() < 1e-9,
        "{high_entry:?}"
    );
}

#[test]
fn percent_of_equity_short_strategy_order_uses_same_bar_fill_price_across_versions() {
    for version in [1, 2, 3, 4, 5, 6] {
        let source = SourceFile::new(
            format!("v{version}-short-order-quantity.pine"),
            format!(
                "//@version={version}\nstrategy(\"short order quantity\", initial_capital=1000, default_qty_type=strategy.percent_of_equity, default_qty_value=50, calc_on_order_fills=true)\nif strategy.position_size == 0\n    strategy.order(\"S\", strategy.short)\nelse\n    strategy.close_all()\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let program = analysis.hir.expect("HIR");
        let result = run_historical(
            &program,
            &[
                bar_ohlc(10.0, 10.0, 10.0, 10.0),
                bar_ohlc(12.0, 13.0, 8.0, 9.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy result");
        let next_bar_entry = strategy
            .orders
            .iter()
            .find(|order| order.bar_index == 1 && order.price == 12.0)
            .expect("short order from previous close");
        assert!((next_bar_entry.qty - 500.0 / 10.0).abs() < 1e-9);
        let same_bar_entry = strategy
            .orders
            .iter()
            .find(|order| order.bar_index == 1 && order.price == 13.0)
            .expect("short order at high");
        assert!(
            (same_bar_entry.qty - 500.0 / 13.0).abs() < 1e-9,
            "v{version}: {same_bar_entry:?}"
        );
        let chart = ChartContext::default()
            .with_quantity_precision(6)
            .expect("six-decimal quantity grid");
        let gridded = run_historical_with_request_environment(
            &program,
            &[
                bar_ohlc(10.0, 10.0, 10.0, 10.0),
                bar_ohlc(12.0, 13.0, 8.0, 9.0),
            ],
            RequestEnvironment::default().for_chart(chart),
        )
        .expect("gridded runtime result");
        let high_entry = gridded
            .strategy
            .expect("gridded strategy result")
            .orders
            .into_iter()
            .find(|order| order.bar_index == 1 && order.price == 13.0)
            .expect("gridded short entry at high");
        assert_eq!(high_entry.qty, 38.461538, "v{version}");
    }
}

#[test]
fn guarded_first_read_of_previous_strategy_position_uses_previous_script_pass() {
    let source = SourceFile::new(
        "guarded-position.pine",
        r#"//@version=6
strategy("guarded position history", process_orders_on_close=true)
fire = bar_index == 2
if fire and strategy.position_size <= 0
    strategy.entry("L", strategy.long)
var float stop = na
if fire and strategy.position_size[1] <= 0
    stop := close - 1
plot(stop)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0)],
    )
    .expect("runtime result");
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(11.0)]
    );
}

#[test]
fn marketable_exit_placed_at_close_fills_on_that_bar_when_enabled() {
    let source = SourceFile::new(
        "same-bar-close-exit.pine",
        r#"//@version=6
strategy("same bar exit", process_orders_on_close=true)
if bar_index == 0
    strategy.entry("L", strategy.long)
if strategy.position_size > 0
    strategy.exit("X", "L", stop=close + 1)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0)],
    )
    .expect("runtime result");
    let trades = &result.strategy.expect("strategy result").trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].entry_bar_index, 0);
    assert_eq!(trades[0].exit_bar_index, 1);
    assert_eq!(trades[0].exit_price, 11.0);
}

#[test]
fn marketable_exit_placed_at_close_waits_without_close_processing() {
    let source = SourceFile::new(
        "next-bar-exit.pine",
        r#"//@version=6
strategy("next bar exit", process_orders_on_close=false)
if bar_index == 0
    strategy.entry("L", strategy.long)
if strategy.position_size > 0
    strategy.exit("X", "L", stop=close + 1)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0)],
    )
    .expect("runtime result");
    let trades = &result.strategy.expect("strategy result").trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].entry_bar_index, 1);
    assert_eq!(trades[0].exit_bar_index, 2);
}

#[test]
fn strategy_declaration_emits_empty_strategy_result() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("empty")
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    assert_eq!(hir.script_mode, ScriptMode::Strategy);

    let result = run_historical(&hir, &[bar(1.0), bar(2.0)]).expect("runtime result");

    assert!(result.strategy.is_some());
    let strategy = result.strategy.expect("strategy output");
    assert!(strategy.orders.is_empty());
    assert!(strategy.trades.is_empty());
    assert!(strategy.position.is_empty());
    assert_eq!(strategy.equity.len(), 2);
    assert_eq!(strategy.equity[0].bar_index, 0);
    assert_eq!(strategy.equity[0].cash, 1000000.0);
    assert_eq!(strategy.equity[0].market_value, 0.0);
    assert_eq!(strategy.equity[0].equity, 1000000.0);
    assert_eq!(strategy.equity[0].net_profit, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(1.0), PineValue::Float(2.0)]
    );
}

#[test]
fn indicator_result_does_not_include_strategy_output() {
    let source = SourceFile::new(
        "indicator.pine",
        r#"indicator("plain")
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0)]).expect("runtime result");

    assert!(result.strategy.is_none());
}

fn const_float_arg(name: &str, value: f64) -> HirCallArg {
    HirCallArg {
        name: Some(name.to_owned()),
        value: HirExpr {
            kind: HirExprKind::Literal(HirLiteral::Float(value)),
            pine_type: PineType::new(Qualifier::Const, ValueKind::Float),
            series_id: None,
        },
    }
}

fn strategy_exit_args_mut(program: &mut HirProgram) -> &mut Vec<HirCallArg> {
    fn find_in_stmts(statements: &mut [HirStmt]) -> Option<&mut Vec<HirCallArg>> {
        for statement in statements {
            match &mut statement.kind {
                HirStmtKind::Expr(expr) => {
                    if let Some(args) = find_in_expr(expr) {
                        return Some(args);
                    }
                }
                HirStmtKind::If {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    if let Some(args) = find_in_stmts(then_branch) {
                        return Some(args);
                    }
                    if let Some(args) = find_in_stmts(else_branch) {
                        return Some(args);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn find_in_expr(expr: &mut HirExpr) -> Option<&mut Vec<HirCallArg>> {
        if let HirExprKind::Call { callee, args, .. } = &mut expr.kind
            && callee == "strategy.exit"
        {
            return Some(args);
        }
        None
    }

    find_in_stmts(&mut program.statements).expect("strategy.exit call")
}

#[test]
fn strategy_entry_opens_long_position_at_next_bar_open() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry")
if bar_index == 1
    strategy.entry("L", strategy.long, qty=2)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].time, 30);
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 2);
    assert_eq!(strategy.position[0].size, 2.0);
    assert_eq!(strategy.position[0].avg_price, Some(3.0));
    assert_eq!(strategy.equity[2].cash, 999994.0);
    assert_eq!(strategy.equity[2].market_value, 6.0);
    assert_eq!(strategy.equity[2].equity, 1000000.0);
}

#[test]
fn strategy_entry_opens_short_position_at_next_bar_open() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("short entry")
if bar_index == 1
    strategy.entry("S", strategy.short, qty=2)
plot(strategy.position_size)
plot(strategy.position_avg_price)
plot(strategy.max_contracts_held_short)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].time, 30);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 2);
    assert_eq!(strategy.position[0].size, -2.0);
    assert_eq!(strategy.position[0].avg_price, Some(3.0));
    assert_eq!(strategy.equity[2].cash, 1000006.0);
    assert_eq!(strategy.equity[2].market_value, -6.0);
    assert_eq!(strategy.equity[2].equity, 1000000.0);
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(-2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(3.0)]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
}

#[test]
fn strategy_close_short_records_signed_qty_and_cover_pnl() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("close short")
if bar_index == 1
    strategy.entry("S", strategy.short, qty=2)
if bar_index == 2
    strategy.close("S")
plot(strategy.position_size)
plot(strategy.netprofit)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 4.0,
            volume: 1.0,
        },
        Bar {
            time: 40,
            open: 4.0,
            high: 4.0,
            low: 4.0,
            close: 4.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "S");
    assert_eq!(strategy.trades[0].entry_price, 3.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, -2.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(0.0)
    );
    assert_eq!(strategy.equity[3].cash, 999998.0);
    assert_eq!(strategy.equity[3].net_profit, -2.0);
}

#[test]
fn strategy_entry_short_reverses_long_at_next_bar_open() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("short reverses long")
if bar_index == 1
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.entry("S", strategy.short, qty=1)
plot(strategy.position_size)
plot(strategy.netprofit)
plot(strategy.max_contracts_held_short)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
        Bar {
            time: 40,
            open: 4.0,
            high: 4.0,
            low: 4.0,
            close: 4.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.orders[1].direction, "strategy.short");
    assert_eq!(strategy.orders[1].qty, 3.0);
    assert_eq!(strategy.orders[1].price, 4.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].qty, 2.0);
    assert_eq!(strategy.trades[0].entry_price, 3.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(-1.0)
    );
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(-1.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
}

#[test]
fn strategy_entry_limit_fills_on_later_low_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry limit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, limit=2)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(5.0, 5.0, 5.0, 5.0),
            bar_ohlc(4.0, 4.0, 3.0, 4.0),
            bar_ohlc(3.0, 3.0, 2.0, 3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 2);
    assert_eq!(strategy.position[0].size, 2.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(2.0),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_limit_short_fills_on_later_high_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry limit short")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2, limit=2)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(1.5, 1.5, 1.5, 1.5),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 2);
    assert_eq!(strategy.position[0].size, -2.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(-2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(2.0),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_stop_short_fills_on_later_low_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry stop short")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2, stop=2)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 2);
    assert_eq!(strategy.position[0].size, -2.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(-2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(2.0),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_stop_limit_short_activates_then_fills_on_later_high_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry stop limit short")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2, stop=2, limit=3)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(3.0, 3.0, 2.5, 3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 2);
    assert_eq!(strategy.position[0].size, -2.0);
    assert_eq!(strategy.position[0].avg_price, Some(3.0));
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(-2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(3.0),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_limit_allows_same_calculation_absolute_exit_attachment() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry limit exit attachment")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, limit=2)
    strategy.exit("XL", "L", stop=1.5)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar_ohlc(5.0, 5.0, 5.0, 5.0), bar_ohlc(3.0, 3.0, 1.0, 2.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.orders[1].id, "XL");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 1.5);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 1.5);
    assert_eq!(strategy.trades[0].profit, -1.0);
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(0.0), PineValue::Float(0.0),]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(0), PineValue::Int(1),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_cancel_cancels_pending_entry_before_fill() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("cancel entry")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, limit=2)
    strategy.cancel("L")
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar_ohlc(5.0, 5.0, 5.0, 5.0), bar_ohlc(2.0, 2.0, 2.0, 2.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.position.is_empty());
    assert!(strategy.trades.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(0.0), PineValue::Float(0.0)]
    );
    assert_eq!(result.plots[1].values, vec![PineValue::Na, PineValue::Na]);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_cancel_unknown_cancelled_and_filled_ids_are_noop() {
    let source = SourceFile::new(
        "strategy_cancel_noop.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_cancel_noop.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert!(strategy.trades.is_empty());
    assert_eq!(strategy.position.last().unwrap().size, 2.0);
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_stop_fills_on_later_high_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry stop")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, stop=3)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 1.0, 2.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 2);
    assert_eq!(strategy.position[0].size, 2.0);
    assert_eq!(strategy.position[0].avg_price, Some(3.0));
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(3.0),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_stop_allows_same_calculation_absolute_exit_attachment() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry stop exit attachment")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, stop=3)
    strategy.exit("XL", "L", limit=3.5)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar_ohlc(1.0, 1.0, 1.0, 1.0), bar_ohlc(3.0, 3.5, 2.5, 3.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.orders[1].id, "XL");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 3.5);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].entry_price, 3.0);
    assert_eq!(strategy.trades[0].exit_price, 3.5);
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(0.0), PineValue::Float(0.0),]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(0), PineValue::Int(1),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_cancel_cancels_pending_exit_before_fill() {
    let source = SourceFile::new(
        "strategy_cancel_exit.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_cancel_exit.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].size, 2.0);
    assert!(strategy.trades.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_cancel_shared_id_entry_and_exit() {
    let strategy = run_named_strategy_fixture(
        "strategy_cancel_shared_id_entry_exit.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_cancel_shared_id_entry_exit.pine"
        ),
    );
    assert!(strategy.orders.is_empty());
    assert_eq!(strategy.position.last().map(|snapshot| snapshot.size), None);
    assert!(strategy.trades.is_empty());
}

#[test]
fn strategy_cancel_shared_id_close_and_exit() {
    let strategy = run_named_strategy_fixture(
        "strategy_cancel_shared_id_close_exit.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_cancel_shared_id_close_exit.pine"
        ),
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert!(strategy.trades.is_empty());
}

#[test]
fn strategy_cancel_all_clears_pending_families() {
    let strategy = run_named_strategy_fixture(
        "strategy_cancel_all_families.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_cancel_all_families.pine"),
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
    assert!(strategy.trades.is_empty());
}

#[test]
fn strategy_cancel_all_cancels_pending_entry_and_attached_exit_before_fill() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("cancel all entry exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, limit=2)
    strategy.exit("XL", "L", stop=1.5)
    strategy.cancel_all()
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar_ohlc(5.0, 5.0, 5.0, 5.0), bar_ohlc(2.0, 2.0, 1.0, 2.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.position.is_empty());
    assert!(strategy.trades.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(0.0), PineValue::Float(0.0)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(0), PineValue::Int(0)]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_cancel_all_cancels_pending_exit_before_fill() {
    let source = SourceFile::new(
        "strategy_cancel_all_exit.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_cancel_all_exit.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].size, 2.0);
    assert!(strategy.trades.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_stop_limit_activates_then_fills_on_later_low_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry stop limit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, stop=3, limit=2)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(3.0, 3.0, 2.0, 3.0),
            bar_ohlc(2.0, 2.5, 2.0, 2.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 1);
    assert_eq!(strategy.position[0].size, 2.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Float(2.0), PineValue::Float(2.0),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_stop_limit_orders_triggered_together_can_exceed_pyramiding_limit() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry")
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1, stop=11, limit=10)
    strategy.entry("L2", strategy.long, qty=3, stop=11, limit=10)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(11.0, 11.0, 10.0, 11.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L1");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 10.0);
    assert_eq!(strategy.orders[1].id, "L2");
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].qty, 3.0);
    assert_eq!(strategy.orders[1].price, 10.0);
    assert_eq!(strategy.position.last().unwrap().size, 4.0);
    assert_eq!(strategy.position.last().unwrap().avg_price, Some(10.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Na,
            PineValue::Float(10.0),
            PineValue::Float(10.0),
            PineValue::Float(10.0),
        ]
    );
}

#[test]
fn strategy_entry_stop_limit_allows_same_calculation_absolute_exit_attachment() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry stop limit exit attachment")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, stop=3, limit=2)
    strategy.exit("XL", "L", stop=1.5)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(3.0, 3.0, 2.0, 3.0),
            bar_ohlc(2.0, 2.0, 1.0, 1.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.orders[1].id, "XL");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 1.5);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 1.5);
    assert_eq!(strategy.trades[0].profit, -1.0);
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(0), PineValue::Int(0), PineValue::Int(1),]
    );
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_entry_ignores_repeated_entry_without_pyramiding() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry")
strategy.entry("L1", strategy.long, qty=1)
strategy.entry("L2", strategy.long, qty=1)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result =
        run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L1");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].bar_index, 1);
    assert_eq!(strategy.position[0].size, 1.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_order_market_netting_crosses_zero_in_both_directions() {
    let oversized = run_named_strategy_fixture(
        "strategy_order_short_oversized_against_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_short_oversized_against_long.pine"
        ),
    );
    assert_eq!(oversized.position.last().map(|s| s.size), Some(-2.0));
    assert_eq!(oversized.trades.len(), 1);
    assert_eq!(oversized.orders.last().map(|order| order.qty), Some(3.0));

    let cover = run_named_strategy_fixture(
        "strategy_order_long_against_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_long_against_short.pine"),
    );
    assert_eq!(cover.position.last().map(|s| s.size), Some(1.0));
    assert_eq!(cover.trades.len(), 1);
    assert_eq!(cover.orders.last().map(|order| order.qty), Some(2.0));
}

#[test]
fn strategy_order_replace_cancel_and_close_rule_interaction() {
    let replaced = run_named_strategy_fixture(
        "strategy_order_replace_limit_with_stop.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_replace_limit_with_stop.pine"
        ),
    );
    assert_eq!(
        replaced.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(replaced.orders.last().map(|order| order.price), Some(3.0));
    assert_eq!(replaced.trades.len(), 0);

    let reversed = run_named_strategy_fixture(
        "strategy_order_replace_long_with_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_replace_long_with_short.pine"
        ),
    );
    assert_eq!(
        reversed.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(reversed.trades.len(), 0);

    let cancelled = run_named_strategy_fixture(
        "strategy_order_cancel_shared_id.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_cancel_shared_id.pine"),
    );
    assert_eq!(
        cancelled.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(cancelled.trades.len(), 0);
    assert_eq!(cancelled.orders.len(), 1);

    let fifo = run_named_strategy_fixture(
        "strategy_order_reduce_fifo.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_reduce_fifo.pine"),
    );
    assert_eq!(
        fifo.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(fifo.trades.len(), 1);
    assert_eq!(fifo.trades[0].id, "A");

    let any = run_named_strategy_fixture(
        "strategy_order_reduce_any_matching_id.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_reduce_any_matching_id.pine"
        ),
    );
    assert_eq!(any.position.last().map(|snapshot| snapshot.size), Some(1.0));
    assert_eq!(any.trades.len(), 1);
    assert_eq!(any.trades[0].id, "B");
}

#[test]
fn strategy_order_oca_cancel_cancels_peer_and_keeps_unrelated_group() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_oca_cancel.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_oca_cancel.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
    assert_eq!(strategy.orders.len(), 2);
    let ids: Vec<_> = strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert!(ids.contains(&"A"));
    assert!(ids.contains(&"C"));
    assert!(!ids.contains(&"B"));
}

#[test]
fn strategy_exit_oca_reduce_lets_stop_cover_grouped_limit() {
    let strategy = run_named_strategy_fixture(
        "strategy_exit_oca_reduce.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_exit_oca_reduce.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(strategy.orders.len(), 3);
    assert_eq!(strategy.orders[1].id, "TP");
    assert_eq!(strategy.orders[1].qty, 1.0);
    assert_eq!(strategy.orders[2].id, "SL");
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.trades.len(), 2);
}

#[test]
fn strategy_exit_oca_reduce_bracket_keeps_reduced_peer() {
    let strategy = run_named_strategy_fixture(
        "strategy_exit_oca_reduce_bracket.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_exit_oca_reduce_bracket.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert!(
        strategy.orders.iter().any(|order| order.id == "BR"),
        "same-price bracket limit is created before the stop peer and fills first"
    );
}

#[test]
fn strategy_order_oca_reduce_cuts_peer_quantity_and_fills_remainder() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_oca_reduce.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_oca_reduce.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "A");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[1].id, "B");
    assert_eq!(strategy.orders[1].qty, 1.0);
}

#[test]
fn strategy_order_oca_reduce_zero_cancels_over_reduced_peer() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_oca_reduce_zero.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_oca_reduce_zero.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "A");
    assert_eq!(strategy.orders[0].qty, 2.0);
}

#[test]
fn strategy_order_oca_none_fills_grouped_orders_independently() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_oca_none.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_oca_none.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.trades.len(), 0);
}

#[test]
fn strategy_mixed_oca_entry_order_cancel_cancels_peer() {
    let strategy = run_named_strategy_fixture(
        "strategy_mixed_oca_entry_order_cancel.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_mixed_oca_entry_order_cancel.pine"
        ),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert!(!strategy.orders.iter().any(|order| order.id == "O"));
}

#[test]
fn strategy_mixed_oca_entry_order_reduce_cuts_peer_quantity() {
    let strategy = run_named_strategy_fixture(
        "strategy_mixed_oca_entry_order_reduce.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_mixed_oca_entry_order_reduce.pine"
        ),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].qty, 1.0);
}

#[test]
fn strategy_mixed_oca_none_fills_entry_and_order_independently() {
    let strategy = run_named_strategy_fixture(
        "strategy_mixed_oca_none.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_mixed_oca_none.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
    assert_eq!(strategy.orders.len(), 2);
}

#[test]
fn strategy_mixed_oca_order_exit_reduce_cuts_stop_reservation() {
    let strategy = run_named_strategy_fixture(
        "strategy_mixed_oca_order_exit_reduce.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_mixed_oca_order_exit_reduce.pine"
        ),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert!(strategy.orders.iter().any(|order| order.id == "L"));
    assert!(strategy.orders.iter().any(|order| order.id == "TP"));
    assert!(!strategy.orders.iter().any(|order| order.id == "SL"));
    assert_eq!(
        strategy
            .orders
            .iter()
            .find(|order| order.id == "TP")
            .map(|order| order.qty),
        Some(1.0)
    );
}

#[test]
fn strategy_mixed_oca_high_first_stop_cancels_limit_peer() {
    let source = SourceFile::new(
        "strategy_mixed_oca_high_first.pine",
        r#"
strategy("mixed oca high first")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11.5, oca_name="g", oca_type=strategy.oca.cancel)
    strategy.order("LIM", strategy.long, qty=1, limit=8.5, oca_name="g", oca_type=strategy.oca.cancel)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(11.0, 12.0, 8.0, 9.0),
        ],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "STP");
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
}

#[test]
fn strategy_mixed_oca_low_first_limit_cancels_stop_peer() {
    let source = SourceFile::new(
        "strategy_mixed_oca_low_first.pine",
        r#"
strategy("mixed oca low first")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11.5, oca_name="g", oca_type=strategy.oca.cancel)
    strategy.order("LIM", strategy.long, qty=1, limit=8.5, oca_name="g", oca_type=strategy.oca.cancel)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 12.0, 8.0, 11.0),
        ],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "LIM");
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
}

#[test]
fn strategy_mixed_oca_calc_on_order_fills_does_not_resurrect_cancelled_peer() {
    let source = SourceFile::new(
        "strategy_mixed_oca_calc_on_order_fills.pine",
        r#"
strategy("mixed oca recalc", calc_on_order_fills=true)
if bar_index == 1
    strategy.entry("E", strategy.long, qty=1, limit=3, oca_name="g", oca_type=strategy.oca.cancel)
    strategy.order("O", strategy.long, qty=1, limit=3, oca_name="g", oca_type=strategy.oca.cancel)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "E");
}

#[test]
fn strategy_ordinary_chart_up_gap_fills_stop_at_next_open() {
    let source = SourceFile::new(
        "strategy_ordinary_chart_up_gap_stop.pine",
        r#"
strategy("ordinary up gap")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.5),
        ],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "STP");
    assert_eq!(strategy.orders[0].price, 12.0);
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
}

#[test]
fn strategy_ordinary_chart_down_gap_fills_limit_at_next_open() {
    let source = SourceFile::new(
        "strategy_ordinary_chart_down_gap_limit.pine",
        r#"
strategy("ordinary down gap")
if bar_index == 0
    strategy.entry("LIM", strategy.long, qty=1, limit=11)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(12.0, 12.0, 12.0, 12.0),
            bar_ohlc(10.0, 10.0, 9.0, 9.5),
        ],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "LIM");
    assert_eq!(strategy.orders[0].price, 10.0);
}

#[test]
fn strategy_ordinary_chart_no_gap_stop_still_fills_at_trigger() {
    let source = SourceFile::new(
        "strategy_ordinary_chart_no_gap_stop.pine",
        r#"
strategy("ordinary no gap")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 12.0, 10.0, 11.0),
        ],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].price, 11.0);
}

#[test]
fn strategy_ordinary_chart_gap_stop_limit_activates_without_preactivation_fill() {
    let source = SourceFile::new(
        "strategy_ordinary_chart_gap_stop_limit.pine",
        r#"
strategy("ordinary gap stop-limit")
if bar_index == 0
    strategy.entry("SL", strategy.long, qty=1, stop=11, limit=10.5)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.5),
        ],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert!(
        strategy.orders.is_empty(),
        "stop-limit must not fill from prices that existed only before activation: {:?}",
        strategy.orders
    );
}

#[test]
fn strategy_ordinary_chart_short_down_gap_fills_stop_at_next_open() {
    let source = SourceFile::new(
        "strategy_ordinary_chart_short_down_gap.pine",
        r#"
strategy("ordinary short down gap")
if bar_index == 0
    strategy.entry("STP", strategy.short, qty=1, stop=9)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let strategy = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(8.0, 8.0, 7.0, 7.5),
        ],
    )
    .expect("run")
    .strategy
    .expect("strategy");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "STP");
    assert_eq!(strategy.orders[0].price, 8.0);
}

fn run_ordinary_chart_strategy(name: &str, source: &str, bars: &[Bar]) -> crate::StrategyResult {
    let source = SourceFile::new(name, source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{name} diagnostics: {:?}",
        analysis.diagnostics
    );
    run_historical(&analysis.hir.expect("HIR"), bars)
        .expect(name)
        .strategy
        .expect("strategy")
}

#[test]
fn strategy_ordinary_chart_gap_at_trigger_fills_stop_at_open() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_at_trigger.pine",
        r#"
strategy("ordinary at trigger")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(11.0, 12.0, 11.0, 11.5),
        ],
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].price, 11.0);
}

#[test]
fn strategy_ordinary_chart_flat_bar_gap_fills_stop_at_next_open() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_flat_bar_gap.pine",
        r#"
strategy("ordinary flat gap")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 12.0, 12.0, 12.0),
        ],
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].price, 12.0);
}

#[test]
fn strategy_ordinary_chart_gap_bracket_fills_limit_at_next_open() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_bracket.pine",
        r#"
strategy("ordinary gap bracket")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XR", "L", stop=9, limit=11)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 12.0, 12.0, 12.0),
        ],
    );
    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XR");
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_price, 12.0);
}

#[test]
fn strategy_ordinary_chart_gap_trailing_fills_at_next_open_not_trigger() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_trailing.pine",
        r#"
strategy("ordinary gap trailing")
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1)
    strategy.exit("TR", "EN", trail_price=10.5, trail_offset=50)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 11.0, 10.0, 11.0),
            bar_ohlc(9.0, 9.0, 8.0, 8.5),
        ],
    );
    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "TR");
    assert_eq!(strategy.orders[1].price, 9.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
}

#[test]
fn strategy_ordinary_chart_gap_stop_applies_slippage_to_next_open() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_slippage.pine",
        r#"
strategy("ordinary gap slippage", slippage=100)
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.5),
        ],
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].price, 13.0);
}

#[test]
fn strategy_ordinary_chart_gap_limit_verification_requires_tick_beyond_open() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_limit_verification.pine",
        r#"
strategy("ordinary gap verify", backtest_fill_limits_assumption=100)
if bar_index == 0
    strategy.entry("LIM", strategy.long, qty=1, limit=11)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(12.0, 12.0, 12.0, 12.0),
            bar_ohlc(10.5, 10.5, 10.5, 10.5),
        ],
    );
    assert!(
        strategy.orders.is_empty(),
        "limit verification must reject a gap open that does not trade a tick beyond the limit: {:?}",
        strategy.orders
    );
}

#[test]
fn strategy_ordinary_chart_gap_calc_on_order_fills_does_not_backfill_gap_prices() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_calc_on_order_fills.pine",
        r#"
strategy("ordinary gap recalc", calc_on_order_fills=true, pyramiding=2)
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
if strategy.position_size > 0
    strategy.entry("LIM", strategy.long, qty=1, limit=10.5)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.5),
        ],
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "STP");
    assert_eq!(strategy.orders[0].price, 12.0);
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
}

#[test]
fn strategy_ordinary_chart_gap_oca_cancel_fills_one_peer() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_oca.pine",
        r#"
strategy("ordinary gap oca", pyramiding=2)
if bar_index == 0
    strategy.entry("E", strategy.long, qty=1, stop=11, oca_name="g", oca_type=strategy.oca.cancel)
    strategy.order("O", strategy.long, qty=1, stop=11, oca_name="g", oca_type=strategy.oca.cancel)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.5),
        ],
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
}

#[test]
fn strategy_ordinary_chart_gap_max_intraday_filled_orders_trips_after_first_open_fill() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_risk.pine",
        r#"
strategy("ordinary gap risk", pyramiding=2)
strategy.risk.max_intraday_filled_orders(1)
if bar_index == 0
    strategy.entry("A", strategy.long, qty=1, stop=11)
    strategy.entry("B", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.5),
        ],
    );
    assert_eq!(strategy.orders[0].id, "A");
    assert_eq!(strategy.orders[0].price, 12.0);
    assert!(
        strategy.orders.iter().all(|order| order.id != "B"),
        "the second stop must be cancelled after the filled-order cap trips: {:?}",
        strategy.orders
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(0.0),
        "max_intraday_filled_orders flattens after the first gap fill: {:?}",
        strategy.position
    );
    assert_eq!(strategy.trades.len(), 1);
}

#[test]
fn strategy_ordinary_chart_pending_market_and_stop_each_fill_once_at_open() {
    let strategy = run_ordinary_chart_strategy(
        "strategy_ordinary_chart_gap_market_and_stop.pine",
        r#"
strategy("ordinary gap market and stop", pyramiding=2)
if bar_index == 0
    strategy.entry("M", strategy.long, qty=1)
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.5),
        ],
    );
    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "M");
    assert_eq!(strategy.orders[0].price, 12.0);
    assert_eq!(strategy.orders[1].id, "STP");
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(2.0)
    );
}

#[test]
fn strategy_ordinary_chart_gap_incremental_append_matches_batch() {
    let source = SourceFile::new(
        "strategy_ordinary_chart_gap_incremental.pine",
        r#"
strategy("ordinary gap incremental")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let bars = [
        bar_ohlc(10.0, 10.0, 10.0, 10.0),
        bar_ohlc(12.0, 13.0, 12.0, 12.5),
    ];
    let batch = run_historical(&hir, &bars)
        .expect("batch")
        .strategy
        .expect("strategy");
    let mut incremental = HistoricalRuntime::new(&hir);
    for bar in bars {
        incremental.append_bar(bar).expect("append");
    }
    assert_eq!(
        incremental.result().strategy.expect("strategy").orders,
        batch.orders
    );
    assert_eq!(
        incremental.result().strategy.expect("strategy").position,
        batch.position
    );
}

#[test]
fn strategy_ordinary_chart_and_magnifier_share_next_open_gap_fill() {
    let ordinary_source = SourceFile::new(
        "strategy_ordinary_chart_gap_shared.pine",
        r#"
strategy("ordinary shared gap")
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
    );
    let magnifier_source = SourceFile::new(
        "strategy_ordinary_chart_gap_shared_magnifier.pine",
        r#"
strategy("magnifier shared gap", use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=11)
plot(strategy.position_size)
"#,
    );
    let ordinary = analyze_source(&ordinary_source);
    let magnifier = analyze_source(&magnifier_source);
    assert!(
        ordinary.diagnostics.is_empty(),
        "{:?}",
        ordinary.diagnostics
    );
    assert!(
        magnifier.diagnostics.is_empty(),
        "{:?}",
        magnifier.diagnostics
    );
    let bars = [
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
            open: 12.0,
            high: 13.0,
            low: 12.0,
            close: 12.5,
            volume: 1.0,
        },
    ];
    let ordinary_fill = run_historical(&ordinary.hir.expect("HIR"), &bars)
        .expect("ordinary")
        .strategy
        .expect("strategy")
        .orders
        .into_iter()
        .find(|order| order.id == "STP")
        .expect("ordinary stop");
    let input = crate::magnifier_input_from_groups(vec![
        crate::MagnifierChartBarInput {
            chart_bar_index: 0,
            bars: vec![bars[0]],
        },
        crate::MagnifierChartBarInput {
            chart_bar_index: 1,
            bars: vec![bars[1]],
        },
    ])
    .expect("magnifier");
    let magnifier_fill = HistoricalRuntime::new(&magnifier.hir.expect("HIR"))
        .with_magnifier_input(input)
        .run(&bars)
        .expect("magnifier")
        .strategy
        .expect("strategy")
        .orders
        .into_iter()
        .find(|order| order.id == "STP")
        .expect("magnifier stop");
    assert_eq!(ordinary_fill.price, 12.0);
    assert_eq!(magnifier_fill.price, ordinary_fill.price);
    assert_eq!(magnifier_fill.bar_index, ordinary_fill.bar_index);
}

#[test]
fn strategy_session_window_overnight_does_not_false_reset_filled_orders() {
    let source = SourceFile::new(
        "strategy_session_overnight.pine",
        r#"
strategy("session overnight", pyramiding=2)
strategy.risk.max_intraday_filled_orders(2)
if bar_index == 0
    strategy.entry("A", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("B", strategy.long, qty=1)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let bar = |time: i64| crate::Bar {
        time,
        open: 10.0,
        high: 10.0,
        low: 10.0,
        close: 10.0,
        volume: 1.0,
    };
    let bars = [
        bar(86_400_000 - 1_000),
        bar(86_400_000 + 1_000),
        bar(2 * 86_400_000 + 1_000),
    ];
    let utc = run_historical(&hir, &bars)
        .expect("utc")
        .strategy
        .expect("strategy");
    assert_eq!(utc.position.last().map(|snapshot| snapshot.size), Some(2.0));
    let windows = crate::session_window_input_from_json(
        r#"{"schemaVersion":1,"bars":[{"barIndex":0,"windowId":"eth","tradingDayId":"d1"},{"barIndex":1,"windowId":"eth","tradingDayId":"d1"},{"barIndex":2,"windowId":"eth","tradingDayId":"d1"}]}"#,
    )
    .expect("windows");
    let session = HistoricalRuntime::new(&hir)
        .with_session_windows(windows)
        .expect("session input")
        .run(&bars)
        .expect("session")
        .strategy
        .expect("strategy");
    assert!(
        session.position.last().map(|snapshot| snapshot.size) != Some(2.0),
        "same overnight window must count both fills toward max_intraday_filled_orders: {:?}",
        session.position.last()
    );
    let mut incremental = HistoricalRuntime::new(&hir).with_session_windows(
        crate::session_window_input_from_json(
            r#"{"schemaVersion":1,"bars":[{"barIndex":0,"windowId":"eth","tradingDayId":"d1"},{"barIndex":1,"windowId":"eth","tradingDayId":"d1"},{"barIndex":2,"windowId":"eth","tradingDayId":"d1"}]}"#,
        )
        .expect("windows"),
    ).expect("session input");
    for bar in bars {
        incremental.append_bar(bar).expect("append");
    }
    assert_eq!(
        incremental.result().strategy.expect("strategy").position,
        session.position
    );
}

#[test]
fn strategy_entry_limit_reverses_short_after_trigger() {
    let strategy = run_named_strategy_fixture(
        "strategy_entry_limit_reverses_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_entry_limit_reverses_short.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.orders.last().map(|order| order.qty), Some(2.0));
    assert_eq!(strategy.orders.last().map(|order| order.price), Some(3.0));
}

#[test]
fn strategy_entry_price_based_reverses_both_directions() {
    let limit_long = run_named_strategy_fixture(
        "strategy_entry_limit_reverses_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_entry_limit_reverses_long.pine"),
    );
    assert_eq!(
        limit_long.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(limit_long.trades.len(), 1);

    let qty = run_named_strategy_fixture(
        "strategy_entry_limit_reverses_short_qty.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_entry_limit_reverses_short_qty.pine"
        ),
    );
    assert_eq!(qty.position.last().map(|snapshot| snapshot.size), Some(1.0));
    assert_eq!(qty.trades.len(), 1);
    assert_eq!(qty.orders.last().map(|order| order.qty), Some(3.0));

    let stop_short = run_named_strategy_fixture(
        "strategy_entry_stop_reverses_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_entry_stop_reverses_short.pine"),
    );
    assert_eq!(
        stop_short.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(stop_short.trades.len(), 1);

    let stop_long = run_named_strategy_fixture(
        "strategy_entry_stop_reverses_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_entry_stop_reverses_long.pine"),
    );
    assert_eq!(
        stop_long.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(stop_long.trades.len(), 1);

    let stop_limit_short = run_named_strategy_fixture(
        "strategy_entry_stop_limit_reverses_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_entry_stop_limit_reverses_short.pine"
        ),
    );
    assert_eq!(
        stop_limit_short
            .position
            .last()
            .map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(stop_limit_short.trades.len(), 1);
    assert_eq!(
        stop_limit_short.orders.last().map(|order| order.price),
        Some(3.0)
    );

    let stop_limit_long = run_named_strategy_fixture(
        "strategy_entry_stop_limit_reverses_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_entry_stop_limit_reverses_long.pine"
        ),
    );
    assert_eq!(
        stop_limit_long
            .position
            .last()
            .map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(stop_limit_long.trades.len(), 1);
}

#[test]
fn strategy_order_limit_nets_opposite_side_after_trigger() {
    let cover = run_named_strategy_fixture(
        "strategy_order_limit_long_against_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_limit_long_against_short.pine"
        ),
    );
    assert_eq!(
        cover.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(cover.trades.len(), 1);
    assert_eq!(cover.orders.last().map(|order| order.qty), Some(2.0));
    assert_eq!(cover.orders.last().map(|order| order.price), Some(3.0));

    let reverse = run_named_strategy_fixture(
        "strategy_order_limit_short_against_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_limit_short_against_long.pine"
        ),
    );
    assert_eq!(
        reverse.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(reverse.trades.len(), 1);
    assert_eq!(reverse.orders.last().map(|order| order.qty), Some(2.0));
}

#[test]
fn strategy_order_limit_flattens_and_reduces_both_sides() {
    let flatten_short = run_named_strategy_fixture(
        "strategy_order_limit_long_flatten_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_limit_long_flatten_short.pine"
        ),
    );
    assert_eq!(
        flatten_short.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(flatten_short.trades.len(), 1);

    let flatten_long = run_named_strategy_fixture(
        "strategy_order_limit_short_flatten_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_limit_short_flatten_long.pine"
        ),
    );
    assert_eq!(
        flatten_long.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(flatten_long.trades.len(), 1);

    let reduce_short = run_named_strategy_fixture(
        "strategy_order_limit_long_reduce_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_limit_long_reduce_short.pine"
        ),
    );
    assert_eq!(
        reduce_short.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(reduce_short.trades.len(), 1);

    let reduce_long = run_named_strategy_fixture(
        "strategy_order_limit_short_reduce_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_limit_short_reduce_long.pine"
        ),
    );
    assert_eq!(
        reduce_long.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(reduce_long.trades.len(), 1);
}

#[test]
fn strategy_order_stop_nets_opposite_side_after_trigger() {
    let cover = run_named_strategy_fixture(
        "strategy_order_stop_long_against_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_long_against_short.pine"
        ),
    );
    assert_eq!(
        cover.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(cover.trades.len(), 1);
    assert_eq!(cover.orders.last().map(|order| order.price), Some(3.0));

    let reverse = run_named_strategy_fixture(
        "strategy_order_stop_short_against_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_short_against_long.pine"
        ),
    );
    assert_eq!(
        reverse.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(reverse.trades.len(), 1);

    let flatten = run_named_strategy_fixture(
        "strategy_order_stop_long_flatten_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_long_flatten_short.pine"
        ),
    );
    assert_eq!(
        flatten.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(flatten.trades.len(), 1);

    let reduce = run_named_strategy_fixture(
        "strategy_order_stop_short_reduce_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_short_reduce_long.pine"
        ),
    );
    assert_eq!(
        reduce.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(reduce.trades.len(), 1);
}

#[test]
fn strategy_order_stop_limit_nets_after_activation_and_limit() {
    let cover = run_named_strategy_fixture(
        "strategy_order_stop_limit_long_against_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_limit_long_against_short.pine"
        ),
    );
    assert_eq!(
        cover.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(cover.trades.len(), 1);
    assert_eq!(cover.orders.last().map(|order| order.price), Some(3.0));

    let reverse = run_named_strategy_fixture(
        "strategy_order_stop_limit_short_against_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_limit_short_against_long.pine"
        ),
    );
    assert_eq!(
        reverse.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(reverse.trades.len(), 1);

    let flatten = run_named_strategy_fixture(
        "strategy_order_stop_limit_long_flatten_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_limit_long_flatten_short.pine"
        ),
    );
    assert_eq!(
        flatten.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(flatten.trades.len(), 1);

    let reduce = run_named_strategy_fixture(
        "strategy_order_stop_limit_short_reduce_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_stop_limit_short_reduce_long.pine"
        ),
    );
    assert_eq!(
        reduce.position.last().map(|snapshot| snapshot.size),
        Some(1.0)
    );
    assert_eq!(reduce.trades.len(), 1);
}

fn run_named_strategy_fixture(name: &str, source: &str) -> crate::StrategyResult {
    let source = SourceFile::new(name, source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{name} diagnostics: {:?}",
        analysis.diagnostics
    );
    run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect(name)
    .strategy
    .expect("strategy output")
}

#[test]
fn strategy_order_market_long_adds_to_existing_long_without_pyramiding() {
    let source = SourceFile::new(
        "strategy_order_market_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_market_long.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(4.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 4.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, 1.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert_eq!(strategy.position[1].size, 3.0);
    assert_eq!(strategy.position[1].avg_price, Some(10.0 / 3.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(10.0 / 3.0),
        ]
    );
}

#[test]
fn strategy_order_market_long_uses_default_qty_when_qty_is_absent() {
    for (name, declaration, bars, expected_qty, expected_price) in [
        (
            "fixed",
            r#"strategy("order", default_qty_type=strategy.fixed, default_qty_value=3)"#,
            vec![bar(2.0), bar(4.0)],
            3.0,
            4.0,
        ),
        (
            "cash",
            r#"strategy("order", initial_capital=1000, default_qty_type=strategy.cash, default_qty_value=100)"#,
            vec![bar(10.0), bar(20.0)],
            10.0,
            20.0,
        ),
        (
            "percent",
            r#"strategy("order", initial_capital=1000, default_qty_type=strategy.percent_of_equity, default_qty_value=25)"#,
            vec![bar(10.0), bar(20.0)],
            25.0,
            20.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_order_default_{name}.pine"),
            format!(
                r#"{declaration}
if bar_index == 0
    strategy.order("D", strategy.long)
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name} diagnostics: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 1, "{name}");
        assert_eq!(strategy.orders[0].id, "D", "{name}");
        assert_eq!(strategy.orders[0].direction, "strategy.long", "{name}");
        assert_eq!(strategy.orders[0].qty, expected_qty, "{name}");
        assert_eq!(strategy.orders[0].price, expected_price, "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_order_limit_long_adds_to_existing_long_without_pyramiding() {
    let source = SourceFile::new(
        "strategy_order_limit_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_limit_long.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(9.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 11.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 9.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, 1.0);
    assert_eq!(strategy.position[0].avg_price, Some(11.0));
    assert_eq!(strategy.position[1].size, 3.0);
    assert_eq!(strategy.position[1].avg_price, Some(29.0 / 3.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Float(11.0),
            PineValue::Float(29.0 / 3.0),
        ]
    );
}

#[test]
fn strategy_order_stop_limit_short_adds_to_existing_short_without_pyramiding() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("order stop limit short", pyramiding=1)
if bar_index == 0
    strategy.entry("S", strategy.short, qty=1)
if bar_index == 1
    strategy.order("O", strategy.short, qty=2, stop=2, limit=3)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(3.0, 3.0, 2.5, 3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].direction, "strategy.short");
    assert_eq!(strategy.orders[1].bar_index, 3);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 3.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, -1.0);
    assert_eq!(strategy.position[0].avg_price, Some(3.0));
    assert_eq!(strategy.position[1].size, -3.0);
    assert_eq!(strategy.position[1].avg_price, Some(3.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-1.0),
            PineValue::Float(-1.0),
            PineValue::Float(-3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
}

#[test]
fn strategy_order_stop_short_adds_to_existing_short_without_pyramiding() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("order stop short")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=1)
if bar_index == 1
    strategy.order("O", strategy.short, qty=2, stop=2)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].direction, "strategy.short");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 2.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, -1.0);
    assert_eq!(strategy.position[0].avg_price, Some(3.0));
    assert_eq!(strategy.position[1].size, -3.0);
    assert_eq!(strategy.position[1].avg_price, Some(7.0 / 3.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-1.0),
            PineValue::Float(-3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Float(7.0 / 3.0),
        ]
    );
}

#[test]
fn strategy_order_limit_short_adds_to_existing_short_without_pyramiding() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("order limit short")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=1)
if bar_index == 1
    strategy.order("O", strategy.short, qty=2, limit=3)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].direction, "strategy.short");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 3.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, -1.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert_eq!(strategy.position[1].size, -3.0);
    assert_eq!(strategy.position[1].avg_price, Some(8.0 / 3.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-1.0),
            PineValue::Float(-3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(8.0 / 3.0),
        ]
    );
}

#[test]
fn strategy_cancel_cancels_pending_limit_strategy_order_before_fill() {
    let source = SourceFile::new(
        "strategy_order_cancel_limit.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_cancel_limit.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result =
        run_historical(&analysis.hir.expect("HIR"), &[bar(5.0), bar(2.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.position.is_empty());
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(0.0), PineValue::Float(0.0)]
    );
    assert_eq!(result.plots[1].values, vec![PineValue::Na, PineValue::Na]);
}

#[test]
fn strategy_order_metadata_survives_price_order_and_reduce_fill() {
    let source = SourceFile::new(
        "strategy_order_metadata.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_metadata.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(9.0), bar(12.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].price, 9.0);
    assert_eq!(strategy.orders[1].id, "R");
    assert_eq!(strategy.orders[1].direction, "strategy.short");
    assert_eq!(strategy.orders[1].bar_index, 3);
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "R");
    assert_eq!(strategy.alerts.len(), 2);
    assert_eq!(strategy.alerts[0].id, "L");
    assert_eq!(strategy.alerts[0].message, "limit alert");
    assert_eq!(strategy.alerts[0].entry_id.as_deref(), Some("L"));
    assert_eq!(strategy.alerts[0].exit_id, None);
    assert_eq!(strategy.alerts[1].id, "R");
    assert_eq!(strategy.alerts[1].message, "reduce alert");
    assert_eq!(strategy.alerts[1].entry_id, None);
    assert_eq!(strategy.alerts[1].exit_id.as_deref(), Some("R"));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Int(1),
        ]
    );
}

#[test]
fn strategy_order_stop_long_adds_to_existing_long_without_pyramiding() {
    let source = SourceFile::new(
        "strategy_order_stop_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_stop_long.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 11.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, 1.0);
    assert_eq!(strategy.position[0].avg_price, Some(11.0));
    assert_eq!(strategy.position[1].size, 3.0);
    assert_eq!(strategy.position[1].avg_price, Some(35.0 / 3.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Float(11.0),
            PineValue::Float(35.0 / 3.0),
        ]
    );
}

#[test]
fn strategy_cancel_all_cancels_pending_stop_strategy_order_before_fill() {
    let source = SourceFile::new(
        "strategy_order_cancel_all_stop.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_cancel_all_stop.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result =
        run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(3.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.position.is_empty());
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(0.0), PineValue::Float(0.0)]
    );
    assert_eq!(result.plots[1].values, vec![PineValue::Na, PineValue::Na]);
}

#[test]
fn strategy_order_stop_limit_long_adds_after_activation_without_pyramiding() {
    let source = SourceFile::new(
        "strategy_order_stop_limit_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_stop_limit_long.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(12.0), bar(10.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 11.0);
    assert_eq!(strategy.orders[1].id, "O");
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].bar_index, 3);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 10.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, 1.0);
    assert_eq!(strategy.position[0].avg_price, Some(11.0));
    assert_eq!(strategy.position[1].size, 3.0);
    assert_eq!(strategy.position[1].avg_price, Some(31.0 / 3.0));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Float(11.0),
            PineValue::Float(11.0),
            PineValue::Float(31.0 / 3.0),
        ]
    );
}

#[test]
fn strategy_cancel_cancels_pending_stop_limit_strategy_order_before_activation() {
    let source = SourceFile::new(
        "strategy_order_cancel_stop_limit.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_cancel_stop_limit.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(3.0), bar(2.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.position.is_empty());
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Na]
    );
}

#[test]
fn strategy_order_market_short_reduces_existing_long_without_reversal() {
    let source = SourceFile::new(
        "strategy_order_reduce_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_reduce_long.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(4.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.orders[1].id, "R");
    assert_eq!(strategy.orders[1].direction, "strategy.short");
    assert_eq!(strategy.orders[1].qty, 0.5);
    assert_eq!(strategy.orders[1].price, 4.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[1].size, 0.5);
    assert_eq!(strategy.position[1].avg_price, Some(2.0));
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "E");
    assert_eq!(strategy.trades[0].exit_id, "R");
    assert_eq!(strategy.trades[0].qty, 0.5);
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(0.5),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(1.0),
        ]
    );
}

#[test]
fn strategy_order_market_short_increases_existing_short() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_market_short_increase.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_market_short_increase.pine"
        ),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(-3.0)
    );
    assert!(strategy.trades.is_empty());
    assert_eq!(strategy.orders.last().map(|order| order.qty), Some(2.0));
}

#[test]
fn strategy_order_long_flattens_matching_short() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_long_flatten_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_long_flatten_short.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.orders.last().map(|order| order.qty), Some(2.0));
}

#[test]
fn strategy_order_long_reduces_existing_short() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_long_reduce_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_long_reduce_short.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.orders.last().map(|order| order.qty), Some(1.0));
}

#[test]
fn strategy_order_short_flattens_matching_long() {
    let strategy = run_named_strategy_fixture(
        "strategy_order_short_flatten_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_short_flatten_long.pine"),
    );
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.orders.last().map(|order| order.qty), Some(2.0));
}

#[test]
fn strategy_order_market_short_opens_while_flat() {
    let source = SourceFile::new(
        "strategy_order_short_flat_noop.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_order_short_flat_noop.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(4.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "R");
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert!(strategy.trades.is_empty());
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(-1.0)
    );
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-1.0),
            PineValue::Float(-1.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(0), PineValue::Int(0), PineValue::Int(0),]
    );
}

#[test]
fn strategy_entry_pyramiding_allows_multiple_long_market_entries() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.entry("L3", strategy.long, qty=5)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.position_avg_price)
plot(strategy.max_contracts_held_long)
plot(strategy.opentrades.entry_price(1))
plot(strategy.opentrades.size(1))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .expect("HIR")
            .strategy_settings
            .pyramiding_limit,
        2
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(8.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L1");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 2.0);
    assert_eq!(strategy.orders[1].id, "L2");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 3.0);
    assert_eq!(strategy.orders[1].price, 4.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, 1.0);
    assert_eq!(strategy.position[0].avg_price, Some(2.0));
    assert_eq!(strategy.position[1].size, 4.0);
    assert_eq!(strategy.position[1].avg_price, Some(3.5));
    assert!(strategy.trades.is_empty());
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(3.5),
            PineValue::Float(3.5),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
        ]
    );
    assert_eq!(
        result.plots[4].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(4.0),
            PineValue::Float(4.0),
        ]
    );
    assert_eq!(
        result.plots[5].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
}

#[test]
fn strategy_entry_limit_orders_triggered_together_can_exceed_pyramiding_limit() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry")
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1, limit=9)
    strategy.entry("L2", strategy.long, qty=3, limit=9)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(9.0), bar(9.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L1");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 9.0);
    assert_eq!(strategy.orders[1].id, "L2");
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].qty, 3.0);
    assert_eq!(strategy.orders[1].price, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 4.0);
    assert_eq!(strategy.position.last().unwrap().avg_price, Some(9.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Int(0), PineValue::Int(2), PineValue::Int(2)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![PineValue::Na, PineValue::Float(9.0), PineValue::Float(9.0),]
    );
}

#[test]
fn strategy_entry_stop_orders_triggered_together_can_exceed_pyramiding_limit() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry")
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1, stop=11)
    strategy.entry("L2", strategy.long, qty=3, stop=11)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.position_avg_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(11.0), bar(11.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "L1");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 11.0);
    assert_eq!(strategy.orders[1].id, "L2");
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].qty, 3.0);
    assert_eq!(strategy.orders[1].price, 11.0);
    assert_eq!(strategy.position.last().unwrap().size, 4.0);
    assert_eq!(strategy.position.last().unwrap().avg_price, Some(11.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Int(0), PineValue::Int(2), PineValue::Int(2)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Na,
            PineValue::Float(11.0),
            PineValue::Float(11.0),
        ]
    );
}

#[test]
fn strategy_close_pyramiding_entry_id_closes_matching_open_trade() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("close", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.close("L1")
if bar_index == 3
    strategy.close("L2")
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.position_avg_price)
plot(strategy.closedtrades)
plot(strategy.opentrades.entry_id(0) == "L2" ? 1 : 0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(8.0), bar(16.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "L1");
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 8.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 6.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "L2");
    assert_eq!(strategy.trades[1].entry_bar_index, 2);
    assert_eq!(strategy.trades[1].exit_bar_index, 4);
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 16.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 36.0);
    assert_eq!(strategy.position.len(), 4);
    assert_eq!(strategy.position[1].size, 4.0);
    assert_eq!(strategy.position[1].avg_price, Some(3.5));
    assert_eq!(strategy.position[2].size, 3.0);
    assert_eq!(strategy.position[2].avg_price, Some(4.0));
    assert_eq!(strategy.position[3].size, 0.0);
    assert_eq!(strategy.position[3].avg_price, None);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(3.5),
            PineValue::Float(4.0),
            PineValue::Na,
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
        ]
    );
    assert_eq!(
        result.plots[4].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(0),
        ]
    );
}

#[test]
fn strategy_close_all_pyramiding_flattens_all_open_trades() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("close_all", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.close_all()
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(8.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "L1");
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 8.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 6.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "L2");
    assert_eq!(strategy.trades[1].entry_bar_index, 2);
    assert_eq!(strategy.trades[1].exit_bar_index, 3);
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 8.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 12.0);
    assert_eq!(strategy.position.len(), 3);
    assert_eq!(strategy.position[1].size, 4.0);
    assert_eq!(strategy.position[1].avg_price, Some(3.5));
    assert_eq!(strategy.position[2].size, 0.0);
    assert_eq!(strategy.position[2].avg_price, None);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_close_entries_rule_fifo_preserves_default_allocation_order() {
    let close_all_source = SourceFile::new(
        "strategy_close_entries_rule_fifo_close_all.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_close_entries_rule_fifo_close_all.pine"
        ),
    );
    let close_all_analysis = analyze_source(&close_all_source);
    assert!(
        close_all_analysis.diagnostics.is_empty(),
        "{:?}",
        close_all_analysis.diagnostics
    );
    assert_eq!(
        close_all_analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Fifo
    );

    let close_all_result = run_historical(
        &close_all_analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(5.0)],
    )
    .expect("runtime result");
    let close_all_strategy = close_all_result.strategy.expect("strategy output");

    assert_eq!(close_all_strategy.trades.len(), 2);
    assert_eq!(close_all_strategy.trades[0].id, "L1");
    assert_eq!(close_all_strategy.trades[0].exit_id, "L1");
    assert_eq!(close_all_strategy.trades[0].entry_bar_index, 1);
    assert_eq!(close_all_strategy.trades[0].exit_bar_index, 3);
    assert_eq!(close_all_strategy.trades[0].entry_price, 2.0);
    assert_eq!(close_all_strategy.trades[0].exit_price, 5.0);
    assert_eq!(close_all_strategy.trades[0].qty, 1.0);
    assert_eq!(close_all_strategy.trades[0].profit, 3.0);
    assert_eq!(close_all_strategy.trades[1].id, "L2");
    assert_eq!(close_all_strategy.trades[1].exit_id, "L2");
    assert_eq!(close_all_strategy.trades[1].entry_bar_index, 2);
    assert_eq!(close_all_strategy.trades[1].exit_bar_index, 3);
    assert_eq!(close_all_strategy.trades[1].entry_price, 4.0);
    assert_eq!(close_all_strategy.trades[1].exit_price, 5.0);
    assert_eq!(close_all_strategy.trades[1].qty, 3.0);
    assert_eq!(close_all_strategy.trades[1].profit, 3.0);
    assert_eq!(close_all_strategy.position.last().unwrap().size, 0.0);
    assert!(close_all_strategy.diagnostics.is_empty());
    assert_eq!(
        close_all_result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
    for plot_index in 1..=2 {
        assert_eq!(
            close_all_result.plots[plot_index].values.last(),
            Some(&PineValue::Int(1))
        );
    }

    let exit_source = SourceFile::new(
        "strategy_close_entries_rule_fifo.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_close_entries_rule_fifo.pine"),
    );
    let exit_analysis = analyze_source(&exit_source);
    assert!(
        exit_analysis.diagnostics.is_empty(),
        "{:?}",
        exit_analysis.diagnostics
    );
    assert_eq!(
        exit_analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Fifo
    );

    let exit_result = run_historical(
        &exit_analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(5.0), bar(5.0)],
    )
    .expect("runtime result");
    let exit_strategy = exit_result.strategy.expect("strategy output");

    assert_eq!(exit_strategy.trades.len(), 2);
    assert_eq!(exit_strategy.trades[0].id, "L1");
    assert_eq!(exit_strategy.trades[0].exit_id, "XL");
    assert_eq!(exit_strategy.trades[0].entry_price, 2.0);
    assert_eq!(exit_strategy.trades[0].exit_price, 5.0);
    assert_eq!(exit_strategy.trades[0].qty, 1.0);
    assert_eq!(exit_strategy.trades[0].profit, 3.0);
    assert_eq!(exit_strategy.trades[1].id, "L2");
    assert_eq!(exit_strategy.trades[1].exit_id, "XL");
    assert_eq!(exit_strategy.trades[1].entry_price, 4.0);
    assert_eq!(exit_strategy.trades[1].exit_price, 5.0);
    assert_eq!(exit_strategy.trades[1].qty, 3.0);
    assert_eq!(exit_strategy.trades[1].profit, 3.0);
    assert_eq!(exit_strategy.position.last().unwrap().size, 0.0);
    assert!(exit_strategy.diagnostics.is_empty());
    assert_eq!(
        exit_result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
    for plot_index in 1..=2 {
        assert_eq!(
            exit_result.plots[plot_index].values.last(),
            Some(&PineValue::Int(1))
        );
    }
}

#[test]
fn strategy_close_entries_rule_any_uses_entry_id_allocation() {
    let close_source = SourceFile::new(
        "strategy_close_entries_rule_any_close.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_close_entries_rule_any_close.pine"
        ),
    );
    let close_analysis = analyze_source(&close_source);
    assert!(
        close_analysis.diagnostics.is_empty(),
        "{:?}",
        close_analysis.diagnostics
    );
    assert_eq!(
        close_analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Any
    );

    let close_result = run_historical(
        &close_analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(6.0)],
    )
    .expect("runtime result");
    let close_strategy = close_result.strategy.expect("strategy output");

    assert_eq!(close_strategy.trades.len(), 1);
    assert_eq!(close_strategy.trades[0].id, "target");
    assert_eq!(close_strategy.trades[0].exit_id, "target");
    assert_eq!(close_strategy.trades[0].entry_price, 4.0);
    assert_eq!(close_strategy.trades[0].exit_price, 6.0);
    assert_eq!(close_strategy.trades[0].qty, 2.0);
    assert_eq!(close_strategy.trades[0].profit, 4.0);
    assert_eq!(close_strategy.position.last().unwrap().size, 1.0);
    assert!(close_strategy.diagnostics.is_empty());
    assert_eq!(
        close_result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        close_result.plots[1].values.last(),
        Some(&PineValue::Int(1))
    );
    assert_eq!(
        close_result.plots[2].values.last(),
        Some(&PineValue::Float(1.0))
    );

    let exit_source = SourceFile::new(
        "strategy_close_entries_rule_any_exit_from_entry.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_close_entries_rule_any_exit_from_entry.pine"
        ),
    );
    let exit_analysis = analyze_source(&exit_source);
    assert!(
        exit_analysis.diagnostics.is_empty(),
        "{:?}",
        exit_analysis.diagnostics
    );
    assert_eq!(
        exit_analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Any
    );

    let exit_result = run_historical(
        &exit_analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(5.0), bar(5.0)],
    )
    .expect("runtime result");
    let exit_strategy = exit_result.strategy.expect("strategy output");

    assert_eq!(exit_strategy.trades.len(), 1);
    assert_eq!(exit_strategy.trades[0].id, "target");
    assert_eq!(exit_strategy.trades[0].exit_id, "XT");
    assert_eq!(exit_strategy.trades[0].entry_price, 4.0);
    assert_eq!(exit_strategy.trades[0].exit_price, 5.0);
    assert_eq!(exit_strategy.trades[0].qty, 2.0);
    assert_eq!(exit_strategy.trades[0].profit, 2.0);
    assert_eq!(exit_strategy.position.last().unwrap().size, 1.0);
    assert!(exit_strategy.diagnostics.is_empty());
    assert_eq!(
        exit_result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(exit_result.plots[1].values.last(), Some(&PineValue::Int(1)));
    assert_eq!(
        exit_result.plots[2].values.last(),
        Some(&PineValue::Float(1.0))
    );
}

#[test]
fn strategy_close_entries_rule_any_uses_short_entry_id_allocation() {
    let close_source = SourceFile::new(
        "strategy_close_entries_rule_any_close_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_close_entries_rule_any_close_short.pine"
        ),
    );
    let close_analysis = analyze_source(&close_source);
    assert!(
        close_analysis.diagnostics.is_empty(),
        "{:?}",
        close_analysis.diagnostics
    );
    assert_eq!(
        close_analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Any
    );

    let close_result = run_historical(
        &close_analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let close_strategy = close_result.strategy.expect("strategy output");

    assert_eq!(close_strategy.trades.len(), 1);
    assert_eq!(close_strategy.trades[0].id, "target");
    assert_eq!(close_strategy.trades[0].exit_id, "target");
    assert_eq!(close_strategy.trades[0].entry_price, 3.0);
    assert_eq!(close_strategy.trades[0].exit_price, 4.0);
    assert_eq!(close_strategy.trades[0].qty, -2.0);
    assert_eq!(close_strategy.trades[0].profit, -2.0);
    assert_eq!(close_strategy.position.last().unwrap().size, -1.0);
    assert!(close_strategy.diagnostics.is_empty());
    assert_eq!(
        close_result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        close_result.plots[1].values.last(),
        Some(&PineValue::Int(1))
    );
    assert_eq!(
        close_result.plots[2].values.last(),
        Some(&PineValue::Float(-1.0))
    );

    let exit_source = SourceFile::new(
        "strategy_close_entries_rule_any_exit_from_entry_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_close_entries_rule_any_exit_from_entry_short.pine"
        ),
    );
    let exit_analysis = analyze_source(&exit_source);
    assert!(
        exit_analysis.diagnostics.is_empty(),
        "{:?}",
        exit_analysis.diagnostics
    );
    assert_eq!(
        exit_analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Any
    );

    let exit_result = run_historical(
        &exit_analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
        ],
    )
    .expect("runtime result");
    let exit_strategy = exit_result.strategy.expect("strategy output");

    assert_eq!(exit_strategy.trades.len(), 1);
    assert_eq!(exit_strategy.trades[0].id, "target");
    assert_eq!(exit_strategy.trades[0].exit_id, "XT");
    assert_eq!(exit_strategy.trades[0].entry_price, 4.0);
    assert_eq!(exit_strategy.trades[0].exit_price, 3.0);
    assert_eq!(exit_strategy.trades[0].qty, -2.0);
    assert_eq!(exit_strategy.trades[0].profit, 2.0);
    assert_eq!(exit_strategy.position.last().unwrap().size, -1.0);
    assert!(exit_strategy.diagnostics.is_empty());
    assert_eq!(
        exit_result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(exit_result.plots[1].values.last(), Some(&PineValue::Int(1)));
    assert_eq!(
        exit_result.plots[2].values.last(),
        Some(&PineValue::Float(-1.0))
    );
}

#[test]
fn strategy_close_entries_rule_any_partial_exit_same_id_preserves_ledger_order() {
    let source = SourceFile::new(
        "strategy_close_entries_rule_any_exit_same_id_partial.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_close_entries_rule_any_exit_same_id_partial.pine"
        ),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Any
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(5.0), bar(5.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 3.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_bar_index, 2);
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 5.0);
    assert_eq!(strategy.trades[1].qty, 0.5);
    assert_eq!(strategy.trades[1].profit, 0.5);
    assert_eq!(strategy.position.last().unwrap().size, 2.5);
    assert_eq!(strategy.position.last().unwrap().avg_price, Some(4.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
    assert_eq!(result.plots[1].values.last(), Some(&PineValue::Int(1)));
    assert_eq!(result.plots[2].values.last(), Some(&PineValue::Int(1)));
    assert_eq!(result.plots[3].values.last(), Some(&PineValue::Float(2.5)));
}

#[test]
fn strategy_close_entries_rule_any_partial_exit_same_short_id_preserves_ledger_order() {
    let source = SourceFile::new(
        "strategy_close_entries_rule_any_exit_same_id_partial_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_close_entries_rule_any_exit_same_id_partial_short.pine"
        ),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .close_entries_rule,
        pine_ir::StrategyCloseEntriesRule::Any
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "S");
    assert_eq!(strategy.trades[0].exit_id, "XS");
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 3.0);
    assert_eq!(strategy.trades[0].qty, -1.0);
    assert_eq!(strategy.trades[0].profit, -1.0);
    assert_eq!(strategy.trades[1].id, "S");
    assert_eq!(strategy.trades[1].exit_id, "XS");
    assert_eq!(strategy.trades[1].entry_bar_index, 2);
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 3.0);
    assert_eq!(strategy.trades[1].qty, -0.5);
    assert_eq!(strategy.trades[1].profit, 0.5);
    assert_eq!(strategy.position.last().unwrap().size, -2.5);
    assert_eq!(strategy.position.last().unwrap().avg_price, Some(4.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
    assert_eq!(result.plots[1].values.last(), Some(&PineValue::Int(1)));
    assert_eq!(result.plots[2].values.last(), Some(&PineValue::Int(1)));
    assert_eq!(result.plots[3].values.last(), Some(&PineValue::Float(-2.5)));
}

#[test]
fn strategy_exit_pyramiding_entry_id_closes_matching_open_trade() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XL1", "L1", limit=5)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(6.0), bar(8.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 3);
    assert_eq!(strategy.orders[2].id, "XL1");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XL1");
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 4.0);
    assert_eq!(strategy.position.len(), 3);
    assert_eq!(strategy.position[1].size, 4.0);
    assert_eq!(strategy.position[1].avg_price, Some(3.5));
    assert_eq!(strategy.position[2].size, 3.0);
    assert_eq!(strategy.position[2].avg_price, Some(4.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
}

#[test]
fn strategy_exit_pyramiding_same_entry_id_closes_each_matching_trade() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XL", "L", limit=5)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(6.0), bar(8.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[1].id, "L");
    assert_eq!(strategy.orders[2].id, "XL");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.orders[3].id, "XL");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 3);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 6.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 4.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 6.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 6.0);
    assert_eq!(strategy.position.len(), 3);
    assert_eq!(strategy.position[2].size, 0.0);
    assert_eq!(strategy.position[2].avg_price, None);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_pyramiding_profit_ticks_use_matching_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XP1", "L1", profit=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(4.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar(5.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 3);
    assert_eq!(strategy.orders[2].id, "XP1");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XP1");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.position[2].size, 3.0);
    assert_eq!(strategy.position[2].avg_price, Some(4.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
}

#[test]
fn strategy_exit_pyramiding_bracket_ticks_use_matching_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB1", "L1", profit=200, loss=50)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(4.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar(5.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 3);
    assert_eq!(strategy.orders[2].id, "XB1");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XB1");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.position[2].size, 3.0);
    assert_eq!(strategy.position[2].avg_price, Some(4.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
}

#[test]
fn strategy_exit_pyramiding_trail_points_use_matching_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XT1", "L1", trail_points=200, trail_offset=50)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(4.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(4.0, 4.0, 3.5, 3.5),
            bar(3.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 3);
    assert_eq!(strategy.orders[2].id, "XT1");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 3.5);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XT1");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 3.5);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.5);
    assert_eq!(strategy.position[1].size, 4.0);
    assert_eq!(strategy.position[1].avg_price, Some(3.5));
    assert_eq!(strategy.position[2].size, 3.0);
    assert_eq!(strategy.position[2].avg_price, Some(4.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_closes_current_open_entries() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XL", limit=5)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(5.0), bar(5.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XL");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XL");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 3);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 5.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 3.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 5.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 3.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_persists_for_later_entries() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XL", limit=10)
if bar_index == 2
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(4.0), bar(10.0), bar(10.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[0].id, "L1");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[1].id, "L2");
    assert_eq!(strategy.orders[1].bar_index, 3);
    assert_eq!(strategy.orders[2].id, "XL");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 10.0);
    assert_eq!(strategy.orders[3].id, "XL");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 10.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 10.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 8.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 10.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 18.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_profit_uses_each_open_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XP", profit=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(4.0), bar(6.0), bar(6.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XP");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.0);
    assert_eq!(strategy.orders[3].id, "XP");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 6.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XP");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XP");
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 6.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 6.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_profit_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XP", profit=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(4.0), bar(4.0), bar(6.0), bar(6.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XP");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.0);
    assert_eq!(strategy.orders[3].id, "XP");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 6.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XP");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XP");
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 6.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 6.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_profit_persists_for_later_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XP", profit=300)
if bar_index == 2
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(4.0),
            bar(4.0),
            bar(5.0),
            bar(7.0),
            bar(7.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XP");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XP");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 7.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XP");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 3.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XP");
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 7.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_profit_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XP", profit=300)
if bar_index == 2
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(4.0),
            bar(4.0),
            bar(5.0),
            bar(7.0),
            bar(7.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XP");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XP");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 7.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XP");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 3.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XP");
    assert_eq!(strategy.trades[1].entry_price, 4.0);
    assert_eq!(strategy.trades[1].exit_price, 7.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_uses_each_open_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XL", loss=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(8.0), bar(6.0), bar(6.0), bar(4.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XL");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.orders[3].id, "XL");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 4.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 4.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, -6.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XL", loss=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(8.0), bar(6.0), bar(6.0), bar(4.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XL");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.orders[3].id, "XL");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 4.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 4.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, -6.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_persists_for_later_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XL", loss=300)
if bar_index == 2
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(5.0),
            bar(3.0),
            bar(3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XL");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XL");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 3.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -3.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 3.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, -9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XL", loss=300)
if bar_index == 2
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(5.0),
            bar(3.0),
            bar(3.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XL");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XL");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 3.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -3.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XL");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 3.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, -9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_profit_bracket_uses_each_open_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", loss=200, profit=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar_ohlc(6.0, 6.0, 6.0, 6.0),
            bar_ohlc(6.0, 8.0, 6.0, 8.0),
            bar(8.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 8.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 8.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 6.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_profit_bracket_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", loss=200, profit=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar_ohlc(6.0, 6.0, 6.0, 6.0),
            bar_ohlc(6.0, 8.0, 6.0, 8.0),
            bar(8.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 8.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 8.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 6.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_profit_bracket_persists_for_later_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", loss=300, profit=300)
if bar_index == 2
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(5.0),
            bar(9.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -3.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_profit_bracket_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", loss=300, profit=300)
if bar_index == 2
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(5.0),
            bar(9.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -3.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_profit_bracket_uses_each_open_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", stop=5, profit=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar_ohlc(6.0, 8.0, 6.0, 8.0),
            bar_ohlc(8.0, 8.0, 5.0, 5.0),
            bar(5.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 3.0);
    assert_eq!(strategy.orders[2].price, 8.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 1.0);
    assert_eq!(strategy.orders[3].price, 5.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L2");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 6.0);
    assert_eq!(strategy.trades[0].exit_price, 8.0);
    assert_eq!(strategy.trades[0].qty, 3.0);
    assert_eq!(strategy.trades[0].profit, 6.0);
    assert_eq!(strategy.trades[1].id, "L1");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 8.0);
    assert_eq!(strategy.trades[1].exit_price, 5.0);
    assert_eq!(strategy.trades[1].qty, 1.0);
    assert_eq!(strategy.trades[1].profit, -3.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(1.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_profit_bracket_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", stop=5, profit=200)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar_ohlc(6.0, 8.0, 6.0, 8.0),
            bar_ohlc(8.0, 8.0, 5.0, 5.0),
            bar(5.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 3.0);
    assert_eq!(strategy.orders[2].price, 8.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 1.0);
    assert_eq!(strategy.orders[3].price, 5.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 6.0);
    assert_eq!(strategy.trades[0].exit_price, 8.0);
    assert_eq!(strategy.trades[0].qty, 3.0);
    assert_eq!(strategy.trades[0].profit, 6.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 8.0);
    assert_eq!(strategy.trades[1].exit_price, 5.0);
    assert_eq!(strategy.trades[1].qty, 1.0);
    assert_eq!(strategy.trades[1].profit, -3.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(1.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_profit_bracket_persists_for_later_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", stop=5, profit=300)
if bar_index == 2
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(9.0),
            bar(5.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 3.0);
    assert_eq!(strategy.orders[2].price, 9.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 1.0);
    assert_eq!(strategy.orders[3].price, 5.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L2");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 6.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].qty, 3.0);
    assert_eq!(strategy.trades[0].profit, 9.0);
    assert_eq!(strategy.trades[1].id, "L1");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 8.0);
    assert_eq!(strategy.trades[1].exit_price, 5.0);
    assert_eq!(strategy.trades[1].qty, 1.0);
    assert_eq!(strategy.trades[1].profit, -3.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(1.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_profit_bracket_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", stop=5, profit=300)
if bar_index == 2
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(9.0),
            bar(5.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 3.0);
    assert_eq!(strategy.orders[2].price, 9.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 1.0);
    assert_eq!(strategy.orders[3].price, 5.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 6.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].qty, 3.0);
    assert_eq!(strategy.trades[0].profit, 9.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 8.0);
    assert_eq!(strategy.trades[1].exit_price, 5.0);
    assert_eq!(strategy.trades[1].qty, 1.0);
    assert_eq!(strategy.trades[1].profit, -3.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(1.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_limit_bracket_uses_each_open_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", loss=200, limit=9)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar_ohlc(6.0, 6.0, 6.0, 6.0),
            bar_ohlc(6.0, 9.0, 6.0, 9.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_limit_bracket_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", loss=200, limit=9)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar_ohlc(6.0, 6.0, 6.0, 6.0),
            bar_ohlc(6.0, 9.0, 6.0, 9.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 6.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 6.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_limit_bracket_persists_for_later_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", loss=300, limit=9)
if bar_index == 2
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(5.0),
            bar(9.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -3.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_loss_limit_bracket_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", loss=300, limit=9)
if bar_index == 2
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar(8.0),
            bar(6.0),
            bar(6.0),
            bar(5.0),
            bar(9.0),
            bar(9.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 5);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, -3.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(3.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_limit_bracket_closes_all_open_entries() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", stop=5, limit=9)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(8.0), bar(6.0), bar(9.0), bar(9.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 9.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 3);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_limit_bracket_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XB", stop=5, limit=9)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(8.0), bar(6.0), bar(9.0), bar(9.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 3);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 9.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 3);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_limit_bracket_persists_for_later_entry() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", stop=5, limit=9)
if bar_index == 2
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(8.0), bar(6.0), bar(6.0), bar(9.0), bar(9.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 9.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_stop_limit_bracket_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XB", stop=5, limit=9)
if bar_index == 2
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(8.0), bar(6.0), bar(6.0), bar(9.0), bar(9.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XB");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 9.0);
    assert_eq!(strategy.orders[3].id, "XB");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 9.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XB");
    assert_eq!(strategy.trades[0].entry_price, 8.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XB");
    assert_eq!(strategy.trades[1].entry_price, 6.0);
    assert_eq!(strategy.trades[1].exit_price, 9.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 9.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_price_closes_all_open_entries() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XT", trail_price=2.5, trail_offset=50)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 3.0, 1.5, 2.0),
            bar_ohlc(3.0, 4.0, 2.8, 3.0),
            bar_ohlc(3.5, 4.0, 3.6, 3.8),
            bar_ohlc(3.5, 3.5, 3.5, 3.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 3.5);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 3.5);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 3.5);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.5);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 3.5);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 1.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_price_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XT", trail_price=2.5, trail_offset=50)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 3.0, 1.5, 2.0),
            bar_ohlc(3.0, 4.0, 2.8, 3.0),
            bar_ohlc(3.5, 4.0, 3.6, 3.8),
            bar_ohlc(3.5, 3.5, 3.5, 3.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 3.5);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 3.5);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 3.5);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.5);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 3.5);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 1.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_price_persists_for_later_entry() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XT", trail_price=4.5, trail_offset=50)
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 2.2, 1.8, 2.0),
            bar_ohlc(3.0, 3.2, 2.8, 3.0),
            bar_ohlc(4.5, 5.0, 4.7, 4.8),
            bar_ohlc(4.6, 4.6, 4.5, 4.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.5);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 4.5);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.5);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.5);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 4.5);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 4.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_price_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XT", trail_price=4.5, trail_offset=50)
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 2.2, 1.8, 2.0),
            bar_ohlc(3.0, 3.2, 2.8, 3.0),
            bar_ohlc(4.5, 5.0, 4.7, 4.8),
            bar_ohlc(4.6, 4.6, 4.5, 4.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.5);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 4.5);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.5);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.5);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 4.5);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 4.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_points_uses_each_open_entry_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XT", trail_points=100, trail_offset=50)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 3.0, 1.5, 2.0),
            bar_ohlc(3.0, 4.0, 2.8, 3.0),
            bar_ohlc(3.5, 4.0, 3.6, 3.8),
            bar_ohlc(3.5, 3.5, 3.5, 3.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 3.5);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 3.5);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 3.5);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.5);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 3.5);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 1.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_points_handles_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=3)
if bar_index == 2
    strategy.exit("XT", trail_points=100, trail_offset=50)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 3.0, 1.5, 2.0),
            bar_ohlc(3.0, 4.0, 2.8, 3.0),
            bar_ohlc(3.5, 4.0, 3.6, 3.8),
            bar_ohlc(3.5, 3.5, 3.5, 3.5),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 3.5);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 3.5);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 3.5);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 1.5);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 3.5);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 1.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_points_persists_for_later_entry() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XT", trail_points=100, trail_offset=50)
    strategy.entry("L2", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 2.2, 1.8, 2.0),
            bar_ohlc(3.0, 3.2, 2.8, 3.0),
            bar_ohlc(4.5, 4.5, 4.2, 4.5),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.0);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 4.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.trades[1].id, "L2");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 4.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 3.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_exit_omitted_from_entry_trail_points_persists_for_later_same_entry_id() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit", pyramiding=2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.exit("XT", trail_points=100, trail_offset=50)
    strategy.entry("L", strategy.long, qty=3)
plot(strategy.opentrades)
plot(strategy.position_size)
plot(strategy.closedtrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar_ohlc(2.0, 2.2, 1.8, 2.0),
            bar_ohlc(3.0, 3.2, 2.8, 3.0),
            bar_ohlc(4.5, 4.5, 4.2, 4.5),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.orders[2].id, "XT");
    assert_eq!(strategy.orders[2].direction, "strategy.exit");
    assert_eq!(strategy.orders[2].bar_index, 4);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 4.0);
    assert_eq!(strategy.orders[3].id, "XT");
    assert_eq!(strategy.orders[3].direction, "strategy.exit");
    assert_eq!(strategy.orders[3].bar_index, 4);
    assert_eq!(strategy.orders[3].qty, 3.0);
    assert_eq!(strategy.orders[3].price, 4.0);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XT");
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.trades[1].id, "L");
    assert_eq!(strategy.trades[1].exit_id, "XT");
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 4.0);
    assert_eq!(strategy.trades[1].qty, 3.0);
    assert_eq!(strategy.trades[1].profit, 3.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_entry_uses_fixed_default_qty_when_qty_is_absent() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry", default_qty_type=strategy.fixed, default_qty_value=3)
if bar_index == 0
    strategy.entry("D", strategy.long)
if bar_index == 1
    strategy.entry("E", strategy.long, qty=5)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .default_entry_qty(1000000.0, 2.0),
        Some(3.0)
    );

    let result =
        run_historical(&analysis.hir.expect("HIR"), &[bar(2.0), bar(4.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "D");
    assert_eq!(strategy.orders[0].qty, 3.0);
    assert_eq!(strategy.orders[0].price, 4.0);
    assert_eq!(strategy.position[0].size, 3.0);
    assert_eq!(strategy.equity[0].cash, 1000000.0);
    assert_eq!(strategy.equity[0].market_value, 0.0);
    assert_eq!(strategy.equity[1].cash, 999988.0);
    assert_eq!(strategy.equity[1].market_value, 12.0);
    assert_eq!(strategy.equity[1].equity, 1000000.0);
}

#[test]
fn strategy_entry_uses_builtin_default_qty_when_qty_is_absent() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry")
if bar_index == 0
    strategy.entry("D", strategy.long)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .default_entry_qty(100_000.0, 2.0),
        Some(1.0)
    );

    let result =
        run_historical(&analysis.hir.expect("HIR"), &[bar(2.0), bar(3.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "D");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 3.0);
}

#[test]
fn strategy_default_entry_qty_supports_direct_udf_named_and_history_reads() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("default qty helper", default_qty_type=strategy.fixed, default_qty_value=3)
identity(value) => value
if bar_index == 0
    strategy.entry("L", strategy.long, qty=10)
plot(strategy.default_entry_qty(close))
plot(identity(strategy.default_entry_qty(fill_price=close * 2)))
plot(strategy.default_entry_qty(close)[1])
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");

    let expected = vec![
        PineValue::Float(3.0),
        PineValue::Float(3.0),
        PineValue::Float(3.0),
        PineValue::Float(3.0),
    ];
    assert_eq!(result.plots[0].values, expected.clone());
    assert_eq!(result.plots[1].values, expected);
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
}

#[test]
fn strategy_currency_conversions_are_identity_for_explicit_symbol_currency() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("currency conversion usd", currency=currency.USD)
identity(value) => value
plot(strategy.account_currency == "USD" ? 1 : 0)
plot(identity(strategy.account_currency) == "USD" ? 1 : 0)
plot(strategy.convert_to_account(close))
plot(identity(strategy.convert_to_symbol(value=close * 2)))
plot(strategy.convert_to_account(7))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(3.0)])
        .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(1), PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(1), PineValue::Int(1), PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(1.0),
            PineValue::Float(2.0),
            PineValue::Float(3.0)
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Float(2.0),
            PineValue::Float(4.0),
            PineValue::Float(6.0)
        ]
    );
    assert_eq!(
        result.plots[4].values,
        vec![
            PineValue::Float(7.0),
            PineValue::Float(7.0),
            PineValue::Float(7.0)
        ]
    );
}

#[test]
fn strategy_currency_conversions_are_identity_in_default_currency() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("currency conversion", currency=currency.NONE)
identity(value) => value
plot(strategy.convert_to_account(close))
plot(identity(strategy.convert_to_symbol(value=close * 2)))
plot(strategy.convert_to_account(7))
plot(strategy.convert_to_symbol(close)[1])
plot(strategy.convert_to_account(float(na)))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(3.0)])
        .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(1.0),
            PineValue::Float(2.0),
            PineValue::Float(3.0)
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(2.0),
            PineValue::Float(4.0),
            PineValue::Float(6.0)
        ]
    );
    assert_eq!(result.plots[2].values, vec![PineValue::Float(7.0); 3]);
    assert_eq!(
        result.plots[3].values,
        vec![PineValue::Na, PineValue::Float(1.0), PineValue::Float(2.0)]
    );
    assert_eq!(result.plots[4].values, vec![PineValue::Na; 3]);
}

#[test]
fn strategy_default_entry_qty_reuses_cash_and_percent_of_equity_sizing() {
    let cash_source = SourceFile::new(
        "cash.pine",
        r#"strategy("cash helper", default_qty_type=strategy.cash, default_qty_value=100)
plot(strategy.default_entry_qty(close))
plot(strategy.default_entry_qty(bar_index == 0 ? na : 0))
"#,
    );
    let cash_analysis = analyze_source(&cash_source);
    assert!(
        cash_analysis.diagnostics.is_empty(),
        "{:?}",
        cash_analysis.diagnostics
    );
    let cash_result = run_historical(
        &cash_analysis.hir.expect("cash HIR"),
        &[bar(10.0), bar(20.0)],
    )
    .expect("cash runtime result");
    assert_eq!(
        cash_result.plots[0].values,
        vec![PineValue::Float(10.0), PineValue::Float(5.0)]
    );
    assert_eq!(
        cash_result.plots[1].values,
        vec![PineValue::Na, PineValue::Na]
    );

    let percent_source = SourceFile::new(
        "percent.pine",
        r#"strategy("percent helper", initial_capital=1000, default_qty_type=strategy.percent_of_equity, default_qty_value=25)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=100)
plot(strategy.default_entry_qty(10))
"#,
    );
    let percent_analysis = analyze_source(&percent_source);
    assert!(
        percent_analysis.diagnostics.is_empty(),
        "{:?}",
        percent_analysis.diagnostics
    );
    let percent_result = run_historical(
        &percent_analysis.hir.expect("percent HIR"),
        &[bar(10.0), bar(20.0), bar(10.0), bar(30.0)],
    )
    .expect("percent runtime result");
    assert_eq!(
        percent_result.plots[0].values,
        vec![
            PineValue::Float(25.0),
            PineValue::Float(25.0),
            PineValue::Na,
            PineValue::Float(50.0),
        ]
    );
}

#[test]
fn native_default_entry_qty_reserves_commission_and_uses_chart_quantity_grid() {
    for version in [5, 6] {
        let source = SourceFile::new(
            "native-default-entry-qty.pine",
            format!(
                "//@version={version}\nstrategy(\"Default entry qty native probe\", initial_capital=10000, default_qty_type=strategy.percent_of_equity, default_qty_value=25, commission_type=strategy.commission.percent, commission_value=0.05, precision=8)\nplot(strategy.default_entry_qty(100), title=\"Default Qty\")\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default()
            .with_quantity_precision(6)
            .expect("valid quantity grid");
        let environment = RequestEnvironment::default().for_chart(chart);
        let result = run_historical_with_request_environment(
            &analysis.hir.expect("HIR"),
            &[bar(100.0)],
            environment,
        )
        .expect("runtime result");
        assert_eq!(
            result.plots[0].values,
            vec![PineValue::Float(24.987506)],
            "v{version}"
        );
    }
}

#[test]
fn native_percent_sizing_rounds_cash_budget_before_contract_grid() {
    let cases = [
        (1157890.70824, 100, "short", 956317.0),
        (1157890.70849, 100, "short", 956317.0),
        (1157890.70851, 100, "short", 956318.0),
        (1157890.70999, 100, "short", 956318.0),
        (115788.10214, 100, "short", 95630.0),
        (115788.10216, 100, "short", 95631.0),
        (463152.40868, 25, "short", 95631.0),
        (115788.10216, 100, "long", 95631.0),
    ];
    let bars = [
        Bar {
            time: 1611770400000,
            open: 1.2118,
            high: 1.21293,
            low: 1.20932,
            close: 1.21078,
            volume: 71082.0,
        },
        Bar {
            time: 1611784800000,
            open: 1.21078,
            high: 1.21113,
            low: 1.20823,
            close: 1.21014,
            volume: 29064.0,
        },
    ];
    for (capital, percent, direction, expected) in cases {
        let source = SourceFile::new(
            "native-percent-budget.pine",
            format!(
                "//@version=4\nstrategy(\"native percent budget\", initial_capital={capital}, currency=currency.USD, default_qty_type=strategy.percent_of_equity, default_qty_value={percent})\nif time == 1611770400000\n    strategy.entry(\"E\", strategy.{direction})\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default().with_quantity_precision(0).unwrap();
        let result = run_historical_with_request_environment(
            &analysis.hir.unwrap(),
            &bars,
            RequestEnvironment::default().for_chart(chart),
        )
        .unwrap();
        let strategy = result.strategy.unwrap();
        assert_eq!(strategy.orders.len(), 1);
        assert_eq!(strategy.orders[0].qty, expected, "{capital} / {percent}%");
    }
    let source = SourceFile::new(
        "native-percent-budget-helper.pine",
        "//@version=5\nstrategy(\"native percent helper\", initial_capital=115788.10216, currency=currency.USD, default_qty_type=strategy.percent_of_equity, default_qty_value=100)\nplot(strategy.default_entry_qty(1.21078))\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let chart = ChartContext::default().with_quantity_precision(0).unwrap();
    let result = run_historical_with_request_environment(
        &analysis.hir.unwrap(),
        &bars,
        RequestEnvironment::default().for_chart(chart),
    )
    .unwrap();
    assert_eq!(result.plots[0].values, vec![PineValue::Float(95631.0); 2]);
}

#[test]
fn native_half_tick_marks_value_long_and_short_positions() {
    // FX:EURUSD 4h native exports, 2023-06-29/30. The final quotient is
    // just below a half tick even though multiplying by 100000 gives .5.
    let closes = [
        1.09143,
        1.0881150000000002,
        1.08708,
        1.08654,
        1.087225,
        1.086995,
    ];
    let short_profits = [
        0.0,
        3253.6968999999253,
        4276.006500000075,
        4806.82110000006,
        4128.557999999982,
        4364.4756,
    ];
    let bars: Vec<_> = closes
        .iter()
        .enumerate()
        .map(|(i, &close)| Bar {
            time: 1688014800000 + i as i64 * 14400000,
            open: if i == 0 { 1.08852 } else { closes[i - 1] },
            high: 1.095,
            low: 1.085,
            close,
            volume: 100.0,
        })
        .collect();
    for (direction, sign) in [("short", 1.0), ("long", -1.0)] {
        let source = SourceFile::new(
            "native-mark-price.pine",
            format!(
                r#"//@version=5
strategy("native mark", initial_capital=2000000, currency=currency.USD, margin_long=0, margin_short=0)
if time == 1688014800000
    strategy.entry("E", strategy.{direction}, qty=982990)
plot(close)
plot(strategy.equity)
plot(strategy.openprofit)
plot(strategy.opentrades.profit(0))
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default().with_price_grid(1, 100000).unwrap();
        let result = run_historical_with_request_environment(
            &analysis.hir.unwrap(),
            &bars,
            RequestEnvironment::default().for_chart(chart),
        )
        .unwrap();
        assert_values_close(&result.plots[0].values, &closes);
        for (i, &profit) in short_profits.iter().enumerate() {
            for (plot, expected) in [
                (1, 2000000.0 + sign * profit),
                (2, sign * profit),
                (3, sign * profit),
            ] {
                if i == 0 && plot == 3 {
                    continue;
                }
                let actual = result.plots[plot].values[i].as_f64().unwrap();
                assert!(
                    (actual - expected).abs() < 1e-7,
                    "{direction}, bar {i}, plot {plot}: {actual} != {expected}"
                );
            }
        }
        let strategy = result.strategy.unwrap();
        for (i, snapshot) in strategy.equity.iter().enumerate() {
            assert!((snapshot.equity - (2000000.0 + sign * short_profits[i])).abs() < 1e-7);
        }
    }
}

#[test]
fn native_open_trade_profit_percent_marks_direction_and_absent_indices() {
    let closes = [
        1.09143,
        1.0881150000000002,
        1.08708,
        1.08654,
        1.087225,
        1.086995,
        1.08388,
        1.0897,
    ];
    let native_short_percent = [
        0.0,
        0.3032718543561989,
        0.3985596877491122,
        0.44803606278002794,
        0.3848162502405065,
        0.40680575025425403,
        0.0,
        0.0,
    ];
    let bars: Vec<_> = closes
        .iter()
        .enumerate()
        .map(|(i, &close)| Bar {
            time: 1688014800000 + i as i64 * 14400000,
            open: if i == 0 { 1.08852 } else { closes[i - 1] },
            high: 1.095,
            low: 1.08,
            close,
            volume: 100.0,
        })
        .collect();
    for (direction, sign) in [("short", 1.0), ("long", -1.0)] {
        let source = SourceFile::new(
            "native-profit-percent.pine",
            format!(
                r#"//@version=5
strategy("native profit percent", initial_capital=2000000, currency=currency.USD, margin_long=0, margin_short=0)
if time == 1688014800000
    strategy.entry("E", strategy.{direction}, qty=982990)
if time == 1688086800000
    strategy.close("E")
plot(strategy.opentrades.profit_percent(0))
plot(strategy.opentrades.profit(0))
plot(strategy.opentrades.profit(1))
plot(strategy.opentrades.profit(-1))
plot(strategy.opentrades.profit_percent(1))
plot(strategy.opentrades.profit_percent(-1))
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default().with_price_grid(1, 100000).unwrap();
        let result = run_historical_with_request_environment(
            &analysis.hir.unwrap(),
            &bars,
            RequestEnvironment::default().for_chart(chart),
        )
        .unwrap();
        let expected: Vec<_> = native_short_percent.iter().map(|p| p * sign).collect();
        assert_values_close(&result.plots[0].values, &expected);
        for plot in &result.plots[2..] {
            assert_eq!(plot.values, vec![PineValue::Float(0.0); 8]);
        }
        for i in [0, 6, 7] {
            assert_eq!(result.plots[1].values[i], PineValue::Float(0.0));
        }
        let trades = result.strategy.unwrap().trades;
        assert_eq!(trades.len(), 1);
        assert!((trades[0].exit_price - 1.08699).abs() < 1e-12);
    }
}

#[test]
fn native_cash_and_fixed_default_entry_qty_use_chart_quantity_grid() {
    for (version, qty_type, value, fill_price, expected) in [
        (5, "cash", 100.0, 9.87, 10.131712),
        (6, "cash", 100.0, 9.87, 10.131712),
        (5, "fixed", 7.123456789, 10.0, 7.123456),
        (6, "fixed", 7.123456789, 10.0, 7.123456),
    ] {
        let source = SourceFile::new(
            "native-default-entry-qty.pine",
            format!(
                "//@version={version}\nstrategy(\"Default entry qty native probe\", default_qty_type=strategy.{qty_type}, default_qty_value={value}, commission_type=strategy.commission.percent, commission_value=0.05, precision=9)\nplot(strategy.default_entry_qty({fill_price}), title=\"Default Qty\")\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version} {qty_type}: {:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default()
            .with_quantity_precision(6)
            .expect("valid quantity grid");
        let environment = RequestEnvironment::default().for_chart(chart);
        let result = run_historical_with_request_environment(
            &analysis.hir.expect("HIR"),
            &[bar(100.0)],
            environment,
        )
        .expect("runtime result");
        assert_eq!(
            result.plots[0].values,
            vec![PineValue::Float(expected)],
            "v{version} {qty_type}"
        );
    }
}

#[test]
fn native_percent_equity_default_qty_reserves_fixed_commission_types() {
    for (version, commission_type, price, expected) in [
        (5, "cash_per_contract", 100.0, 24.752475),
        (6, "cash_per_contract", 100.0, 24.752475),
        (5, "cash_per_order", 100.0, 24.99),
        (6, "cash_per_order", 100.0, 24.99),
        (6, "cash_per_contract", 370.0, 6.738544),
        (6, "cash_per_order", 370.0, 6.754054),
    ] {
        let source = SourceFile::new(
            "native-fixed-commission-qty.pine",
            format!(
                "//@version={version}\nstrategy(\"Fixed fee qty probe\", initial_capital=10000, default_qty_type=strategy.percent_of_equity, default_qty_value=25, commission_type=strategy.commission.{commission_type}, commission_value=1, precision=8, process_orders_on_close=true)\nplot(strategy.default_entry_qty({price}), title=\"Default Qty\")\nif bar_index == 0\n    strategy.entry(\"L\", strategy.long)\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version} {commission_type}: {:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default()
            .with_quantity_precision(6)
            .expect("valid quantity grid");
        let environment = RequestEnvironment::default().for_chart(chart);
        let result = run_historical_with_request_environment(
            &analysis.hir.expect("HIR"),
            &[bar(price)],
            environment,
        )
        .expect("runtime result");
        assert_eq!(
            result.plots[0].values,
            vec![PineValue::Float(expected)],
            "v{version} {commission_type}"
        );
        assert_eq!(
            result.strategy.expect("strategy output").orders[0].qty,
            expected,
            "v{version} {commission_type}"
        );
    }
}

#[test]
fn strategy_entry_uses_percent_of_equity_default_qty_when_qty_is_absent() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry", initial_capital=1000, default_qty_type=strategy.percent_of_equity, default_qty_value=25)
if bar_index == 0
    strategy.entry("D", strategy.long)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .default_entry_qty(1000.0, 10.0),
        Some(25.0)
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(10.0), bar(20.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "D");
    assert_eq!(strategy.orders[0].qty, 25.0);
    assert_eq!(strategy.orders[0].price, 20.0);
    assert_eq!(strategy.position[0].size, 25.0);
    assert_eq!(strategy.equity[1].cash, 500.0);
    assert_eq!(strategy.equity[1].market_value, 500.0);
    assert_eq!(strategy.equity[1].equity, 1000.0);
}

#[test]
fn cash_default_entry_uses_chart_quantity_grid_for_the_order() {
    let source = SourceFile::new(
        "cash-default-grid.pine",
        "//@version=6\nstrategy(\"cash grid\", initial_capital=1000, default_qty_type=strategy.cash, default_qty_value=1000, process_orders_on_close=true)\nplot(strategy.default_entry_qty(close))\nif bar_index == 0\n    strategy.entry(\"L\", strategy.long)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let chart = ChartContext::default()
        .with_quantity_precision(0)
        .expect("integer contract grid");
    let result = run_historical_with_request_environment(
        &analysis.hir.expect("HIR"),
        &[bar(3.0)],
        RequestEnvironment::default().for_chart(chart),
    )
    .expect("cash order result");
    assert_eq!(result.plots[0].values, vec![PineValue::Float(333.0)]);
    assert_eq!(
        result.strategy.expect("strategy output").orders[0].qty,
        333.0
    );
}

#[test]
fn native_percent_equity_order_reserves_commission_and_uses_chart_quantity_grid() {
    for command in ["entry", "order"] {
        let source = SourceFile::new(
            "native-percent-quantity.pine",
            format!(
                "//@version=6\nstrategy(\"native quantity\", initial_capital=10000, default_qty_type=strategy.percent_of_equity, default_qty_value=25, commission_type=strategy.commission.percent, commission_value=0.05, slippage=2, process_orders_on_close=true)\nif bar_index == 0\n    strategy.{command}(\"L\", strategy.long)\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let program = analysis.hir.expect("HIR");
        let chart = ChartContext::default()
            .with_quantity_precision(5)
            .expect("valid quantity grid");
        let environment = RequestEnvironment::default().for_chart(chart);
        let result =
            run_historical_with_request_environment(&program, &[bar(46979.35)], environment)
                .expect("runtime result");
        let order = &result.strategy.expect("strategy output").orders[0];
        assert_eq!(order.qty, 0.05318, "{command}");
        assert!((order.price - 46979.37).abs() < 1e-8, "{command}");
    }
}

#[test]
fn strategy_entry_uses_cash_default_qty_when_qty_is_absent() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry", initial_capital=1000, default_qty_type=strategy.cash, default_qty_value=100)
if bar_index == 0
    strategy.entry("D", strategy.long)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .default_entry_qty(1000.0, 10.0),
        Some(10.0)
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(10.0), bar(20.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "D");
    assert_eq!(strategy.orders[0].qty, 10.0);
    assert_eq!(strategy.orders[0].price, 20.0);
    assert_eq!(strategy.position[0].size, 10.0);
    assert_eq!(strategy.equity[1].cash, 800.0);
    assert_eq!(strategy.equity[1].market_value, 200.0);
    assert_eq!(strategy.equity[1].equity, 1000.0);
}

#[test]
fn cash_market_order_sizing_includes_slippage_at_placement() {
    for command in ["entry", "order"] {
        for (direction, expected_price, fill_price) in [
            ("strategy.long", 10.01, 20.01),
            ("strategy.short", 9.99, 19.99),
        ] {
            let source = SourceFile::new(
                "cash_slippage.pine",
                format!(
                    "strategy(\"cash slippage\", initial_capital=1000, default_qty_type=strategy.cash, default_qty_value=100, slippage=1)\nif bar_index == 0\n    strategy.{command}(\"D\", {direction})\n"
                ),
            );
            let analysis = analyze_source(&source);
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            let result = run_historical(&analysis.hir.expect("HIR"), &[bar(10.0), bar(20.0)])
                .expect("runtime result");
            let order = &result.strategy.expect("strategy output").orders[0];
            assert!((order.qty - 100.0 / expected_price).abs() < 1e-10);
            assert!((order.price - fill_price).abs() < 1e-10);
        }
    }
}

#[test]
fn strategy_limit_entry_uses_cash_default_qty_at_placement_close() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry", initial_capital=1000, default_qty_type=strategy.cash, default_qty_value=100)
if bar_index == 0
    strategy.entry("D", strategy.long, limit=20)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(10.0), bar(20.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "D");
    assert_eq!(strategy.orders[0].qty, 10.0);
    assert_eq!(strategy.orders[0].price, 20.0);
}

#[test]
fn strategy_entry_explicit_qty_overrides_fixed_default_qty() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("entry", default_qty_type=strategy.fixed, default_qty_value=3)
if bar_index == 0
    strategy.entry("E", strategy.long, qty=5)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result =
        run_historical(&analysis.hir.expect("HIR"), &[bar(2.0), bar(3.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "E");
    assert_eq!(strategy.orders[0].qty, 5.0);
    assert_eq!(strategy.position[0].size, 5.0);
    assert_eq!(strategy.equity[1].cash, 999985.0);
}

#[test]
fn strategy_close_records_closed_trade_and_flat_position() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("close")
if bar_index == 1
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close("L")
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
        Bar {
            time: 40,
            open: 4.0,
            high: 4.0,
            low: 4.0,
            close: 4.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].entry_bar_index, 2);
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].entry_time, 30);
    assert_eq!(strategy.trades[0].exit_time, 40);
    assert_eq!(strategy.trades[0].entry_price, 3.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 2.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(strategy.equity[2].cash, 999994.0);
    assert_eq!(strategy.equity[3].cash, 1000002.0);
    assert_eq!(strategy.equity[3].market_value, 0.0);
    assert_eq!(strategy.equity[3].equity, 1000002.0);
}

#[test]
fn strategy_close_immediately_fills_on_signal_bar_close() {
    let source = SourceFile::new(
        "strategy_close_immediately.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_close_immediately.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert_eq!(strategy.trades[0].exit_price, 3.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 0.0]);
}

#[test]
fn strategy_close_immediately_false_fills_next_bar_open() {
    let source = SourceFile::new(
        "strategy_close_immediately_false.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_close_immediately_false.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 0.0]);
}

#[test]
fn strategy_close_immediately_pyramiding_closes_matching_id_on_signal_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("close immediately pyramiding", pyramiding=2)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("L2", strategy.long, qty=3)
if bar_index == 2
    strategy.close("L1", immediately=true)
plot(strategy.opentrades)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L1");
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(3.0)
    );
    assert_values_close(&result.plots[0].values, &[0.0, 1.0, 2.0, 1.0]);
    assert_values_close(&result.plots[1].values, &[0.0, 1.0, 4.0, 3.0]);
}

#[test]
fn strategy_process_orders_on_close_fills_market_entry_at_signal_bar_close() {
    let source = SourceFile::new(
        "strategy_process_orders_on_close.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_process_orders_on_close.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders[0].bar_index, 0);
    assert_eq!(strategy.orders[0].price, 1.0);
    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 2.0]);
}

#[test]
fn strategy_process_orders_on_close_fills_close_after_script_at_bar_close() {
    let source = SourceFile::new(
        "strategy_process_orders_on_close_close.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_process_orders_on_close_close.pine"
        ),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert_eq!(strategy.trades[0].exit_price, 3.0);
    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 0.0]);
}

#[test]
fn strategy_immediately_close_fills_after_script_when_process_orders_on_close() {
    let source = SourceFile::new(
        "strategy_process_orders_on_close_immediately.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_process_orders_on_close_immediately.pine"
        ),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert_eq!(strategy.trades[0].exit_price, 3.0);
    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 0.0]);
}

#[test]
fn strategy_process_orders_on_close_fills_market_generic_order_at_bar_close() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("poc order", process_orders_on_close=true)
if bar_index == 0
    strategy.order("O", strategy.long, qty=2)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(3.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders[0].id, "O");
    assert_eq!(strategy.orders[0].bar_index, 0);
    assert_eq!(strategy.orders[0].price, 1.0);
    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0]);
}

#[test]
fn strategy_process_orders_on_close_fills_close_all_after_script() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("poc close_all", process_orders_on_close=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close_all()
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert_eq!(strategy.trades[0].exit_price, 3.0);
    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 0.0]);
}

#[test]
fn strategy_close_all_records_closed_trade_and_flat_position() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("close_all")
if bar_index == 0
    strategy.close_all()
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close_all()
if bar_index == 3
    strategy.close_all()
plot(strategy.position_size)
plot(strategy.closedtrades)
plot(strategy.opentrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 0.0]);
    assert_values_close(&result.plots[1].values, &[0.0, 0.0, 0.0, 1.0]);
    assert_values_close(&result.plots[2].values, &[0.0, 1.0, 1.0, 0.0]);
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "L");
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].qty, 2.0);
    assert_eq!(strategy.trades[0].profit, 4.0);
    assert_eq!(
        strategy.position.last().map(|snapshot| snapshot.size),
        Some(0.0)
    );
    assert_eq!(strategy.equity[3].cash, 1000004.0);
    assert_eq!(strategy.equity[3].market_value, 0.0);
    assert_eq!(strategy.equity[3].equity, 1000004.0);
    assert_eq!(strategy.equity[3].net_profit, 4.0);
}

#[test]
fn strategy_close_all_cancels_pending_exit_before_evaluation() {
    let source = SourceFile::new(
        "strategy_close_all_exit.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_close_all_exit.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].profit, 4.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_close_cancels_pending_limit_exit_fixture() {
    let source = SourceFile::new(
        "strategy_close_exit.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_close_exit.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "Close entry(s) order L");
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "L");
    assert_eq!(strategy.trades[0].exit_bar_index, 3);
    assert_eq!(strategy.trades[0].exit_price, 4.0);
    assert_eq!(strategy.trades[0].profit, 4.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_stop_stages_pending_exit_without_public_fill() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
strategy.entry("L", strategy.long, qty=1)
strategy.exit("XL", "L", stop=low)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(2.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 0);
    assert!(strategy.trades.is_empty());
    assert_eq!(strategy.position.len(), 0);
    assert_eq!(strategy.equity.len(), 1);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_stop_without_matching_entry_is_noop() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
strategy.exit("XL", "L", stop=low)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(2.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.trades.is_empty());
    assert!(strategy.position.is_empty());
    assert_eq!(strategy.equity.len(), 1);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_trailing_hir_without_matching_entry_is_noop() {
    let source = SourceFile::new(
        "strategy_exit_trailing_dispatch_hir.pine",
        r#"strategy("exit")
strategy.exit("XT", "L", stop=95)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let mut hir = analysis.hir.expect("HIR");
    let args = strategy_exit_args_mut(&mut hir);
    let stop_arg = args.get_mut(2).expect("stop arg");
    stop_arg.name = Some("trail_price".to_owned());
    args.push(const_float_arg("trail_offset", 50.0));

    let result = run_historical(&hir, &[bar(100.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.trades.is_empty());
    assert!(strategy.position.is_empty());
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_malformed_trailing_hir_is_guarded_before_fixed_exit() {
    let source = SourceFile::new(
        "strategy_exit_trailing_guard_hir.pine",
        r#"strategy("exit")
strategy.exit("XT", "L", stop=95)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let mut hir = analysis.hir.expect("HIR");
    let args = strategy_exit_args_mut(&mut hir);
    args.push(const_float_arg("trail_price", 100.0));
    args.push(const_float_arg("trail_offset", 50.0));

    let result = run_historical(&hir, &[bar(100.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.orders.is_empty());
    assert!(strategy.trades.is_empty());
    assert!(strategy.position.is_empty());
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_stop_fills_on_later_low_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("XL", "L", stop=9)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 10.0,
            high: 10.0,
            low: 8.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 11.0,
            high: 12.0,
            low: 8.0,
            close: 11.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XL");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 9.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].entry_bar_index, 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 1);
    assert_eq!(strategy.trades[0].entry_price, 11.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].qty, 2.0);
    assert_eq!(strategy.trades[0].profit, -4.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[1].size, 0.0);
    assert_eq!(strategy.equity[1].cash, 999996.0);
    assert_eq!(strategy.equity[1].market_value, 0.0);
    assert_eq!(strategy.equity[1].net_profit, -4.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_stop_short_covers_on_later_high_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit short")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2)
    strategy.exit("XS", "S", stop=12)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 11.0,
            high: 11.0,
            low: 11.0,
            close: 11.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 12.0,
            high: 13.0,
            low: 12.0,
            close: 12.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XS");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "S");
    assert_eq!(strategy.trades[0].qty, -2.0);
    assert_eq!(strategy.trades[0].entry_price, 11.0);
    assert_eq!(strategy.trades[0].exit_price, 12.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_qty_single_trigger_forms_dispatch_partial_quantity() {
    for (name, exit_call, high, low, expected_price) in [
        (
            "stop",
            r#"strategy.exit("XQ", "L", stop=95, qty=0.75)"#,
            100.0,
            94.0,
            95.0,
        ),
        (
            "limit",
            r#"strategy.exit("XQ", "L", limit=110, qty=0.75)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "profit",
            r#"strategy.exit("XQ", "L", profit=1000, qty=0.75)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "loss",
            r#"strategy.exit("XQ", "L", loss=500, qty=0.75)"#,
            100.0,
            94.0,
            95.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_exit_qty_{name}.pine"),
            format!(
                r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    {exit_call}
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name}: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(
            &analysis.hir.expect("HIR"),
            &[
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, high, low, 100.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 2, "{name}");
        assert_eq!(strategy.orders[1].id, "XQ", "{name}");
        assert_eq!(strategy.orders[1].qty, 0.75, "{name}");
        assert_eq!(strategy.orders[1].price, expected_price, "{name}");
        assert_eq!(strategy.trades.len(), 1, "{name}");
        assert_eq!(strategy.trades[0].qty, 0.75, "{name}");
        assert_eq!(strategy.trades[0].exit_price, expected_price, "{name}");
        assert_eq!(strategy.position[1].size, 1.25, "{name}");
        assert_eq!(strategy.position[1].avg_price, Some(100.0), "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_exit_qty_percent_single_trigger_forms_dispatch_partial_quantity() {
    for (name, exit_call, high, low, expected_price) in [
        (
            "stop",
            r#"strategy.exit("XP", "L", stop=95, qty_percent=37.5)"#,
            100.0,
            94.0,
            95.0,
        ),
        (
            "limit",
            r#"strategy.exit("XP", "L", limit=110, qty_percent=37.5)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "profit",
            r#"strategy.exit("XP", "L", profit=1000, qty_percent=37.5)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "loss",
            r#"strategy.exit("XP", "L", loss=500, qty_percent=37.5)"#,
            100.0,
            94.0,
            95.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_exit_qty_percent_{name}.pine"),
            format!(
                r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    {exit_call}
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name}: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(
            &analysis.hir.expect("HIR"),
            &[
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, high, low, 100.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 2, "{name}");
        assert_eq!(strategy.orders[1].id, "XP", "{name}");
        assert_eq!(strategy.orders[1].qty, 0.75, "{name}");
        assert_eq!(strategy.orders[1].price, expected_price, "{name}");
        assert_eq!(strategy.trades.len(), 1, "{name}");
        assert_eq!(strategy.trades[0].qty, 0.75, "{name}");
        assert_eq!(strategy.position[1].size, 1.25, "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_exit_qty_and_qty_percent_single_trigger_forms_use_qty() {
    for (name, exit_call, high, low, expected_price) in [
        (
            "stop",
            r#"strategy.exit("XQP", "L", stop=95, qty=0.75, qty_percent=25)"#,
            100.0,
            94.0,
            95.0,
        ),
        (
            "limit",
            r#"strategy.exit("XQP", "L", limit=110, qty=0.75, qty_percent=25)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "profit",
            r#"strategy.exit("XQP", "L", profit=1000, qty=0.75, qty_percent=25)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "loss",
            r#"strategy.exit("XQP", "L", loss=500, qty=0.75, qty_percent=25)"#,
            100.0,
            94.0,
            95.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_exit_qty_and_qty_percent_{name}.pine"),
            format!(
                r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    {exit_call}
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name}: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(
            &analysis.hir.expect("HIR"),
            &[
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, high, low, 100.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 2, "{name}");
        assert_eq!(strategy.orders[1].id, "XQP", "{name}");
        assert_eq!(strategy.orders[1].qty, 0.75, "{name}");
        assert_eq!(strategy.orders[1].price, expected_price, "{name}");
        assert_eq!(strategy.trades.len(), 1, "{name}");
        assert_eq!(strategy.trades[0].qty, 0.75, "{name}");
        assert_eq!(strategy.position[1].size, 1.25, "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_exit_stop_not_reached_keeps_position_open() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", stop=9)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 11.0,
            high: 12.0,
            low: 10.0,
            close: 11.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert!(strategy.trades.is_empty());
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.equity[1].market_value, 11.0);
    assert_eq!(strategy.equity[1].net_profit, 0.0);
}

#[test]
fn strategy_exit_stop_replacement_uses_updated_stop_on_later_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", stop=8)
if bar_index == 1
    strategy.exit("XL", "L", stop=9)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 10.0,
            high: 10.0,
            low: 9.5,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 10.0,
            high: 10.0,
            low: 8.0,
            close: 10.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.orders[1].price, 9.0);
}

#[test]
fn strategy_close_cancels_pending_stop_before_evaluation() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", stop=9)
if bar_index == 1
    strategy.close("L")
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 11.0,
            high: 11.0,
            low: 11.0,
            close: 11.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 11.0,
            high: 11.0,
            low: 8.0,
            close: 11.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "Close entry(s) order L");
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_price, 11.0);
    assert_eq!(strategy.trades[0].profit, 0.0);
}

#[test]
fn strategy_exit_limit_fills_on_later_high_crossing_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("XL", "L", limit=12)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 10.0,
            high: 12.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 11.0,
            high: 12.0,
            low: 10.0,
            close: 11.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XL");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XL");
    assert_eq!(strategy.trades[0].exit_price, 12.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.position[1].size, 0.0);
    assert_eq!(strategy.equity[1].cash, 1000002.0);
}

#[test]
fn strategy_exit_limit_not_reached_keeps_position_open() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", limit=12)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(11.0, 11.0, 10.0, 11.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert!(strategy.trades.is_empty());
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.equity[1].market_value, 11.0);
}

#[test]
fn strategy_exit_limit_replacement_uses_updated_limit_on_later_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", limit=12)
if bar_index == 1
    strategy.exit("XL", "L", limit=11)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 11.0, 10.0, 10.0),
            bar_ohlc(10.0, 11.0, 10.0, 10.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert_eq!(strategy.trades[0].exit_price, 11.0);
}

#[test]
fn strategy_exit_profit_ticks_fill_through_converted_limit() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.exit("XP", "L", profit=200)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 12.0, 10.0, 11.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XP");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_price, 12.0);
    assert_eq!(strategy.trades[0].profit, 4.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_loss_ticks_fill_through_converted_stop() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.exit("XL", "L", loss=100)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 9.0, 10.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 9.0, 10.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XL");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].price, 9.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_profit_ticks_short_cover_below_entry() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit short profit")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2)
if bar_index == 1
    strategy.exit("XP", "S", profit=100)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 9.0, 10.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XP");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].price, 9.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "S");
    assert_eq!(strategy.trades[0].qty, -2.0);
    assert_eq!(strategy.trades[0].exit_price, 9.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_loss_ticks_short_cover_above_entry() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit short loss")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2)
if bar_index == 1
    strategy.exit("XL", "S", loss=100)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 11.0, 10.0, 10.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XL");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].direction, "strategy.exit");
    assert_eq!(strategy.orders[1].price, 11.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "S");
    assert_eq!(strategy.trades[0].qty, -2.0);
    assert_eq!(strategy.trades[0].exit_price, 11.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_bracket_stop_limit_short_stop_covers_on_later_high() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit short bracket")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2)
    strategy.exit("XB", "S", stop=12, limit=1)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(11.0, 11.0, 11.0, 11.0),
            bar_ohlc(12.0, 13.0, 12.0, 12.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XB");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.trades[0].qty, -2.0);
    assert_eq!(strategy.trades[0].exit_price, 12.0);
    assert_eq!(strategy.trades[0].profit, -2.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_trail_price_short_activates_then_covers() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit short trail")
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2)
    strategy.exit("XT", "S", trail_price=9.5, trail_offset=50)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(9.5, 9.6, 9.0, 9.2),
            bar_ohlc(9.2, 9.6, 9.1, 9.3),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "XT");
    assert_eq!(strategy.orders[1].bar_index, 3);
    assert_eq!(strategy.orders[1].price, 9.5);
    assert_eq!(strategy.trades[0].qty, -2.0);
    assert_eq!(strategy.trades[0].exit_price, 9.5);
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_bracket_forms_dispatch_to_bracket_pending_exit() {
    for (name, exit_call, high, low, expected_price) in [
        (
            "stop_limit",
            r#"strategy.exit("XB", "L", stop=95, limit=110)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "stop_profit",
            r#"strategy.exit("XB", "L", stop=95, profit=1000)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "loss_limit",
            r#"strategy.exit("XB", "L", loss=500, limit=110)"#,
            111.0,
            94.0,
            95.0,
        ),
        (
            "loss_profit",
            r#"strategy.exit("XB", "L", loss=500, profit=1000)"#,
            111.0,
            94.0,
            95.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_exit_bracket_{name}.pine"),
            format!(
                r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    {exit_call}
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name}: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(
            &analysis.hir.expect("HIR"),
            &[
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, high, low, 100.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 2, "{name}");
        assert_eq!(strategy.orders[1].id, "XB", "{name}");
        assert_eq!(strategy.orders[1].bar_index, 2, "{name}");
        assert_eq!(strategy.orders[1].direction, "strategy.exit", "{name}");
        assert_eq!(strategy.orders[1].price, expected_price, "{name}");
        assert_eq!(strategy.trades.len(), 1, "{name}");
        assert_eq!(strategy.trades[0].id, "L", "{name}");
        assert_eq!(strategy.trades[0].exit_price, expected_price, "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_exit_qty_bracket_forms_dispatch_partial_quantity() {
    for (name, exit_call, high, low, expected_price) in [
        (
            "stop_limit",
            r#"strategy.exit("BQ", "L", stop=95, limit=110, qty=0.5)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "stop_profit",
            r#"strategy.exit("BQ", "L", stop=95, profit=1000, qty=0.5)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "loss_limit",
            r#"strategy.exit("BQ", "L", loss=500, limit=110, qty=0.5)"#,
            111.0,
            94.0,
            95.0,
        ),
        (
            "loss_profit",
            r#"strategy.exit("BQ", "L", loss=500, profit=1000, qty=0.5)"#,
            111.0,
            94.0,
            95.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_exit_qty_bracket_{name}.pine"),
            format!(
                r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    {exit_call}
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name}: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(
            &analysis.hir.expect("HIR"),
            &[
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, high, low, 100.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 2, "{name}");
        assert_eq!(strategy.orders[1].id, "BQ", "{name}");
        assert_eq!(strategy.orders[1].qty, 0.5, "{name}");
        assert_eq!(strategy.orders[1].price, expected_price, "{name}");
        assert_eq!(strategy.trades.len(), 1, "{name}");
        assert_eq!(strategy.trades[0].qty, 0.5, "{name}");
        assert_eq!(strategy.trades[0].exit_price, expected_price, "{name}");
        assert_eq!(strategy.position[1].size, 1.5, "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_exit_qty_percent_bracket_forms_dispatch_partial_quantity() {
    for (name, exit_call, high, low, expected_price) in [
        (
            "stop_limit",
            r#"strategy.exit("BP", "L", stop=95, limit=110, qty_percent=25)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "stop_profit",
            r#"strategy.exit("BP", "L", stop=95, profit=1000, qty_percent=25)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "loss_limit",
            r#"strategy.exit("BP", "L", loss=500, limit=110, qty_percent=25)"#,
            111.0,
            94.0,
            95.0,
        ),
        (
            "loss_profit",
            r#"strategy.exit("BP", "L", loss=500, profit=1000, qty_percent=25)"#,
            111.0,
            94.0,
            95.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_exit_qty_percent_bracket_{name}.pine"),
            format!(
                r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    {exit_call}
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name}: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(
            &analysis.hir.expect("HIR"),
            &[
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, high, low, 100.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 2, "{name}");
        assert_eq!(strategy.orders[1].id, "BP", "{name}");
        assert_eq!(strategy.orders[1].qty, 0.5, "{name}");
        assert_eq!(strategy.orders[1].price, expected_price, "{name}");
        assert_eq!(strategy.trades.len(), 1, "{name}");
        assert_eq!(strategy.trades[0].qty, 0.5, "{name}");
        assert_eq!(strategy.position[1].size, 1.5, "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_exit_qty_and_qty_percent_bracket_forms_use_qty() {
    for (name, exit_call, high, low, expected_price) in [
        (
            "stop_limit",
            r#"strategy.exit("BQP", "L", stop=95, limit=110, qty=0.75, qty_percent=25)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "stop_profit",
            r#"strategy.exit("BQP", "L", stop=95, profit=1000, qty=0.75, qty_percent=25)"#,
            111.0,
            100.0,
            110.0,
        ),
        (
            "loss_limit",
            r#"strategy.exit("BQP", "L", loss=500, limit=110, qty=0.75, qty_percent=25)"#,
            111.0,
            94.0,
            95.0,
        ),
        (
            "loss_profit",
            r#"strategy.exit("BQP", "L", loss=500, profit=1000, qty=0.75, qty_percent=25)"#,
            111.0,
            94.0,
            95.0,
        ),
    ] {
        let source = SourceFile::new(
            format!("strategy_exit_qty_and_qty_percent_bracket_{name}.pine"),
            format!(
                r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    {exit_call}
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{name}: {:?}",
            analysis.diagnostics
        );

        let result = run_historical(
            &analysis.hir.expect("HIR"),
            &[
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, 100.0, 100.0, 100.0),
                bar_ohlc(100.0, high, low, 100.0),
            ],
        )
        .expect("runtime result");
        let strategy = result.strategy.expect("strategy output");

        assert_eq!(strategy.orders.len(), 2, "{name}");
        assert_eq!(strategy.orders[1].id, "BQP", "{name}");
        assert_eq!(strategy.orders[1].qty, 0.75, "{name}");
        assert_eq!(strategy.orders[1].price, expected_price, "{name}");
        assert_eq!(strategy.trades.len(), 1, "{name}");
        assert_eq!(strategy.trades[0].qty, 0.75, "{name}");
        assert_eq!(strategy.position[1].size, 1.25, "{name}");
        assert!(strategy.diagnostics.is_empty(), "{name}");
    }
}

#[test]
fn strategy_exit_qty_trailing_dispatches_partial_quantity() {
    let source = SourceFile::new(
        "strategy_exit_qty_trailing.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.exit("TQ", "L", trail_points=100, trail_offset=50, qty=0.5)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(100.0, 100.0, 100.0, 100.0),
            bar_ohlc(100.0, 102.0, 101.75, 102.0),
            bar_ohlc(102.0, 102.0, 101.0, 101.25),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "TQ");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 0.5);
    assert_eq!(strategy.orders[1].price, 101.5);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].qty, 0.5);
    assert_eq!(strategy.trades[0].exit_price, 101.5);
    assert_eq!(strategy.position[1].size, 1.5);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_qty_percent_trailing_dispatches_partial_quantity() {
    let source = SourceFile::new(
        "strategy_exit_qty_percent_trailing.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.exit("TP", "L", trail_points=100, trail_offset=50, qty_percent=25)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(100.0, 100.0, 100.0, 100.0),
            bar_ohlc(100.0, 102.0, 101.75, 102.0),
            bar_ohlc(102.0, 102.0, 101.0, 101.25),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "TP");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 0.5);
    assert_eq!(strategy.orders[1].price, 101.5);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].qty, 0.5);
    assert_eq!(strategy.position[1].size, 1.5);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_qty_and_qty_percent_trailing_uses_qty() {
    let source = SourceFile::new(
        "strategy_exit_qty_and_qty_percent_trailing.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.exit("TQP", "L", trail_points=100, trail_offset=50, qty=0.75, qty_percent=25)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(100.0, 100.0, 100.0, 100.0),
            bar_ohlc(100.0, 102.0, 101.75, 102.0),
            bar_ohlc(102.0, 102.0, 101.0, 101.25),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "TQP");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 0.75);
    assert_eq!(strategy.orders[1].price, 101.5);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].qty, 0.75);
    assert_eq!(strategy.trades[0].exit_price, 101.5);
    assert_eq!(strategy.position[1].size, 1.25);
    assert!(strategy.diagnostics.is_empty());
}

#[test]
fn strategy_exit_bracket_invalid_downside_price_preempts_upside_tick_diagnostic() {
    let source = SourceFile::new(
        "strategy_exit_bracket_invalid_order.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.exit("XB", "L", stop=close / (close - close), profit=0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(100.0), bar(100.0)])
        .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.trades.is_empty());
    assert_eq!(strategy.diagnostics.len(), 1);
    assert_eq!(strategy.diagnostics[0].code, "E_STRATEGY_EXIT_PRICE");
}

#[test]
fn strategy_exit_invalid_bracket_preserves_existing_pending_exit() {
    let source = SourceFile::new(
        "strategy_exit_bracket_invalid_preserves_pending.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("KEEP", "L", limit=120)
if bar_index == 1
    strategy.exit("BAD", "L", stop=95, profit=-1)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(100.0, 100.0, 100.0, 100.0),
            bar_ohlc(100.0, 110.0, 100.0, 100.0),
            bar_ohlc(100.0, 121.0, 100.0, 100.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "KEEP");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].price, 120.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_price, 120.0);
    assert_eq!(strategy.diagnostics.len(), 1);
    assert_eq!(strategy.diagnostics[0].code, "E_STRATEGY_EXIT_TICKS");
}

#[test]
fn strategy_exit_invalid_qty_preserves_existing_pending_exit() {
    let source = SourceFile::new(
        "strategy_exit_invalid_qty_preserves_pending.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("KEEP", "L", limit=120)
if bar_index == 1
    strategy.exit("BAD", "L", stop=95, qty=0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(100.0, 100.0, 100.0, 100.0),
            bar_ohlc(100.0, 110.0, 100.0, 100.0),
            bar_ohlc(100.0, 121.0, 94.0, 100.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "KEEP");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 120.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_price, 120.0);
    assert_eq!(strategy.diagnostics.len(), 1);
    assert_eq!(strategy.diagnostics[0].code, "E_STRATEGY_EXIT_QTY");
}

#[test]
fn strategy_exit_invalid_qty_percent_preserves_existing_pending_exit() {
    let source = SourceFile::new(
        "strategy_exit_invalid_qty_percent_preserves_pending.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("KEEP", "L", limit=120)
if bar_index == 1
    strategy.exit("BAD", "L", stop=95, qty_percent=0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(100.0, 100.0, 100.0, 100.0),
            bar_ohlc(100.0, 110.0, 100.0, 100.0),
            bar_ohlc(100.0, 121.0, 94.0, 100.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "KEEP");
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].qty, 2.0);
    assert_eq!(strategy.orders[1].price, 120.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_price, 120.0);
    assert_eq!(strategy.diagnostics.len(), 1);
    assert_eq!(strategy.diagnostics[0].code, "E_STRATEGY_EXIT_QTY_PERCENT");
}

#[test]
fn strategy_exit_bracket_runtime_json_uses_existing_strategy_shape() {
    let source = SourceFile::new(
        "strategy_exit_bracket_json_shape.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("XB", "L", stop=95, limit=110)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(100.0, 100.0, 100.0, 100.0),
            bar_ohlc(100.0, 111.0, 100.0, 100.0),
        ],
    )
    .expect("runtime result");
    let output = public_runtime_result_json(&result);

    assert!(output.contains(r#""strategy":{"orders":"#));
    assert!(!output.contains("pending"));
    assert!(!output.contains("bracket"));
    assert!(!output.contains("leg"));
    assert!(!output.contains("exitReason"));
}

#[test]
fn strategy_close_cancels_pending_limit_before_evaluation() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", limit=12)
if bar_index == 1
    strategy.close("L")
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(11.0, 11.0, 10.0, 11.0),
            bar_ohlc(11.0, 12.0, 10.0, 11.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].id, "Close entry(s) order L");
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_price, 11.0);
}

#[test]
fn strategy_initial_capital_and_equity_mark_open_position_to_close() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("equity", initial_capital=1000)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=10)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .initial_capital,
        1000.0
    );

    let bars = [
        Bar {
            time: 10,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 9.0,
            high: 9.0,
            low: 9.0,
            close: 9.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 12.0,
            high: 12.0,
            low: 12.0,
            close: 12.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.equity.len(), 3);
    assert_eq!(strategy.equity[0].cash, 1000.0);
    assert_eq!(strategy.equity[0].market_value, 0.0);
    assert_eq!(strategy.equity[0].equity, 1000.0);
    assert_eq!(strategy.equity[1].cash, 910.0);
    assert_eq!(strategy.equity[1].market_value, 90.0);
    assert_eq!(strategy.equity[1].equity, 1000.0);
    assert_eq!(strategy.equity[1].net_profit, 0.0);
    assert_eq!(strategy.equity[2].cash, 910.0);
    assert_eq!(strategy.equity[2].market_value, 120.0);
    assert_eq!(strategy.equity[2].equity, 1030.0);
    assert_eq!(strategy.equity[2].net_profit, 30.0);
}

#[test]
fn strategy_cash_per_contract_commission_updates_profit_and_trade_fields() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("commission", commission_type=strategy.commission.cash_per_contract, commission_value=0.5)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close("L")
plot(strategy.opentrades.commission(0))
plot(strategy.closedtrades.commission(0))
plot(strategy.netprofit)
plot(strategy.equity)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.hir.as_ref().unwrap().strategy_settings.commission,
        Some(pine_ir::StrategyCommission::CashPerContract(0.5))
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            Bar {
                time: 1,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            },
            Bar {
                time: 2,
                open: 2.0,
                high: 2.0,
                low: 2.0,
                close: 2.0,
                volume: 1.0,
            },
            Bar {
                time: 3,
                open: 3.0,
                high: 3.0,
                low: 3.0,
                close: 3.0,
                volume: 1.0,
            },
            Bar {
                time: 4,
                open: 4.0,
                high: 4.0,
                low: 4.0,
                close: 4.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-1.0),
            PineValue::Float(-1.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Float(1000000.0),
            PineValue::Float(999999.0),
            PineValue::Float(1000001.0),
            PineValue::Float(1000002.0),
        ]
    );

    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.equity[1].cash, 999995.0);
    assert_eq!(strategy.equity[1].equity, 999999.0);
    assert_eq!(strategy.equity[3].cash, 1000002.0);
}

#[test]
fn strategy_cash_per_order_commission_updates_profit_and_trade_fields() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("commission", commission_type=strategy.commission.cash_per_order, commission_value=1.5)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close("L")
plot(strategy.opentrades.commission(0))
plot(strategy.closedtrades.commission(0))
plot(strategy.netprofit)
plot(strategy.equity)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.hir.as_ref().unwrap().strategy_settings.commission,
        Some(pine_ir::StrategyCommission::CashPerOrder(1.5))
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            Bar {
                time: 1,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            },
            Bar {
                time: 2,
                open: 2.0,
                high: 2.0,
                low: 2.0,
                close: 2.0,
                volume: 1.0,
            },
            Bar {
                time: 3,
                open: 3.0,
                high: 3.0,
                low: 3.0,
                close: 3.0,
                volume: 1.0,
            },
            Bar {
                time: 4,
                open: 4.0,
                high: 4.0,
                low: 4.0,
                close: 4.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.5),
            PineValue::Float(1.5),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-1.5),
            PineValue::Float(-1.5),
            PineValue::Float(1.0),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Float(1000000.0),
            PineValue::Float(999998.5),
            PineValue::Float(1000000.5),
            PineValue::Float(1000001.0),
        ]
    );

    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades[0].profit, 1.0);
    assert_eq!(strategy.equity[1].cash, 999994.5);
    assert_eq!(strategy.equity[1].equity, 999998.5);
    assert_eq!(strategy.equity[3].cash, 1000001.0);
}

#[test]
fn omitted_commission_type_defaults_to_percent_commission() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("commission", commission_value=10)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close("L")
plot(strategy.opentrades.commission(0))
plot(strategy.closedtrades.commission(0))
plot(strategy.netprofit)
plot(strategy.equity)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.hir.as_ref().unwrap().strategy_settings.commission,
        Some(pine_ir::StrategyCommission::Percent(10.0))
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.4),
            PineValue::Float(0.4),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.4 + 0.8),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-0.4),
            PineValue::Float(-0.4),
            PineValue::Float(4.0 - (0.4 + 0.8)),
        ]
    );

    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades[0].profit, 4.0 - (0.4 + 0.8));
}

#[test]
fn strategy_percent_commission_updates_profit_and_trade_fields() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("commission", commission_type=strategy.commission.percent, commission_value=10)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close("L")
plot(strategy.opentrades.commission(0))
plot(strategy.closedtrades.commission(0))
plot(strategy.netprofit)
plot(strategy.equity)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.hir.as_ref().unwrap().strategy_settings.commission,
        Some(pine_ir::StrategyCommission::Percent(10.0))
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.4),
            PineValue::Float(0.4),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.4 + 0.8),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-0.4),
            PineValue::Float(-0.4),
            PineValue::Float(4.0 - (0.4 + 0.8)),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Float(1000000.0),
            PineValue::Float(999999.6),
            PineValue::Float(1000001.6),
            // Cash records the entry cost/fee and exit proceeds/fee separately.
            PineValue::Float(1000000.0 - 4.0 - 0.4 + 8.0 - 0.8),
        ]
    );

    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades[0].profit, 4.0 - (0.4 + 0.8));
    assert_eq!(strategy.equity[1].cash, 999995.6);
    assert_eq!(strategy.equity[1].equity, 999999.6);
    assert_eq!(strategy.equity[3].cash, 1000000.0 - 4.0 - 0.4 + 8.0 - 0.8);
}

#[test]
fn strategy_slippage_updates_fill_prices_profit_and_equity() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("slippage", slippage=100)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close("L")
plot(strategy.closedtrades.entry_price(0))
plot(strategy.closedtrades.exit_price(0))
plot(strategy.closedtrades.profit(0))
plot(strategy.equity)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .slippage_ticks,
        100.0
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            Bar {
                time: 1,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            },
            Bar {
                time: 2,
                open: 2.0,
                high: 2.0,
                low: 2.0,
                close: 2.0,
                volume: 1.0,
            },
            Bar {
                time: 3,
                open: 3.0,
                high: 3.0,
                low: 3.0,
                close: 3.0,
                volume: 1.0,
            },
            Bar {
                time: 4,
                open: 4.0,
                high: 4.0,
                low: 4.0,
                close: 4.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Float(1000000.0),
            PineValue::Float(999998.0),
            PineValue::Float(1000000.0),
            PineValue::Float(1000000.0),
        ]
    );

    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.trades[0].entry_price, 3.0);
    assert_eq!(strategy.trades[0].exit_price, 3.0);
    assert_eq!(strategy.trades[0].profit, 0.0);
}

#[test]
fn strategy_slippage_does_not_change_limit_exit_fill_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("exit slippage", slippage=100)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("XL", "L", limit=3)
plot(strategy.closedtrades.exit_price(0))
plot(strategy.closedtrades.profit(0))
plot(strategy.equity)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            Bar {
                time: 1,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            },
            Bar {
                time: 2,
                open: 2.0,
                high: 2.0,
                low: 2.0,
                close: 2.0,
                volume: 1.0,
            },
            Bar {
                time: 3,
                open: 3.0,
                high: 3.0,
                low: 3.0,
                close: 3.0,
                volume: 1.0,
            },
            Bar {
                time: 4,
                open: 4.0,
                high: 4.0,
                low: 4.0,
                close: 4.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(1000000.0),
            PineValue::Float(999998.0),
            PineValue::Float(1000000.0),
            PineValue::Float(1000000.0),
        ]
    );

    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders[0].price, 3.0);
    assert_eq!(strategy.orders[1].price, 3.0);
    assert_eq!(strategy.trades[0].entry_price, 3.0);
    assert_eq!(strategy.trades[0].exit_price, 3.0);
    assert_eq!(strategy.trades[0].profit, 0.0);
}

#[test]
fn strategy_limit_verification_delays_limit_entry_until_price_moves_past_limit() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("limit verification", backtest_fill_limits_assumption=100)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2, limit=100)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .hir
            .as_ref()
            .unwrap()
            .strategy_settings
            .backtest_fill_limit_ticks,
        100.0
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            Bar {
                time: 1,
                open: 101.0,
                high: 101.0,
                low: 101.0,
                close: 101.0,
                volume: 1.0,
            },
            Bar {
                time: 2,
                open: 100.0,
                high: 101.0,
                low: 99.5,
                close: 100.0,
                volume: 1.0,
            },
            Bar {
                time: 3,
                open: 100.0,
                high: 100.0,
                low: 99.0,
                close: 100.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].price, 100.0);
    assert!(strategy.trades.is_empty());
}

#[test]
fn strategy_limit_verification_delays_limit_exit_but_keeps_limit_fill_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("limit verification", backtest_fill_limits_assumption=100)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("XL", "L", limit=12)
plot(strategy.closedtrades.exit_price(0))
plot(strategy.closedtrades.profit(0))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
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
                open: 11.0,
                high: 12.0,
                low: 10.0,
                close: 11.0,
                volume: 1.0,
            },
            Bar {
                time: 3,
                open: 12.0,
                high: 13.0,
                low: 11.0,
                close: 12.0,
                volume: 1.0,
            },
            Bar {
                time: 4,
                open: 12.0,
                high: 12.0,
                low: 12.0,
                close: 12.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(12.0),
            PineValue::Float(12.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].price, 12.0);
    assert_eq!(strategy.trades[0].exit_price, 12.0);
    assert_eq!(strategy.trades[0].profit, 2.0);
}

#[test]
fn strategy_close_without_matching_position_is_noop() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("close")
strategy.close("L")
strategy.entry("L", strategy.long, qty=1)
strategy.close("other")
strategy.close("L")
strategy.close("L")
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0)]).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert!(strategy.trades.is_empty());
    assert!(strategy.position.is_empty());
}

#[test]
fn strategy_account_currency_inherits_default_symbol_currency() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("account currency")
identity(value) => value
plot(strategy.account_currency == "USD" ? 1 : 0)
plot(identity(strategy.account_currency) == "USD" ? 1 : 0)
plot(na(strategy.account_currency[1]) ? na : strategy.account_currency[1] == "USD" ? 1 : 0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(3.0)])
        .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(1), PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(1), PineValue::Int(1), PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[2].values,
        vec![PineValue::Na, PineValue::Int(1), PineValue::Int(1)]
    );
}

#[test]
fn strategy_position_state_variables_follow_broker_mutations() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("position state")
identity(value) => value
position_entry_name = strategy.position_entry_name
position_entry_name_udf = identity(strategy.position_entry_name)
position_entry_name_history = strategy.position_entry_name[1]
plot(strategy.position_size)
plot(strategy.position_avg_price)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=2)
plot(strategy.position_size)
plot(strategy.position_avg_price)
independent_position_avg_price = strategy.position_avg_price * 0
position_avg_price_i = 0
while position_avg_price_i < 1
    independent_position_avg_price := strategy.position_avg_price
    position_avg_price_i := position_avg_price_i + 1
plot(independent_position_avg_price)
plot(identity(strategy.position_avg_price))
position_avg_price_history = strategy.position_avg_price[1]
if bar_index == 2
    strategy.close("L")
plot(strategy.position_size)
plot(strategy.position_avg_price)
plot(strategy.max_contracts_held_all)
plot(identity(strategy.max_contracts_held_all))
independent_max_contracts_held_all = strategy.max_contracts_held_all * 0
max_contracts_held_all_i = 0
while max_contracts_held_all_i < 1
    independent_max_contracts_held_all := strategy.max_contracts_held_all
    max_contracts_held_all_i := max_contracts_held_all_i + 1
plot(independent_max_contracts_held_all)
plot(strategy.max_contracts_held_long)
plot(identity(strategy.max_contracts_held_long))
independent_max_contracts_held_long = strategy.max_contracts_held_long * 0
max_contracts_held_long_i = 0
while max_contracts_held_long_i < 1
    independent_max_contracts_held_long := strategy.max_contracts_held_long
    max_contracts_held_long_i := max_contracts_held_long_i + 1
plot(independent_max_contracts_held_long)
plot(strategy.max_contracts_held_short)
plot(identity(strategy.max_contracts_held_short))
independent_max_contracts_held_short = strategy.max_contracts_held_short * 0
max_contracts_held_short_i = 0
while max_contracts_held_short_i < 1
    independent_max_contracts_held_short := strategy.max_contracts_held_short
    max_contracts_held_short_i := max_contracts_held_short_i + 1
plot(independent_max_contracts_held_short)
plot(position_avg_price_history)
plot(strategy.max_contracts_held_all[1])
plot(strategy.max_contracts_held_long[1])
plot(strategy.max_contracts_held_short[1])
plot(na(position_entry_name) ? na : position_entry_name == "L" ? 1 : 0)
plot(na(position_entry_name_udf) ? na : position_entry_name_udf == "L" ? 1 : 0)
plot(na(position_entry_name_history) ? na : position_entry_name_history == "L" ? 1 : 0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Na,
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Na,
        ]
    );
    assert_eq!(
        result.plots[4].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Na,
        ]
    );
    assert_eq!(
        result.plots[5].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Na,
        ]
    );
    assert_eq!(
        result.plots[6].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[7].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
            PineValue::Na,
        ]
    );
    assert_eq!(
        result.plots[8].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[9].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[10].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[11].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[12].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[13].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[14].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[15].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[16].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[17].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(3.0),
        ]
    );
    assert_eq!(
        result.plots[18].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[19].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[20].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    let position_entry_name = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Int(1),
        PineValue::Na,
    ];
    assert_eq!(result.plots[21].values, position_entry_name.clone());
    assert_eq!(result.plots[22].values, position_entry_name);
    assert_eq!(
        result.plots[23].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Int(1)
        ]
    );
}

#[test]
fn strategy_position_entry_name_tracks_the_continuous_net_position() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("position entry name", pyramiding=2)
if bar_index == 0
    strategy.entry("A", strategy.long, qty=1)
if bar_index == 1
    strategy.entry("B", strategy.long, qty=1)
if bar_index == 2
    strategy.close("A")
if bar_index == 3
    strategy.close("B")
if bar_index == 4
    strategy.entry("C", strategy.long, qty=1)
plot(na(strategy.position_entry_name) ? na : strategy.position_entry_name == "A" ? 1 : strategy.position_entry_name == "C" ? 2 : 0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0), bar(5.0), bar(6.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Na,
            PineValue::Int(2),
        ]
    );
}

#[test]
fn strategy_margin_call_short_partially_covers_on_later_high() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("margin call short", initial_capital=340, margin_short=50)
if bar_index == 0
    strategy.entry("S", strategy.short, qty=100)
plot(strategy.position_size)
plot(strategy.opentrades.capital_held)
plot(strategy.closedtrades)
plot(strategy.margin_liquidation_price)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
            bar_ohlc(4.0, 5.0, 4.0, 5.0),
            bar_ohlc(5.0, 5.0, 5.0, 5.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 2);
    assert_eq!(strategy.orders[0].id, "S");
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].qty, 100.0);
    assert_eq!(strategy.orders[0].price, 4.0);
    assert_eq!(strategy.orders[1].id, "Margin Call");
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].qty, 16.0);
    assert_eq!(strategy.orders[1].price, 5.0);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "S");
    assert_eq!(strategy.trades[0].exit_id, "Margin Call");
    assert_eq!(strategy.trades[0].qty, -16.0);
    assert_eq!(strategy.trades[0].profit, -16.0);
    assert_eq!(strategy.position.len(), 2);
    assert_eq!(strategy.position[0].size, -100.0);
    assert_eq!(strategy.position[1].size, -84.0);
    assert_eq!(strategy.position[1].avg_price, Some(4.0));
    assert!(strategy.diagnostics.is_empty());
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(-84.0),
            PineValue::Float(-84.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(210.0),
            PineValue::Float(210.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![PineValue::Int(0), PineValue::Int(1), PineValue::Int(1),]
    );
    match result.plots[3].values.as_slice() {
        [
            PineValue::Na,
            PineValue::Float(after_call),
            PineValue::Float(later),
        ] => {
            assert!((after_call - 5.24).abs() < 1e-10);
            assert!((later - 5.24).abs() < 1e-10);
        }
        other => panic!("unexpected liquidation-price plots: {other:?}"),
    }
}

#[test]
fn strategy_capital_held_history_reads_follow_short_margin_state() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("margin capital held short", margin_short=50)
identity(value) => value
if bar_index == 0
    strategy.entry("S", strategy.short, qty=2)
if bar_index == 3
    strategy.close("S")
plot(strategy.opentrades.capital_held)
independent_capital_held = strategy.opentrades.capital_held * 0
capital_held_i = 0
while capital_held_i < 1
    independent_capital_held := strategy.opentrades.capital_held
    capital_held_i := capital_held_i + 1
plot(independent_capital_held)
plot(identity(strategy.opentrades.capital_held))
plot(strategy.opentrades.capital_held[1])
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0), bar(5.0)],
    )
    .expect("runtime result");

    let current_values = vec![
        PineValue::Float(0.0),
        PineValue::Float(2.0),
        PineValue::Float(3.0),
        PineValue::Float(4.0),
        PineValue::Float(0.0),
    ];
    assert_eq!(result.plots[0].values, current_values);
    assert_eq!(result.plots[1].values, current_values);
    assert_eq!(result.plots[2].values, current_values);
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(3.0),
            PineValue::Float(4.0),
        ]
    );
}

#[test]
fn strategy_margin_entry_affordability_short_rejects_then_accepts_covered_fill() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("margin affordability short", initial_capital=4, margin_short=100)
if bar_index == 0
    strategy.entry("too-big-market", strategy.short, qty=3)
if bar_index == 1
    strategy.entry("too-big-stop", strategy.short, qty=2, stop=3)
if bar_index == 2
    strategy.entry("covered-market", strategy.short, qty=1)
plot(strategy.position_size)
plot(strategy.opentrades.capital_held)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "covered-market");
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].bar_index, 3);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[0].price, 4.0);
    assert_eq!(strategy.position.len(), 1);
    assert_eq!(strategy.position[0].size, -1.0);
    assert_eq!(strategy.position[0].avg_price, Some(4.0));
    assert_eq!(strategy.diagnostics.len(), 2);
    assert_eq!(strategy.diagnostics[0].code, "E_STRATEGY_MARGIN");
    assert_eq!(strategy.diagnostics[1].code, "E_STRATEGY_MARGIN");
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(-1.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(4.0),
        ]
    );
}

#[test]
fn strategy_capital_held_history_reads_follow_margin_state() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("margin capital held", margin_long=50)
identity(value) => value
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 3
    strategy.close("L")
plot(strategy.opentrades.capital_held)
independent_capital_held = strategy.opentrades.capital_held * 0
capital_held_i = 0
while capital_held_i < 1
    independent_capital_held := strategy.opentrades.capital_held
    capital_held_i := capital_held_i + 1
plot(independent_capital_held)
plot(identity(strategy.opentrades.capital_held))
plot(strategy.opentrades.capital_held[1])
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0), bar(5.0)],
    )
    .expect("runtime result");

    let current_values = vec![
        PineValue::Float(0.0),
        PineValue::Float(2.0),
        PineValue::Float(3.0),
        PineValue::Float(4.0),
        PineValue::Float(0.0),
    ];
    assert_eq!(result.plots[0].values, current_values);
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(3.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(3.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(3.0),
            PineValue::Float(4.0),
        ]
    );
}

#[test]
fn strategy_trade_count_variables_follow_entry_and_close_mutations() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("trade count state")
plot(strategy.closedtrades)
plot(strategy.opentrades)
if bar_index == 1
    strategy.entry("L", strategy.long, qty=2)
plot(strategy.closedtrades)
plot(strategy.opentrades)
independent_opentrades = strategy.opentrades * 0
opentrades_i = 0
while opentrades_i < 1
    independent_opentrades := strategy.opentrades
    opentrades_i := opentrades_i + 1
if bar_index == 2
    strategy.close("L")
plot(strategy.closedtrades)
plot(strategy.opentrades)
independent_closedtrades = strategy.closedtrades * 0
closedtrades_i = 0
while closedtrades_i < 1
    independent_closedtrades := strategy.closedtrades
    closedtrades_i := closedtrades_i + 1
plot(independent_closedtrades)
plot(independent_opentrades)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[4].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[5].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[6].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[7].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(0),
        ]
    );
}

#[test]
fn strategy_closed_trade_field_functions_read_recorded_trades() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("closed trade fields")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.close("L")
plot(strategy.closedtrades.entry_price(0))
plot(strategy.closedtrades.entry_id(0) == "L" ? 1 : 0)
plot(strategy.closedtrades.exit_price(0))
plot(strategy.closedtrades.exit_id(0) == "L" ? 1 : 0)
plot(strategy.closedtrades.entry_bar_index(0))
plot(strategy.closedtrades.exit_bar_index(0))
plot(strategy.closedtrades.entry_time(0))
plot(strategy.closedtrades.exit_time(0))
plot(strategy.closedtrades.commission(0))
plot(strategy.closedtrades.size(0))
plot(strategy.closedtrades.profit(0))
plot(strategy.closedtrades.max_runup(0))
plot(strategy.closedtrades.max_drawdown(0))
plot(na(strategy.closedtrades.entry_id(1)) ? 1 : 0)
plot(na(strategy.closedtrades.exit_id(1)) ? 1 : 0)
plot(strategy.closedtrades.entry_price(1))
plot(strategy.closedtrades.entry_price(-1))
plot(strategy.closedtrades.entry_price(0.5))
plot(strategy.closedtrades.profit_percent(0))
plot(strategy.closedtrades.max_runup_percent(0))
plot(strategy.closedtrades.max_drawdown_percent(0))
plot(strategy.closedtrades.profit_percent(1))
plot(strategy.closedtrades.max_runup_percent(-1))
plot(strategy.closedtrades.max_drawdown_percent(0.5))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            Bar {
                time: 10,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            },
            Bar {
                time: 20,
                open: 2.0,
                high: 3.0,
                low: 1.0,
                close: 3.0,
                volume: 1.0,
            },
            Bar {
                time: 30,
                open: 4.0,
                high: 4.0,
                low: 4.0,
                close: 4.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(2.0)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Int(0), PineValue::Int(0), PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[2].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(4.0)]
    );
    assert_eq!(
        result.plots[3].values,
        vec![PineValue::Int(0), PineValue::Int(0), PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[4].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[5].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Int(2)]
    );
    assert_eq!(
        result.plots[6].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Int(20)]
    );
    assert_eq!(
        result.plots[7].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Int(30)]
    );
    assert_eq!(result.plots[8].values, vec![PineValue::Float(0.0); 3]);
    assert_eq!(
        result.plots[9].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0)
        ]
    );
    assert_eq!(
        result.plots[10].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(4.0)
        ]
    );
    assert_eq!(
        result.plots[11].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(2.0)]
    );
    assert_eq!(
        result.plots[12].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(2.0)]
    );
    assert_eq!(
        result.plots[13].values,
        vec![PineValue::Int(1), PineValue::Int(1), PineValue::Int(1)]
    );
    assert_eq!(
        result.plots[14].values,
        vec![PineValue::Int(1), PineValue::Int(1), PineValue::Int(1)]
    );
    for values in result.plots[15..18].iter().map(|plot| &plot.values) {
        assert_eq!(values, &vec![PineValue::Na, PineValue::Na, PineValue::Na]);
    }
    assert_eq!(
        result.plots[18].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(100.0)
        ]
    );
    assert_eq!(
        result.plots[19].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(50.0)]
    );
    assert_eq!(
        result.plots[20].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(50.0)]
    );
    assert_eq!(result.plots[21].values, vec![PineValue::Float(0.0); 3]);
    for values in result.plots[22..].iter().map(|plot| &plot.values) {
        assert_eq!(values, &vec![PineValue::Na, PineValue::Na, PineValue::Na]);
    }
}

#[test]
fn strategy_open_trade_fields_read_current_position() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("open trade fields")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    strategy.close("L")
plot(strategy.opentrades.entry_price(0))
plot(na(strategy.opentrades.entry_id(0)) ? na : strategy.opentrades.entry_id(0) == "L" ? 1 : 0)
plot(strategy.opentrades.entry_bar_index(0))
plot(strategy.opentrades.entry_time(0))
plot(strategy.opentrades.size(0))
plot(strategy.opentrades.profit(0))
plot(strategy.opentrades.commission(0))
plot(strategy.opentrades.max_runup(0))
plot(strategy.opentrades.max_drawdown(0))
plot(strategy.opentrades.capital_held)
plot(strategy.opentrades.entry_price(1))
plot(na(strategy.opentrades.entry_id(1)) ? na : 0)
plot(strategy.opentrades.entry_bar_index(1))
plot(strategy.opentrades.entry_time(1))
plot(strategy.opentrades.size(1))
plot(strategy.opentrades.profit(1))
plot(strategy.opentrades.commission(1))
plot(strategy.opentrades.max_runup(1))
plot(strategy.opentrades.max_drawdown(1))
plot(strategy.opentrades.entry_price(-1))
plot(na(strategy.opentrades.entry_id(-1)) ? na : 0)
plot(strategy.opentrades.entry_bar_index(-1))
plot(strategy.opentrades.entry_time(-1))
plot(strategy.opentrades.size(-1))
plot(strategy.opentrades.profit(-1))
plot(strategy.opentrades.commission(-1))
plot(strategy.opentrades.max_runup(-1))
plot(strategy.opentrades.max_drawdown(-1))
plot(strategy.opentrades.entry_price(0.5))
plot(na(strategy.opentrades.entry_id(0.5)) ? na : 0)
plot(strategy.opentrades.entry_bar_index(0.5))
plot(strategy.opentrades.entry_time(0.5))
plot(strategy.opentrades.size(0.5))
plot(strategy.opentrades.profit(0.5))
plot(strategy.opentrades.commission(0.5))
plot(strategy.opentrades.max_runup(0.5))
plot(strategy.opentrades.max_drawdown(0.5))
plot(strategy.opentrades.profit_percent(0))
plot(strategy.opentrades.max_runup_percent(0))
plot(strategy.opentrades.max_drawdown_percent(0))
plot(strategy.opentrades.profit_percent(1))
plot(strategy.opentrades.max_runup_percent(-1))
plot(strategy.opentrades.max_drawdown_percent(0.5))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            Bar {
                time: 10,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            },
            Bar {
                time: 20,
                open: 2.0,
                high: 4.0,
                low: 1.0,
                close: 2.0,
                volume: 1.0,
            },
            Bar {
                time: 30,
                open: 3.0,
                high: 3.0,
                low: 3.0,
                close: 3.0,
                volume: 1.0,
            },
            Bar {
                time: 40,
                open: 3.0,
                high: 3.0,
                low: 3.0,
                close: 3.0,
                volume: 1.0,
            },
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Na
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Na,
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Na
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Na,
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Na
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Na,
            PineValue::Int(20),
            PineValue::Int(20),
            PineValue::Na
        ]
    );
    assert_eq!(
        result.plots[4].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0)
        ]
    );
    assert_eq!(
        result.plots[5].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0)
        ]
    );
    assert_eq!(
        result.plots[6].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0)
        ]
    );
    assert_eq!(
        result.plots[7].values,
        vec![
            PineValue::Na,
            PineValue::Float(4.0),
            PineValue::Float(4.0),
            PineValue::Na
        ]
    );
    assert_eq!(
        result.plots[8].values,
        vec![
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Na
        ]
    );
    assert_eq!(
        result.plots[9].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Na, PineValue::Na]
    );
    for (index, plot) in result.plots.iter().enumerate().take(37).skip(10) {
        let expected = if matches!(index, 14 | 15 | 16 | 23 | 24 | 25) {
            PineValue::Float(0.0)
        } else {
            PineValue::Na
        };
        assert_eq!(plot.values, vec![expected; 4]);
    }
    assert_eq!(
        result.plots[37].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(50.0),
            PineValue::Float(0.0)
        ]
    );
    assert_eq!(
        result.plots[38].values,
        vec![
            PineValue::Na,
            PineValue::Float(100.0),
            PineValue::Float(100.0),
            PineValue::Na
        ]
    );
    assert_eq!(
        result.plots[39].values,
        vec![
            PineValue::Na,
            PineValue::Float(50.0),
            PineValue::Float(50.0),
            PineValue::Na
        ]
    );
    assert_eq!(result.plots[40].values, vec![PineValue::Float(0.0); 4]);
    for plot in &result.plots[41..] {
        assert_eq!(
            plot.values,
            vec![PineValue::Na, PineValue::Na, PineValue::Na, PineValue::Na]
        );
    }
}

#[test]
fn strategy_closed_trade_exit_id_reads_pending_exit_identity() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("closed trade exit id")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
    strategy.exit("XL", "L", limit=12)
plot(strategy.closedtrades.exit_id(0) == "XL" ? 1 : 0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(10.0, 10.0, 10.0, 10.0),
            bar_ohlc(11.0, 12.0, 10.0, 11.0),
            bar_ohlc(13.0, 13.0, 13.0, 13.0),
        ],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Int(0), PineValue::Int(1), PineValue::Int(1)]
    );
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(strategy.trades[0].exit_id, "XL");
}

#[test]
fn strategy_trade_outcome_count_variables_follow_closed_trade_profits() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("trade outcome counts")
identity(value) => value
if bar_index == 0
    strategy.entry("W", strategy.long, qty=1)
if bar_index == 2
    strategy.close("W")
if bar_index == 3
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 5
    strategy.close("L")
if bar_index == 6
    strategy.entry("E", strategy.long, qty=1)
if bar_index == 8
    strategy.close("E")
plot(strategy.wintrades)
plot(strategy.losstrades)
plot(strategy.eventrades)
plot(strategy.closedtrades)
plot(strategy.grossprofit)
plot(strategy.grossloss)
plot(strategy.avg_trade)
plot(strategy.avg_trade_percent)
plot(strategy.avg_winning_trade)
plot(strategy.avg_winning_trade_percent)
plot(strategy.avg_losing_trade)
plot(strategy.avg_losing_trade_percent)
independent_wintrades = strategy.wintrades * 0
wintrades_i = 0
while wintrades_i < 1
    independent_wintrades := strategy.wintrades
    wintrades_i := wintrades_i + 1
plot(independent_wintrades)
independent_losstrades = strategy.losstrades * 0
losstrades_i = 0
while losstrades_i < 1
    independent_losstrades := strategy.losstrades
    losstrades_i := losstrades_i + 1
plot(independent_losstrades)
independent_eventrades = strategy.eventrades * 0
eventrades_i = 0
while eventrades_i < 1
    independent_eventrades := strategy.eventrades
    eventrades_i := eventrades_i + 1
plot(independent_eventrades)
independent_grossprofit = strategy.grossprofit * 0
grossprofit_i = 0
while grossprofit_i < 1
    independent_grossprofit := strategy.grossprofit
    grossprofit_i := grossprofit_i + 1
plot(independent_grossprofit)
independent_grossloss = strategy.grossloss * 0
grossloss_i = 0
while grossloss_i < 1
    independent_grossloss := strategy.grossloss
    grossloss_i := grossloss_i + 1
plot(independent_grossloss)
independent_avg_trade = strategy.avg_trade * 0
avg_trade_i = 0
while avg_trade_i < 1
    independent_avg_trade := strategy.avg_trade
    avg_trade_i := avg_trade_i + 1
plot(independent_avg_trade)
independent_avg_trade_percent = strategy.avg_trade_percent * 0
avg_trade_percent_i = 0
while avg_trade_percent_i < 1
    independent_avg_trade_percent := strategy.avg_trade_percent
    avg_trade_percent_i := avg_trade_percent_i + 1
plot(independent_avg_trade_percent)
independent_avg_winning_trade = strategy.avg_winning_trade * 0
avg_winning_trade_i = 0
while avg_winning_trade_i < 1
    independent_avg_winning_trade := strategy.avg_winning_trade
    avg_winning_trade_i := avg_winning_trade_i + 1
plot(independent_avg_winning_trade)
independent_avg_winning_trade_percent = strategy.avg_winning_trade_percent * 0
avg_winning_trade_percent_i = 0
while avg_winning_trade_percent_i < 1
    independent_avg_winning_trade_percent := strategy.avg_winning_trade_percent
    avg_winning_trade_percent_i := avg_winning_trade_percent_i + 1
plot(independent_avg_winning_trade_percent)
independent_avg_losing_trade = strategy.avg_losing_trade * 0
avg_losing_trade_i = 0
while avg_losing_trade_i < 1
    independent_avg_losing_trade := strategy.avg_losing_trade
    avg_losing_trade_i := avg_losing_trade_i + 1
plot(independent_avg_losing_trade)
independent_avg_losing_trade_percent = strategy.avg_losing_trade_percent * 0
avg_losing_trade_percent_i = 0
while avg_losing_trade_percent_i < 1
    independent_avg_losing_trade_percent := strategy.avg_losing_trade_percent
    avg_losing_trade_percent_i := avg_losing_trade_percent_i + 1
plot(independent_avg_losing_trade_percent)
plot(identity(strategy.grossprofit))
plot(identity(strategy.grossloss))
plot(identity(strategy.avg_trade))
plot(identity(strategy.avg_trade_percent))
plot(identity(strategy.avg_winning_trade))
plot(identity(strategy.avg_winning_trade_percent))
plot(identity(strategy.avg_losing_trade))
plot(identity(strategy.avg_losing_trade_percent))
plot(strategy.grossprofit[1])
plot(strategy.grossloss[1])
plot(strategy.avg_trade[1])
plot(strategy.avg_trade_percent[1])
plot(strategy.avg_winning_trade[1])
plot(strategy.avg_winning_trade_percent[1])
plot(strategy.avg_losing_trade[1])
plot(strategy.avg_losing_trade_percent[1])
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(3.0),
            bar(4.0),
            bar(4.0),
            bar(2.0),
            bar(3.0),
            bar(5.0),
            bar(5.0),
            bar(5.0),
        ],
    )
    .expect("runtime result");
    let wintrades = vec![
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
    ];
    let losstrades = vec![
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
    ];
    let eventrades = vec![
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(1),
    ];
    let closedtrades = vec![
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(0),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(1),
        PineValue::Int(2),
        PineValue::Int(2),
        PineValue::Int(2),
        PineValue::Int(3),
    ];
    let grossprofit = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
    ];
    let grossloss = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(1.0),
        PineValue::Float(1.0),
        PineValue::Float(1.0),
        PineValue::Float(1.0),
    ];
    let avg_trade = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(0.5),
        PineValue::Float(0.5),
        PineValue::Float(0.5),
        PineValue::Float(1.0 / 3.0),
    ];
    let avg_trade_percent = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Float(100.0),
        PineValue::Float(100.0),
        PineValue::Float(100.0),
        PineValue::Float(37.5),
        PineValue::Float(37.5),
        PineValue::Float(37.5),
        PineValue::Float(25.0),
    ];
    let avg_winning_trade = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
    ];
    let avg_winning_trade_percent = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Float(100.0),
        PineValue::Float(100.0),
        PineValue::Float(100.0),
        PineValue::Float(100.0),
        PineValue::Float(100.0),
        PineValue::Float(100.0),
        PineValue::Float(100.0),
    ];
    let avg_losing_trade = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Float(1.0),
        PineValue::Float(1.0),
        PineValue::Float(1.0),
        PineValue::Float(1.0),
    ];
    let avg_losing_trade_percent = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Na,
        PineValue::Float(25.0),
        PineValue::Float(25.0),
        PineValue::Float(25.0),
        PineValue::Float(25.0),
    ];
    assert_eq!(result.plots[0].values, wintrades);
    assert_eq!(result.plots[1].values, losstrades);
    assert_eq!(result.plots[2].values, eventrades);
    assert_eq!(result.plots[3].values, closedtrades);
    assert_eq!(result.plots[4].values, grossprofit);
    assert_eq!(result.plots[5].values, grossloss);
    assert_eq!(result.plots[6].values, avg_trade);
    assert_eq!(result.plots[7].values, avg_trade_percent);
    assert_eq!(result.plots[8].values, avg_winning_trade);
    assert_eq!(result.plots[9].values, avg_winning_trade_percent);
    assert_eq!(result.plots[10].values, avg_losing_trade);
    assert_eq!(result.plots[11].values, avg_losing_trade_percent);
    assert_eq!(result.plots[12].values, wintrades);
    assert_eq!(result.plots[13].values, losstrades);
    assert_eq!(result.plots[14].values, eventrades);
    assert_eq!(result.plots[15].values, grossprofit);
    assert_eq!(result.plots[16].values, grossloss);
    assert_eq!(result.plots[17].values, avg_trade);
    assert_eq!(result.plots[18].values, avg_trade_percent);
    assert_eq!(result.plots[19].values, avg_winning_trade);
    assert_eq!(result.plots[20].values, avg_winning_trade_percent);
    assert_eq!(result.plots[21].values, avg_losing_trade);
    assert_eq!(result.plots[22].values, avg_losing_trade_percent);
    assert_eq!(result.plots[23].values, grossprofit);
    assert_eq!(result.plots[24].values, grossloss);
    assert_eq!(result.plots[25].values, avg_trade);
    assert_eq!(result.plots[26].values, avg_trade_percent);
    assert_eq!(result.plots[27].values, avg_winning_trade);
    assert_eq!(result.plots[28].values, avg_winning_trade_percent);
    assert_eq!(result.plots[29].values, avg_losing_trade);
    assert_eq!(result.plots[30].values, avg_losing_trade_percent);
    assert_eq!(
        result.plots[31].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[32].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
        ]
    );
    assert_eq!(
        result.plots[33].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(0.5),
            PineValue::Float(0.5),
            PineValue::Float(0.5),
        ]
    );
    assert_eq!(
        result.plots[34].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(100.0),
            PineValue::Float(100.0),
            PineValue::Float(100.0),
            PineValue::Float(37.5),
            PineValue::Float(37.5),
            PineValue::Float(37.5),
        ]
    );
    assert_eq!(
        result.plots[35].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[36].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(100.0),
            PineValue::Float(100.0),
            PineValue::Float(100.0),
            PineValue::Float(100.0),
            PineValue::Float(100.0),
            PineValue::Float(100.0),
        ]
    );
    assert_eq!(
        result.plots[37].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
        ]
    );
    assert_eq!(
        result.plots[38].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(25.0),
            PineValue::Float(25.0),
            PineValue::Float(25.0),
        ]
    );
    let strategy = result.strategy.as_ref().expect("strategy result");
    assert_eq!(strategy.trades.len(), 3);
    assert_eq!(strategy.trades[0].profit, 2.0);
    assert_eq!(strategy.trades[1].profit, -1.0);
    assert_eq!(strategy.trades[2].profit, 0.0);
}

#[test]
fn strategy_buy_and_hold_return_percent_uses_first_close_denominator() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("buy and hold")
identity(value) => value
plot(strategy.buy_and_hold_return_percent)
plot(identity(strategy.buy_and_hold_return_percent))
independent_buy_and_hold = strategy.buy_and_hold_return_percent * 0
buy_and_hold_i = 0
while buy_and_hold_i < 1
    independent_buy_and_hold := strategy.buy_and_hold_return_percent
    buy_and_hold_i := buy_and_hold_i + 1
plot(independent_buy_and_hold)
plot(strategy.buy_and_hold_return_percent[1])
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(2.0), bar(3.0), bar(5.0)])
        .expect("runtime result");
    let expected = vec![
        PineValue::Float(0.0),
        PineValue::Float(50.0),
        PineValue::Float(150.0),
    ];

    assert_eq!(result.plots[0].values, expected);
    assert_eq!(result.plots[1].values, result.plots[0].values);
    assert_eq!(result.plots[2].values, result.plots[0].values);
    assert_eq!(
        result.plots[3].values,
        vec![PineValue::Na, PineValue::Float(0.0), PineValue::Float(50.0)]
    );
}

#[test]
fn strategy_profit_percent_variables_use_documented_denominators() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("profit percent", initial_capital=1000)
identity(value) => value
openprofit_percent = strategy.openprofit_percent
openprofit_percent_udf = identity(strategy.openprofit_percent)
openprofit_percent_history = strategy.openprofit_percent[1]
if bar_index == 0
    strategy.entry("W", strategy.long, qty=1)
if bar_index == 2
    strategy.close("W")
if bar_index == 3
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 5
    strategy.close("L")
plot(strategy.netprofit_percent)
plot(identity(strategy.netprofit_percent))
plot(strategy.grossprofit_percent)
plot(strategy.grossloss_percent)
independent_netprofit_percent = strategy.netprofit_percent * 0
netprofit_percent_i = 0
while netprofit_percent_i < 1
    independent_netprofit_percent := strategy.netprofit_percent
    netprofit_percent_i := netprofit_percent_i + 1
plot(independent_netprofit_percent)
independent_grossprofit_percent = strategy.grossprofit_percent * 0
grossprofit_percent_i = 0
while grossprofit_percent_i < 1
    independent_grossprofit_percent := strategy.grossprofit_percent
    grossprofit_percent_i := grossprofit_percent_i + 1
plot(independent_grossprofit_percent)
independent_grossloss_percent = strategy.grossloss_percent * 0
grossloss_percent_i = 0
while grossloss_percent_i < 1
    independent_grossloss_percent := strategy.grossloss_percent
    grossloss_percent_i := grossloss_percent_i + 1
plot(independent_grossloss_percent)
plot(identity(strategy.grossprofit_percent))
plot(identity(strategy.grossloss_percent))
plot(strategy.netprofit_percent[1])
plot(strategy.grossprofit_percent[1])
plot(strategy.grossloss_percent[1])
plot(openprofit_percent)
plot(openprofit_percent_udf)
plot(openprofit_percent_history)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(3.0),
            bar(4.0),
            bar(4.0),
            bar(2.0),
            bar(2.0),
        ],
    )
    .expect("runtime result");

    let net_profit_percent = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.2),
        PineValue::Float(0.2),
        PineValue::Float(0.2),
        PineValue::Float(0.0),
    ];
    let gross_profit_percent = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.2),
        PineValue::Float(0.2),
        PineValue::Float(0.2),
        PineValue::Float(0.2),
    ];
    let gross_loss_percent = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.2),
    ];
    assert_eq!(result.plots[0].values, net_profit_percent);
    assert_eq!(result.plots[1].values, net_profit_percent);
    assert_eq!(result.plots[2].values, gross_profit_percent);
    assert_eq!(result.plots[3].values, gross_loss_percent);
    assert_eq!(result.plots[4].values, net_profit_percent);
    assert_eq!(result.plots[5].values, gross_profit_percent);
    assert_eq!(result.plots[6].values, gross_loss_percent);
    assert_eq!(result.plots[7].values, gross_profit_percent);
    assert_eq!(result.plots[8].values, gross_loss_percent);
    assert_eq!(
        result.plots[9].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.2),
            PineValue::Float(0.2),
            PineValue::Float(0.2),
        ]
    );
    assert_eq!(
        result.plots[10].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.2),
            PineValue::Float(0.2),
            PineValue::Float(0.2),
        ]
    );
    assert_eq!(
        result.plots[11].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    let open_profit_percent = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.1),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(-2.0 / 1002.0 * 100.0),
        PineValue::Float(0.0),
    ];
    assert_eq!(result.plots[12].values, open_profit_percent.clone());
    assert_eq!(result.plots[13].values, open_profit_percent);
    assert_eq!(
        result.plots[14].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.1),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(-2.0 / 1002.0 * 100.0),
        ]
    );
}

#[test]
fn strategy_openprofit_percent_is_na_without_positive_realized_equity() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("open profit percent denominator", initial_capital=1)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 2
    strategy.close("L")
plot(strategy.openprofit_percent)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(1.0), bar(0.0), bar(0.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(-100.0),
            PineValue::Na,
        ]
    );
}

#[test]
fn strategy_trade_count_variables_observe_pending_exit_on_next_bar() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"//@version=6
strategy("pending exit trade counts")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", limit=2.5)
plot(strategy.closedtrades)
plot(strategy.opentrades)
plot(strategy.closedtrades[1])
plot(strategy.opentrades[1])
plot(strategy.closedtrades.first_index)
plot(strategy.closedtrades.first_index[1])
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar_ohlc(1.0, 1.0, 1.0, 1.0),
            bar_ohlc(2.0, 2.0, 2.0, 2.0),
            bar_ohlc(3.0, 3.0, 3.0, 3.0),
            bar_ohlc(4.0, 4.0, 4.0, 4.0),
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Na,
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(1),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Na,
            PineValue::Int(0),
            PineValue::Int(1),
            PineValue::Int(0),
        ]
    );
    assert_eq!(result.plots[4].values, vec![PineValue::Int(0); 4]);
    assert_eq!(
        result.plots[5].values,
        vec![
            PineValue::Na,
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Int(0),
        ]
    );
}

#[test]
fn strategy_profit_state_variables_follow_realized_and_open_profit() {
    let source = SourceFile::new(
        "strategy.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_profit_state.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(1.0),
            bar(2.0),
            bar(3.0),
            bar(1.0),
            bar(4.0),
            bar(6.0),
            bar(6.0),
        ],
    )
    .expect("runtime result");

    let open_profit = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(-4.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
    ];
    let net_profit = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
    ];
    let net_profit_history = vec![
        PineValue::Na,
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
    ];
    let equity = vec![
        PineValue::Float(1000.0),
        PineValue::Float(1000.0),
        PineValue::Float(1000.0),
        PineValue::Float(996.0),
        PineValue::Float(1002.0),
        PineValue::Float(1002.0),
        PineValue::Float(1002.0),
    ];
    let equity_history = vec![
        PineValue::Na,
        PineValue::Float(1000.0),
        PineValue::Float(1000.0),
        PineValue::Float(1000.0),
        PineValue::Float(996.0),
        PineValue::Float(1002.0),
        PineValue::Float(1002.0),
    ];
    let max_runup = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
    ];
    let max_runup_percent = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(2.0 / 6.0 * 100.0),
        PineValue::Float(2.0 / 6.0 * 100.0),
    ];
    let max_drawdown = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(4.0),
        PineValue::Float(4.0),
        PineValue::Float(4.0),
        PineValue::Float(4.0),
    ];
    let max_drawdown_history = vec![
        PineValue::Na,
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(4.0),
        PineValue::Float(4.0),
        PineValue::Float(4.0),
    ];
    let max_drawdown_percent = vec![
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(4.0 / 6.0 * 100.0),
        PineValue::Float(4.0 / 6.0 * 100.0),
        PineValue::Float(4.0 / 6.0 * 100.0),
        PineValue::Float(4.0 / 6.0 * 100.0),
    ];

    assert_eq!(result.plots[0].values, open_profit);
    assert_eq!(result.plots[1].values, net_profit.clone());
    assert_eq!(result.plots[2].values, equity.clone());
    assert_eq!(result.plots[3].values, max_runup.clone());
    assert_eq!(result.plots[4].values, max_runup_percent.clone());
    assert_eq!(result.plots[5].values, max_drawdown.clone());
    assert_eq!(result.plots[6].values, max_drawdown_percent.clone());
    assert_eq!(result.plots[7].values, open_profit.clone());
    assert_eq!(result.plots[8].values, open_profit);
    assert_eq!(result.plots[9].values, net_profit.clone());
    assert_eq!(result.plots[10].values, net_profit.clone());
    assert_eq!(result.plots[11].values, equity.clone());
    assert_eq!(result.plots[12].values, max_runup);
    assert_eq!(result.plots[13].values, max_runup_percent);
    assert_eq!(result.plots[14].values, max_drawdown.clone());
    assert_eq!(result.plots[15].values, max_drawdown_percent);
    assert_eq!(result.plots[16].values, net_profit);
    assert_eq!(result.plots[17].values, max_drawdown.clone());
    assert_eq!(result.plots[18].values, equity.clone());
    assert_eq!(result.plots[19].values, max_drawdown);
    assert_eq!(result.plots[20].values, equity);
    assert_eq!(result.plots[21].values, net_profit_history);
    assert_eq!(result.plots[22].values, equity_history);
    assert_eq!(result.plots[23].values, max_drawdown_history);
    let initial_capital = vec![PineValue::Float(1000.0); 7];
    assert_eq!(result.plots[24].values, initial_capital.clone());
    assert_eq!(result.plots[25].values, initial_capital);
    assert_eq!(
        result.plots[26].values,
        vec![
            PineValue::Na,
            PineValue::Float(1000.0),
            PineValue::Float(1000.0),
            PineValue::Float(1000.0),
            PineValue::Float(1000.0),
            PineValue::Float(1000.0),
            PineValue::Float(1000.0),
        ]
    );
}

#[test]
fn strategy_max_drawdown_follows_intrabar_low_and_max_equity() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("drawdown", initial_capital=1000)
plot(strategy.max_drawdown)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=10)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(10.0), bar(12.0), bar(9.0), bar(11.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(30.0),
            PineValue::Float(30.0),
        ]
    );
}

#[test]
fn strategy_max_drawdown_uses_intrabar_low() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("drawdown", initial_capital=1000)
plot(strategy.max_drawdown)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=10)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar_ohlc(10.0, 10.0, 8.0, 10.0),
            bar_ohlc(10.0, 12.0, 10.0, 12.0),
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(20.0),
            PineValue::Float(20.0),
        ]
    );
}

#[test]
fn strategy_max_runup_follows_intrabar_high_and_min_equity() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("runup", initial_capital=1000)
plot(strategy.max_runup)
if bar_index == 0
    strategy.entry("L1", strategy.long, qty=10)
if bar_index == 2
    strategy.close("L1")
if bar_index == 3
    strategy.entry("L2", strategy.long, qty=10)
if bar_index == 5
    strategy.close("L2")
if bar_index == 6
    strategy.entry("L3", strategy.long, qty=10)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar_ohlc(10.0, 11.0, 10.0, 10.0),
            bar_ohlc(10.0, 10.0, 8.0, 8.0),
            bar(8.0),
            bar_ohlc(10.0, 12.0, 10.0, 12.0),
            bar_ohlc(12.0, 12.0, 12.0, 12.0),
            bar(12.0),
            bar_ohlc(10.0, 11.0, 10.0, 10.0),
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(10.0),
            PineValue::Float(10.0),
            PineValue::Float(10.0),
            PineValue::Float(20.0),
            PineValue::Float(20.0),
            PineValue::Float(20.0),
            PineValue::Float(30.0),
        ]
    );
}

#[test]
fn strategy_max_runup_and_drawdown_percent_use_trade_value_denominator() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("percent", initial_capital=1000)
plot(strategy.max_runup_percent)
plot(strategy.max_drawdown_percent)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=10)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[
            bar(10.0),
            bar_ohlc(10.0, 12.0, 8.0, 10.0),
            bar_ohlc(10.0, 11.0, 9.0, 10.0),
        ],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(20.0),
            PineValue::Float(20.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(20.0),
            PineValue::Float(20.0),
        ]
    );
}

#[test]
fn strategy_variables_work_in_supported_expression_contexts() {
    let source = SourceFile::new(
        "strategy.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_variable_interactions.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");

    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(-1.0),
            PineValue::Float(-1.0),
            PineValue::Float(2.0),
            PineValue::Float(-1.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(-1.0),
            PineValue::Float(-1.0),
            PineValue::Float(2.0),
            PineValue::Float(-1.0),
        ]
    );
    assert_eq!(
        result.plots[2].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(4.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[3].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[4].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[5].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(20.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[6].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(2.0),
        ]
    );
    assert_eq!(
        result.plots[7].values,
        vec![
            PineValue::Na,
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
}

fn strategy_bar_phases(source: &str) -> Vec<crate::runtime::strategy_scheduler::StrategyBarPhase> {
    use crate::runtime::historical::HistoricalRuntime;

    let source = SourceFile::new("strategy.pine", source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&[bar(100.0), bar(110.0), bar(90.0)])
        .expect("run");
    runtime.strategy_phase_trace
}

#[test]
fn historical_fill_path_orders_open_then_long_price_then_short_price() {
    use crate::runtime::strategy_scheduler::HistoricalFillStep::*;
    let mut steps = [
        StopLong,
        SameBarMarketEntriesAtClose,
        MarketClosesAtOpen,
        LimitShort,
        LimitLong,
        MarketEntriesAtOpen,
        SameBarMarketClosesAtClose,
    ];
    steps.sort_by_key(|step| step.ordering_key());
    assert!(MarketClosesAtOpen < MarketEntriesAtOpen);
    assert!(MarketEntriesAtOpen < LimitLong);
    assert!(LimitLong < StopLong);
    assert!(StopLong < LimitShort);
    assert!(SameBarMarketClosesAtClose < SameBarMarketEntriesAtClose);
    assert!(StopLimitShort < SameBarMarketClosesAtClose);
}

fn fill_path_order_ids(strategy: &crate::StrategyResult) -> Vec<&str> {
    strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect()
}

fn run_fill_path_fixture(name: &str, source: &str, bars: &[Bar]) -> crate::StrategyResult {
    let source = SourceFile::new(name, source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{name} diagnostics: {:?}",
        analysis.diagnostics
    );
    run_historical(&analysis.hir.expect("HIR"), bars)
        .unwrap_or_else(|error| panic!("{name} runtime: {error:?}"))
        .strategy
        .unwrap_or_else(|| panic!("{name} missing strategy output"))
}

#[test]
fn legacy_v3_short_entry_attached_bracket_fills_on_entry_bar() {
    let source =
        include_str!("../../../../tests/fixtures/runtime/legacy_v3_short_attached_bracket.pine");
    let cases = [
        (bar_ohlc(100.0, 100.01, 99.90, 99.99), 99.95),
        (bar_ohlc(100.0, 100.06, 99.50, 99.99), 100.04),
    ];
    for (entry_bar, expected_exit) in cases {
        let strategy = run_fill_path_fixture(
            "legacy_v3_short_attached_bracket.pine",
            source,
            &[bar(100.0), entry_bar],
        );
        assert_eq!(fill_path_order_ids(&strategy), vec!["S", "XS"]);
        assert_eq!(strategy.orders[0].bar_index, 1);
        assert_eq!(strategy.orders[1].bar_index, 1);
        assert_eq!(strategy.orders[1].price, expected_exit);
        assert_eq!(strategy.trades[0].exit_price, expected_exit);
    }
}

#[test]
fn legacy_v3_short_all_entry_bracket_fills_on_entry_bar() {
    let source =
        include_str!("../../../../tests/fixtures/runtime/legacy_v3_short_all_entry_bracket.pine");
    let cases = [
        (bar_ohlc(100.0, 100.01, 99.90, 99.99), 99.95),
        (bar_ohlc(100.0, 100.06, 99.50, 99.99), 100.04),
    ];
    for (entry_bar, expected_exit) in cases {
        let strategy = run_fill_path_fixture(
            "legacy_v3_short_all_entry_bracket.pine",
            source,
            &[bar(100.0), entry_bar],
        );
        assert_eq!(fill_path_order_ids(&strategy), vec!["S", "XS"]);
        assert_eq!(strategy.orders[0].bar_index, 1);
        assert_eq!(strategy.orders[1].bar_index, 1);
        assert_eq!(strategy.orders[1].price, expected_exit);
        assert_eq!(strategy.trades[0].exit_price, expected_exit);
    }
}

#[test]
fn legacy_v3_short_all_entry_sma_control_enters_and_exits() {
    let mut bars = vec![bar(100.0); 20];
    bars.push(bar(90.0));
    bars.push(bar_ohlc(90.0, 91.0, 84.0, 89.0));
    let strategy = run_fill_path_fixture(
        "legacy_v3_short_all_entry_sma_control.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/legacy_v3_short_all_entry_sma_control.pine"
        ),
        &bars,
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["S", "X"]);
    assert_eq!(strategy.orders[0].bar_index, 21);
    assert_eq!(strategy.orders[0].price, 90.0);
    assert_eq!(strategy.orders[1].bar_index, 21);
    assert_eq!(strategy.orders[1].price, 85.0);
}

#[test]
fn strategy_same_bar_limit_and_stop_fill_limit_family_first() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_limit_stop_collision.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_limit_stop_collision.pine"
        ),
        &[bar(1.0), bar_ohlc(2.0, 3.0, 1.0, 2.0), bar(3.0), bar(4.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["LIM", "STP"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[1].bar_index, 1);
}

#[test]
fn strategy_fill_path_high_first_long_fills_stop_then_limit() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_high_first_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_high_first_long.pine"),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["STP", "LIM"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 10.8);
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 8.2);
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.orders[1].qty, 1.0);
    assert_eq!(strategy.position.last().unwrap().size, 2.0);
}

#[test]
fn strategy_fill_path_low_first_long_fills_limit_then_stop() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_low_first_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_low_first_long.pine"),
        &[bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0), bar(11.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["LIM", "STP"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 8.2);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 10.8);
    assert_eq!(strategy.position.last().unwrap().size, 2.0);
}

#[test]
fn strategy_fill_path_high_first_short_fills_limit_then_stop() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_high_first_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_high_first_short.pine"),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["LIM", "STP"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 10.8);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 8.2);
    assert_eq!(strategy.orders[1].direction, "strategy.short");
    assert_eq!(strategy.position.last().unwrap().size, -2.0);
}

#[test]
fn strategy_fill_path_low_first_short_fills_stop_then_limit() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_low_first_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_low_first_short.pine"),
        &[bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0), bar(11.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["STP", "LIM"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 8.2);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 10.8);
    assert_eq!(strategy.position.last().unwrap().size, -2.0);
}

#[test]
fn strategy_fill_path_same_price_uses_creation_order_not_family_rank() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_same_price_creation_order.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_same_price_creation_order.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["LIM", "STP"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 10.8);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 10.8);
    assert_eq!(strategy.orders[1].direction, "strategy.long");
    assert_eq!(strategy.position.last().unwrap().size, 1.0);
}

#[test]
fn strategy_fill_path_order_oca_cancel_does_not_fill_stale_peer() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_order_oca_cancel.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_order_oca_cancel.pine"),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["STP"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 10.8);
    assert_eq!(strategy.position.last().unwrap().size, 1.0);
}

#[test]
fn strategy_fill_path_stop_limit_long_fills_same_bar_after_high_first_activation() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_stop_limit_long.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_stop_limit_long.pine"),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["SL"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 8.2);
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.position.last().unwrap().size, 1.0);
}

#[test]
fn strategy_fill_path_stop_limit_short_does_not_fill_same_bar() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_stop_limit_short.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_stop_limit_short.pine"),
        &[
            bar(10.0),
            bar_ohlc(10.0, 13.0, 8.0, 11.0),
            bar_ohlc(10.8, 10.8, 10.8, 10.8),
        ],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["SL"]);
    assert_eq!(strategy.orders[0].bar_index, 2);
    assert_eq!(strategy.orders[0].price, 10.8);
    assert_eq!(strategy.position.last().unwrap().size, -1.0);
}

#[test]
fn strategy_fill_path_entry_then_exit_same_bar() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_entry_then_exit_same_bar.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_entry_then_exit_same_bar.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "EX"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 10.8);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 8.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_fill_path_exit_then_entry_same_bar() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_exit_then_entry_same_bar.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_exit_then_entry_same_bar.pine"
        ),
        &[bar(10.0), bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "EX", "NX"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 10.0);
    assert_eq!(strategy.orders[1].bar_index, 2);
    assert_eq!(strategy.orders[1].price, 10.8);
    assert_eq!(strategy.orders[2].bar_index, 2);
    assert_eq!(strategy.orders[2].price, 8.2);
    assert_eq!(strategy.position.last().unwrap().size, 1.0);
}

#[test]
fn strategy_fill_path_bracket_high_first_takes_limit_before_stop() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_bracket_high_first.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_bracket_high_first.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "BR"]);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 10.8);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_fill_path_bracket_low_first_takes_stop_before_limit() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_bracket_low_first.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_bracket_low_first.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0), bar(11.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "BR"]);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 8.5);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_fill_path_bracket_short_high_first_takes_stop() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_bracket_short_high_first.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_bracket_short_high_first.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "BR"]);
    assert_eq!(strategy.orders[1].price, 10.8);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_fill_path_bracket_short_low_first_takes_limit() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_bracket_short_low_first.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_bracket_short_low_first.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0), bar(11.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "BR"]);
    assert_eq!(strategy.orders[1].price, 8.2);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_fill_path_trailing_activation_then_fill() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_trailing_activation_then_fill.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_trailing_activation_then_fill.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "TR"]);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_fill_path_trailing_no_future_extreme() {
    let high_first = run_fill_path_fixture(
        "strategy_fill_path_trailing_no_future_extreme.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_trailing_no_future_extreme.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 12.0, 8.0, 9.0), bar(9.0)],
    );
    let low_first = run_fill_path_fixture(
        "strategy_fill_path_trailing_no_future_extreme.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_trailing_no_future_extreme.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0), bar(11.0)],
    );
    assert_eq!(fill_path_order_ids(&high_first), vec!["EN", "TR"]);
    assert_eq!(fill_path_order_ids(&low_first), vec!["EN", "TR"]);
    assert_ne!(high_first.orders[1].price, low_first.orders[1].price);
}

#[test]
fn strategy_fill_path_exit_oca_reduce_updates_peer_once() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_exit_oca_reduce.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_fill_path_exit_oca_reduce.pine"),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "TP", "SL"]);
    assert_eq!(strategy.orders[1].price, 10.8);
    assert_eq!(strategy.orders[1].qty, 1.0);
    assert_eq!(strategy.orders[2].price, 8.2);
    assert_eq!(strategy.orders[2].qty, 1.0);
    assert_eq!(strategy.position.last().unwrap().size, 0.0);
}

#[test]
fn strategy_fill_path_partial_exit_reservation() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_partial_exit_reservation.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_partial_exit_reservation.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0), bar(9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["EN", "EX"]);
    assert_eq!(strategy.orders[1].qty, 1.0);
    assert_eq!(strategy.orders[1].price, 8.2);
    assert_eq!(strategy.position.last().unwrap().size, 1.0);
}

#[test]
fn strategy_fill_path_exit_before_margin_long() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_exit_before_margin_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_exit_before_margin_long.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0)],
    );
    let ids = fill_path_order_ids(&strategy);
    assert_eq!(ids[0], "EN");
    let exit = ids.iter().position(|id| *id == "EX").expect("user exit");
    let margin = ids
        .iter()
        .position(|id| *id == "Margin Call")
        .expect("margin");
    assert!(
        exit < margin,
        "sample-locked user exit before margin: {ids:?}"
    );
    assert_eq!(strategy.orders[exit].price, 8.5);
}

#[test]
fn strategy_fill_path_margin_before_exit_long() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_margin_before_exit_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_margin_before_exit_long.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0)],
    );
    let ids = fill_path_order_ids(&strategy);
    assert_eq!(ids[0], "EN");
    assert!(
        ids.contains(&"Margin Call"),
        "low-first path should liquidate before the later profit exit: {ids:?}"
    );
    if let Some(exit) = ids.iter().position(|id| *id == "EX") {
        let margin = ids.iter().position(|id| *id == "Margin Call").unwrap();
        assert!(margin < exit, "{ids:?}");
    }
}

#[test]
fn strategy_fill_path_exit_before_margin_short() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_exit_before_margin_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_exit_before_margin_short.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0)],
    );
    let ids = fill_path_order_ids(&strategy);
    assert_eq!(ids[0], "EN");
    let exit = ids.iter().position(|id| *id == "EX").expect("user exit");
    let margin = ids
        .iter()
        .position(|id| *id == "Margin Call")
        .expect("margin");
    assert!(exit < margin, "{ids:?}");
}

#[test]
fn strategy_fill_path_margin_before_exit_short() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_margin_before_exit_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_margin_before_exit_short.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0)],
    );
    let ids = fill_path_order_ids(&strategy);
    assert_eq!(ids[0], "EN");
    assert!(ids.contains(&"Margin Call"), "{ids:?}");
    if let Some(exit) = ids.iter().position(|id| *id == "EX") {
        let margin = ids.iter().position(|id| *id == "Margin Call").unwrap();
        assert!(margin < exit, "{ids:?}");
    }
}

fn run_fill_path_runtime(name: &str, source: &str, bars: &[Bar]) -> crate::RuntimeResult {
    let source = SourceFile::new(name, source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{name} diagnostics: {:?}",
        analysis.diagnostics
    );
    run_historical(&analysis.hir.expect("HIR"), bars)
        .unwrap_or_else(|error| panic!("{name} runtime: {error:?}"))
}

#[test]
fn strategy_fill_path_drawdown_intrabar_ordering() {
    let source = include_str!(
        "../../../../tests/fixtures/runtime/strategy_fill_path_drawdown_intrabar_ordering.pine"
    );
    let high_first = run_fill_path_runtime(
        "strategy_fill_path_drawdown_intrabar_ordering.pine",
        source,
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0)],
    );
    let low_first = run_fill_path_runtime(
        "strategy_fill_path_drawdown_intrabar_ordering.pine",
        source,
        &[bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0)],
    );
    let high_strategy = high_first.strategy.as_ref().expect("strategy");
    let low_strategy = low_first.strategy.as_ref().expect("strategy");
    assert_eq!(fill_path_order_ids(high_strategy), vec!["EN"]);
    assert_eq!(fill_path_order_ids(low_strategy), vec!["EN"]);
    let high_runup = high_first.plots[0].values[1].as_f64().expect("runup");
    let low_runup = low_first.plots[0].values[1].as_f64().expect("runup");
    let high_drawdown = high_first.plots[1].values[1].as_f64().expect("drawdown");
    assert!(high_runup > 0.0, "{high_runup}");
    assert!(high_drawdown > 0.0, "{high_drawdown}");
    assert!(low_runup > high_runup, "{low_runup} vs {high_runup}");
}

#[test]
fn strategy_fill_path_margin_invalidates_entry() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_margin_invalidates_entry.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_margin_invalidates_entry.pine"
        ),
        &[bar(10.0), bar(10.0), bar_ohlc(10.0, 13.0, 8.0, 11.0)],
    );
    let ids = fill_path_order_ids(&strategy);
    assert_eq!(ids[0], "EN");
    assert!(ids.contains(&"Margin Call"), "{ids:?}");
    assert!(
        !ids.contains(&"NX"),
        "margin must invalidate the stale stop entry: {ids:?}"
    );
}

#[test]
fn strategy_fill_path_calc_on_order_fills_resume() {
    let strategy = run_fill_path_fixture(
        "strategy_fill_path_calc_on_order_fills_resume.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_calc_on_order_fills_resume.pine"
        ),
        &[bar(10.0), bar_ohlc(10.0, 11.0, 8.0, 9.0)],
    );
    assert_eq!(fill_path_order_ids(&strategy), vec!["A", "B"]);
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[0].price, 10.8);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[1].price, 8.2);
    assert_eq!(strategy.position.last().unwrap().size, 2.0);
}

#[test]
fn strategy_fill_path_calc_on_order_fills_guard() {
    use crate::runtime::historical::HistoricalRuntime;

    let source = SourceFile::new(
        "strategy_fill_path_calc_on_order_fills_guard.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_calc_on_order_fills_guard.pine"
        ),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.strategy_scheduler.set_max_recalculation_passes(1);
    let error = runtime
        .append_bars(&[bar(10.0), bar(11.0)])
        .expect_err("self-triggering fills must hit the pass guard");
    assert!(
        error
            .message
            .contains("strategy recalculation pass limit exceeded"),
        "{}",
        error.message
    );
}

fn expected_strategy_bar_phases() -> Vec<crate::runtime::strategy_scheduler::StrategyBarPhase> {
    use crate::runtime::strategy_scheduler::StrategyBarPhase::*;
    vec![
        EligibleEntryFills,
        TradeExtremes,
        MarginCall,
        BuiltinRefresh,
        ScriptStatements,
        ExitFills,
        Equity,
        OutputCommit,
    ]
}

#[test]
fn scheduler_traces_current_order_for_market_entry() {
    let phases = strategy_bar_phases(
        r#"
strategy("market entry")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
"#,
    );
    assert_eq!(phases, expected_strategy_bar_phases().repeat(3));
}

#[test]
fn scheduler_traces_current_order_for_price_entry() {
    let phases = strategy_bar_phases(
        r#"
strategy("limit entry")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=105)
"#,
    );
    assert_eq!(phases, expected_strategy_bar_phases().repeat(3));
}

#[test]
fn scheduler_traces_current_order_for_close_exit_and_margin_call() {
    let close_phases = strategy_bar_phases(
        r#"
strategy("close")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.close("L")
"#,
    );
    assert_eq!(close_phases, expected_strategy_bar_phases().repeat(3));

    let immediate_phases = strategy_bar_phases(
        r#"
strategy("close immediately")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1
    strategy.close("L", immediately=true)
"#,
    );
    use crate::runtime::strategy_scheduler::StrategyBarPhase::*;
    let mut expected = expected_strategy_bar_phases();
    expected.extend([
        EligibleEntryFills,
        TradeExtremes,
        MarginCall,
        BuiltinRefresh,
        ScriptStatements,
        CurrentTickMarketFills,
        ExitFills,
        Equity,
        OutputCommit,
        EligibleEntryFills,
        TradeExtremes,
        MarginCall,
        BuiltinRefresh,
        ScriptStatements,
        ExitFills,
        Equity,
        OutputCommit,
    ]);
    assert_eq!(immediate_phases, expected);

    let exit_phases = strategy_bar_phases(
        r#"
strategy("exit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("XL", "L", stop=95)
"#,
    );
    assert_eq!(exit_phases, expected_strategy_bar_phases().repeat(3));

    let margin_phases = strategy_bar_phases(
        r#"
strategy("margin", margin_long=25, initial_capital=165)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=100)
"#,
    );
    assert_eq!(margin_phases, expected_strategy_bar_phases().repeat(3));
}

#[test]
fn indicator_run_does_not_trace_strategy_phases() {
    use crate::runtime::historical::HistoricalRuntime;

    let source = SourceFile::new(
        "indicator.pine",
        r#"
indicator("plain")
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    let hir = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&[bar(1.0)]).expect("run");
    assert!(runtime.strategy_phase_trace.is_empty());
}

#[test]
fn strategy_risk_calls_stay_semantically_rejected() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("risk")
strategy.risk.not_a_rule(1)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("strategy.risk broker risk rules are not implemented")),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_none());
}

#[test]
fn strategy_risk_allow_entry_in_long_closes_opposite_entry_without_short() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("allow long")
strategy.risk.allow_entry_in(strategy.direction.long)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 1
    strategy.entry("S", strategy.short, qty=1)
plot(strategy.position_size)
plot(strategy.max_contracts_held_short)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
        Bar {
            time: 40,
            open: 4.0,
            high: 4.0,
            low: 4.0,
            close: 4.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "L");
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].id, "L");
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(0.0)
    );
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(
        result.plots[1].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
}

#[test]
fn strategy_risk_max_drawdown_cash_flattens_and_blocks_later_entry() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("drawdown", initial_capital=1000)
strategy.risk.max_drawdown(40, strategy.cash)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=10)
if bar_index == 3
    strategy.entry("X", strategy.long, qty=1)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        bar(10.0),
        bar(10.0),
        bar_ohlc(10.0, 10.0, 5.0, 5.0),
        bar(5.0),
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(10.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.trades.len(), 1);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(0.0)
    );
}

#[test]
fn strategy_risk_max_intraday_filled_orders_flattens_and_blocks_later_entry() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("filled", pyramiding=3)
strategy.risk.max_intraday_filled_orders(1)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 2
    strategy.entry("X", strategy.long, qty=1)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades.len(), 1);
}

#[test]
fn strategy_risk_max_intraday_loss_cash_flattens_and_resets_next_window() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("loss", initial_capital=1000)
strategy.risk.max_intraday_loss(40, strategy.cash)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=10)
if bar_index == 3
    strategy.entry("X", strategy.long, qty=1)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let bars = [
        bar(10.0),
        bar(10.0),
        bar_ohlc(10.0, 10.0, 5.0, 5.0),
        bar(5.0),
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(10.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
}

#[test]
fn strategy_risk_max_cons_loss_days_blocks_after_consecutive_loss_windows() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("cons", pyramiding=3)
strategy.risk.max_cons_loss_days(2)
if bar_index == 0 or bar_index == 2
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 1 or bar_index == 3
    strategy.close("L", immediately=true)
if bar_index == 4
    strategy.entry("X", strategy.long, qty=1)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let day = 86_400_000;
    let bars = [
        Bar {
            time: 0,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 100.0,
        },
        Bar {
            time: 1_000,
            open: 10.0,
            high: 10.0,
            low: 5.0,
            close: 5.0,
            volume: 100.0,
        },
        Bar {
            time: day,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 100.0,
        },
        Bar {
            time: day + 1_000,
            open: 10.0,
            high: 10.0,
            low: 5.0,
            close: 5.0,
            volume: 100.0,
        },
        Bar {
            time: 2 * day,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 100.0,
        },
        Bar {
            time: 2 * day + 1_000,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 100.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(0.0),
            PineValue::Float(1.0),
            PineValue::Float(0.0),
            PineValue::Float(0.0),
        ]
    );
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.trades.len(), 2);
    assert!(!strategy.orders.iter().any(|order| order.id == "X"));
}

#[test]
fn strategy_risk_max_position_size_reduces_entry_qty() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("max size")
strategy.risk.max_position_size(2)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=5)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].qty, 2.0);
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(2.0)
    );
}

#[test]
fn strategy_risk_allow_entry_in_does_not_bind_strategy_order() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"strategy("order unaffected")
strategy.risk.allow_entry_in(strategy.direction.long)
if bar_index == 0
    strategy.order("S", strategy.short, qty=1)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = [
        Bar {
            time: 10,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 20,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 30,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(-1.0)
    );
}

#[test]
fn use_bar_magnifier_true_is_accepted() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("magnifier", use_bar_magnifier=true)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    assert!(hir.strategy_settings.use_bar_magnifier);
}

#[test]
fn calc_on_every_tick_series_stays_rejected() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("ticks", calc_on_every_tick=close > open)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("argument `calc_on_every_tick` expects const bool")),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn calc_on_every_tick_false_waits_for_observed_limit_price() {
    let hir = analyze_strategy(
        r#"
strategy("default ticks")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
plot(close)
"#,
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");
    let forming = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("forming skipped");
    assert!(forming.strategy.expect("strategy").orders.is_empty());
    assert_eq!(forming.plots[0].values.len(), 1);

    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("confirmed executes");
    assert!(confirmed.strategy.expect("strategy").orders.is_empty());
    let filled = runtime
        .update(BarUpdate::forming(at_time(120000, bar(89.0))))
        .expect("observed limit price");
    assert_eq!(filled.strategy.expect("strategy").orders.len(), 1);
    assert_eq!(filled.plots[0].values.len(), 2);
}

#[test]
fn calc_on_every_tick_true_does_not_replay_cumulative_extremes() {
    let hir = analyze_strategy(
        r#"
strategy("every tick", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
plot(close)
"#,
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");
    let forming = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("forming fill");
    assert!(forming.strategy.expect("strategy").orders.is_empty());
    assert_eq!(forming.plots[0].values.len(), 2);

    let replaced = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 91.0, 95.0),
        )))
        .expect("replacement forming");
    assert!(replaced.strategy.expect("strategy").orders.is_empty());

    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 91.0, 95.0),
        )))
        .expect("confirmed no fill");
    assert!(confirmed.strategy.expect("strategy").orders.is_empty());
}

#[test]
fn calc_on_every_tick_preserves_var_rollback_and_varip_intrabar() {
    let hir = analyze_strategy(
        r#"
strategy("varip ticks", calc_on_every_tick=true)
var int v = 0
varip int p = 0
v += 1
p += 1
plot(v)
plot(p)
"#,
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    let historical = runtime
        .update(BarUpdate::historical(at_time(0, bar(1.0))))
        .expect("historical bar");
    assert_eq!(historical.plots[0].values, vec![PineValue::Int(1)]);
    assert_eq!(historical.plots[1].values, vec![PineValue::Int(1)]);

    let forming = runtime
        .update(BarUpdate::forming(at_time(60000, bar(2.0))))
        .expect("forming");
    assert_eq!(
        forming.plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(2)]
    );
    assert_eq!(
        forming.plots[1].values,
        vec![PineValue::Int(1), PineValue::Int(2)]
    );

    let replaced = runtime
        .update(BarUpdate::forming(at_time(60000, bar(3.0))))
        .expect("replacement forming");
    assert_eq!(
        replaced.plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(2)]
    );
    assert_eq!(
        replaced.plots[1].values,
        vec![PineValue::Int(1), PineValue::Int(3)]
    );

    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(60000, bar(4.0))))
        .expect("confirmed");
    assert_eq!(
        confirmed.plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(2)]
    );
    assert_eq!(
        confirmed.plots[1].values,
        vec![PineValue::Int(1), PineValue::Int(4)]
    );
}

#[test]
fn calc_on_every_tick_does_not_change_historical_fills() {
    let with_flag = analyze_strategy(
        r#"
strategy("historical ticks", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
plot(close)
"#,
    );
    let without_flag = analyze_strategy(
        r#"
strategy("historical default")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
plot(close)
"#,
    );
    let bars = [bar(1.0), bar(2.0), bar(3.0), bar(4.0)];
    let with_result = run_historical(&with_flag, &bars).expect("with flag");
    let without_result = run_historical(&without_flag, &bars).expect("without flag");
    assert_eq!(
        with_result.strategy.expect("strategy").orders,
        without_result.strategy.expect("strategy").orders
    );
}

#[test]
fn calc_on_order_fills_series_stays_rejected() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("series recalc", calc_on_order_fills=close > open)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("argument `calc_on_order_fills` expects const bool")),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn calc_on_order_fills_places_same_bar_limit_after_market_fill() {
    let source = SourceFile::new(
        "strategy_calc_on_order_fills.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_calc_on_order_fills.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(
        strategy
            .orders
            .iter()
            .map(|order| order.id.as_str())
            .collect::<Vec<_>>(),
        vec!["A", "B"]
    );
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(
        result.plots[0].values,
        vec![
            PineValue::Float(0.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
            PineValue::Float(2.0),
        ]
    );
}

#[test]
fn calc_on_order_fills_false_does_not_fill_same_bar_limit() {
    let source = SourceFile::new(
        "strategy_calc_on_order_fills_false.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_calc_on_order_fills_false.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(
        strategy
            .orders
            .iter()
            .map(|order| order.id.as_str())
            .collect::<Vec<_>>(),
        vec!["A"]
    );
}

#[test]
fn calc_on_order_fills_places_exit_from_post_entry_average() {
    let source = SourceFile::new(
        "strategy_calc_on_order_fills_exit_avg.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_calc_on_order_fills_exit_avg.pine"
        ),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("HIR"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    )
    .expect("runtime result");
    let strategy = result.strategy.expect("strategy output");
    assert_eq!(
        strategy
            .orders
            .iter()
            .map(|order| order.id.as_str())
            .collect::<Vec<_>>(),
        vec!["L", "XL", "L"]
    );
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[1].bar_index, 3);
    assert_eq!(strategy.orders[2].bar_index, 3);
}

#[test]
fn calc_on_order_fills_counts_extra_script_passes() {
    use crate::runtime::historical::HistoricalRuntime;

    let source = SourceFile::new(
        "strategy.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_calc_on_order_fills.pine"),
    );
    let analysis = analyze_source(&source);
    let hir = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&[bar(1.0), bar(2.0), bar(3.0), bar(4.0)])
        .expect("run");
    assert!(runtime.strategy_scheduler.script_passes() > 4);
    assert!(runtime.strategy_scheduler.recalculation_passes() > 0);
    assert!(runtime.strategy_scheduler.max_passes_on_bar() > 1);
    let profile = runtime.profile();
    assert_eq!(
        profile.strategy_script_passes,
        runtime.strategy_scheduler.script_passes()
    );
    assert_eq!(
        profile.strategy_recalculation_passes,
        runtime.strategy_scheduler.recalculation_passes()
    );
}

#[test]
fn historical_strategy_records_one_script_pass_per_bar() {
    use crate::runtime::historical::HistoricalRuntime;
    use crate::runtime::strategy_scheduler::DEFAULT_MAX_RECALCULATION_PASSES;

    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("pass identity")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&[bar(1.0), bar(2.0), bar(3.0)])
        .expect("run");
    assert_eq!(runtime.strategy_scheduler.script_passes(), 3);
    assert_eq!(runtime.strategy_scheduler.recalculation_passes(), 0);
    assert_eq!(runtime.strategy_scheduler.max_passes_on_bar(), 1);
    assert_eq!(
        runtime.strategy_scheduler.max_recalculation_passes(),
        DEFAULT_MAX_RECALCULATION_PASSES
    );
    assert_eq!(runtime.strategy_scheduler.identity.bar_index, 2);
    assert_eq!(runtime.strategy_scheduler.identity.pass, 0);
    assert_eq!(
        runtime.strategy_scheduler.identity.phase,
        crate::runtime::strategy_scheduler::StrategyBarPhase::OutputCommit
    );

    let profile = runtime.profile();
    assert_eq!(profile.strategy_script_passes, 3);
    assert_eq!(profile.strategy_recalculation_passes, 0);
    assert_eq!(profile.strategy_max_passes_on_bar, 1);
    assert_eq!(
        profile.strategy_max_recalculation_passes,
        DEFAULT_MAX_RECALCULATION_PASSES as usize
    );
}

#[test]
fn indicator_profile_does_not_count_strategy_passes() {
    let source = SourceFile::new(
        "indicator.pine",
        r#"
indicator("plain")
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    let profiled =
        run_historical_profiled(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0)]).expect("run");
    assert_eq!(profiled.profile.strategy_script_passes, 0);
    assert_eq!(profiled.profile.strategy_recalculation_passes, 0);
    assert_eq!(profiled.profile.strategy_max_passes_on_bar, 0);
    assert_eq!(profiled.profile.strategy_max_recalculation_passes, 0);
}

#[test]
fn historical_runtime_broker_snapshot_restore_discards_later_fill() {
    use crate::runtime::historical::HistoricalRuntime;

    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("limit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&[bar(100.0)]).expect("place");
    let snapshot = runtime.snapshot_strategy_broker();
    runtime
        .append_bars(&[bar_ohlc(100.0, 100.0, 89.0, 95.0)])
        .expect("fill");
    let filled = runtime.result().strategy.expect("strategy");
    assert_eq!(filled.orders.len(), 1);

    runtime.restore_strategy_broker(snapshot);
    let restored = runtime.result().strategy.expect("strategy");
    assert!(restored.orders.is_empty());
    assert_eq!(restored.position.len(), 0);
}

#[test]
fn forming_bar_broker_does_not_infer_limit_fill_from_past_low() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("forming rollback", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");

    let forming = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("forming fill");
    assert!(forming.strategy.expect("strategy").orders.is_empty());

    let replaced = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 91.0, 95.0),
        )))
        .expect("replacement forming");
    assert!(replaced.strategy.expect("strategy").orders.is_empty());

    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 91.0, 95.0),
        )))
        .expect("confirmed no fill");
    assert!(confirmed.strategy.expect("strategy").orders.is_empty());
    assert!(
        runtime
            .confirmed_result()
            .strategy
            .expect("strategy")
            .orders
            .is_empty()
    );
}

#[test]
fn forming_bar_broker_commit_preserves_pending_limit_until_observed_price() {
    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("forming commit", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");
    runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("forming fill");
    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("confirmed fill");
    assert!(confirmed.strategy.expect("strategy").orders.is_empty());
    runtime
        .update(BarUpdate::confirmed(at_time(120000, bar(89.0))))
        .expect("observed limit fill");
    assert_eq!(
        runtime
            .confirmed_result()
            .strategy
            .expect("strategy")
            .orders
            .len(),
        1
    );
}

#[test]
fn strategy_fill_path_realtime_stop_limit_requires_observed_activation() {
    let hir = analyze_source(&SourceFile::new(
        "strategy_fill_path_realtime_stop_limit_rollback.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_realtime_stop_limit_rollback.pine"
        ),
    ))
    .hir
    .expect("HIR");
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(10.0))))
        .expect("place");
    let forming_fill = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(10.0, 11.0, 8.0, 9.0),
        )))
        .expect("forming fill");
    assert_eq!(
        forming_fill.strategy.expect("strategy").orders.len(),
        0,
        "cumulative extremes are not observed stop-limit ticks"
    );
    runtime
        .update(BarUpdate::forming(at_time(60000, bar(10.0))))
        .expect("replacement forming");
    runtime
        .update(BarUpdate::confirmed(at_time(60000, bar(10.0))))
        .expect("confirmed abandons fill");
    assert!(
        runtime
            .confirmed_result()
            .strategy
            .expect("strategy")
            .orders
            .is_empty()
    );
    runtime
        .update(BarUpdate::forming(at_time(120000, bar(11.0))))
        .expect("observed stop activation");
    let filled = runtime
        .update(BarUpdate::forming(at_time(120000, bar(8.0))))
        .expect("observed limit fill");
    assert_eq!(filled.strategy.as_ref().unwrap().orders.len(), 1);
    assert_eq!(filled.strategy.as_ref().unwrap().orders[0].price, 8.0);
    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(120000, bar(10.0))))
        .unwrap();
    assert_eq!(
        confirmed.strategy.unwrap().orders,
        filled.strategy.unwrap().orders
    );
}

#[test]
fn strategy_fill_path_realtime_trailing_rollback() {
    let hir = analyze_source(&SourceFile::new(
        "strategy_fill_path_realtime_trailing_rollback.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_realtime_trailing_rollback.pine"
        ),
    ))
    .hir
    .expect("HIR");
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(10.0))))
        .expect("place");
    let forming = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(10.0, 11.0, 8.0, 9.0),
        )))
        .expect("forming trail");
    assert!(
        forming
            .strategy
            .expect("strategy")
            .orders
            .iter()
            .any(|order| order.id == "EN")
    );
    runtime
        .update(BarUpdate::forming(at_time(60000, bar(10.0))))
        .expect("replacement forming");
    runtime
        .update(BarUpdate::confirmed(at_time(60000, bar(10.0))))
        .expect("confirmed");
    let confirmed = runtime.confirmed_result().strategy.expect("strategy");
    assert_eq!(confirmed.orders.len(), 1);
    assert_eq!(confirmed.orders[0].id, "EN");
    assert!(confirmed.trades.is_empty());
}

#[test]
fn strategy_fill_path_realtime_margin_requires_observed_adverse_price() {
    let hir = analyze_source(&SourceFile::new(
        "strategy_fill_path_realtime_margin_rollback.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_fill_path_realtime_margin_rollback.pine"
        ),
    ))
    .hir
    .expect("HIR");
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(10.0))))
        .expect("place");
    let forming = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(10.0, 11.0, 8.0, 9.0),
        )))
        .expect("forming margin");
    assert!(
        !forming
            .strategy
            .expect("strategy")
            .orders
            .iter()
            .any(|order| order.id == "Margin Call")
    );
    runtime
        .update(BarUpdate::forming(at_time(60000, bar(10.0))))
        .expect("replacement forming");
    runtime
        .update(BarUpdate::confirmed(at_time(60000, bar(10.0))))
        .expect("confirmed");
    let confirmed = runtime.confirmed_result().strategy.expect("strategy");
    assert_eq!(confirmed.orders.len(), 1);
    assert_eq!(confirmed.orders[0].id, "EN");
    assert!(
        !confirmed
            .orders
            .iter()
            .any(|order| order.id == "Margin Call")
    );
    let adverse = runtime
        .update(BarUpdate::forming(at_time(120000, bar(1.0))))
        .expect("observed adverse price");
    let margin_orders = adverse.strategy.unwrap().orders;
    assert!(margin_orders.iter().any(|order| order.id == "Margin Call"));
    let recovered = runtime
        .update(BarUpdate::confirmed(at_time(120000, bar(10.0))))
        .unwrap();
    assert_eq!(recovered.strategy.unwrap().orders, margin_orders);
}

fn analyze_strategy(source: &str) -> pine_ir::HirProgram {
    let source = SourceFile::new("strategy.pine", source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

#[test]
fn forming_bar_broker_preserves_order_placement_across_updates() {
    let hir = analyze_strategy(
        r#"
strategy("forming place", calc_on_every_tick=true)
if high > 150
    strategy.entry("L", strategy.long, qty=1)
plot(close)
"#,
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");
    let forming = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 160.0, 100.0, 100.0),
        )))
        .expect("forming place");
    assert!(forming.strategy.expect("strategy").orders.is_empty());

    runtime
        .update(BarUpdate::forming(at_time(60000, bar(100.0))))
        .expect("replacement forming");
    runtime
        .update(BarUpdate::confirmed(at_time(60000, bar(100.0))))
        .expect("confirmed no place");
    let later = runtime
        .update(BarUpdate::historical(at_time(120000, bar(110.0))))
        .expect("next bar");
    let orders = later.strategy.expect("strategy").orders;
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].price, 100.0);
    assert!(
        runtime
            .confirmed_result()
            .strategy
            .expect("strategy")
            .orders
            .len()
            == 1
    );
}

#[test]
fn forming_bar_cancel_cannot_undo_fill_on_current_observed_price() {
    let hir = analyze_strategy(
        r#"
strategy("forming cancel", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
if bar_index == 1 and close < 50
    strategy.cancel("L")
plot(close)
"#,
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");
    let cancelled = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(96.0, 96.0, 96.0, 40.0),
        )))
        .expect("forming cancel");
    let orders = cancelled.strategy.expect("strategy").orders;
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].price, 40.0);

    runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(96.0, 96.0, 96.0, 96.0),
        )))
        .expect("replacement forming");
    runtime
        .update(BarUpdate::confirmed(at_time(
            60000,
            bar_ohlc(96.0, 96.0, 96.0, 96.0),
        )))
        .expect("confirmed keeps pending");
    let filled = runtime
        .update(BarUpdate::historical(at_time(
            120000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("later fill");
    assert_eq!(filled.strategy.expect("strategy").orders.len(), 1);
}

#[test]
fn forming_bar_broker_rollback_discards_abandoned_stop_limit_activation() {
    let hir = analyze_strategy(
        r#"
strategy("forming activate", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, stop=110, limit=100)
plot(close)
"#,
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");
    runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 111.0, 105.0, 105.0),
        )))
        .expect("forming activation");
    runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 105.0, 105.0, 105.0),
        )))
        .expect("replacement forming");
    runtime
        .update(BarUpdate::confirmed(at_time(
            60000,
            bar_ohlc(100.0, 105.0, 105.0, 105.0),
        )))
        .expect("confirmed not activated");
    let later = runtime
        .update(BarUpdate::historical(at_time(
            120000,
            bar_ohlc(100.0, 100.0, 99.0, 100.0),
        )))
        .expect("later bar");
    assert!(later.strategy.expect("strategy").orders.is_empty());
}

#[test]
fn forming_bar_fill_alerts_require_observed_fill_not_cumulative_low() {
    let hir = analyze_strategy(
        r#"
strategy("forming alerts", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
if bar_index == 1 and low < 90
    alert("would fill")
plot(close)
"#,
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(100.0))))
        .expect("historical bar");
    let forming = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 89.0, 95.0),
        )))
        .expect("forming fill");
    let forming_strategy = forming.strategy.expect("strategy");
    assert!(forming_strategy.orders.is_empty());
    assert!(forming_strategy.alerts.is_empty());
    assert_eq!(forming.alerts.len(), 1);

    let replaced = runtime
        .update(BarUpdate::forming(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 91.0, 95.0),
        )))
        .expect("replacement forming");
    let replaced_strategy = replaced.strategy.as_ref().expect("strategy");
    assert!(replaced_strategy.orders.is_empty());
    assert!(replaced_strategy.alerts.is_empty());
    assert!(replaced.alerts.is_empty());

    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(
            60000,
            bar_ohlc(100.0, 100.0, 91.0, 95.0),
        )))
        .expect("confirmed no fill");
    let confirmed_strategy = confirmed.strategy.as_ref().expect("strategy");
    assert!(confirmed_strategy.orders.is_empty());
    assert!(confirmed_strategy.alerts.is_empty());
    assert!(confirmed.alerts.is_empty());
    assert!(
        runtime
            .confirmed_result()
            .strategy
            .expect("strategy")
            .alerts
            .is_empty()
    );
}

#[test]
fn forming_observations_and_historical_ohlc_have_distinct_fill_inputs() {
    let hir = analyze_strategy(
        r#"
strategy("parity", calc_on_every_tick=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=90)
plot(close)
"#,
    );
    let bars = [
        at_time(0, bar(100.0)),
        at_time(60000, bar_ohlc(100.0, 100.0, 89.0, 95.0)),
        at_time(120000, bar(110.0)),
    ];
    let historical = run_historical(&hir, &bars).expect("historical");
    let mut realtime = RealtimeRuntime::new(&hir);
    realtime
        .update(BarUpdate::historical(bars[0]))
        .expect("bar 0");
    realtime
        .update(BarUpdate::forming(bars[1]))
        .expect("forming bar 1");
    realtime
        .update(BarUpdate::confirmed(bars[1]))
        .expect("confirmed bar 1");
    let confirmed = realtime
        .update(BarUpdate::historical(bars[2]))
        .expect("bar 2");
    assert!(
        confirmed
            .strategy
            .as_ref()
            .expect("strategy")
            .orders
            .is_empty()
    );
    let historical_orders = &historical.strategy.as_ref().expect("strategy").orders;
    assert_eq!(historical_orders.len(), 1);
    assert_eq!(historical_orders[0].price, 90.0);
    assert_eq!(confirmed.plots, historical.plots);
}

#[test]
fn extra_script_passes_on_a_bar_hit_the_configured_guardrail() {
    use crate::runtime::historical::HistoricalRuntime;

    let source = SourceFile::new(
        "strategy.pine",
        r#"
strategy("guardrail")
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    let hir = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.strategy_scheduler.set_max_recalculation_passes(1);
    runtime.append_bars(&[bar(1.0)]).expect("initial bar");
    runtime.strategy_scheduler.begin_bar(0);
    runtime
        .strategy_scheduler
        .begin_script_pass()
        .expect("simulated initial pass");
    runtime
        .strategy_scheduler
        .begin_script_pass()
        .expect("one extra pass");
    let error = runtime
        .strategy_scheduler
        .begin_script_pass()
        .expect_err("second extra pass is over the limit");
    assert!(
        error
            .message
            .contains("strategy recalculation pass limit exceeded"),
        "{}",
        error.message
    );
}

#[test]
fn immediate_close_preserves_same_pass_account_visibility() {
    for close_call in [
        "strategy.close(\"L\", immediately=true)",
        "strategy.close_all(immediately=true)",
    ] {
        let source = SourceFile::new(
            "immediate-visibility.pine",
            format!(
                r#"//@version=6
strategy("visibility", initial_capital=1000, commission_type=strategy.commission.cash_per_order, commission_value=0.1)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=2)
if bar_index == 2
    {close_call}
plot(strategy.position_size)
plot(strategy.closedtrades)
plot(strategy.equity)
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(
            &analysis.hir.unwrap(),
            &[bar(10.0), bar(11.0), bar(12.0), bar(13.0)],
        )
        .unwrap();
        assert_values_close(&result.plots[0].values, &[0.0, 2.0, 2.0, 0.0]);
        assert_values_close(&result.plots[1].values, &[0.0, 0.0, 0.0, 1.0]);
        assert_values_close(&result.plots[2].values, &[1000.0, 999.9, 1001.9, 1001.8]);
        let strategy = result.strategy.unwrap();
        assert_eq!(strategy.trades.len(), 1);
        assert_eq!(strategy.trades[0].exit_bar_index, 2);
        assert_eq!(strategy.trades[0].exit_price, 12.0);
    }
}

#[test]
fn v4_close_second_positional_argument_is_when() {
    let source = SourceFile::new(
        "v4-close-when.pine",
        r#"//@version=4
strategy("v4 close when")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
strategy.close("L", bar_index == 2)
plot(strategy.position_size)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.unwrap(),
        &[bar(100.0), bar(101.0), bar(102.0), bar(103.0)],
    )
    .unwrap();
    assert_values_close(&result.plots[0].values, &[0.0, 1.0, 1.0, 0.0]);
    let trades = &result.strategy.unwrap().trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].exit_bar_index, 3);
    assert_eq!(trades[0].exit_price, 103.0);
}

#[test]
fn v4_unpaired_trail_points_competes_with_fixed_profit_and_loss() {
    let source = SourceFile::new(
        "v4-trail-profit.pine",
        r#"//@version=4
strategy("v4 trail profit")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
strategy.exit("X", from_entry="L", profit=1000, trail_points=20, loss=600)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.unwrap(),
        &[bar(100.0), bar_ohlc(100.0, 101.0, 100.0, 100.5), bar(100.5)],
    )
    .unwrap();
    let trades = &result.strategy.unwrap().trades;
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].entry_price, 100.0);
    assert_eq!(trades[0].exit_price, 100.2);
}

#[test]
fn v4_unpaired_trail_points_alone_has_no_order_effect() {
    let source = SourceFile::new(
        "v4-trail-alone.pine",
        r#"//@version=4
strategy("v4 trail alone")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
strategy.exit("X", from_entry="L", trail_points=20)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.unwrap(),
        &[bar(100.0), bar_ohlc(100.0, 110.0, 90.0, 100.0)],
    )
    .unwrap();
    let strategy = result.strategy.unwrap();
    assert!(strategy.trades.is_empty());
    assert_eq!(strategy.position.last().map(|p| p.size), Some(1.0));
}

#[test]
fn native_pyramiding_profit_fields_include_commissions() {
    // Frozen FXCM EURUSD 4h native CSV, 2026-09-30; two entries closed separately.
    let bars = vec![
        Bar {
            time: 1688014800000,
            open: 1.08852,
            high: 1.09219,
            low: 1.0884,
            close: 1.09143,
            volume: 47882.0,
        },
        Bar {
            time: 1688029200000,
            open: 1.09143,
            high: 1.09414,
            low: 1.0873300000000001,
            close: 1.0881150000000002,
            volume: 50618.0,
        },
        Bar {
            time: 1688043600000,
            open: 1.0881150000000002,
            high: 1.08969,
            low: 1.08602,
            close: 1.08708,
            volume: 79217.0,
        },
        Bar {
            time: 1688058000000,
            open: 1.08708,
            high: 1.087515,
            low: 1.08622,
            close: 1.08654,
            volume: 24214.0,
        },
        Bar {
            time: 1688072400000,
            open: 1.08654,
            high: 1.087405,
            low: 1.08624,
            close: 1.087225,
            volume: 10468.0,
        },
        Bar {
            time: 1688086800000,
            open: 1.087225,
            high: 1.08755,
            low: 1.08603,
            close: 1.086995,
            volume: 23991.0,
        },
        Bar {
            time: 1688101200000,
            open: 1.086995,
            high: 1.0875599999999999,
            low: 1.08352,
            close: 1.08388,
            volume: 43002.0,
        },
        Bar {
            time: 1688115600000,
            open: 1.08388,
            high: 1.08977,
            low: 1.08354,
            close: 1.0897,
            volume: 49043.0,
        },
    ];
    let base = r#"//@version=5
strategy("Pyramiding fees probe 20260930", overlay=true, initial_capital=2000000, currency=currency.USD, pyramiding=2, commission_type=strategy.commission.cash_per_order, commission_value=10, slippage=0, margin_long=0, margin_short=0)
if time == 1688014800000
    strategy.entry("E1", strategy.short, qty=100000)
if time == 1688029200000
    strategy.entry("E2", strategy.short, qty=300000)
if time == 1688072400000
    strategy.close("E1")
if time == 1688086800000
    strategy.close("E2")
plot(strategy.openprofit_percent, title="Open Percent")
plot(strategy.opentrades.profit(0), title="Open Profit 0")
plot(strategy.opentrades.profit(1), title="Open Profit 1")
plot(strategy.opentrades.profit_percent(0), title="Open Percent 0")
plot(strategy.opentrades.profit_percent(1), title="Open Percent 1")
plot(strategy.closedtrades.profit_percent(0), title="Closed Percent 0")
plot(strategy.closedtrades.profit_percent(1), title="Closed Percent 1")
"#;
    let cases = [
        (
            "cash-short",
            [
                [
                    0.0,
                    0.01655008275041337,
                    0.0373503735037377,
                    0.048150481504817405,
                    0.034350343503435805,
                    0.016946695394399226,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    310.9999999999924,
                    415.0000000000076,
                    469.0000000000061,
                    399.9999999999982,
                    319.0000000000226,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    292.0000000000456,
                    454.00000000004104,
                    247.00000000001722,
                    0.0,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.28492116570318726,
                    0.3802002693467101,
                    0.4296721116231477,
                    0.3664580909365752,
                    0.0977190714543906,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    0.08944817825920923,
                    0.13907353743040446,
                    0.07566335626719273,
                    0.0,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.3664580909365752,
                    0.3664580909365752,
                    0.3664580909365752,
                ],
                [
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0977190714543906,
                    0.0977190714543906,
                ],
            ],
        ),
        (
            "cash-long",
            [
                [
                    0.0,
                    -0.01655008275041337,
                    -0.0373503735037377,
                    -0.048150481504817405,
                    -0.034350343503435805,
                    -0.016953814608287994,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    -350.9999999999924,
                    -455.0000000000076,
                    -509.0000000000061,
                    -439.9999999999982,
                    -359.0000000000226,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    -332.0000000000456,
                    -494.00000000004104,
                    -287.0000000000172,
                    0.0,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    -0.32156697479684476,
                    -0.41684607844036764,
                    -0.4663179207168053,
                    -0.4031039000302328,
                    -0.10997224655838941,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    -0.10170135336320806,
                    -0.15132671253440327,
                    -0.08791653137119156,
                    0.0,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    -0.4031039000302328,
                    -0.4031039000302328,
                    -0.4031039000302328,
                ],
                [
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    -0.10997224655838941,
                    -0.10997224655838941,
                ],
            ],
        ),
        (
            "percent-short",
            [
                [
                    0.0,
                    0.01655090320761401,
                    0.037358136209808726,
                    0.048160488848788485,
                    0.034357482698979036,
                    0.01695105352492876,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    113.04499999999238,
                    217.1490000000076,
                    271.2030000000061,
                    202.13399999999814,
                    -313.53299999997733,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    -340.5599999999544,
                    -178.39799999995893,
                    -385.60499999998274,
                    0.0,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.10347165455601175,
                    0.19875948794891335,
                    0.24823586297983602,
                    0.18501605044031033,
                    -0.09595135959875141,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    -0.10422250616345073,
                    -0.05459562677514796,
                    -0.11800775043799708,
                    0.0,
                    0.0,
                    0.0,
                ],
                [
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.18501605044031033,
                    0.18501605044031033,
                    0.18501605044031033,
                ],
                [
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    -0.09595135959875141,
                    -0.09595135959875141,
                ],
            ],
        ),
    ];
    for (label, expected) in cases {
        let mut script = base.to_owned();
        if label == "cash-long" {
            script = script.replace("strategy.short", "strategy.long");
        } else if label == "percent-short" {
            script = script
                .replace("commission.cash_per_order", "commission.percent")
                .replace("commission_value=10", "commission_value=0.1");
        }
        let source = SourceFile::new(label, script);
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let chart = ChartContext::default().with_price_grid(1, 100000).unwrap();
        let result = run_historical_with_request_environment(
            &analysis.hir.unwrap(),
            &bars,
            RequestEnvironment::default().for_chart(chart),
        )
        .unwrap();
        assert_eq!(result.plots.len(), expected.len());
        for (plot, native) in result.plots.iter().zip(expected) {
            assert_values_close(&plot.values, &native);
        }
        assert_eq!(result.strategy.unwrap().trades.len(), 2);
    }
}

fn at_time(time: i64, mut bar: Bar) -> Bar {
    bar.time = time;
    bar
}
