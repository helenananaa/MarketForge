use pine_runtime::{Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime};
use pine_sema::analyze_source;
use pine_syntax::{Severity, SourceFile};

fn program(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("valuewhen.pine", source));
    assert!(
        !analysis
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("valid HIR")
}

fn bar(index: usize, close: f64) -> Bar {
    Bar {
        time: index as i64 * 60000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn assert_float_bits(value: &PineValue, expected: Option<f64>) {
    match expected {
        Some(expected) => {
            let PineValue::Float(actual) = value else {
                panic!("expected Float, got {value:?}")
            };
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        None => assert_eq!(value, &PineValue::Na),
    }
}

#[test]
fn dynamic_occurrences_keep_event_order_and_bits_across_invalid_reads() {
    let program = program(
        "//@version=6\nindicator(\"events\")\nsource = bar_index == 15 ? na : close\noccurrence = bar_index == 6 ? -1 : bar_index == 9 ? 1000000 : bar_index == 12 ? na : bar_index % 4\nplot(ta.valuewhen(bar_index % 3 == 0, source, occurrence))\n",
    );
    let prices = [0.0, -0.0, 1.0e-310, f64::MAX, -17.75];
    let bars: Vec<_> = (0..2049)
        .map(|i| bar(i, prices[i % prices.len()]))
        .collect();
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bars(&bars).unwrap();
    let result = runtime.result_view();
    assert_eq!(result.plots[0].values.len(), bars.len());
    for (index, actual) in result.plots[0].values.iter().enumerate() {
        let expected = if [6, 9, 12].contains(&index) {
            None
        } else {
            (index / 3).checked_sub(index % 4).and_then(|event| {
                let event_index = event * 3;
                (event_index != 15).then_some(prices[event_index % prices.len()])
            })
        };
        assert_float_bits(actual, expected);
    }
    assert_eq!(runtime.profile().valuewhen_state_values, 683);
}

#[test]
fn offsets_larger_than_wasm_usize_never_wrap_into_valid_occurrences() {
    let program = program(
        "//@version=6\nindicator(\"large occurrence\")\nplot(ta.valuewhen(true, close, 4294967296))\nplot(ta.valuewhen(true, close, 4294967303))\nplot(ta.valuewhen(true, close, 9223372036854775807))\noffset = bar_index % 3 == 0 ? 4294967296 : bar_index % 3 == 1 ? 4294967303 : 9223372036854775807\nplot(ta.valuewhen(true, close, offset))\n",
    );
    let mut runtime = HistoricalRuntime::new(&program);
    let bars: Vec<_> = (0..300).map(|i| bar(i, 100.0 + i as f64 / 4.0)).collect();
    runtime.append_bars(&bars).unwrap();
    let result = runtime.result_view();
    assert_eq!(result.plots.len(), 4);
    for plot in &result.plots {
        assert_eq!(plot.values.len(), bars.len());
        assert!(plot.values.iter().all(|value| value == &PineValue::Na));
    }
    // Invalid fixed offsets keep empty callsite slots; Series offsets continue
    // retaining events so later valid dynamic occurrences remain available.
    assert_eq!(runtime.profile().valuewhen_state_slots, 4);
    assert_eq!(runtime.profile().valuewhen_state_values, bars.len());
}

#[test]
fn repeated_forming_conditions_rollback_then_confirmation_commits_one_event() {
    let program = program(
        "//@version=6\nindicator(\"forming events\")\nplot(ta.valuewhen(close > 0, close, bar_index % 3 + 1))\n",
    );
    let mut runtime = RealtimeRuntime::new(&program);
    let bars: Vec<_> = (0..301).map(|i| bar(i, 100.0 + i as f64 / 4.0)).collect();
    runtime.seed_historical_without_output(&bars).unwrap();
    let mut replica = runtime.replica();
    for close in [777.0, -5.0, 778.0, -6.0] {
        let changes = runtime
            .apply_update(BarUpdate::forming(bar(301, close)))
            .unwrap();
        let expected_index = if close > 0.0 { 299 } else { 298 };
        assert_float_bits(
            runtime.result_view().plots[0].values.iter().last().unwrap(),
            Some(100.0 + expected_index as f64 / 4.0),
        );
        assert_eq!(
            runtime.profile().valuewhen_state_values,
            301 + usize::from(close > 0.0)
        );
        assert_eq!(runtime.confirmed_profile().valuewhen_state_values, 301);
        replica.apply(&changes).unwrap();
        replica.apply(&changes).unwrap();
        assert_eq!(replica.result(), &runtime.result());
    }
    let confirmed = runtime
        .apply_update(BarUpdate::confirmed(bar(301, 888.0)))
        .unwrap();
    replica.apply(&confirmed).unwrap();
    assert_eq!(runtime.confirmed_profile().valuewhen_state_values, 302);
    assert_float_bits(
        runtime.result_view().plots[0].values.iter().last().unwrap(),
        Some(174.75),
    );
    let next = runtime
        .apply_update(BarUpdate::historical(bar(302, 889.0)))
        .unwrap();
    replica.apply(&next).unwrap();
    assert_eq!(runtime.confirmed_profile().valuewhen_state_values, 303);
    assert_float_bits(
        runtime.result_view().plots[0].values.iter().last().unwrap(),
        Some(174.75),
    );
    assert_eq!(runtime.revision(), 7);
    assert_eq!(replica.result(), &runtime.result());
}

#[test]
fn repeated_calls_on_one_bar_preserve_each_true_invocation_in_sequence() {
    let program = program(
        "//@version=6\nindicator(\"loop events\")\ntotal = 0\nfor iteration = 0 to 2\n    selected = ta.valuewhen(true, bar_index * 10 + iteration, bar_index % 3 + 1)\n    total += nz(selected)\nplot(total)\n",
    );
    let mut runtime = HistoricalRuntime::new(&program);
    let bars: Vec<_> = (0..257).map(|i| bar(i, 1.0)).collect();
    runtime.append_bars(&bars).unwrap();
    let result = runtime.result_view();
    for (index, actual) in result.plots[0].values.iter().enumerate() {
        let occurrence = index % 3 + 1;
        let expected = (0..3)
            .filter_map(|iteration| {
                (index * 3 + iteration)
                    .checked_sub(occurrence)
                    .map(|event| (event / 3 * 10 + event % 3) as i64)
            })
            .sum::<i64>();
        assert_eq!(actual, &PineValue::Int(expected));
    }
    assert_eq!(runtime.profile().valuewhen_state_values, 257 * 3);
}

#[test]
fn actual_order_fill_recalculation_restores_the_prior_event_checkpoint() {
    let program = program(
        "//@version=6\nstrategy(\"fill events\", calc_on_order_fills=true, process_orders_on_close=true)\nif bar_index == 198 and strategy.position_size == 0\n    strategy.entry(\"L\", strategy.long, qty=1)\nplot(ta.valuewhen(true, bar_index, bar_index % 3 + 1))\n",
    );
    let mut runtime = HistoricalRuntime::new(&program);
    let bars: Vec<_> = (0..260).map(|i| bar(i, 100.0)).collect();
    runtime.append_bars(&bars).unwrap();
    let result = runtime.result_view();
    for (index, actual) in result.plots[0].values.iter().enumerate() {
        let expected = index
            .checked_sub(index % 3 + 1)
            .map(|i| PineValue::Int(i as i64))
            .unwrap_or(PineValue::Na);
        assert_eq!(actual, &expected);
    }
    let orders: Vec<_> = result.strategy.as_ref().unwrap().orders.iter().collect();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].bar_index, 198);
    assert_eq!(orders[0].qty.to_bits(), 1.0_f64.to_bits());
    assert!(runtime.profile().strategy_recalculation_passes > 0);
    assert_eq!(runtime.profile().valuewhen_state_values, 260);
}
