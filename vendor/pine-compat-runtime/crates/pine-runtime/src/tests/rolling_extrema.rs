use std::sync::Arc;

use pine_syntax::SourceFile;

use super::*;

fn program(text: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("rolling-extrema.pine", text));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bars(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|index| Bar {
            time: index as i64 * 60_000,
            ..bar((index * 137 % 23) as f64)
        })
        .collect()
}

fn expected_extreme(
    values: &[Option<f64>],
    index: usize,
    length: usize,
    high: bool,
) -> (PineValue, PineValue) {
    let mut best = None;
    let mut offset = 0;
    let mut value = values[index].filter(|value| value.is_finite());
    for candidate in (index.saturating_sub(length - 1)..=index).rev() {
        let Some(previous) = values[candidate].filter(|value| value.is_finite()) else {
            continue;
        };
        if candidate < index {
            value = Some(match value {
                Some(current) if high => current.max(previous),
                Some(current) => current.min(previous),
                None => previous,
            });
        }
        if best.is_none_or(|current| {
            if high {
                previous > current
            } else {
                previous < current
            }
        }) {
            best = Some(previous);
            offset = index - candidate;
        }
    }
    (
        if index + 1 < length {
            PineValue::Na
        } else {
            value.map_or(PineValue::Na, PineValue::Float)
        },
        if values[index].is_some_and(f64::is_finite) {
            PineValue::Int(-(offset as i64))
        } else {
            PineValue::Na
        },
    )
}

fn assert_bits(observed: &PineValue, expected: &PineValue) {
    match (observed, expected) {
        (PineValue::Float(observed), PineValue::Float(expected)) => {
            assert_eq!(observed.to_bits(), expected.to_bits());
        }
        _ => assert_eq!(observed, expected),
    }
}

#[test]
fn dynamic_sparse_cached_windows_preserve_values_offsets_and_zero_bits() {
    let hir = program(
        r#"indicator("dynamic extrema")
source = bar_index % 13 == 0 ? na : close
length = bar_index % 997 < 400 ? 257 : bar_index % 997 < 700 ? 33 : 513
float hi = na
float lo = na
float hib = na
float lob = na
if bar_index % 7 == 0 or bar_index % 997 > 800
    hi := ta.highest(source, length)
    lo := ta.lowest(source, length)
    hib := ta.highestbars(source, length)
    lob := ta.lowestbars(source, length)
plot(hi)
plot(lo)
plot(hib)
plot(lob)
"#,
    );
    let mut input = bars(3000);
    for (index, bar) in input.iter_mut().enumerate() {
        if index % 997 > 730 {
            bar.close = if index % 2 == 0 { -0.0 } else { 0.0 };
        }
    }
    let values: Vec<_> = input
        .iter()
        .enumerate()
        .map(|(index, bar)| (index % 13 != 0).then_some(bar.close))
        .collect();
    let output = run_historical(&hir, &input).unwrap();
    for index in 0..input.len() {
        let length = if index % 997 < 400 {
            257
        } else if index % 997 < 700 {
            33
        } else {
            513
        };
        for (plot, high) in [(0, true), (1, false)] {
            let (value, offset) = if index % 7 == 0 || index % 997 > 800 {
                expected_extreme(&values, index, length, high)
            } else {
                (PineValue::Na, PineValue::Na)
            };
            assert_bits(&output.plots[plot].values[index], &value);
            assert_eq!(output.plots[plot + 2].values[index], offset);
        }
    }
    let mut live = RealtimeRuntime::new(&hir);
    live.seed_historical(&input[..2996]).unwrap();
    for close in [-41.0, 29.0, -0.0, 0.0] {
        let update = Bar {
            close,
            ..input[2996]
        };
        let mut reference = input[..2996].to_vec();
        reference.push(update);
        let observed = live.update(BarUpdate::forming(update)).unwrap();
        let expected = run_historical(&hir, &reference).unwrap();
        for (observed, expected) in observed.plots.iter().zip(&expected.plots) {
            for (observed, expected) in observed.values.iter().zip(&expected.values) {
                assert_bits(observed, expected);
            }
        }
    }
}

