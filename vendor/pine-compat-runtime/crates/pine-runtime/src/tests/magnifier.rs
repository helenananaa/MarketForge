use pine_syntax::SourceFile;

use super::*;

fn indicator_program() -> pine_ir::HirProgram {
    let source = SourceFile::new(
        "magnifier.pine",
        r#"
indicator("magnifier ownership")
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

fn strategy_program_false() -> pine_ir::HirProgram {
    let source = SourceFile::new(
        "magnifier-strategy.pine",
        r#"
strategy("magnifier ownership", use_bar_magnifier=false)
plot(close)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

fn timed_bar(time: i64, close: f64) -> Bar {
    Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn group(chart_bar_index: usize, bars: Vec<Bar>) -> MagnifierChartBarInput {
    MagnifierChartBarInput {
        chart_bar_index,
        bars,
    }
}

#[test]
fn historical_runtime_default_magnifier_input_is_empty() {
    let program = indicator_program();
    let runtime = HistoricalRuntime::new(&program);
    assert!(runtime.magnifier_input().is_empty());
    let baseline = runtime
        .clone()
        .run(&[timed_bar(1, 1.0), timed_bar(2, 2.0)])
        .expect("baseline");
    let with_empty = HistoricalRuntime::new(&program)
        .with_magnifier_input(MagnifierInput::new())
        .run(&[timed_bar(1, 1.0), timed_bar(2, 2.0)])
        .expect("empty magnifier");
    assert_eq!(baseline, with_empty);
}

#[test]
fn historical_runtime_owns_sparse_magnifier_input_without_changing_output() {
    let program = indicator_program();
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1, 1.1)]),
        group(2, vec![timed_bar(3, 3.1)]),
    ])
    .expect("valid");
    let runtime = HistoricalRuntime::new(&program).with_magnifier_input(input.clone());
    assert_eq!(runtime.magnifier_input(), &input);
    let cloned = runtime.clone();
    assert_eq!(cloned.magnifier_input(), &input);
    let baseline = HistoricalRuntime::new(&program)
        .run(&[timed_bar(1, 1.0), timed_bar(2, 2.0), timed_bar(3, 3.0)])
        .expect("baseline");
    let with_input = runtime
        .run(&[timed_bar(1, 1.0), timed_bar(2, 2.0), timed_bar(3, 3.0)])
        .expect("inert magnifier");
    assert_eq!(baseline, with_input);
}

#[test]
fn historical_runtime_rejects_out_of_range_magnifier_before_bar_zero() {
    let program = indicator_program();
    let input =
        magnifier_input_from_groups(vec![group(2, vec![timed_bar(1, 1.0)])]).expect("valid");
    let error = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1, 1.0), timed_bar(2, 2.0)])
        .expect_err("out of range");
    assert!(
        error.message.contains("E_MAGNIFIER_CHART_BAR_RANGE"),
        "{}",
        error.message
    );
}

#[test]
fn historical_runtime_false_setting_keeps_valid_input_inert() {
    let program = strategy_program_false();
    assert!(!program.strategy_settings.use_bar_magnifier);
    let input =
        magnifier_input_from_groups(vec![group(0, vec![timed_bar(1, 9.0)])]).expect("valid");
    let baseline = HistoricalRuntime::new(&program)
        .run(&[timed_bar(1, 1.0)])
        .expect("baseline");
    let with_input = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1, 1.0)])
        .expect("inert");
    assert_eq!(baseline, with_input);
}

#[test]
fn realtime_runtime_rejects_magnifier_group_for_forming_bar() {
    let program = indicator_program();
    let input =
        magnifier_input_from_groups(vec![group(0, vec![timed_bar(1, 9.0)])]).expect("valid");
    let error = RealtimeRuntime::new(&program)
        .with_magnifier_input(input)
        .update(BarUpdate::forming(timed_bar(1, 1.0)))
        .expect_err("forming slot");
    assert!(
        error.message.contains("E_MAGNIFIER_FORMING_BAR"),
        "{}",
        error.message
    );
}

