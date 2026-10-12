use pine_runtime::{Bar, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bars() -> [Bar; 3] {
    [0, 1, 2].map(|index| Bar {
        time: index * 86_400_000,
        open: 10.0,
        high: 10.0,
        low: 10.0,
        close: 10.0,
        volume: 1.0,
    })
}

fn compile(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("qty.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

#[test]
fn negative_entry_and_order_quantities_stop_script_at_call_bar() {
    for command in ["entry", "order"] {
        let source = format!(
            "//@version=6\nstrategy(\"invalid quantity\", margin_long=0)\nstrategy.{command}(\"L\", strategy.long, qty=bar_index == 0 ? 1 : -2)\n"
        );
        let error = run_historical(&compile(&source), &bars()).expect_err("negative quantity");
        assert!(
            error.message.contains(&format!(
                "`strategy.{command}` invalid `qty` value (-2) on bar 1"
            )),
            "{}",
            error.message
        );
    }
}

#[test]
fn zero_quantity_is_noop_and_na_uses_declared_default() {
    for command in ["entry", "order"] {
        let source = format!(
            "//@version=6\nstrategy(\"quantity fallback\", default_qty_value=2)\nstrategy.{command}(\"Z\", strategy.long, qty=0)\nif bar_index == 0\n    strategy.{command}(\"L\", strategy.long, qty=na)\n"
        );
        let result = run_historical(&compile(&source), &bars()).expect("valid quantities");
        let strategy = result.strategy.expect("strategy result");
        assert_eq!(strategy.orders.len(), 1);
        assert_eq!(strategy.orders[0].id, "L");
        assert_eq!(strategy.orders[0].qty, 2.0);
    }
}

#[test]
fn quantity_above_native_limit_stops_script() {
    let program = compile(
        "//@version=6\nstrategy(\"quantity limit\")\nstrategy.entry(\"L\", strategy.long, qty=1000000000001)\n",
    );
    let error = run_historical(&program, &bars()).expect_err("oversized quantity");
    assert!(
        error
            .message
            .contains("invalid `qty` value (1000000000001) on bar 0"),
        "{}",
        error.message
    );
}
