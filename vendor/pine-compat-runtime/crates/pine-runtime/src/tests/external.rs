use super::*;

fn program(text: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&modern_source_file("external", text));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn account(time: i64, qty: f64) -> ExternalAccountFrame {
    ExternalAccountFrame {
        time,
        position_size: qty,
        position_avg_price: (qty != 0.0).then_some(10.0),
        equity: 1000.0,
        initial_capital: 1000.0,
        netprofit: 0.0,
        openprofit: 0.0,
    }
}

#[test]
fn external_mode_never_exposes_a_native_ledger_on_any_result_surface() {
    let hir = program(
        "strategy(\"External\")\nstrategy.entry(\"L\", strategy.long)\nplot(strategy.position_size[1])",
    );
    let bars = [
        bar(10.0),
        Bar {
            time: 60000,
            ..bar(10.0)
        },
    ];
    let mut runtime = HistoricalRuntime::new(&hir)
        .with_external_accounts(vec![account(0, 2.0), account(60000, 0.0)])
        .unwrap();
    runtime.append_bars(&bars).unwrap();
    assert!(runtime.result().strategy.is_none());
    assert!(runtime.result_view().strategy.is_none());
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Na, PineValue::Float(2.0)]
    );
    assert_eq!(runtime.external_intents().unwrap().len(), 2);
    assert!(!public_runtime_result_json(&runtime.result()).contains("\"strategy\""));
}

#[test]
fn unsupported_account_read_is_an_execution_error_and_disables_reuse() {
    let hir = program("strategy(\"Unsupported\")\nplot(strategy.wintrades)");
    let mut runtime = HistoricalRuntime::new(&hir)
        .with_external_accounts(vec![account(0, 0.0)])
        .unwrap();
    assert!(
        runtime
            .append_bar(bar(10.0))
            .unwrap_err()
            .message
            .contains("E_EXTERNAL_UNSUPPORTED")
    );
    assert!(
        runtime
            .append_bar(bar(10.0))
            .unwrap_err()
            .message
            .contains("E_RUNTIME_POISONED")
    );
}

#[test]
fn historical_horizon_can_only_be_set_before_execution() {
    let hir = program("indicator(\"Horizon\")\nplot(last_bar_index)");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.set_historical_horizon(2, 60000).unwrap();
    runtime.append_bar(bar(10.0)).unwrap();
    let prior = runtime.result();
    assert!(runtime.set_historical_horizon(3, 120000).is_err());
    assert_eq!(runtime.result(), prior);
    runtime
        .append_bar(Bar {
            time: 60000,
            ..bar(10.0)
        })
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(1)]
    );
}
