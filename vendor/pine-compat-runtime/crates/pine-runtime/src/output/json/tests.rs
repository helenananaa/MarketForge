use super::*;
use crate::{
    RuntimeChanges, RuntimeDiagnostic, StrategyEquitySnapshot, StrategyOrderEvent,
    StrategyOrderFillAlertOutput, StrategyPositionSnapshot, StrategyResult, StrategyTrade,
};

pub(super) fn empty_result() -> RuntimeResult {
    RuntimeResult {
        plots: Vec::new(),
        plot_chars: Vec::new(),
        plot_shapes: Vec::new(),
        plot_arrows: Vec::new(),
        plot_bars: Vec::new(),
        plot_candles: Vec::new(),
        bg_colors: Vec::new(),
        bar_colors: Vec::new(),
        hlines: Vec::new(),
        fills: Vec::new(),
        labels: Vec::new(),
        lines: Vec::new(),
        line_fills: Vec::new(),
        polylines: Vec::new(),
        boxes: Vec::new(),
        tables: Vec::new(),
        alerts: Vec::new(),
        strategy: None,
        diagnostics: Vec::new(),
    }
}

#[test]
fn runtime_json_serializes_non_finite_plot_floats_as_null() {
    let mut result = empty_result();
    result.plots.push(PlotSeries::new(
        1,
        vec![
            PineValue::Float(f64::NAN),
            PineValue::Float(f64::INFINITY),
            PineValue::Float(1.5),
        ],
    ));

    let output = public_runtime_result_json(&result);

    assert!(output.contains(r#""values":[null,null,1.5]"#));
    assert!(!output.contains("NaN"));
    assert!(!output.contains("inf"));
}

#[test]
fn runtime_json_serializes_top_level_diagnostics() {
    let mut result = empty_result();
    result.diagnostics.push(RuntimeDiagnostic {
        code: "E_RUNTIME".to_owned(),
        message: "runtime \"warning\"\nline".to_owned(),
    });

    let output = public_runtime_result_json(&result);

    assert!(
        output.contains(
            r#""diagnostics":[{"code":"E_RUNTIME","message":"runtime \"warning\"\nline"}]"#
        ),
        "{output}"
    );
}

#[test]
fn runtime_json_serializes_non_finite_strategy_floats_as_null() {
    let mut result = empty_result();
    result.strategy = Some(StrategyResult {
        orders: vec![StrategyOrderEvent {
            id: "O".to_owned(),
            bar_index: 0,
            time: 10,
            direction: "long".to_owned(),
            qty: f64::INFINITY,
            price: f64::NAN,
        }],
        trades: vec![StrategyTrade {
            id: "T".to_owned(),
            exit_id: "X".to_owned(),
            entry_bar_index: 0,
            exit_bar_index: 1,
            entry_time: 10,
            exit_time: 20,
            entry_price: f64::NAN,
            exit_price: f64::NEG_INFINITY,
            qty: 1.0,
            profit: f64::INFINITY,
        }],
        position: vec![StrategyPositionSnapshot {
            bar_index: 0,
            size: f64::INFINITY,
            avg_price: Some(f64::NAN),
        }],
        equity: vec![StrategyEquitySnapshot {
            bar_index: 0,
            cash: f64::NAN,
            market_value: f64::INFINITY,
            equity: f64::NEG_INFINITY,
            net_profit: 2.0,
        }],
        alerts: vec![StrategyOrderFillAlertOutput {
            id: "A".to_owned(),
            bar_index: 0,
            time: 10,
            direction: "strategy.exit".to_owned(),
            qty: f64::INFINITY,
            price: f64::NAN,
            entry_id: Some("T".to_owned()),
            exit_id: None,
            message: "message".to_owned(),
        }],
        diagnostics: Vec::new(),
    });

    let output = public_runtime_result_json(&result);

    assert!(output.contains(r#""qty":null,"price":null"#));
    assert!(output.contains(r#""entryPrice":null,"exitPrice":null,"qty":1,"profit":null"#));
    assert!(output.contains(r#""size":null,"avgPrice":null"#));
    assert!(output.contains(r#""cash":null,"marketValue":null,"equity":null,"netProfit":2"#));
    assert!(output.contains(r#""qty":null,"price":null,"entryId":"T","exitId":null"#));
    assert!(!output.contains("NaN"));
    assert!(!output.contains("inf"));
}

#[test]
fn changes_json_serializes_schema_visibility_and_series_delta() {
    use crate::{
        PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION, SeriesChange, SeriesChangeOp, SeriesFamily,
        SeriesFields, StreamingVisibility,
    };

    let mut changes = RuntimeChanges::new(4, StreamingVisibility::Preview);
    changes.retained_from = 2;
    changes.series.push(SeriesChange {
        family: SeriesFamily::Plot,
        id: 1,
        op: SeriesChangeOp::ReplaceLast,
        start: 3,
        fields: SeriesFields {
            values: vec![PineValue::Int(13)],
            ..SeriesFields::default()
        },
        header: None,
    });

    let output = public_runtime_changes_json(&changes);
    assert!(output.contains(&format!(
        r#""schemaVersion":{PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION}"#
    )));
    assert!(output.contains(r#""revision":4"#));
    assert!(output.contains(r#""baseRevision":3"#));
    assert!(output.contains(r#""retainedFrom":2"#));
    assert!(output.contains(r#""visibility":"preview""#));
    assert!(output.contains(r#""op":"replaceLast""#));
    assert!(output.contains(r#""values":[13]"#));
    assert!(!output.contains("NaN"));
}