#[test]
fn streaming_historical_requires_complete_range_preflight_before_bar_zero() {
    let program = indicator_program();
    let input = magnifier_input_from_groups(vec![group(1, vec![timed_bar(2, 2.0)])])
        .expect("valid sparse future group");
    let mut runtime = HistoricalRuntime::new(&program).with_magnifier_input(input.clone());
    let error = runtime
        .append_bar(timed_bar(1, 1.0))
        .expect_err("streaming must declare its complete range");
    assert!(
        error
            .message
            .contains("E_MAGNIFIER_CHART_BAR_COUNT_REQUIRED"),
        "{}",
        error.message
    );
    assert!(
        runtime.result().plots.is_empty(),
        "bar zero must not execute"
    );

    let mut prepared = HistoricalRuntime::new(&program).with_magnifier_input(input);
    prepared
        .prepare_magnifier_chart_bar_count(2)
        .expect("preflight");
    prepared.append_bar(timed_bar(1, 1.0)).expect("bar zero");
    prepared.append_bar(timed_bar(2, 2.0)).expect("bar one");
    assert_eq!(prepared.result().plots[0].values.len(), 2);
}

#[test]
fn streaming_preflight_rejects_out_of_range_group_before_bar_zero() {
    let program = indicator_program();
    let input = magnifier_input_from_groups(vec![group(2, vec![timed_bar(3, 3.0)])])
        .expect("structurally valid");
    let mut runtime = HistoricalRuntime::new(&program).with_magnifier_input(input);
    let error = runtime
        .prepare_magnifier_chart_bar_count(2)
        .expect_err("out of range");
    assert!(
        error.message.contains("E_MAGNIFIER_CHART_BAR_RANGE"),
        "{}",
        error.message
    );
    assert!(
        runtime.result().plots.is_empty(),
        "bar zero must not execute"
    );
}

#[test]
fn realtime_historical_stream_requires_complete_range_preflight() {
    let program = indicator_program();
    let input = magnifier_input_from_groups(vec![group(1, vec![timed_bar(2, 2.0)])])
        .expect("valid sparse future group");
    let mut runtime = RealtimeRuntime::new(&program).with_magnifier_input(input);
    let error = runtime
        .update(BarUpdate::historical(timed_bar(1, 1.0)))
        .expect_err("preflight required");
    assert!(
        error
            .message
            .contains("E_MAGNIFIER_CHART_BAR_COUNT_REQUIRED"),
        "{}",
        error.message
    );
    runtime
        .prepare_magnifier_chart_bar_count(2)
        .expect("preflight");
    runtime
        .update(BarUpdate::historical(timed_bar(1, 1.0)))
        .expect("bar zero");
    runtime
        .update(BarUpdate::historical(timed_bar(2, 2.0)))
        .expect("bar one");
    assert_eq!(runtime.confirmed_result().plots[0].values.len(), 2);
}

#[test]
fn strategy_forming_early_return_still_rejects_magnifier_group() {
    let program = enabled_strategy(
        r#"
strategy("forming rejection", use_bar_magnifier=true)
plot(close)
"#,
    );
    assert!(!program.strategy_settings.calc_on_every_tick);
    let input =
        magnifier_input_from_groups(vec![group(0, vec![timed_bar(1, 9.0)])]).expect("valid");
    let error = RealtimeRuntime::new(&program)
        .with_magnifier_input(input)
        .update(BarUpdate::forming(timed_bar(1, 1.0)))
        .expect_err("forming slot");
    assert!(
        error.message.contains("E_MAGNIFIER_FORMING_BAR"),
        "{}",
        error.message
    );
}

#[test]
fn realtime_confirmed_bar_never_consumes_magnifier_group() {
    let program = indicator_program();
    let input =
        magnifier_input_from_groups(vec![group(0, vec![timed_bar(1, 9.0)])]).expect("valid");
    let error = RealtimeRuntime::new(&program)
        .with_magnifier_input(input)
        .update(BarUpdate::confirmed(timed_bar(1, 1.0)))
        .expect_err("live confirmed slot");
    assert!(
        error.message.contains("E_MAGNIFIER_FORMING_BAR"),
        "{}",
        error.message
    );
}

