use pine_runtime::{Bar, HistoricalRuntime};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn run(source: String) -> pine_runtime::RuntimeResult {
    let analysis = analyze_source(&SourceFile::new("comparison.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&[Bar {
            time: 0,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 1.0,
        }])
        .unwrap();
    runtime.result()
}

#[test]
fn native_boundary_preserves_raw_values_and_versioned_constant_behavior() {
    for version in [4, 5, 6] {
        let declaration = if version == 4 { "study" } else { "indicator" };
        let sign = if version == 4 { "sign" } else { "math.sign" };
        let source = format!(
            r#"//@version={version}
{declaration}("comparison")
a=close
b=a+0.00000000005
plot(a==b?1:0)
plot(a!=b?1:0)
plot(a<b?1:0)
plot(a<=b?1:0)
plot(a>b?1:0)
plot(a>=b?1:0)
plot({sign}(a-b))
plot(0.0==0.00000000005?1:0)
plot(0.1+0.2==0.3?1:0)
plot(0.0==0.0000000001001?1:0)
"#
        );
        let result = run(source);
        let constant = if version == 4 { 0.0 } else { 1.0 };
        for (plot, expected) in result
            .plots
            .iter()
            .zip([1.0, 0.0, 0.0, 1.0, 0.0, 1.0, -1.0, constant, constant, 0.0])
        {
            assert_eq!(plot.values[0].as_f64(), Some(expected), "version {version}");
        }
    }
}

#[test]
fn history_bounds_and_array_lookup_use_the_same_modern_comparison() {
    let result = run(r#"//@version=6
indicator("constant bounds")
max_bars_back(close,2)
offset=0.0==0.00000000005?1:99
plot(close[offset])
a=array.from(close)
plot(array.indexof(a,close+0.00000000005))
plot(array.includes(a,close+0.00000000005)?1:0)
plot(close==close+0.000000001?1:0)
"#
    .to_owned());
    assert!(result.plots[0].values[0].is_na());
    assert_eq!(result.plots[1].values[0].as_f64(), Some(0.0));
    assert_eq!(result.plots[2].values[0].as_f64(), Some(1.0));
    assert_eq!(result.plots[3].values[0].as_f64(), Some(0.0));
}
