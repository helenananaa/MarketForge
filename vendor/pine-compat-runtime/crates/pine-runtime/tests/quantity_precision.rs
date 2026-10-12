use pine_runtime::{Bar, ChartContext, HistoricalRuntime, RequestEnvironment};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn explicit_point_value_accepts_only_the_existing_unit_profile() {
    let chart = ChartContext::default()
        .with_price_grid(1, 10)
        .unwrap()
        .with_quantity_precision(6)
        .unwrap();
    assert_eq!(chart.clone().with_point_value(1.0).unwrap(), chart);
    assert_eq!(chart.point_value(), 1.0);
    for value in [
        0.0,
        -1.0,
        0.5,
        5.0,
        1.0000000001,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert!(
            chart
                .clone()
                .with_point_value(value)
                .unwrap_err()
                .contains("non-unit contract multipliers")
        );
    }
}

fn execute(precision: u32) -> pine_runtime::RuntimeResult {
    let source = r#"//@version=6
strategy("Quantity precision",initial_capital=40450,margin_long=50,margin_short=50)
if bar_index==0
    strategy.entry("Long",strategy.long,qty=1)
if bar_index==2
    strategy.close_all()
plot(syminfo.mincontract)
lookback=syminfo.mincontract==1?1:2
plot(close[lookback])
f(simple float value=syminfo.mincontract) => value
plot(f())
int missing=na
plot(strategy.closedtrades.size(missing))
plot(strategy.closedtrades.size(-1))
plot(strategy.opentrades.size(missing))
"#;
    let analysis = analyze_source(&SourceFile::new("precision.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let chart = ChartContext::default()
        .with_price_grid(1, 10)
        .unwrap()
        .with_quantity_precision(precision)
        .unwrap();
    let mut runtime = HistoricalRuntime::with_request_environment(
        &hir,
        RequestEnvironment::default().for_chart(chart),
    );
    let bars = quantity_bars();
    runtime.append_bars(&bars).unwrap();
    runtime.result()
}

#[test]
fn host_quantity_precision_enables_native_fractional_margin_cover() {
    let result = execute(6);
    assert_eq!(result.plots[0].values[0].as_f64(), Some(0.000001));
    assert_eq!(result.plots[2].values[0].as_f64(), Some(0.000001));
    assert_eq!(result.plots[3].values[0].as_f64(), Some(0.0));
    assert_eq!(result.plots[3].values[3].as_f64(), Some(0.009996));
    assert_eq!(result.plots[4].values[3].as_f64(), Some(0.0));
    assert_eq!(result.plots[5].values[1].as_f64(), Some(1.0));
    assert_eq!(result.plots[5].values[3].as_f64(), Some(0.0));
    let strategy = result.strategy.unwrap();
    assert_eq!(strategy.trades.len(), 2);
    assert!((strategy.trades[0].qty - 0.009996).abs() < 1e-12);
    assert_eq!(strategy.trades[0].exit_bar_index, 2);
    assert!((strategy.trades[1].qty - 0.990004).abs() < 1e-12);
}

#[test]
fn default_integer_profile_is_preserved_and_precision_is_validated() {
    assert_eq!(execute(0).strategy.unwrap().trades.len(), 1);
    assert_eq!(ChartContext::default().min_contract(), 1.0);
    assert!(ChartContext::default().with_quantity_precision(10).is_err());
}

#[test]
fn runtime_quantity_metadata_is_not_folded_to_the_default_history_offset() {
    let result = execute(6);
    assert_eq!(result.plots[1].values[3].as_f64(), Some(80838.5));
}

#[test]
fn short_fractional_margin_cover_preserves_signed_size_and_remaining_profit() {
    let source = include_str!("../../../tests/fixtures/runtime/quantity_precision.pine")
        .replace("strategy.long", "strategy.short");
    let analysis = analyze_source(&SourceFile::new("short-precision.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let chart = ChartContext::default()
        .with_price_grid(1, 10)
        .unwrap()
        .with_quantity_precision(6)
        .unwrap();
    let mut runtime = HistoricalRuntime::with_request_environment(
        &hir,
        RequestEnvironment::default().for_chart(chart),
    );
    runtime.append_bars(&quantity_bars()).unwrap();
    let result = runtime.result();
    let trades = &result.strategy.as_ref().unwrap().trades;
    assert_eq!(trades.len(), 2);
    assert_eq!(trades[0].exit_bar_index, 1);
    assert_eq!(trades[0].exit_price, 81016.5);
    assert_eq!(trades[0].qty, -0.023516);
    assert_eq!(trades[1].qty, -0.976484);
    assert!((trades.iter().map(|trade| trade.profit).sum::<f64>() - 177.981386).abs() < 1e-9);
    assert_eq!(result.plots[5].values[3].as_f64(), Some(-0.023516));
}

#[test]
fn fractional_margin_history_parity_and_realtime_liquidation_persistence() {
    use pine_runtime::{BarUpdate, RealtimeRuntime, public_runtime_result_json};

    let source = include_str!("../../../tests/fixtures/runtime/quantity_precision.pine").replace(
        "margin_short=50)",
        "margin_short=50,calc_on_every_tick=true)",
    );
    let analysis = analyze_source(&SourceFile::new("quantity-modes.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let environment = RequestEnvironment::default().for_chart(
        ChartContext::default()
            .with_price_grid(1, 10)
            .unwrap()
            .with_quantity_precision(6)
            .unwrap(),
    );
    let bars = quantity_bars();
    let mut batch = HistoricalRuntime::with_request_environment(&hir, environment.clone());
    batch.append_bars(&bars).unwrap();
    let expected = public_runtime_result_json(&batch.result());
    let mut incremental = HistoricalRuntime::with_request_environment(&hir, environment.clone());
    let mut streamed = RealtimeRuntime::with_request_environment(&hir, environment.clone());
    for bar in &bars {
        incremental.append_bar(*bar).unwrap();
        streamed.update(BarUpdate::historical(*bar)).unwrap();
    }
    assert_eq!(public_runtime_result_json(&incremental.result()), expected);
    assert_eq!(public_runtime_result_json(&streamed.result()), expected);

    // Internal lifecycle control: an accepted losing mark can liquidate.
    // Later recovery must preserve that trade, unlike a runtime that never saw it.
    // This is not an independent TradingView tick-sequence oracle.
    let mut realtime = RealtimeRuntime::with_request_environment(&hir, environment.clone());
    realtime.seed_historical(&bars[..2]).unwrap();
    let confirmed = public_runtime_result_json(&realtime.confirmed_result());
    let mut losing = bars[2];
    losing.close = losing.low;
    let forming = realtime.update(BarUpdate::forming(losing)).unwrap();
    let liquidations = forming.strategy.unwrap().trades;
    assert!(!liquidations.is_empty());
    assert_eq!(
        public_runtime_result_json(&realtime.confirmed_result()),
        confirmed
    );
    let replacement = realtime.update(BarUpdate::forming(bars[2])).unwrap();
    let mut control = RealtimeRuntime::with_request_environment(&hir, environment);
    control.seed_historical(&bars[..2]).unwrap();
    let control_forming = control.update(BarUpdate::forming(bars[2])).unwrap();
    let replacement_trades = &replacement.strategy.as_ref().unwrap().trades;
    assert!(replacement_trades.starts_with(&liquidations));
    // The script also requested strategy.close_all on the losing tick. That order
    // fills the remaining position on this next observed price, after the
    // previously recorded partial liquidation, instead of being rolled back.
    assert_eq!(replacement_trades.len(), liquidations.len() + 1);
    let close = replacement_trades.last().unwrap();
    assert_eq!(close.exit_id, "Long");
    assert_eq!(close.exit_price, bars[2].close);
    assert_eq!(close.qty, 0.990004);
    assert_ne!(
        public_runtime_result_json(&replacement),
        public_runtime_result_json(&control_forming)
    );
    for bar in &bars[2..] {
        realtime.update(BarUpdate::confirmed(*bar)).unwrap();
        control.update(BarUpdate::confirmed(*bar)).unwrap();
    }
    assert!(
        realtime
            .result()
            .strategy
            .as_ref()
            .unwrap()
            .trades
            .starts_with(&liquidations)
    );
    assert_ne!(
        public_runtime_result_json(&realtime.result()),
        public_runtime_result_json(&control.result())
    );
}

fn quantity_bars() -> Vec<Bar> {
    let prices = [
        (80836.6, 80836.6, 80836.6, 80836.6),
        (80836.6, 81016.5, 80800.0, 80838.5),
        (80990.0, 81090.3, 80571.8, 80808.5),
        (80650.0, 81240.3, 80503.9, 81156.8),
    ];
    prices
        .into_iter()
        .enumerate()
        .map(|(i, (open, high, low, close))| Bar {
            time: i as i64 * 3600000,
            open,
            high,
            low,
            close,
            volume: 1.0,
        })
        .collect()
}