fn enabled_strategy(source: &str) -> pine_ir::HirProgram {
    let file = SourceFile::new("magnifier-enabled.pine", source);
    let analysis = analyze_source(&file);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    assert!(
        program.strategy_settings.use_bar_magnifier,
        "enabled_strategy sources must set use_bar_magnifier=true"
    );
    program
}

fn ohlc(time: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        time,
        open,
        high,
        low,
        close,
        volume: 1.0,
    }
}

#[test]
fn magnifier_three_lower_bars_walk_independent_host_paths() {
    use crate::runtime::strategy_scheduler::StrategyPathPhase;

    let program = enabled_strategy(
        r#"
strategy("three lower bars", use_bar_magnifier=true)
plot(close)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.4, 9.8, 10.2),
                ohlc(2_300, 10.2, 10.8, 10.1, 10.6),
                ohlc(2_600, 11.0, 11.8, 10.5, 11.0),
            ],
        ),
    ])
    .expect("valid");
    let mut runtime = HistoricalRuntime::new(&program).with_magnifier_input(input);
    runtime
        .append_bars(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run");
    let hosts: Vec<_> = runtime
        .strategy_path_trace
        .iter()
        .filter(|entry| entry.chart_bar_index == 1)
        .map(|entry| entry.host_bar_index)
        .collect();
    assert!(hosts.contains(&0));
    assert!(hosts.contains(&1));
    assert!(hosts.contains(&2));
    assert!(
        runtime.strategy_path_trace.iter().any(|entry| {
            entry.chart_bar_index == 1
                && entry.host_bar_index == 2
                && entry.path_phase == StrategyPathPhase::HostOpen
                && (entry.mark - 11.0).abs() < 1e-10
        }),
        "gap 10.6 -> 11.0 must be a host-open point: {:?}",
        runtime.strategy_path_trace
    );
    let fallback = runtime
        .result()
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.starts_with("W_MAGNIFIER"));
    assert!(!fallback);
}

#[test]
fn magnifier_empty_group_falls_back_once_and_keeps_chart_identity() {
    let program = enabled_strategy(
        r#"
strategy("empty group", use_bar_magnifier=true)
plot(close)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1, 1.0)]),
        MagnifierChartBarInput {
            chart_bar_index: 1,
            bars: Vec::new(),
        },
    ])
    .expect("valid");
    let result = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1_000, 1.0), timed_bar(2_000, 2.0)])
        .expect("run");
    let warnings: Vec<_> = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect();
    assert_eq!(warnings, vec!["W_MAGNIFIER_GAP"]);
}

#[test]
fn magnifier_doji_lower_bar_terminates_without_replay() {
    let program = enabled_strategy(
        r#"
strategy("doji", use_bar_magnifier=true)
plot(close)
"#,
    );
    let input = magnifier_input_from_groups(vec![group(
        0,
        vec![
            ohlc(1, 10.0, 10.0, 10.0, 10.0),
            ohlc(2, 10.0, 10.0, 10.0, 10.0),
        ],
    )])
    .expect("valid");
    HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1, 10.0)])
        .expect("doji walk must terminate");
}

