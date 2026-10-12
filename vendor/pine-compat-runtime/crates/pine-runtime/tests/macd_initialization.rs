use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bars() -> Vec<Bar> {
    (1..=64)
        .map(|n| Bar {
            time: n * 60_000,
            open: n as f64,
            high: 1000.0,
            low: 0.0,
            close: n as f64,
            volume: 1.0,
        })
        .collect()
}

#[test]
fn macd_seed_and_missing_samples_match_native_controls() {
    for (source, first_m, first_s, expected) in [
        ("[m,s,h]=ta.macd(close,12,26,9)", 25, 33, 7.0),
        (
            "src=bar_index%2==0 ? na : close\n[m,s,h]=ta.macd(src,2,3,2)",
            5,
            7,
            1.0,
        ),
    ] {
        let source =
            format!("//@version=6\nindicator(\"seed\")\n{source}\nplot(m)\nplot(s)\nplot(h)\n");
        let analysis = analyze_source(&SourceFile::new("macd.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&hir);
        runtime.append_bars(&bars()).unwrap();
        let output = runtime.result();
        assert_eq!(
            output.plots[0]
                .values
                .iter()
                .position(|v| v.as_f64().is_some()),
            Some(first_m)
        );
        assert_eq!(
            output.plots[1]
                .values
                .iter()
                .position(|v| v.as_f64().is_some()),
            Some(first_s)
        );
        assert_eq!(output.plots[0].values[first_m].as_f64(), Some(expected));
        assert_eq!(output.plots[1].values[first_s].as_f64(), Some(expected));
        if first_m == 5 {
            assert!(output.plots[0].values[10].as_f64().is_none());
            assert!(output.plots[1].values[10].as_f64().is_none());
            assert_eq!(output.plots[0].values[11].as_f64(), Some(1.0));
        }
    }
}

#[test]
fn macd_repeated_calls_and_forming_replacements_keep_the_final_bar_sample() {
    let source = r#"//@version=6
indicator("Repeated MACD")
float repeatedM=na
float repeatedS=na
for i=1 to 2
    [m,s,h]=ta.macd(close*10+i,2,3,2)
    repeatedM:=m
    repeatedS:=s
[onceM,onceS,onceH]=ta.macd(close*10+2,2,3,2)
plot(repeatedM)
plot(repeatedS)
plot(onceM)
plot(onceS)
"#;
    let analysis = analyze_source(&SourceFile::new("repeat.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let input = bars();
    let mut batch = HistoricalRuntime::new(&hir);
    batch.append_bars(&input).unwrap();
    let result = batch.result();
    assert_eq!(result.plots[0].values, result.plots[2].values);
    assert_eq!(result.plots[1].values, result.plots[3].values);
    let expected = public_runtime_result_json(&result);
    let mut incremental = HistoricalRuntime::new(&hir);
    let mut realtime = RealtimeRuntime::new(&hir);
    for bar in &input {
        incremental.append_bar(*bar).unwrap();
        let mut provisional = *bar;
        provisional.close += 100.0;
        realtime.update(BarUpdate::forming(provisional)).unwrap();
        realtime.update(BarUpdate::forming(*bar)).unwrap();
        let confirmed = realtime.update(BarUpdate::confirmed(*bar)).unwrap();
        assert_eq!(
            public_runtime_result_json(&confirmed),
            public_runtime_result_json(&incremental.result())
        );
    }
    assert_eq!(public_runtime_result_json(&incremental.result()), expected);
}