#[test]
fn repeated_same_bar_calls_read_final_committed_source_history() {
    let hir = program(
        r#"indicator("repeated extrema")
var source = 0.0
float hi = na
float lo = na
float hib = na
float lob = na
for step = 0 to 2
    source := close + step
    hi := ta.highest(source, 257)
    lo := ta.lowest(source, 257)
    hib := ta.highestbars(source, 257)
    lob := ta.lowestbars(source, 257)
plot(hi)
plot(lo)
plot(hib)
plot(lob)
"#,
    );
    let input = bars(700);
    let output = run_historical(&hir, &input).unwrap();
    let values: Vec<_> = input.iter().map(|bar| Some(bar.close + 2.0)).collect();
    for index in 0..input.len() {
        for (plot, high) in [(0, true), (1, false)] {
            let (value, offset) = expected_extreme(&values, index, 257, high);
            assert_bits(&output.plots[plot].values[index], &value);
            assert_eq!(output.plots[plot + 2].values[index], offset);
        }
    }
}

#[test]
fn cached_zero_ties_preserve_scan_bits_and_prefer_the_current_offset() {
    let hir = program(
        "indicator(\"zero extrema\")\nplot(ta.highest(close, 257))\nplot(ta.lowest(close, 257))\nplot(ta.highestbars(close, 257))\nplot(ta.lowestbars(close, 257))\n",
    );
    let mut input = bars(700);
    for (index, bar) in input.iter_mut().enumerate() {
        bar.close = if index % 2 == 0 { -0.0 } else { 0.0 };
    }
    let output = run_historical(&hir, &input).unwrap();
    let values: Vec<_> = input.iter().map(|bar| Some(bar.close)).collect();
    for index in 0..input.len() {
        for (plot, high) in [(0, true), (1, false)] {
            let (value, offset) = expected_extreme(&values, index, 257, high);
            assert_bits(&output.plots[plot].values[index], &value);
            assert_eq!(output.plots[plot + 2].values[index], offset);
        }
    }
}

#[test]
fn requested_cached_windows_use_requested_history() {
    let hir = program(
        r#"indicator("request extrema")
[hi, lo, hib, lob] = request.security("B", "1", [ta.highest(close, 257), ta.lowest(close, 257), ta.highestbars(close, 257), ta.lowestbars(close, 257)])
plot(hi)
plot(lo)
plot(hib)
plot(lob)
"#,
    );
    let reference = program(
        "indicator(\"reference\")\nplot(ta.highest(close, 257))\nplot(ta.lowest(close, 257))\nplot(ta.highestbars(close, 257))\nplot(ta.lowestbars(close, 257))\n",
    );
    let requested = bars(700);
    let chart: Vec<_> = requested
        .iter()
        .map(|bar| Bar {
            close: 1000.0 + bar.close,
            ..*bar
        })
        .collect();
    let timeframe = RequestTimeframe::parse("1").unwrap();
    let provider = InMemoryRequestDataProvider::from_streams([(
        RequestKey::new("B", timeframe.clone()),
        requested.clone(),
    )])
    .unwrap();
    let environment =
        RequestEnvironment::new(ChartContext::new("A", timeframe), Arc::new(provider));
    let output = HistoricalRuntime::with_request_environment(&hir, environment)
        .run(&chart)
        .unwrap();
    let expected = run_historical(&reference, &requested).unwrap();
    for (observed, expected) in output.plots.iter().zip(&expected.plots) {
        assert_eq!(observed.values, expected.values);
    }
}

#[test]
fn fill_recalculations_restore_cached_windows_and_keep_storage_bounded() {
    let hir = program(
        r#"strategy("fill extrema", calc_on_order_fills=true, process_orders_on_close=true)
if bar_index > 260
    if bar_index == 270 and strategy.position_size == 0
        strategy.entry("long", strategy.long)
    if bar_index == 300 and strategy.position_size > 0
        strategy.close("long")
plot(ta.highest(close, 257))
plot(ta.lowest(close, 257))
plot(ta.highestbars(close, 257))
plot(ta.lowestbars(close, 257))
"#,
    );
    let input = bars(700);
    let mut runtime = HistoricalRuntime::new(&hir);
    for bar in &input {
        runtime.append_bar(*bar).unwrap();
    }
    let output = runtime.result();
    let values: Vec<_> = input.iter().map(|bar| Some(bar.close)).collect();
    for index in 0..input.len() {
        for (plot, high) in [(0, true), (1, false)] {
            let (value, offset) = expected_extreme(&values, index, 257, high);
            assert_bits(&output.plots[plot].values[index], &value);
            assert_eq!(output.plots[plot + 2].values[index], offset);
        }
    }
    let profile = runtime.profile();
    assert!(profile.strategy_recalculation_passes > 0);
    assert_eq!(profile.rolling_window_slots, 4);
    assert!(profile.rolling_window_values <= 4 * 256);
    assert!(profile.rolling_window_value_capacity <= 4 * 512);
}
