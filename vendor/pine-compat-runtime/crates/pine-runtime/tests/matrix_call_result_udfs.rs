use pine_runtime::{Bar, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn matrix_call_result_mutation_in_udfs_preserves_copy_and_return_values() {
    for version in [5, 6] {
        let source = format!(
            "//@version={version}\nindicator(\"copy mutations\")\nremoved(m)=>\n    matrix.copy(m).remove_row(0).get(0)\nfillTemporary()=>\n    matrix.new<float>(1,1,0).fill(close)\n    close\nm=matrix.new<float>(1,1,close+7)\nplot(removed(m))\nplot(m.get(0,0))\nplot(m.rows())\nplot(fillTemporary())\n"
        );
        let analysis = analyze_source(&SourceFile::new("matrix-calls.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let bars = (0..3)
            .map(|i| Bar {
                time: i * 60000,
                open: 10. + i as f64,
                high: 10. + i as f64,
                low: 10. + i as f64,
                close: 10. + i as f64,
                volume: 1.,
            })
            .collect::<Vec<_>>();
        let result = run_historical(&analysis.hir.unwrap(), &bars).unwrap();
        for (index, bar) in bars.iter().enumerate() {
            for (plot, value) in
                result
                    .plots
                    .iter()
                    .zip([bar.close + 7., bar.close + 7., 1., bar.close])
            {
                assert_eq!(plot.values[index].as_f64(), Some(value));
            }
        }
    }
}
