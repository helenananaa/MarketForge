use pine_runtime::{
    Bar, ChartContext, HistoricalRuntime, RequestEnvironment, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(time: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        time,
        open,
        high,
        low,
        close,
        volume: 1.0,
    }
}

fn run(source: &str, bars: &[Bar], chart: ChartContext) -> serde_json::Value {
    let a = analyze_source(&SourceFile::new("g3.pine", source));
    assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
    let hir = a.hir.unwrap();
    let mut rt = HistoricalRuntime::with_request_environment(
        &hir,
        RequestEnvironment::default().for_chart(chart),
    );
    rt.append_bars(bars).unwrap();
    serde_json::from_str(&public_runtime_result_json(&rt.result())).unwrap()
}

#[test]
fn host_price_grid_drives_builtin_rounding_formatting_and_trailing_fill() {
    let src = r#"//@version=6
strategy("grid", initial_capital=10000)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
    strategy.exit("T", "L", trail_price=105, trail_offset=5)
plot(syminfo.mintick)
plot(syminfo.minmove)
plot(syminfo.pricescale)
plot(math.round_to_mintick(10.26))
plot(str.tostring(10.26, format.mintick) == "10.3" ? 1 : 0)
plot(str.tostring(array.from(10.26), format.mintick) == "[10.3]" ? 1 : 0)
"#;
    let bars = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 110.0, 99.0, 100.0),
    ];
    let r = run(
        src,
        &bars,
        ChartContext::default().with_price_grid(1, 10).unwrap(),
    );
    assert_eq!(r["plots"][0]["values"], serde_json::json!([0.1, 0.1]));
    assert_eq!(r["plots"][1]["values"], serde_json::json!([1, 1]));
    assert_eq!(r["plots"][2]["values"], serde_json::json!([10, 10]));
    assert_eq!(r["plots"][4]["values"], serde_json::json!([1, 1]));
    assert_eq!(r["plots"][5]["values"], serde_json::json!([1, 1]));
    assert!((r["plots"][3]["values"][0].as_f64().unwrap() - 10.3).abs() < 1e-9);
    assert_eq!(r["strategy"]["trades"][0]["exitPrice"], 109.5);
    let default = run(src, &bars, ChartContext::default());
    assert_eq!(
        default["plots"][0]["values"],
        serde_json::json!([0.01, 0.01])
    );
    assert_eq!(default["strategy"]["trades"][0]["exitPrice"], 109.95);
    assert!(ChartContext::default().with_price_grid(0, 10).is_err());
    assert!(ChartContext::default().with_price_grid(1, 0).is_err());
}

#[test]
fn rejected_cash_fee_reversal_keeps_original_position_and_fees() {
    let src = r#"//@version=6
strategy("reject",initial_capital=1000,margin_long=100,margin_short=100,commission_type=strategy.commission.cash_per_order,commission_value=3)
if bar_index == 0
    strategy.entry("L",strategy.long,qty=1)
if bar_index == 1
    strategy.entry("S",strategy.short,qty=10000)
plot(strategy.position_size)
plot(strategy.netprofit)
"#;
    let bars = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.0, 100.0, 100.0),
        bar(2, 100.0, 100.0, 100.0, 100.0),
    ];
    let r = run(src, &bars, ChartContext::default());
    assert_eq!(r["plots"][0]["values"], serde_json::json!([0, 1, 1]));
    assert_eq!(r["plots"][1]["values"], serde_json::json!([0, -3, -3]));
    assert!(r["strategy"]["trades"].as_array().unwrap().is_empty());
    assert_eq!(r["strategy"]["orders"].as_array().unwrap().len(), 1);
}

#[test]
fn cash_per_order_reversal_charges_once_in_both_directions() {
    for (first, second, sign) in [("long", "short", 1.0), ("short", "long", -1.0)] {
        let src = format!(
            r#"//@version=6
strategy("fees",initial_capital=10000,commission_type=strategy.commission.cash_per_order,commission_value=3)
if bar_index == 0
    strategy.entry("A",strategy.{first},qty=2)
if bar_index == 2
    strategy.entry("B",strategy.{second},qty=1)
if bar_index == 4
    strategy.close_all()
plot(strategy.closedtrades.commission(0))
plot(strategy.closedtrades.commission(1))
plot(strategy.closedtrades.commission(-1))
plot(strategy.closedtrades.commission(99))
plot(na(strategy.closedtrades.entry_price(99)) ? 1 : 0)
plot(strategy.netprofit)
"#
        );
        let bars: Vec<_> = [100.0, 100.0, 110.0, 110.0, 120.0, 120.0]
            .into_iter()
            .enumerate()
            .map(|(i, p)| bar(i as i64, p, p, p, p))
            .collect();
        let r = run(&src, &bars, ChartContext::default());
        assert_eq!(r["strategy"]["trades"].as_array().unwrap().len(), 2);
        assert_eq!(r["strategy"]["trades"][0]["profit"], 20.0 * sign - 5.0);
        assert_eq!(r["strategy"]["trades"][1]["profit"], -10.0 * sign - 4.0);
        assert_eq!(r["plots"][0]["values"][5], 5.0);
        assert_eq!(r["plots"][1]["values"][5], 4.0);
        assert_eq!(r["plots"][0]["values"][0], 0.0);
        assert_eq!(r["plots"][5]["values"][1], -3.0);
        assert_eq!(r["plots"][5]["values"][3], 20.0 * sign - 6.0);
        assert_eq!(r["plots"][5]["values"][5], 10.0 * sign - 9.0);
        assert_eq!(
            r["plots"][2]["values"],
            serde_json::json!([0, 0, 0, 0, 0, 0])
        );
        assert_eq!(
            r["plots"][3]["values"],
            serde_json::json!([0, 0, 0, 0, 0, 0])
        );
        assert_eq!(
            r["plots"][4]["values"],
            serde_json::json!([1, 1, 1, 1, 1, 1])
        );
    }
}

#[test]
fn absent_closed_trade_profit_is_zero_without_changing_identity_or_na_indices() {
    let source = include_str!("../../../tests/fixtures/runtime/strategy_absent_trade_profit.pine");
    let bars = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.0, 100.0, 100.0),
        bar(2, 110.0, 110.0, 110.0, 110.0),
        bar(3, 110.0, 110.0, 110.0, 110.0),
    ];
    for version in [5, 6] {
        let r = run(
            &source.replace("//@version=6", &format!("//@version={version}")),
            &bars,
            ChartContext::default(),
        );
        assert_eq!(r["plots"][0]["values"], serde_json::json!([0, 0, 0, 10]));
        for index in [1, 2] {
            assert_eq!(r["plots"][index]["values"], serde_json::json!([0, 0, 0, 0]));
        }
        for index in [3, 4] {
            assert_eq!(
                r["plots"][index]["values"],
                serde_json::json!([null, null, null, null])
            );
        }
    }
}