#[test]
fn magnifier_fill_resumes_from_current_host_leg_without_replay() {
    use crate::runtime::strategy_scheduler::StrategyPathPhase;

    let program = enabled_strategy(
        r#"
strategy("resume", calc_on_order_fills=true, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1, stop=10.5)
if strategy.position_size > 0
    strategy.exit("EX", "EN", limit=11.5)
plot(open)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.4, 9.8, 10.2),
                ohlc(2_300, 10.2, 10.8, 10.1, 10.6),
                ohlc(2_600, 10.6, 11.8, 10.5, 11.0),
            ],
        ),
    ])
    .expect("valid");
    let mut runtime = HistoricalRuntime::new(&program).with_magnifier_input(input);
    runtime
        .append_bars(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run");
    let result = runtime.result();
    let strategy = result.strategy.expect("strategy");
    let entry = strategy
        .orders
        .iter()
        .find(|order| order.id == "EN")
        .expect("entry fill");
    assert_eq!(entry.bar_index, 1);
    assert_eq!(entry.time, 2_000);
    assert!((entry.price - 10.5).abs() < 1e-10, "{entry:?}");
    let exit = strategy
        .orders
        .iter()
        .find(|order| order.id == "EX")
        .expect("exit fill");
    assert_eq!(exit.bar_index, 1);
    assert_eq!(exit.time, 2_000);
    assert!((exit.price - 11.5).abs() < 1e-10, "{exit:?}");
    assert_eq!(
        result.plots[0].values.get(1),
        Some(&PineValue::Float(10.0)),
        "extra pass must keep chart-bar open: {:?}",
        result.plots[0].values
    );

    let fill_trace = runtime
        .strategy_path_trace
        .iter()
        .find(|entry| {
            entry.chart_bar_index == 1
                && entry.path_phase == StrategyPathPhase::PathLeg
                && (entry.mark - 10.5).abs() < 1e-10
        })
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "entry fill host/leg/mark missing; orders={:?} trace={:?}",
                strategy.orders, runtime.strategy_path_trace
            )
        });
    assert_eq!(
        fill_trace.host_bar_index, 1,
        "EN must fill on lower bar 1: {fill_trace:?}"
    );
    assert_eq!(
        fill_trace.path_phase,
        StrategyPathPhase::PathLeg,
        "{fill_trace:?}"
    );
    assert!((fill_trace.mark - 10.5).abs() < 1e-10, "{fill_trace:?}");

    let fill_index = runtime
        .strategy_path_trace
        .iter()
        .position(|entry| {
            entry.chart_bar_index == fill_trace.chart_bar_index
                && entry.host_bar_index == fill_trace.host_bar_index
                && entry.path_phase == fill_trace.path_phase
                && entry.leg_index == fill_trace.leg_index
                && (entry.mark - fill_trace.mark).abs() < 1e-10
        })
        .expect("fill trace index");
    let mut last_host = fill_trace.host_bar_index;
    for entry in runtime.strategy_path_trace[fill_index..]
        .iter()
        .filter(|entry| entry.chart_bar_index == 1)
    {
        assert!(
            entry.host_bar_index >= last_host,
            "resume must not replay earlier host bars: {last_host} -> {} in {:?}",
            entry.host_bar_index,
            runtime.strategy_path_trace
        );
        last_host = entry.host_bar_index;
        assert!(
            entry.host_bar_index >= fill_trace.host_bar_index,
            "consumed host 0/earlier marks must stay consumed: {entry:?}"
        );
        if entry.host_bar_index == fill_trace.host_bar_index
            && entry.path_phase == StrategyPathPhase::PathLeg
            && entry.leg_index == fill_trace.leg_index
        {
            assert!(
                entry.mark + 1e-10 >= fill_trace.mark,
                "consumed prices on the fill leg must not replay: fill={} later={entry:?}",
                fill_trace.mark
            );
        }
    }
    assert!(
        runtime.strategy_path_trace[fill_index..]
            .iter()
            .any(|entry| {
                entry.chart_bar_index == 1
                    && entry.host_bar_index == 2
                    && (entry.mark - 11.5).abs() < 1e-10
            }),
        "post-fill exit must fill on unconsumed later host bar 2: {:?}",
        runtime.strategy_path_trace
    );
    assert!(
        !runtime.strategy_path_trace[fill_index..]
            .iter()
            .any(|entry| { entry.chart_bar_index == 1 && entry.host_bar_index == 0 }),
        "post-fill path must not revisit consumed lower bar 0: {:?}",
        runtime.strategy_path_trace
    );
}

