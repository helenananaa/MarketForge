use pine_runtime::{Bar, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn table_creation_is_not_limited_by_cumulative_lifetime_count() {
    let source = SourceFile::new(
        "tables.pine",
        "//@version=5\nindicator(\"tables\")\nfor i=0 to 50\n    table.new(position.top_left,1,1)\nplot(close)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let bars = (0..2)
        .map(|index| Bar {
            time: index,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        })
        .collect::<Vec<_>>();
    let result = run_historical(&analysis.hir.unwrap(), &bars).unwrap();
    assert_eq!(result.tables.len(), 102);
}
