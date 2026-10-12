use pine_runtime::{Bar, PineValue, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn for_in_rechecks_array_size_after_pop_or_clear() {
    for (mutation, expected_size) in [("array.pop(values)", 1), ("array.clear(values)", 0)] {
        let source = SourceFile::new(
            "for_in_shrink.pine",
            format!(
                "//@version=5\nindicator(\"shrink\")\nvalues=array.from(1,2)\nfor value in values\n    {mutation}\nplot(array.size(values))\n"
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
            &[Bar {
                time: 1,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            }],
        )
        .unwrap();
        assert_eq!(result.plots[0].values, vec![PineValue::Int(expected_size)]);
    }
}