#[test]
fn magnifier_gap_fills_stop_at_next_open_not_requested_price() {
    let program = enabled_strategy(
        r#"
strategy("gap stop", pyramiding=2, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("STP", strategy.long, qty=1, stop=10.5)
plot(close)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.2, 9.9, 10.1),
                ohlc(2_300, 11.0, 11.2, 10.8, 11.1),
            ],
        ),
    ])
    .expect("valid");
    let result = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run");
    let strategy = result.strategy.expect("strategy");
    let fill = strategy
        .orders
        .iter()
        .find(|order| order.id == "STP")
        .expect("stop fill");
    assert_eq!(fill.bar_index, 1);
    assert!((fill.price - 11.0).abs() < 1e-10, "{fill:?}");
    assert_eq!(fill.time, 2_000);
}

#[test]
fn magnifier_gap_stop_limit_does_not_fill_above_its_limit() {
    let program = enabled_strategy(
        r#"
strategy("gap stop-limit", initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("SL", strategy.long, qty=1, stop=10.5, limit=10.2)
plot(strategy.position_size)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.2, 9.9, 10.1),
                ohlc(2_300, 11.0, 11.2, 10.8, 11.1),
            ],
        ),
    ])
    .expect("valid");
    let strategy = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run")
        .strategy
        .expect("strategy");
    assert!(strategy.orders.is_empty(), "{:?}", strategy.orders);
    assert!(strategy.position.is_empty(), "{:?}", strategy.position);
}

#[test]
fn magnifier_gap_activates_trailing_exit_before_later_path_events() {
    let program = enabled_strategy(
        r#"
strategy("gap trailing activation", initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1)
    strategy.exit("TR", "EN", trail_price=10.5, trail_offset=50)
plot(strategy.position_size)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.2, 9.9, 10.1),
                ohlc(2_300, 11.0, 11.2, 10.9, 11.1),
            ],
        ),
    ])
    .expect("valid");
    let strategy = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run")
        .strategy
        .expect("strategy");
    let ids: Vec<_> = strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert_eq!(ids, vec!["EN"], "{ids:?}");
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(1.0)
    );
}

#[test]
fn magnifier_same_chart_bar_entry_and_exit_uses_chart_bar_index() {
    let program = enabled_strategy(
        r#"
strategy("same bar", initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1, stop=10.5)
    strategy.exit("EX", "EN", limit=11.5)
plot(close)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.4, 9.8, 10.2),
                ohlc(2_300, 10.2, 10.8, 10.1, 10.6),
                ohlc(2_600, 10.6, 11.8, 10.5, 11.0),
            ],
        ),
    ])
    .expect("valid");
    let result = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run");
    let strategy = result.strategy.expect("strategy");
    let ids: Vec<_> = strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert!(ids.contains(&"EN"), "{ids:?}");
    assert!(ids.contains(&"EX"), "{ids:?}");
    assert!(
        strategy.orders.iter().all(|order| order.bar_index == 1),
        "{:?}",
        strategy.orders
    );
}

#[test]
fn magnifier_calc_on_order_fills_false_does_not_add_script_passes() {
    let with_fills = enabled_strategy(
        r#"
strategy("fills on", calc_on_order_fills=true, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1, stop=10.5)
if strategy.position_size > 0
    strategy.exit("EX", "EN", limit=11.5)
plot(close)
"#,
    );
    let without_fills = enabled_strategy(
        r#"
strategy("fills off", calc_on_order_fills=false, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1, stop=10.5)
if strategy.position_size > 0
    strategy.exit("EX", "EN", limit=11.5)
plot(close)
"#,
    );
    let bars = [timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)];
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.4, 9.8, 10.2),
                ohlc(2_300, 10.2, 10.8, 10.1, 10.6),
                ohlc(2_600, 10.6, 11.8, 10.5, 11.0),
            ],
        ),
    ])
    .expect("valid");
    let mut on = HistoricalRuntime::new(&with_fills).with_magnifier_input(input.clone());
    on.append_bars(&bars).expect("on");
    let mut off = HistoricalRuntime::new(&without_fills).with_magnifier_input(input);
    off.append_bars(&bars).expect("off");
    assert!(
        on.strategy_scheduler.script_passes() > off.strategy_scheduler.script_passes(),
        "on={} off={}",
        on.strategy_scheduler.script_passes(),
        off.strategy_scheduler.script_passes()
    );
    assert_eq!(off.strategy_scheduler.recalculation_passes(), 0);
}

