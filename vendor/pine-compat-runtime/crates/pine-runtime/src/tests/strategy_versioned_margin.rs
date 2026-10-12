use super::*;

#[test]
fn source_version_selects_default_margin_and_explicit_zero_overrides_it() {
    for direction in ["long", "short"] {
        for (version, properties, margin, admitted) in [
            (5, "", 0.0, true),
            (6, "", 100.0, false),
            (6, ", margin_long=0, margin_short=0", 0.0, true),
            (5, ", margin_long=100, margin_short=100", 100.0, false),
        ] {
            let source = pine_syntax::SourceFile::new(
                "versioned_margin.pine",
                format!(
                    "//@version={version}\nstrategy(\"Margin\", initial_capital=1000{properties})\nif bar_index == 0\n    strategy.entry(\"E\", strategy.{direction}, qty=20)\nplot(strategy.position_size)\n"
                ),
            );
            let analysis = analyze_source(&source);
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            let hir = analysis.hir.expect("HIR");
            assert_eq!(hir.strategy_settings.margin_long.value_percent, margin);
            assert_eq!(hir.strategy_settings.margin_short.value_percent, margin);
            assert_eq!(
                hir.strategy_settings.margin_long.explicit,
                !properties.is_empty()
            );
            let result = run_historical(&hir, &[bar(100.0); 3]).unwrap();
            assert_eq!(result.strategy.unwrap().orders.len(), usize::from(admitted));
        }
    }
}

#[test]
fn manually_constructed_v6_hir_resolves_omitted_margin_in_the_runtime() {
    let source = pine_syntax::SourceFile::new(
        "manual_margin.pine",
        "//@version=6\nstrategy(\"Manual\", initial_capital=1000)\nif bar_index == 0\n    strategy.entry(\"E\", strategy.long, qty=20)\n",
    );
    let mut hir = analyze_source(&source).hir.expect("HIR");
    hir.strategy_settings.margin_long = Default::default();
    hir.strategy_settings.margin_short = Default::default();
    let result = run_historical(&hir, &[bar(100.0); 3]).unwrap();
    assert!(result.strategy.unwrap().orders.is_empty());
}
