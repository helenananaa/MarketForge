use super::*;

#[test]
fn native_v5_v6_default_capital_and_explicit_override() {
    for version in [5, 6] {
        for (declaration, expected) in [("", 1_000_000.0), (", initial_capital=1234", 1234.0)] {
            let source = pine_syntax::SourceFile::new(
                "capital.pine",
                format!(
                    "//@version={version}\nstrategy(\"Capital\"{declaration})\nplot(strategy.initial_capital)\nplot(strategy.equity)\n"
                ),
            );
            let analysis = analyze_source(&source);
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            let result = run_historical(&analysis.hir.expect("HIR"), &[bar(100.0)]).unwrap();
            assert_values_close(&result.plots[0].values, &[expected]);
            assert_values_close(&result.plots[1].values, &[expected]);
        }
    }
}

#[test]
fn native_open_commission_absent_indices_are_zero_before_and_after_trade() {
    for version in [5, 6] {
        let source = pine_syntax::SourceFile::new(
            "commission.pine",
            format!(
                r#"//@version={version}
strategy("Commission", initial_capital=1000, commission_type=strategy.commission.cash_per_order, commission_value=1.5)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
if bar_index == 2
    strategy.close("L")
plot(strategy.opentrades.commission(-1))
plot(strategy.opentrades.commission(0))
plot(strategy.opentrades.commission(1))
plot(na(strategy.opentrades.commission(0)) ? 1 : 0)
plot(strategy.opentrades.entry_price(-1))
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(&analysis.hir.expect("HIR"), &[bar(100.0); 4]).unwrap();
        assert_values_close(&result.plots[0].values, &[0.0; 4]);
        assert_values_close(&result.plots[1].values, &[0.0, 1.5, 1.5, 0.0]);
        assert_values_close(&result.plots[2].values, &[0.0; 4]);
        assert_values_close(&result.plots[3].values, &[0.0; 4]);
        assert_eq!(result.plots[4].values, vec![PineValue::Na; 4]);
    }
}