#[test]
fn magnifier_false_setting_matches_standard_ohlc_baseline() {
    let source = r#"
strategy("baseline", initial_capital=100000)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1, stop=10.5)
plot(close)
"#;
    let file = SourceFile::new("baseline.pine", source);
    let analysis = analyze_source(&file);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    assert!(!program.strategy_settings.use_bar_magnifier);
    let bars = [timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)];
    let baseline = HistoricalRuntime::new(&program)
        .run(&bars)
        .expect("baseline");
    let input =
        magnifier_input_from_groups(vec![group(1, vec![ohlc(2_000, 10.0, 10.1, 9.9, 10.0)])])
            .expect("valid");
    let with_input = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&bars)
        .expect("inert");
    assert_eq!(baseline, with_input);
}

#[test]
fn magnifier_first_open_fills_at_lower_bar_open_not_chart_open() {
    let program = enabled_strategy(
        r#"
strategy("23.0 first open mismatch", overlay=false, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1)
plot(open)
"#,
    );
    let chart = [
        timed_bar(1_000_000, 10.0),
        ohlc(2_000_000, 10.0, 12.0, 8.0, 11.0),
    ];
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000_000, 10.0)]),
        group(1, vec![ohlc(2_000_000, 10.8, 11.0, 10.6, 10.9)]),
    ])
    .expect("valid");
    let fallback = HistoricalRuntime::new(&program)
        .run(&chart)
        .expect("fallback")
        .strategy
        .expect("strategy");
    let result = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&chart)
        .expect("run");
    let strategy = result.strategy.expect("strategy");
    let fill = strategy
        .orders
        .iter()
        .find(|order| order.id == "EN")
        .expect("market fill");
    assert_eq!(fill.bar_index, 1);
    assert_eq!(fill.time, 2_000_000);
    assert!((fill.price - 10.8).abs() < 1e-10, "{fill:?}");
    assert_eq!(strategy.orders.len(), 1, "{:?}", strategy.orders);
    assert_eq!(
        result.plots[0].values.get(1),
        Some(&PineValue::Float(10.0)),
        "script-visible open stays the chart open: {:?}",
        result.plots[0].values
    );
    let fallback_fill = fallback
        .orders
        .iter()
        .find(|order| order.id == "EN")
        .expect("fallback fill");
    assert!(
        (fallback_fill.price - 10.0).abs() < 1e-10,
        "{fallback_fill:?}"
    );
}

#[test]
fn magnifier_post_fill_exit_cannot_use_consumed_lower_bar_path() {
    let program = enabled_strategy(
        r#"
strategy("consumed path", calc_on_order_fills=true, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1, stop=10.5)
if strategy.position_size > 0 and strategy.opentrades == 1
    strategy.exit("EX", "EN", stop=9.85)
plot(close)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.4, 9.8, 10.2),
                ohlc(2_300, 10.2, 10.8, 10.1, 10.6),
                ohlc(2_600, 10.6, 11.8, 10.5, 11.0),
            ],
        ),
    ])
    .expect("valid");
    let result = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run");
    let strategy = result.strategy.expect("strategy");
    assert!(
        strategy.orders.iter().any(|order| order.id == "EN"),
        "{:?}",
        strategy.orders
    );
    assert!(
        strategy.orders.iter().all(|order| order.id != "EX"),
        "stop 9.85 exists only on consumed lower bar 0: {:?}",
        strategy.orders
    );
}

