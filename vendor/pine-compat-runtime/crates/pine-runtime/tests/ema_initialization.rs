use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bars() -> Vec<Bar> {
    (1..=7)
        .map(|n| Bar {
            time: n * 60_000,
            open: n as f64,
            high: n as f64 + 10.0,
            low: 0.0,
            close: n as f64,
            volume: 1.0,
        })
        .collect()
}

#[test]
fn ema_seeds_with_mean_and_keeps_callsites_independent() {
    for version in [5, 6] {
        let source = format!(
            "//@version={version}\nindicator(\"seed\")\nplot(ta.ema(close,3))\nplot(ta.ema(close,2))\nplot(ta.ema(close,1))\n"
        );
        let analysis = analyze_source(&SourceFile::new("seed.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let input = bars();
        let mut batch = HistoricalRuntime::new(&hir);
        batch.append_bars(&input[..4]).unwrap();
        let result = batch.result();
        assert_eq!(
            result.plots[0].values,
            vec![
                PineValue::Na,
                PineValue::Na,
                PineValue::Float(2.0),
                PineValue::Float(3.0)
            ]
        );
        assert_eq!(
            result.plots[1].values,
            vec![
                PineValue::Na,
                PineValue::Float(1.5),
                PineValue::Float(2.5),
                PineValue::Float(3.5)
            ]
        );
        assert_eq!(
            result.plots[2].values,
            vec![
                PineValue::Float(1.0),
                PineValue::Float(2.0),
                PineValue::Float(3.0),
                PineValue::Float(4.0)
            ]
        );
        let expected = public_runtime_result_json(&result);
        let mut incremental = HistoricalRuntime::new(&hir);
        for bar in &input[..4] {
            incremental.append_bar(*bar).unwrap();
        }
        assert_eq!(public_runtime_result_json(&incremental.result()), expected);
        // Form and replace each bar, including both unseeded and seed bars.
        let mut realtime = RealtimeRuntime::new(&hir);
        realtime.seed_historical(&input[..1]).unwrap();
        for original in &input[1..4] {
            for replacement in [8.0, 9.0, original.close] {
                let mut forming = *original;
                forming.close = replacement;
                realtime.update(BarUpdate::forming(forming)).unwrap();
            }
            realtime.update(BarUpdate::confirmed(*original)).unwrap();
        }
        let confirmed = realtime.update(BarUpdate::forming(input[4])).unwrap();
        assert_eq!(
            &confirmed.plots[0].values[..4],
            result.plots[0].values.as_slice()
        );
    }
}

#[test]
fn missing_observations_do_not_count_toward_seed_or_destroy_previous_value() {
    let source = "//@version=6\nindicator(\"gaps\")\nx = bar_index == 0 or bar_index == 2 or bar_index == 5 ? float(na) : close\nplot(ta.ema(x,3))\n";
    let analysis = analyze_source(&SourceFile::new("gaps.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&bars()).unwrap();
    let result = runtime.result();
    assert!(
        result.plots[0].values[..4]
            .iter()
            .all(|value| matches!(value, PineValue::Na))
    );
    assert_eq!(result.plots[0].values[4], PineValue::Float(11.0 / 3.0));
    assert_eq!(result.plots[0].values[5], PineValue::Na);
    assert_eq!(
        result.plots[0].values[6],
        PineValue::Float(0.5 * 7.0 + 0.5 * (11.0 / 3.0))
    );
}

#[test]
fn repeated_calls_replace_current_bar_samples_and_last_missing_discards_them() {
    let source = "//@version=6\nindicator(\"repeat\")\nfloat a=0\nfloat b=0\nfloat c=0\nfor i=0 to 2\n    a += nz(ta.ema(float(bar_index+i+1),2))\nfor i=0 to 2\n    b += nz(ta.ema(i==2 ? float(na) : float(bar_index+i+1),2))\nfor i=0 to 2\n    c += nz(ta.sma(float(bar_index+i+1),2))\nplot(a)\nplot(b)\nplot(c)\n";
    let analysis = analyze_source(&SourceFile::new("repeat.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let input = bars();
    let mut batch = HistoricalRuntime::new(&hir);
    batch.append_bars(&input[..4]).unwrap();
    let result = batch.result();
    for (plot, expected) in result.plots.iter().zip([
        [0.0, 9.0, 11.5, 14.5],
        [0.0, 0.0, 0.0, 0.0],
        [0.0, 9.0, 12.0, 15.0],
    ]) {
        for (value, expected) in plot.values.iter().zip(expected) {
            assert!((value.as_f64().unwrap() - expected).abs() < 1e-12);
        }
    }
    let expected = public_runtime_result_json(&result);
    let mut incremental = HistoricalRuntime::new(&hir);
    for bar in &input[..4] {
        incremental.append_bar(*bar).unwrap();
    }
    assert_eq!(public_runtime_result_json(&incremental.result()), expected);
    let mut realtime = RealtimeRuntime::new(&hir);
    realtime.seed_historical(&input[..1]).unwrap();
    let mut final_result = None;
    for bar in &input[1..4] {
        for _ in 0..3 {
            realtime.update(BarUpdate::forming(*bar)).unwrap();
        }
        final_result = Some(realtime.update(BarUpdate::confirmed(*bar)).unwrap());
    }
    assert_eq!(public_runtime_result_json(&final_result.unwrap()), expected);
}