#[test]
fn magnifier_stop_limit_activates_on_one_lower_bar_and_fills_on_a_later_one() {
    let program = enabled_strategy(
        r#"
strategy("stop limit later host", overlay=false, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("SL", strategy.long, qty=1, stop=10.8, limit=8.2)
plot(close)
"#,
    );
    let chart = [timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 11.0, 8.0, 9.0)];
    let activate_only = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(1, vec![ohlc(2_000, 10.0, 11.0, 10.0, 10.8)]),
    ])
    .expect("valid");
    let activated = HistoricalRuntime::new(&program)
        .with_magnifier_input(activate_only)
        .run(&chart)
        .expect("activate")
        .strategy
        .expect("strategy");
    assert!(
        activated.orders.is_empty(),
        "stop-limit must stay unfilled after activation-only lower bar: {:?}",
        activated.orders
    );

    let activate_then_fill = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 11.0, 10.0, 10.8),
                ohlc(2_300, 10.8, 10.8, 8.0, 8.5),
            ],
        ),
    ])
    .expect("valid");
    let filled = HistoricalRuntime::new(&program)
        .with_magnifier_input(activate_then_fill)
        .run(&chart)
        .expect("fill")
        .strategy
        .expect("strategy");
    assert_eq!(filled.orders.len(), 1, "{:?}", filled.orders);
    assert_eq!(filled.orders[0].id, "SL");
    assert_eq!(filled.orders[0].bar_index, 1);
    assert_eq!(filled.orders[0].time, 2_000);
    assert!(
        (filled.orders[0].price - 8.2).abs() < 1e-10,
        "{:?}",
        filled.orders[0]
    );
}

#[test]
fn magnifier_trailing_ratchet_is_monotonic_across_lower_bars() {
    let program = enabled_strategy(
        r#"
strategy("23.0 trailing ratchet", overlay=false, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1)
    strategy.exit("TR", "EN", trail_price=10.4, trail_offset=2)
plot(close)
"#,
    );
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.6, 10.6, 10.6, 10.6),
                ohlc(2_300, 10.6, 10.6, 10.59, 10.6),
                ohlc(2_600, 10.50, 10.50, 10.40, 10.45),
            ],
        ),
    ])
    .expect("valid");
    let result = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&[timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)])
        .expect("run");
    let strategy = result.strategy.expect("strategy");
    let entry = strategy
        .orders
        .iter()
        .find(|order| order.id == "EN")
        .expect("entry");
    let trail = strategy
        .orders
        .iter()
        .find(|order| order.id == "TR")
        .expect("trailing fill");
    assert_eq!(entry.bar_index, 1);
    assert_eq!(trail.bar_index, 1);
    assert_eq!(trail.time, 2_000);
    assert!(
        (trail.price - 10.50).abs() < 1e-10,
        "monotonic 10.6 ratchet crosses 10.58 in the 10.6->10.50 gap and fills at next open 10.50, not an unwound 10.48 stop: {trail:?}"
    );
}

#[test]
fn magnifier_oca_cancel_blocks_later_lower_bar_peer() {
    let program = enabled_strategy(
        r#"
strategy("oca cancel later host", overlay=false, pyramiding=2, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.order("LIM", strategy.long, qty=1, limit=8.2, oca_name="g", oca_type=strategy.oca.cancel)
    strategy.order("STP", strategy.long, qty=1, stop=10.8, oca_name="g", oca_type=strategy.oca.cancel)
plot(close)
"#,
    );
    let chart = [timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 11.0, 8.0, 9.0)];
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.2, 8.0, 8.5),
                ohlc(2_300, 8.5, 11.0, 8.5, 10.0),
            ],
        ),
    ])
    .expect("valid");
    let strategy = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&chart)
        .expect("run")
        .strategy
        .expect("strategy");
    let ids: Vec<_> = strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert_eq!(ids, vec!["LIM"], "{ids:?}");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert!((strategy.orders[0].price - 8.2).abs() < 1e-10);
    let standard = HistoricalRuntime::new(&program)
        .run(&chart)
        .expect("standard")
        .strategy
        .expect("strategy");
    let standard_ids: Vec<_> = standard
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert_eq!(standard_ids, vec!["STP"], "{standard_ids:?}");
}

#[test]
fn mixed_oca_magnifier_entry_fill_cancels_later_lower_bar_order() {
    let program = enabled_strategy(
        r#"
strategy("mixed oca magnifier", overlay=false, pyramiding=2, initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("LIM", strategy.long, qty=1, limit=8.2, oca_name="g", oca_type=strategy.oca.cancel)
    strategy.order("STP", strategy.long, qty=1, stop=10.8, oca_name="g", oca_type=strategy.oca.cancel)
plot(close)
"#,
    );
    let chart = [timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 11.0, 8.0, 9.0)];
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 10.2, 8.0, 8.5),
                ohlc(2_300, 8.5, 11.0, 8.5, 10.0),
            ],
        ),
    ])
    .expect("valid");
    let strategy = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&chart)
        .expect("run")
        .strategy
        .expect("strategy");
    let ids: Vec<_> = strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert_eq!(ids, vec!["LIM"], "{ids:?}");
    assert_eq!(strategy.orders[0].bar_index, 1);
}

#[test]
fn magnifier_risk_close_loses_to_earlier_lower_bar_exit() {
    let program = enabled_strategy(
        r#"
strategy("risk vs exit", overlay=false, initial_capital=1000, use_bar_magnifier=true)
strategy.risk.max_drawdown(40, strategy.cash)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=10)
    strategy.exit("EX", "EN", limit=11)
plot(close)
"#,
    );
    let chart = [timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 5.0, 11.0)];
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 11.0, 10.0, 10.8),
                ohlc(2_300, 10.8, 10.8, 5.0, 5.0),
            ],
        ),
    ])
    .expect("valid");
    let strategy = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&chart)
        .expect("run")
        .strategy
        .expect("strategy");
    let ids: Vec<_> = strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert!(ids.contains(&"EN"), "{ids:?}");
    assert!(ids.contains(&"EX"), "{ids:?}");
    assert!(
        !ids.iter()
            .any(|id| id.contains("Drawdown") || id.contains("Risk") || *id == "Max Drawdown"),
        "profit exit on the first lower bar must flatten before the later risk close: {ids:?}"
    );
    let exit = strategy
        .orders
        .iter()
        .find(|order| order.id == "EX")
        .expect("exit");
    assert_eq!(exit.bar_index, 1);
    assert!((exit.price - 11.0).abs() < 1e-10, "{exit:?}");
    assert_eq!(
        strategy.position.last().map(|position| position.size),
        Some(0.0)
    );
}

#[test]
fn magnifier_exit_fills_before_later_lower_bar_margin_call() {
    let program = enabled_strategy(
        r#"
strategy("margin vs exit", overlay=false, initial_capital=120, margin_long=25, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=40)
    strategy.exit("EX", "EN", limit=10.8)
plot(close)
"#,
    );
    let chart = [timed_bar(1_000, 10.0), ohlc(2_000, 10.0, 12.0, 8.0, 11.0)];
    let input = magnifier_input_from_groups(vec![
        group(0, vec![timed_bar(1_000, 10.0)]),
        group(
            1,
            vec![
                ohlc(2_000, 10.0, 11.0, 10.0, 10.8),
                ohlc(2_300, 10.8, 10.8, 8.0, 8.5),
            ],
        ),
    ])
    .expect("valid");
    let strategy = HistoricalRuntime::new(&program)
        .with_magnifier_input(input)
        .run(&chart)
        .expect("run")
        .strategy
        .expect("strategy");
    let ids: Vec<_> = strategy
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert_eq!(ids[0], "EN", "{ids:?}");
    assert!(ids.contains(&"EX"), "{ids:?}");
    assert!(
        !ids.contains(&"Margin Call"),
        "user exit on the first lower bar must flatten before later margin: {ids:?}"
    );
    let exit = strategy
        .orders
        .iter()
        .find(|order| order.id == "EX")
        .expect("exit");
    assert_eq!(exit.bar_index, 1);
    assert!((exit.price - 10.8).abs() < 1e-10, "{exit:?}");
}
