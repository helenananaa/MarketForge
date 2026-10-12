use std::sync::Arc;

use pine_syntax::SourceFile;

use super::*;

fn compile_fixture(name: &str, source: &str) -> pine_ir::HirProgram {
    let analysis = pine_sema::analyze_source(&SourceFile::new(name, source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{name}: {:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("legacy fixture HIR")
}

fn timed_close(time: i64, close: f64) -> Bar {
    Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

#[test]
fn v4_identical_duplicate_plotshape_transp_matches_single_argument_output() {
    let duplicate = compile_fixture(
        "v4-repeated-transp.pine",
        "//@version=4\nstudy(\"repeated\")\nplotshape(close > 9, color=color.green, style=shape.circle, transp=0, transp=0)\n",
    );
    let single = compile_fixture(
        "v4-single-transp.pine",
        "//@version=4\nstudy(\"single\")\nplotshape(close > 9, color=color.green, style=shape.circle, transp=0)\n",
    );
    let bars = [timed_close(0, 8.0), timed_close(86_400_000, 10.0)];
    let repeated = run_historical(&duplicate, &bars).expect("duplicate v4 transparency run");
    let normal = run_historical(&single, &bars).expect("single v4 transparency run");
    assert_eq!(repeated.plot_shapes, normal.plot_shapes);
    assert_eq!(
        repeated.plot_shapes[0].values,
        vec![PineValue::Bool(false), PineValue::Bool(true)]
    );
}

#[test]
fn v5_deprecated_plot_transp_and_numeric_bool_equality_run_together() {
    let program = compile_fixture(
        "v5-plot-transp-bool-equality.pine",
        "//@version=5\nindicator(\"v5 deprecated plot\")\nup = close > open\nplot(up == 1 ? close : na, color=color.green, transp=40)\nplot(up == 0 ? close : na)\n",
    );
    let bars = [
        Bar {
            time: 0,
            open: 9.0,
            high: 10.0,
            low: 9.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 86_400_000,
            open: 11.0,
            high: 11.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&program, &bars).expect("v5 legacy plot run");
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(10.0), PineValue::Na]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Float(10.0)]
    );
    assert_eq!(
        result.plots[0].colors[0],
        PineValue::Color(crate::builtins::colors::apply_transparency(0x4caf50, 40))
    );

    let v6 = pine_sema::analyze_source(&SourceFile::new(
        "v6-reject-removed-transp.pine",
        "//@version=6\nindicator(\"v6\")\nplot((close > open) == 1 ? close : na, transp=40)\n",
    ));
    assert!(
        v6.diagnostics
            .iter()
            .any(|item| item.code == "E_OPERATOR_TYPE")
    );
    assert!(
        v6.diagnostics
            .iter()
            .any(|item| item.code == "E_CALL_ARG_NAME")
    );
}

#[test]
fn v6_extreme_length_integer_quotient_is_truncated_only_in_length_context() {
    let program = compile_fixture(
        "v6-extreme-quotient.pine",
        "//@version=6\nindicator(\"extreme quotient\")\nlength = input.int(5)\ndivisor = input.int(2)\nplot(ta.lowest(length / divisor))\nplot(ta.highest(length / divisor))\nplot(length / divisor)\n",
    );
    let bars = [10.0, 9.0, 8.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();
    let result = run_historical(&program, &bars).expect("v6 extreme quotient run");
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Na, PineValue::Float(9.0), PineValue::Float(8.0)]
    );
    assert_eq!(
        result.plots[1].values,
        vec![PineValue::Na, PineValue::Float(10.0), PineValue::Float(9.0)]
    );
    assert_values_close(&result.plots[2].values, &[2.5, 2.5, 2.5]);

    let unsupported_float = pine_sema::analyze_source(&SourceFile::new(
        "v6-series-float-extreme.pine",
        "//@version=6\nindicator(\"float extreme\")\nplot(ta.lowest(close / 2))\n",
    ));
    assert!(
        unsupported_float
            .diagnostics
            .iter()
            .any(|item| item.code == "E_CALL_ARG_TYPE")
    );
}

#[test]
fn legacy_v2_self_referencing_float_extrema_infer_and_run() {
    let program = compile_fixture(
        "v2-self-extrema.pine",
        "//@version=2\nstudy(\"self extrema\")\nlower = close - 1\nupper = close + 1\nlowBand = close[1] > lowBand[1] ? max(lower, lowBand[1]) : lower\nhighBand = close[1] < highBand[1] ? min(upper, highBand[1]) : upper\nplot(lowBand)\nplot(highBand)\n",
    );
    let bars = [10.0, 11.0, 12.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();
    let result = run_historical(&program, &bars).expect("v2 self extrema run");
    assert_values_close(&result.plots[0].values, &[9.0, 10.0, 11.0]);
    assert_values_close(&result.plots[1].values, &[11.0, 11.0, 13.0]);
}

#[test]
fn legacy_v2_nested_iff_nz_self_reference_infers_integer_state() {
    let program = compile_fixture(
        "v2-self-iff-nz.pine",
        "//@version=2\nstudy(\"self iff nz\")\npos = iff(close > 10, 1, iff(close < 10, -1, nz(pos[1], 0)))\nplot(pos)\n",
    );
    let bars = [9.0, 10.0, 11.0, 10.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();
    let result = run_historical(&program, &bars).expect("v2 nested iff nz run");
    assert_values_close(&result.plots[0].values, &[-1.0, -1.0, 1.0, 1.0]);
}

#[test]
fn legacy_v3_v4_inverse_trigonometric_names_use_math_builtins() {
    for version in [3, 4] {
        let source = format!(
            "//@version={version}\nstudy(\"inverse trig\")\nplot(acos(-1))\nplot(asin(1))\nplot(atan(1))\n"
        );
        let program = compile_fixture("legacy-inverse-trig.pine", &source);
        let result = run_historical(&program, &[timed_close(0, 1.0)]).expect("inverse trig run");
        assert_values_close(&result.plots[0].values, &[std::f64::consts::PI]);
        assert_values_close(&result.plots[1].values, &[std::f64::consts::FRAC_PI_2]);
        assert_values_close(&result.plots[2].values, &[std::f64::consts::FRAC_PI_4]);
    }
}

#[test]
fn legacy_udf_input_and_prior_local_support_nested_history_recursion() {
    let body = r#"study("legacy local input")
smooth(x) =>
    factor = input(0.5)
    baseline = lowest(x, 2)
    value = na(value[1]) ? baseline : value[1] + factor * (baseline - value[1])
    value
plot(smooth(close))
"#;
    let bars = [2.0, 4.0, 6.0, 8.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();
    for source in [body.to_owned(), format!("//@version=2\n{body}")] {
        let program = compile_fixture("legacy-udf-local-input.pine", &source);
        let result = run_historical(&program, &bars).expect("legacy local input run");
        assert_eq!(result.plots[0].values[0], PineValue::Na);
        assert_values_close(&result.plots[0].values[1..], &[2.0, 3.0, 4.5]);
    }
}

#[test]
fn legacy_function_local_self_history_keeps_separate_call_site_series() {
    let body = r#"study("legacy recursive average")
wwma(length, price) =>
    value = (nz(value[1]) * (length - 1) + price) / length
plot(wwma(2, close))
plot(wwma(3, close))
"#;
    let bars = [2.0, 4.0, 6.0, 8.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();
    for source in [body.to_owned(), format!("//@version=2\n{body}")] {
        let program = compile_fixture("legacy-udf-self-history.pine", &source);
        let result = run_historical(&program, &bars).expect("legacy recursive average");
        assert_values_close(&result.plots[0].values, &[1.0, 2.5, 4.25, 6.125]);
        assert_values_close(
            &result.plots[1].values,
            &[2.0 / 3.0, 16.0 / 9.0, 86.0 / 27.0, 388.0 / 81.0],
        );
    }
}

#[test]
fn legacy_v1_extremes_skip_na_inside_a_complete_bar_window() {
    let source = r#"study("legacy extremes")
source = bar_index == 0 ? na : close
plot(highest(source, 3))
plot(lowest(source, 3))
"#;
    let program = compile_fixture("legacy-v1-na-extremes.pine", source);
    let bars = [2.0, 4.0, 6.0, 8.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();
    let result = run_historical(&program, &bars).expect("legacy extrema run");
    assert_eq!(result.plots[0].values[0], PineValue::Na);
    assert_eq!(result.plots[0].values[1], PineValue::Na);
    assert_values_close(&result.plots[0].values[2..], &[6.0, 8.0]);
    assert_values_close(&result.plots[1].values[2..], &[4.0, 4.0]);
}

#[test]
fn implicit_v1_sma_reversal_strategy_matches_canonical_plots_and_orders() {
    let source =
        include_str!("../../../../tests/fixtures/runtime/legacy_v1_strategy_sma_reversal.pine");
    let legacy = compile_fixture("implicit-v1-sma-reversal.pine", source);
    let explicit = compile_fixture(
        "explicit-v1-sma-reversal.pine",
        &format!("//@version=1\n{source}"),
    );
    let canonical = compile_fixture(
        "v6-sma-reversal.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/legacy_v1_strategy_sma_reversal_canonical.pine"
        ),
    );
    let bars = [10.0, 10.0, 10.0, 12.0, 14.0, 13.0, 10.0, 9.0, 12.0, 15.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();

    let actual = run_historical(&legacy, &bars).expect("implicit v1 strategy run");
    let expected = run_historical(&canonical, &bars).expect("canonical reversal run");
    assert_eq!(actual, expected);
    assert_eq!(
        run_historical(&explicit, &bars).expect("explicit v1 strategy run"),
        expected
    );
    assert!(!actual.strategy.expect("strategy output").orders.is_empty());
}

#[test]
fn v2_v3_sma_cross_strategy_matches_canonical_orders_and_plots() {
    let canonical = compile_fixture(
        "v6-strategy-sma-cross.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/legacy_v2_v3_strategy_sma_cross_canonical.pine"
        ),
    );
    let bars = [10.0, 10.0, 10.0, 12.0, 14.0, 13.0, 10.0, 9.0, 12.0, 15.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();
    let expected = run_historical(&canonical, &bars).expect("canonical SMA cross run");
    assert!(
        !expected
            .strategy
            .as_ref()
            .expect("strategy output")
            .orders
            .is_empty()
    );

    for (name, source) in [
        (
            "v2-strategy-sma-cross.pine",
            include_str!("../../../../tests/fixtures/runtime/legacy_v2_strategy_sma_cross.pine"),
        ),
        (
            "v3-strategy-sma-cross.pine",
            include_str!("../../../../tests/fixtures/runtime/legacy_v3_strategy_sma_cross.pine"),
        ),
    ] {
        let program = compile_fixture(name, source);
        assert_eq!(
            run_historical(&program, &bars).expect(name),
            expected,
            "{name}"
        );
    }
}

#[test]
fn v4_strategy_legacy_wma_and_orders_match_canonical_execution() {
    let legacy = compile_fixture(
        "v4-strategy.pine",
        include_str!("../../../../tests/fixtures/runtime/legacy_v4_strategy_hull_control.pine"),
    );
    let canonical = compile_fixture(
        "v6-strategy.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/legacy_v4_strategy_hull_control_canonical.pine"
        ),
    );
    let bars = [10.0, 11.0, 12.0, 13.0, 12.0, 11.0, 10.0, 12.0, 14.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| timed_close(index as i64 * 86_400_000, close))
        .collect::<Vec<_>>();

    let legacy_result = run_historical(&legacy, &bars).expect("v4 strategy run");
    let canonical_result = run_historical(&canonical, &bars).expect("v6 strategy run");
    assert_eq!(legacy_result, canonical_result);
    assert!(
        !legacy_result
            .strategy
            .expect("strategy output")
            .orders
            .is_empty()
    );
}

fn legacy_security_environment(bars: Vec<Bar>) -> RequestEnvironment {
    let key = RequestKey::new(
        "NYSE:IBM",
        RequestTimeframe::parse("5").expect("five minute timeframe"),
    );
    let provider = InMemoryRequestDataProvider::from_streams([(key, bars)])
        .expect("valid legacy security request bars");
    RequestEnvironment::new(ChartContext::default(), Arc::new(provider))
}

fn legacy_security_environment_with_timeframes(
    requested_timeframe: &str,
    chart_timeframe: &str,
    bars: Vec<Bar>,
) -> RequestEnvironment {
    let key = RequestKey::new(
        "NYSE:IBM",
        RequestTimeframe::parse(requested_timeframe).expect("requested timeframe"),
    );
    let provider = InMemoryRequestDataProvider::from_streams([(key, bars)])
        .expect("valid legacy security request bars");
    RequestEnvironment::new(
        ChartContext::new(
            "NASDAQ:AAPL",
            RequestTimeframe::parse(chart_timeframe).expect("chart timeframe"),
        ),
        Arc::new(provider),
    )
}

fn chart_inherited_study_environment() -> RequestEnvironment {
    RequestEnvironment::new(
        ChartContext::new(
            "TEST:CHART",
            RequestTimeframe::parse("5").expect("five minute chart timeframe"),
        ),
        Arc::new(InMemoryRequestDataProvider::new()),
    )
}

#[test]
fn legacy_v4_timenow_matches_canonical_execution_clock_contract() {
    let legacy = compile_fixture(
        "timenow_execution_clock_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/timenow_execution_clock_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "timenow_execution_clock_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/timenow_execution_clock_canonical.pine"
        ),
    );
    let bars = [timed_close(0, 1.0), timed_close(60_000, 2.0)];
    let execution_times = [1_700_000_000_101, 1_700_000_060_205];

    let legacy_result = run_historical_with_execution_times(&legacy, &bars, &execution_times)
        .expect("legacy timenow execution clock");
    let canonical_result = run_historical_with_execution_times(&canonical, &bars, &execution_times)
        .expect("canonical timenow execution clock");

    assert_eq!(legacy_result, canonical_result);
    assert_eq!(
        legacy_result.plots[0].values,
        vec![
            PineValue::Int(1_700_000_000_101),
            PineValue::Int(1_700_000_060_205)
        ]
    );
    assert_eq!(
        legacy_result.plots[2].values,
        vec![PineValue::Na, PineValue::Int(1_700_000_000_101)]
    );
}

fn legacy_security_profile_program(
    name: &str,
    gaps: &str,
    lookahead: &str,
    version: u16,
) -> pine_ir::HirProgram {
    let mut program = compile_fixture(
        name,
        &format!(
            "//@version=4\nstudy(\"legacy security profile\")\nplot(security(\"NYSE:IBM\", \"5\", close, gaps={gaps}, lookahead={lookahead}))\n"
        ),
    );
    program.language_version = Some(version);
    program
}

#[test]
fn legacy_v4_empty_study_resolution_matches_chart_context_batch_incremental_and_realtime() {
    let legacy = compile_fixture(
        "study_empty_resolution_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/study_empty_resolution_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "study_empty_resolution_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/study_empty_resolution_canonical.pine"
        ),
    );
    assert_eq!(legacy.drawing_settings, canonical.drawing_settings);

    let bars = [
        timed_close(0, 10.0),
        timed_close(300_000, 20.0),
        timed_close(600_000, 30.0),
    ];
    let environment = chart_inherited_study_environment();
    let expected = run_historical_with_request_environment(&canonical, &bars, environment.clone())
        .expect("canonical chart-context run");
    let batch = run_historical_with_request_environment(&legacy, &bars, environment.clone())
        .expect("legacy chart-context run");
    assert_eq!(batch, expected);
    assert_eq!(batch.plots[0].values[0], PineValue::Na);
    assert_values_close(&batch.plots[0].values[1..], &[15.0, 25.0]);
    assert_values_close(&batch.plots[1].values, &[10.0, 20.0, 30.0]);

    let mut incremental = HistoricalRuntime::with_request_environment(&legacy, environment.clone());
    for bar in bars {
        incremental
            .append_bar(bar)
            .expect("incremental chart-context bar");
    }
    assert_eq!(incremental.result(), batch);

    let mut realtime = RealtimeRuntime::with_request_environment(&legacy, environment);
    for bar in bars {
        realtime
            .update(BarUpdate::historical(bar))
            .expect("realtime chart-context historical bar");
    }
    assert_eq!(realtime.confirmed_result(), batch);
}

fn legacy_v2_core_bars() -> [Bar; 6] {
    [
        Bar {
            time: 0,
            open: 10.0,
            high: 12.0,
            low: 9.0,
            close: 11.0,
            volume: 100.0,
        },
        Bar {
            time: 60_000,
            open: 12.0,
            high: 13.0,
            low: 10.0,
            close: 11.0,
            volume: 110.0,
        },
        Bar {
            time: 120_000,
            open: 11.0,
            high: 14.0,
            low: 10.0,
            close: 13.0,
            volume: 120.0,
        },
        Bar {
            time: 180_000,
            open: 13.0,
            high: 14.0,
            low: 9.0,
            close: 10.0,
            volume: 130.0,
        },
        Bar {
            time: 240_000,
            open: 10.0,
            high: 12.0,
            low: 8.0,
            close: 10.0,
            volume: 140.0,
        },
        Bar {
            time: 300_000,
            open: 9.0,
            high: 12.0,
            low: 8.0,
            close: 11.0,
            volume: 150.0,
        },
    ]
}

#[test]
fn legacy_v2_core_matches_explicit_canonical_batch_incremental_and_realtime() {
    let legacy = compile_fixture(
        "legacy_v2_core.pine",
        include_str!("../../../../tests/fixtures/legacy/v2/runtime/core_legacy.pine"),
    );
    let canonical = compile_fixture(
        "legacy_v2_core_canonical.pine",
        include_str!("../../../../tests/fixtures/legacy/v2/runtime/core_canonical.pine"),
    );
    let bars = legacy_v2_core_bars();
    let expected = run_historical(&canonical, &bars).expect("canonical v2 conversion");
    let batch = run_historical(&legacy, &bars).expect("legacy v2 batch");
    assert_eq!(batch, expected);
    assert_values_close(
        &batch.plots[0].values,
        &[11.0, 22.0, 35.0, 45.0, 55.0, 66.0],
    );
    assert_values_close(&batch.plots[1].values, &[0.0, 12.0, 24.0, 38.0, 49.0, 60.0]);
    assert_values_close(
        &batch.plots[3].values,
        &[23.0, 23.0, 27.0, 21.0, 21.0, 23.0],
    );
    assert_eq!(
        batch.plots[4].values,
        vec![
            PineValue::Na,
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(1.0),
            PineValue::Float(2.0),
        ]
    );

    let mut incremental = HistoricalRuntime::new(&legacy);
    for bar in bars {
        incremental.append_bar(bar).expect("incremental v2 bar");
    }
    assert_eq!(incremental.result(), batch);

    let mut realtime = RealtimeRuntime::new(&legacy);
    let mut realtime_result = None;
    for bar in bars {
        realtime_result = Some(
            realtime
                .update(BarUpdate::historical(bar))
                .expect("realtime v2 historical bar"),
        );
    }
    assert_eq!(realtime_result.expect("realtime result"), batch);
}

#[test]
fn implicit_v1_and_explicit_v2_shared_runtime_profile_match() {
    let v1_source = SourceFile::new(
        "shared_v1.pine",
        include_str!("../../../../tests/fixtures/legacy/v1/runtime/shared_v1.pine"),
    );
    let v1_analysis = pine_sema::analyze_source(&v1_source);
    assert!(
        v1_analysis.diagnostics.is_empty(),
        "{:?}",
        v1_analysis.diagnostics
    );
    let v1 = v1_analysis.hir.expect("implicit v1 HIR");
    let v2 = compile_fixture(
        "shared_v2.pine",
        include_str!("../../../../tests/fixtures/legacy/v2/runtime/shared_v2.pine"),
    );
    let bars = legacy_v2_core_bars();
    assert_eq!(
        run_historical(&v1, &bars).expect("implicit v1 run"),
        run_historical(&v2, &bars).expect("explicit v2 run")
    );
}

#[test]
fn implicit_v1_four_space_ternary_continuations_match_canonical_runtime() {
    let legacy = compile_fixture(
        "ternary_continuation_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v1/runtime/ternary_continuation_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "ternary_continuation_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v1/runtime/ternary_continuation_canonical.pine"
        ),
    );
    let bars = legacy_v2_core_bars();

    let legacy_result =
        run_historical(&legacy, &bars).expect("implicit v1 ternary continuation run");
    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical ternary continuation run");

    assert_eq!(legacy_result, canonical_result);
    assert_values_close(
        &legacy_result.plots[0].values,
        &[12.0, 10.0, 14.0, 9.0, 10.0, 12.0],
    );
    assert_values_close(
        &legacy_result.plots[1].values,
        &[12.0, 10.0, 14.0, 9.0, 8.0, 12.0],
    );
}

#[test]
fn implicit_v1_graph_source_order_prerequisite_matches_canonical_runtime() {
    let legacy = compile_fixture(
        "graph_source_order_prerequisite_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v1/runtime/graph_source_order_prerequisite_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "graph_source_order_prerequisite_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v1/runtime/graph_source_order_prerequisite_canonical.pine"
        ),
    );
    let bars = legacy_v2_core_bars();

    let legacy_result = run_historical(&legacy, &bars).expect("implicit v1 source-order graph run");
    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical source-order graph run");

    assert_eq!(legacy_result, canonical_result);
    assert_values_close(
        &legacy_result.plots[0].values,
        &[0.0, 0.0, 1.0, -1.0, -1.0, 1.0],
    );
    assert_values_close(
        &legacy_result.plots[1].values,
        &[0.0, 0.0, 1.0, -1.0, 0.0, 1.0],
    );
}

#[test]
fn spaced_equals_version_annotation_matches_canonical_runtime() {
    let legacy = compile_fixture(
        "spaced_version_annotation_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/spaced_version_annotation_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "spaced_version_annotation_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/spaced_version_annotation_canonical.pine"
        ),
    );
    let bars = legacy_v2_core_bars();

    let legacy_result = run_historical(&legacy, &bars).expect("spaced version annotation run");
    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical version annotation run");

    assert_eq!(legacy_result, canonical_result);
    assert_values_close(
        &legacy_result.plots[0].values,
        &[11.0, 12.0, 13.0, 13.0, 10.0, 11.0],
    );
}

#[test]
fn nested_if_statement_returns_match_canonical_expression_results() {
    let legacy = compile_fixture(
        "nested_if_expression_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/nested_if_expression_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "nested_if_expression_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/nested_if_expression_canonical.pine"
        ),
    );
    let bars = legacy_v2_core_bars();

    let legacy_result = run_historical(&legacy, &bars).expect("legacy nested if run");
    let canonical_result = run_historical(&canonical, &bars).expect("canonical nested if run");

    assert_eq!(legacy_result, canonical_result);
    assert_values_close(
        &legacy_result.plots[0].values,
        &[9.0, 12.0, 13.0, 13.0, 12.0, 12.0],
    );
}

#[test]
fn legacy_v1_v2_bool_numeric_comparisons_match_explicit_float_casts() {
    let legacy = compile_fixture(
        "bool_numeric_comparisons_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v2/runtime/bool_numeric_comparisons_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "bool_numeric_comparisons_canonical.pine",
        "//@version=6\nindicator(\"canonical bool numeric comparisons\")\nup = close > open\nequalOne = float(up) == 1\nnotZero = 0 != float(up)\ngreaterThanZero = float(up) > 0\natMostOne = 1 >= float(up)\nplot(equalOne ? close : na, title=\"bool equals one\")\nplot(notZero ? close : na, title=\"zero differs from bool\")\nplot(greaterThanZero ? close : na, title=\"bool greater than zero\")\nplot(atMostOne ? close : na, title=\"one at least bool\")\n",
    );
    let bars = legacy_v2_core_bars();

    let legacy_result = run_historical(&legacy, &bars).expect("legacy bool comparison run");
    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical bool comparison run");

    assert_eq!(legacy_result, canonical_result);
    assert_eq!(
        legacy_result.plots[0].values,
        vec![
            PineValue::Float(11.0),
            PineValue::Na,
            PineValue::Float(13.0),
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(11.0),
        ]
    );
    assert_eq!(legacy_result.plots[1].values, legacy_result.plots[0].values);
    assert_eq!(legacy_result.plots[2].values, legacy_result.plots[0].values);
    assert_eq!(
        legacy_result.plots[3].values,
        vec![
            PineValue::Float(11.0),
            PineValue::Float(11.0),
            PineValue::Float(13.0),
            PineValue::Float(10.0),
            PineValue::Float(10.0),
            PineValue::Float(11.0),
        ]
    );
}

#[test]
fn v4_const_integer_division_and_input_fractional_division_use_explicit_int_offsets() {
    let legacy = compile_fixture(
        "contextual_integer_division_v4_native_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/contextual_integer_division_v4_native_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "contextual_integer_division_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/contextual_integer_division_canonical.pine"
        ),
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
    ];

    let result = run_historical(&legacy, &bars).expect("legacy integer division run");
    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical integer division run");

    assert_eq!(result, canonical_result);
    assert_eq!(result.plots[0].values[0], PineValue::Na);
    assert_values_close(&result.plots[0].values[1..], &[5.0 / 3.0, 8.0 / 3.0]);
    assert_eq!(result.plots[1].values, result.plots[0].values);
    assert_values_close(&result.plots[2].values, &[2.5, 2.5, 2.5]);
    assert_eq!(
        result.plots[3].values,
        vec![PineValue::Na, PineValue::Na, PineValue::Float(1.0)]
    );
    assert_eq!(result.plots[4].values, result.plots[3].values);
}

#[test]
fn v5_extreme_length_context_truncates_input_quotient_without_changing_value() {
    let source = compile_fixture(
        "v5_extreme_quotient.pine",
        "//@version=5\nindicator('v5 quotient')\nlen = input.int(5)\ndivisor = input.int(2)\nwindow(src) => ta.lowest(src, len / divisor)\nplot(window(close))\nplot(len / divisor)\n",
    );
    let explicit = compile_fixture(
        "v5_extreme_quotient_explicit.pine",
        "//@version=5\nindicator('v5 quotient')\nlen = input.int(5)\ndivisor = input.int(2)\nwindow(src) => ta.lowest(src, int(len / divisor))\nplot(window(close))\nplot(len / divisor)\n",
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 3.0),
        timed_close(120_000, 2.0),
    ];
    let result = run_historical(&source, &bars).expect("v5 contextual quotient run");
    assert_eq!(
        result,
        run_historical(&explicit, &bars).expect("explicit v5 quotient run")
    );
    assert_eq!(result.plots[0].values[2], PineValue::Float(2.0));
    assert_eq!(result.plots[1].values[2], PineValue::Float(2.5));
}

#[test]
fn legacy_v4_udf_builtin_aliases_use_definition_source_order() {
    let legacy = compile_fixture(
        "udf_source_order_builtin_aliases_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/udf_source_order_builtin_aliases_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "udf_source_order_builtin_aliases_canonical.pine",
        "//@version=6\nindicator(\"canonical UDF source-order built-ins\")\nsmoothedRsi(source, length) =>\n    localRsi = ta.rsi(source, length)\n    ta.sma(localRsi, 2)\nglobalRsi = ta.rsi(close, 3)\nplot(smoothedRsi(close, 3), title=\"local RSI\")\nplot(globalRsi, title=\"global RSI\")\n",
    );
    let bars = legacy_v2_core_bars();

    assert_eq!(
        run_historical(&legacy, &bars).expect("legacy UDF source-order run"),
        run_historical(&canonical, &bars).expect("canonical UDF source-order run")
    );
}

#[test]
fn legacy_v4_udf_final_statements_match_explicit_v6_returns() {
    let legacy = compile_fixture(
        "udf_final_statements_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/udf_final_statements_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "udf_final_statements_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/udf_final_statements_canonical.pine"
        ),
    );
    let bars = [
        bar_ohlc(10.0, 12.0, 9.0, 11.0),
        bar_ohlc(12.0, 13.0, 9.0, 10.0),
        bar_ohlc(10.0, 15.0, 10.0, 14.0),
        bar_ohlc(14.0, 16.0, 11.0, 12.0),
    ];

    let legacy_result = run_historical(&legacy, &bars).expect("legacy UDF final-statement run");
    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical explicit-return run");

    assert_eq!(legacy_result, canonical_result);
    assert_values_close(&legacy_result.plots[0].values, &[22.0, 20.0, 28.0, 24.0]);
    assert_values_close(&legacy_result.plots[1].values, &[33.0, 30.0, 42.0, 36.0]);
    assert_values_close(&legacy_result.plots[2].values, &[12.0, 9.0, 15.0, 11.0]);
    assert_values_close(&legacy_result.plots[3].values, &[11.0, -1.0, 14.0, -1.0]);
    assert_values_close(&legacy_result.plots[4].values, &[11.0, 10.0, 14.0, 12.0]);
}

#[test]
fn legacy_numeric_bool_call_arguments_use_pre_v6_truthiness() {
    let program = compile_fixture(
        "numeric_bool_call_arguments_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/numeric_bool_call_arguments_legacy.pine"
        ),
    );
    let bars = legacy_v2_core_bars();

    let result = run_historical(&program, &bars).expect("legacy numeric bool call run");

    assert_eq!(result.plots[0].values[0], PineValue::Na);
    assert_values_close(
        &result.plots[0].values[1..],
        &[11.0, 11.0, 10.0, 10.0, 11.0],
    );
}

#[test]
fn legacy_v4_array_get_set_accept_series_integer_indexes() {
    let program = compile_fixture(
        "array_series_index_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/array_series_index_legacy.pine"),
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
        timed_close(180_000, 4.0),
    ];

    let result = run_historical(&program, &bars).expect("legacy array series index run");

    assert_values_close(&result.plots[0].values, &[1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn legacy_v4_array_insert_accepts_series_integer_indexes() {
    let program = compile_fixture(
        "array_insert_series_index_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/array_insert_series_index_legacy.pine"
        ),
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
        timed_close(180_000, 4.0),
    ];

    let result = run_historical(&program, &bars).expect("legacy array insert series index run");

    assert_values_close(&result.plots[0].values, &[1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn legacy_v4_security_time_alias_executes_same_context_time_function() {
    let program = compile_fixture(
        "security_time_alias_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/security_time_alias_legacy.pine"
        ),
    );
    let bars = [
        timed_close(1_704_067_200_000, 1.0),
        timed_close(1_704_070_800_000, 2.0),
        timed_close(1_704_153_600_000, 3.0),
    ];
    let expected = run_historical(
        &compile_fixture(
            "time_alias_oracle.pine",
            "//@version=4\nstudy(\"oracle\")\ndayOpen = time(\"D\")\nnewDay = dayOpen != dayOpen[1]\nplot(dayOpen)\nplot(valuewhen(newDay, close, 0))\n",
        ),
        &bars,
    )
    .expect("legacy time alias oracle");
    let result = run_historical(&program, &bars).expect("legacy security time alias run");

    assert_eq!(result.plots[0].values, expected.plots[0].values);
    assert_eq!(result.plots[1].values, expected.plots[1].values);
}

#[test]
fn legacy_v4_udf_reference_side_effects_match_explicit_canonical_execution() {
    let legacy = compile_fixture(
        "udf_reference_side_effects_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/udf_reference_side_effects_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "udf_reference_side_effects_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/udf_reference_side_effects_canonical.pine"
        ),
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
        timed_close(180_000, 4.0),
    ];

    let expected = run_historical(&canonical, &bars).expect("canonical reference effects");
    let batch = run_historical(&legacy, &bars).expect("legacy reference effects");
    assert_eq!(batch.plots.len(), 1);
    assert_eq!(batch.plots[0].values, expected.plots[0].values);
    assert_eq!(batch.plots[0].colors, expected.plots[0].colors);
    assert_eq!(batch.plots[0].metadata, expected.plots[0].metadata);
    assert_eq!(batch.lines, expected.lines);
    assert_eq!(batch.labels, expected.labels);
    assert_eq!(batch.diagnostics, expected.diagnostics);
    assert_values_close(&batch.plots[0].values, &[1.0, 3.0, 6.0, 10.0]);
    assert_eq!(batch.lines.len(), bars.len());
    assert_eq!(batch.labels.len(), bars.len());
    assert!(batch.lines.iter().all(|line| {
        line.snapshots
            .last()
            .is_some_and(|snapshot| !snapshot.exists)
    }));
    assert!(batch.labels.iter().all(|label| {
        label
            .snapshots
            .last()
            .is_some_and(|snapshot| !snapshot.exists)
    }));

    let mut incremental = HistoricalRuntime::new(&legacy);
    for bar in bars {
        incremental
            .append_bar(bar)
            .expect("incremental legacy reference effects");
    }
    assert_eq!(incremental.result(), batch);

    let mut realtime = RealtimeRuntime::new(&legacy);
    for bar in bars {
        realtime
            .update(BarUpdate::historical(bar))
            .expect("realtime legacy reference effects");
    }
    assert_eq!(realtime.confirmed_result(), batch);
}

#[test]
fn legacy_v4_udf_line_setters_match_explicit_canonical_execution_and_rollback() {
    let legacy = compile_fixture(
        "udf_line_setters_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/udf_line_setters_legacy.pine"),
    );
    let canonical = compile_fixture(
        "udf_line_setters_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/udf_line_setters_canonical.pine"
        ),
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
        timed_close(180_000, 4.0),
    ];

    let expected = run_historical(&canonical, &bars).expect("canonical line setters");
    let batch = run_historical(&legacy, &bars).expect("legacy line setters");
    assert_eq!(batch.plots[0].values, expected.plots[0].values);
    assert_eq!(batch.lines, expected.lines);
    assert_values_close(&batch.plots[0].values, &[1.0, 2.0, 3.0, 4.0]);
    let final_snapshot = batch.lines[0].snapshots.last().expect("line snapshot");
    assert_eq!(final_snapshot.x2, PineValue::Int(5));
    assert_eq!(
        final_snapshot.extend,
        PineValue::String("extend.none".to_owned())
    );

    let mut incremental = HistoricalRuntime::new(&legacy);
    for bar in bars {
        incremental
            .append_bar(bar)
            .expect("incremental line setter");
    }
    assert_eq!(incremental.result(), batch);

    let mut realtime = RealtimeRuntime::new(&legacy);
    for bar in &bars[..3] {
        realtime
            .update(BarUpdate::historical(*bar))
            .expect("historical line setter prefix");
    }
    let mut mutated = bars[3];
    mutated.open = 40.0;
    mutated.high = 40.0;
    mutated.low = 40.0;
    mutated.close = 40.0;
    realtime
        .update(BarUpdate::forming(mutated))
        .expect("mutated forming line setter");
    realtime
        .update(BarUpdate::forming(bars[3]))
        .expect("replacement forming line setter");
    realtime
        .update(BarUpdate::confirmed(bars[3]))
        .expect("confirmed line setter");
    assert_eq!(realtime.confirmed_result(), batch);
}

#[test]
fn legacy_v4_dynamic_plot_style_uses_the_final_evaluated_enum() {
    let program = compile_fixture(
        "plot_dynamic_style_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/plot_dynamic_style_legacy.pine"),
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
        timed_close(180_000, 4.0),
    ];

    let result = run_historical(&program, &bars).expect("legacy dynamic plot style run");

    assert_eq!(result.plots.len(), 2);
    assert_values_close(&result.plots[0].values, &[1.0, 2.0, 3.0, 4.0]);
    assert_eq!(
        result.plots[0].style,
        PineValue::String("plot.style_histogram".to_owned())
    );
    assert_eq!(
        result.plots[1].style,
        PineValue::String("plot.style_line".to_owned())
    );
}

#[test]
fn legacy_v4_dynamic_plotshape_and_hline_style_use_enums() {
    let program = compile_fixture(
        "plotshape_hline_dynamic_style_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/plotshape_hline_dynamic_style_legacy.pine"
        ),
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
        timed_close(180_000, 4.0),
    ];

    let result =
        run_historical(&program, &bars).expect("legacy dynamic plotshape and hline style run");

    assert_eq!(result.plot_shapes.len(), 1);
    assert_eq!(
        result.plot_shapes[0].styles.last().cloned(),
        Some(PineValue::String("shape.cross".to_owned()))
    );
    assert_eq!(
        result.hlines[0].style,
        PineValue::String("hline.style_dotted".to_owned())
    );
}

#[test]
fn legacy_v4_dynamic_drawing_enums_execute_with_canonical_styles() {
    let program = compile_fixture(
        "dynamic_drawing_enums_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/dynamic_drawing_enums_legacy.pine"
        ),
    );
    let bars = [
        bar_ohlc(1.0, 3.0, 0.0, 2.0),
        bar_ohlc(3.0, 4.0, 1.0, 2.0),
        bar_ohlc(2.0, 5.0, 1.0, 4.0),
        bar_ohlc(5.0, 6.0, 0.0, 1.0),
    ];

    let result = run_historical(&program, &bars).expect("legacy dynamic drawing enum run");

    assert_eq!(result.lines.len(), 4);
    assert_eq!(result.labels.len(), 4);
    for (index, line) in result.lines.iter().enumerate() {
        let snapshot = line.snapshots.last().expect("final line snapshot");
        assert_eq!(
            snapshot.style,
            PineValue::String(
                if index % 2 == 0 {
                    "line.style_solid"
                } else {
                    "line.style_dashed"
                }
                .to_owned()
            )
        );
        assert_eq!(
            snapshot.extend,
            PineValue::String(
                if matches!(index, 0 | 2) {
                    "extend.right"
                } else {
                    "extend.none"
                }
                .to_owned()
            )
        );
    }
    for (index, label) in result.labels.iter().enumerate() {
        assert_eq!(
            label.snapshots.last().expect("final label snapshot").style,
            PineValue::String(
                if matches!(index, 0 | 2) {
                    "label.style_label_up"
                } else {
                    "label.style_label_down"
                }
                .to_owned()
            )
        );
    }
}

#[test]
fn v4_legacy_security_same_context_matches_direct_expression() {
    let legacy = compile_fixture(
        "legacy_security_same_context.pine",
        "//@version=4\nstudy(\"legacy same context\")\nplot(security(syminfo.tickerid, timeframe.period, close + open))\n",
    );
    let direct = compile_fixture(
        "legacy_security_same_context_direct.pine",
        "//@version=4\nstudy(\"legacy same context direct\")\nplot(close + open)\n",
    );
    let bars = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
    ];

    assert_eq!(
        run_historical(&legacy, &bars).expect("legacy same-context run"),
        run_historical(&direct, &bars).expect("direct same-context run")
    );
}

#[test]
fn implicit_v1_security_recomputes_immutable_aliases_in_requested_context() {
    let program = compile_fixture(
        "security_aliases_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v1/runtime/security_aliases_legacy.pine"),
    );
    let environment = legacy_security_environment(vec![
        timed_close(0, 100.0),
        timed_close(300_000, 200.0),
        timed_close(600_000, 300.0),
    ]);
    let chart = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(240_000, 3.0),
        timed_close(300_000, 4.0),
        timed_close(540_000, 5.0),
        timed_close(600_000, 6.0),
    ];

    let result = run_historical_with_request_environment(&program, &chart, environment)
        .expect("legacy security aliases should run");

    assert_eq!(result.plots[0].values[0], PineValue::Na);
    assert_eq!(result.plots[0].values[1], PineValue::Na);
    assert_eq!(result.plots[0].values[2], PineValue::Na);
    assert_values_close(&result.plots[0].values[3..], &[300.0, 300.0, 500.0]);
}

#[test]
fn v4_legacy_security_recomputes_nested_pure_udfs_in_requested_context() {
    let program = compile_fixture(
        "security_pure_udf_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/security_pure_udf_legacy.pine"),
    );
    let environment = legacy_security_environment_with_timeframes(
        "1",
        "1",
        vec![
            timed_close(0, 10.0),
            timed_close(60_000, 20.0),
            timed_close(120_000, 30.0),
        ],
    );
    let chart = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(120_000, 3.0),
    ];

    let result = run_historical_with_request_environment(&program, &chart, environment)
        .expect("legacy security pure UDF should run");

    assert_values_close(&result.plots[0].values, &[22.0, 42.0, 62.0]);
}

#[test]
fn v4_legacy_security_recomputes_udf_local_dependency_graph_in_requested_context() {
    let legacy = compile_fixture(
        "security_udf_local_dependencies_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/security_udf_local_dependencies_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "security_udf_local_dependencies_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/security_udf_local_dependencies_canonical.pine"
        ),
    );
    let environment = legacy_security_environment(vec![
        timed_close(0, 100.0),
        timed_close(300_000, 200.0),
        timed_close(600_000, 300.0),
        timed_close(900_000, 400.0),
        timed_close(1_200_000, 500.0),
    ]);
    let chart = [
        timed_close(0, 1.0),
        timed_close(240_000, 3.0),
        timed_close(300_000, 5.0),
        timed_close(540_000, 7.0),
        timed_close(600_000, 9.0),
        timed_close(840_000, 11.0),
        timed_close(900_000, 13.0),
        timed_close(1_140_000, 15.0),
        timed_close(1_200_000, 17.0),
        timed_close(1_440_000, 19.0),
    ];

    let mut expected =
        run_historical_with_request_environment(&canonical, &chart, environment.clone())
            .expect("canonical local-dependency request run");
    let batch = run_historical_with_request_environment(&legacy, &chart, environment.clone())
        .expect("legacy local-dependency request run");
    for (expected, actual) in expected.plots.iter_mut().zip(&batch.plots) {
        expected.id = actual.id;
    }
    assert_eq!(batch, expected);
    // Preserve the original six-bar prefix; nested EMA/SMA warmup now extends
    // beyond it, so also exercise populated results on subsequent bars.
    assert!(
        batch.plots[1].values[..6]
            .iter()
            .all(|v| *v == PineValue::Na)
    );
    assert_ne!(batch.plots[0].values.last(), Some(&PineValue::Na));
    assert_ne!(batch.plots[1].values.last(), Some(&PineValue::Na));

    let mut incremental = HistoricalRuntime::with_request_environment(&legacy, environment.clone());
    for bar in chart {
        incremental
            .append_bar(bar)
            .expect("incremental local-dependency request bar");
    }
    assert_eq!(incremental.result(), batch);

    let mut realtime = RealtimeRuntime::with_request_environment(&legacy, environment);
    for bar in chart {
        realtime
            .update(BarUpdate::historical(bar))
            .expect("realtime local-dependency historical bar");
    }
    assert_eq!(realtime.confirmed_result(), batch);
}

#[test]
fn legacy_v2_and_v3_security_profiles_have_distinct_historical_alignment() {
    let v2 = legacy_security_profile_program(
        "legacy_v2_security_profile.pine",
        "barmerge.gaps_off",
        "barmerge.lookahead_on",
        2,
    );
    let v3 = legacy_security_profile_program(
        "legacy_v3_security_profile.pine",
        "barmerge.gaps_off",
        "barmerge.lookahead_off",
        3,
    );
    let environment =
        legacy_security_environment(vec![timed_close(0, 100.0), timed_close(300_000, 200.0)]);
    let chart = [
        timed_close(0, 1.0),
        timed_close(60_000, 2.0),
        timed_close(240_000, 3.0),
        timed_close(300_000, 4.0),
        timed_close(540_000, 5.0),
    ];

    let v2_result = run_historical_with_request_environment(&v2, &chart, environment.clone())
        .expect("v2 lookahead profile");
    let v3_result = run_historical_with_request_environment(&v3, &chart, environment)
        .expect("v3 lookahead profile");

    assert_values_close(
        &v2_result.plots[0].values,
        &[100.0, 100.0, 100.0, 200.0, 200.0],
    );
    assert_eq!(v3_result.plots[0].values[0], PineValue::Na);
    assert_eq!(v3_result.plots[0].values[1], PineValue::Na);
    assert_values_close(&v3_result.plots[0].values[2..], &[100.0, 100.0, 200.0]);
    assert_eq!(v2_result.diagnostics.len(), 1);
    assert_eq!(v2_result.diagnostics[0].code, "W_LEGACY_SECURITY_LOOKAHEAD");
    assert!(v3_result.diagnostics.is_empty());
}

#[test]
fn legacy_security_gaps_on_preserves_versioned_mapping_boundaries() {
    let v2 = legacy_security_profile_program(
        "legacy_v2_security_gaps.pine",
        "barmerge.gaps_on",
        "barmerge.lookahead_on",
        2,
    );
    let v3 = legacy_security_profile_program(
        "legacy_v3_security_gaps.pine",
        "barmerge.gaps_on",
        "barmerge.lookahead_off",
        3,
    );
    let environment =
        legacy_security_environment(vec![timed_close(0, 100.0), timed_close(300_000, 200.0)]);
    let chart = [
        timed_close(0, 1.0),
        timed_close(240_000, 2.0),
        timed_close(300_000, 3.0),
        timed_close(540_000, 4.0),
    ];
    let v2_result = run_historical_with_request_environment(&v2, &chart, environment.clone())
        .expect("v2 gaps mapping");
    let v3_result =
        run_historical_with_request_environment(&v3, &chart, environment).expect("v3 gaps mapping");

    assert_eq!(
        v2_result.plots[0].values,
        vec![
            PineValue::Float(100.0),
            PineValue::Na,
            PineValue::Float(200.0),
            PineValue::Na,
        ]
    );
    assert_eq!(
        v3_result.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Float(100.0),
            PineValue::Na,
            PineValue::Float(200.0),
        ]
    );
}

#[test]
fn legacy_v4_security_lookahead_off_confirms_monthly_values_at_calendar_close() {
    let program = compile_fixture(
        "legacy_monthly_security.pine",
        "//@version=4\nstudy(\"legacy monthly security\")\nplot(security(\"NYSE:IBM\", \"M\", close, gaps=barmerge.gaps_off, lookahead=barmerge.lookahead_off))\n",
    );
    let environment = legacy_security_environment_with_timeframes(
        "M",
        "D",
        vec![
            timed_close(1_704_067_200_000, 100.0),
            timed_close(1_706_745_600_000, 200.0),
        ],
    );
    let chart = [
        timed_close(1_706_486_400_000, 1.0),
        timed_close(1_706_572_800_000, 2.0),
        timed_close(1_706_659_200_000, 3.0),
        timed_close(1_706_745_600_000, 4.0),
    ];
    let result = run_historical_with_request_environment(&program, &chart, environment)
        .expect("legacy monthly security should run");

    assert_eq!(result.plots[0].values[0], PineValue::Na);
    assert_eq!(result.plots[0].values[1], PineValue::Na);
    assert_values_close(&result.plots[0].values[2..], &[100.0, 100.0]);
}

#[test]
fn legacy_v4_security_does_not_treat_monthly_as_thirty_day_same_context() {
    let program = compile_fixture(
        "legacy_monthly_security_30d_chart.pine",
        "//@version=4\nstudy(\"legacy monthly security on 30D\")\nplot(security(\"NYSE:IBM\", \"M\", close, gaps=barmerge.gaps_off, lookahead=barmerge.lookahead_off))\n",
    );
    let environment = legacy_security_environment_with_timeframes(
        "M",
        "30D",
        vec![timed_close(1_704_067_200_000, 100.0)],
    );
    let chart = [
        timed_close(1_704_067_200_000, 1.0),
        timed_close(1_706_659_200_000, 2.0),
    ];
    let result = run_historical_with_request_environment(&program, &chart, environment)
        .expect("legacy monthly security on 30D chart should run");

    assert_eq!(result.plots[0].values[0], PineValue::Na);
    assert_values_close(&result.plots[0].values[1..], &[100.0]);
}

#[test]
fn legacy_security_requested_state_isolated_and_incremental_matches_batch() {
    let program = compile_fixture(
        "legacy_security_state.pine",
        "//@version=4\nstudy(\"legacy requested state\")\nchartAverage = sma(close, 2)\nrequestedAverage = security(\"NYSE:IBM\", \"5\", sma(close, 2))\nplot(chartAverage)\nplot(requestedAverage)\n",
    );
    let environment = legacy_security_environment(vec![
        timed_close(0, 100.0),
        timed_close(300_000, 200.0),
        timed_close(600_000, 300.0),
    ]);
    let chart = [
        timed_close(0, 1.0),
        timed_close(240_000, 3.0),
        timed_close(300_000, 5.0),
        timed_close(540_000, 7.0),
        timed_close(600_000, 9.0),
        timed_close(840_000, 11.0),
    ];
    let batch = run_historical_with_request_environment(&program, &chart, environment.clone())
        .expect("batch legacy state run");
    let mut incremental =
        HistoricalRuntime::with_request_environment(&program, environment.clone());
    for bar in chart {
        incremental
            .append_bar(bar)
            .expect("incremental legacy state bar");
    }
    assert_eq!(batch, incremental.result());
    assert_eq!(batch.plots[0].values[0], PineValue::Na);
    assert_values_close(&batch.plots[0].values[1..], &[2.0, 4.0, 6.0, 8.0, 10.0]);
    assert_eq!(batch.plots[1].values[0], PineValue::Na);
    assert_eq!(batch.plots[1].values[1], PineValue::Na);
    assert_eq!(batch.plots[1].values[2], PineValue::Na);
    assert_values_close(&batch.plots[1].values[3..], &[150.0, 150.0, 250.0]);

    let mut realtime = RealtimeRuntime::with_request_environment(&program, environment);
    let realtime_result = chart
        .into_iter()
        .map(|bar| {
            realtime
                .update(BarUpdate::historical(bar))
                .expect("realtime historical handoff")
        })
        .last()
        .expect("realtime result");
    assert_eq!(batch, realtime_result);
}

#[test]
fn legacy_security_requested_runtime_uses_requested_chart_metadata() {
    let program = compile_fixture(
        "legacy_security_requested_metadata.pine",
        "//@version=4\nstudy(\"legacy requested metadata\")\ninRequestedContext = security(\"NYSE:IBM\", \"5\", syminfo.tickerid == \"NYSE:IBM\" and timeframe.period == \"5\")\nplot(inRequestedContext ? 1 : 0)\n",
    );
    let environment = legacy_security_environment(vec![timed_close(0, 100.0)]);
    let result = run_historical_with_request_environment(
        &program,
        &[timed_close(240_000, 1.0)],
        environment,
    )
    .expect("requested metadata should use the requested key");

    assert_values_close(&result.plots[0].values, &[1.0]);
}

#[test]
fn legacy_lookahead_on_realtime_updates_use_confirmed_alignment() {
    let program = legacy_security_profile_program(
        "legacy_v2_security_realtime.pine",
        "barmerge.gaps_off",
        "barmerge.lookahead_on",
        2,
    );
    let environment =
        legacy_security_environment(vec![timed_close(0, 100.0), timed_close(300_000, 200.0)]);
    let mut runtime = RealtimeRuntime::with_request_environment(&program, environment);
    let historical = runtime
        .update(BarUpdate::historical(timed_close(0, 1.0)))
        .expect("historical lookahead update");
    let forming = runtime
        .update(BarUpdate::forming(timed_close(60_000, 2.0)))
        .expect("forming lookahead update");

    assert_values_close(&historical.plots[0].values, &[100.0]);
    assert_eq!(forming.plots[0].values[1], PineValue::Na);
}

#[test]
fn legacy_security_missing_provider_error_keeps_original_source_span() {
    let source =
        "//@version=4\nstudy(\"legacy missing\")\nplot(security(\"NYSE:IBM\", \"5\", close))\n";
    let program = compile_fixture("legacy_security_missing.pine", source);
    let error = run_historical(&program, &[timed_close(0, 1.0)])
        .expect_err("missing legacy provider data should fail");
    let start = source.find("security(").expect("security source start");
    let end = source[start..].find(')').expect("security source end") + start + 1;
    assert_eq!(
        error.message,
        format!(
            "legacy security at source span {start}..{end}: missing request data for symbol `NYSE:IBM` timeframe `5`"
        )
    );
}

#[test]
fn v3_core_fixture_matches_canonical_batch_incremental_and_visual_output() {
    let legacy = compile_fixture(
        "v3_core_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v3/runtime/core_legacy.pine"),
    );
    let canonical = compile_fixture(
        "v3_core_canonical.pine",
        include_str!("../../../../tests/fixtures/legacy/v3/runtime/core_canonical.pine"),
    );
    let bars = [
        bar_ohlc(10.0, 12.0, 9.0, 11.0),
        bar_ohlc(12.0, 13.0, 9.0, 10.0),
        bar_ohlc(10.0, 15.0, 10.0, 14.0),
        bar_ohlc(14.0, 16.0, 12.0, 15.0),
        bar_ohlc(16.0, 17.0, 13.0, 14.0),
    ];

    let legacy_batch = run_historical(&legacy, &bars).expect("legacy v3 core run");
    let canonical_batch = run_historical(&canonical, &bars).expect("canonical core run");
    assert_eq!(legacy_batch, canonical_batch);

    let mut incremental = HistoricalRuntime::new(&legacy);
    for bar in bars {
        incremental
            .append_bar(bar)
            .expect("incremental v3 core bar");
    }
    assert_eq!(legacy_batch, incremental.result());
    assert_eq!(
        legacy_batch.plots[0].style,
        PineValue::String("plot.style_histogram".to_owned())
    );
    assert_eq!(
        legacy_batch.plots[0].colors,
        vec![PineValue::Color(0xF23645BF); bars.len()]
    );
    assert_eq!(
        legacy_batch.hlines[0].style,
        PineValue::String("hline.style_dotted".to_owned())
    );
    assert_eq!(legacy_batch.plots[1].values[1], PineValue::Na);
}

#[test]
fn v3_chart_metadata_aliases_match_canonical_values_in_custom_context() {
    let legacy = compile_fixture(
        "v3_metadata_aliases.pine",
        r#"//@version=3
study("v3 metadata aliases")
matches = ticker == "IBM" and tickerid == "NYSE:IBM" and period == "5" and interval == 5 and isminutes and isintraday
plot(matches ? 1 : 0)
"#,
    );
    let canonical = compile_fixture(
        "v3_metadata_canonical.pine",
        r#"//@version=6
indicator("canonical metadata")
matches = syminfo.ticker == "IBM" and syminfo.tickerid == "NYSE:IBM" and timeframe.period == "5" and timeframe.multiplier == 5 and timeframe.isminutes and timeframe.isintraday
plot(matches ? 1 : 0)
"#,
    );
    let environment = RequestEnvironment::new(
        ChartContext::new(
            "NYSE:IBM",
            RequestTimeframe::parse("5").expect("five minute chart timeframe"),
        ),
        Arc::new(NoRequestDataProvider),
    );
    let bars = [timed_close(0, 1.0), timed_close(300_000, 2.0)];
    let legacy_result =
        run_historical_with_request_environment(&legacy, &bars, environment.clone())
            .expect("legacy v3 metadata run");
    let canonical_result = run_historical_with_request_environment(&canonical, &bars, environment)
        .expect("canonical metadata run");

    assert_eq!(legacy_result, canonical_result);
    assert_values_close(&legacy_result.plots[0].values, &[1.0, 1.0]);
}

#[test]
fn v3_timeframe_aliases_follow_minute_second_day_week_and_month_contexts() {
    let legacy = compile_fixture(
        "v3_timeframe_aliases.pine",
        r#"//@version=3
study("v3 timeframe aliases")
plot(interval)
plot(isminutes ? 1 : 0)
plot(isseconds ? 1 : 0)
plot(isintraday ? 1 : 0)
plot(isdaily ? 1 : 0)
plot(isweekly ? 1 : 0)
plot(ismonthly ? 1 : 0)
plot(isdwm ? 1 : 0)
"#,
    );
    let canonical = compile_fixture(
        "v3_timeframe_aliases_canonical.pine",
        r#"//@version=6
indicator("canonical timeframe metadata")
plot(timeframe.multiplier)
plot(timeframe.isminutes ? 1 : 0)
plot(timeframe.isseconds ? 1 : 0)
plot(timeframe.isintraday ? 1 : 0)
plot(timeframe.isdaily ? 1 : 0)
plot(timeframe.isweekly ? 1 : 0)
plot(timeframe.ismonthly ? 1 : 0)
plot(timeframe.isdwm ? 1 : 0)
"#,
    );
    for (timeframe, expected) in [
        ("5", [5.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0]),
        ("45S", [45.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0]),
        ("2D", [2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
        ("3W", [3.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0]),
        ("4M", [4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0]),
    ] {
        let environment = RequestEnvironment::new(
            ChartContext::new(
                "NYSE:IBM",
                RequestTimeframe::parse(timeframe).expect("valid chart timeframe"),
            ),
            Arc::new(NoRequestDataProvider),
        );
        let bars = [timed_close(0, 1.0)];
        let legacy_result =
            run_historical_with_request_environment(&legacy, &bars, environment.clone())
                .expect("legacy v3 timeframe metadata run");
        let canonical_result =
            run_historical_with_request_environment(&canonical, &bars, environment)
                .expect("canonical timeframe metadata run");
        assert_eq!(legacy_result, canonical_result, "{timeframe}");
        for (plot, expected_value) in legacy_result.plots.iter().zip(expected) {
            assert_values_close(&plot.values, &[expected_value]);
        }
    }
}

#[test]
fn v4_alias_fixture_matches_canonical_historical_output() {
    let legacy = compile_fixture(
        "aliases_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/aliases_legacy.pine"),
    );
    let canonical = compile_fixture(
        "aliases_canonical.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/aliases_canonical.pine"),
    );
    let bars = [
        bar_ohlc(10.0, 12.0, 9.0, 11.0),
        bar_ohlc(11.0, 14.0, 10.0, 13.0),
        bar_ohlc(13.0, 15.0, 11.0, 12.0),
        bar_ohlc(12.0, 16.0, 12.0, 15.0),
        bar_ohlc(15.0, 17.0, 13.0, 14.0),
        bar_ohlc(14.0, 18.0, 14.0, 17.0),
    ];

    let legacy_result = run_historical(&legacy, &bars).expect("legacy v4 run");
    let canonical_result = run_historical(&canonical, &bars).expect("canonical run");

    assert_eq!(legacy_result, canonical_result);
}

#[test]
fn v1_expanded_alias_fixture_matches_canonical_historical_output() {
    let legacy = compile_fixture(
        "aliases_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v1/runtime/aliases_legacy.pine"),
    );
    let canonical = compile_fixture(
        "aliases_canonical.pine",
        include_str!("../../../../tests/fixtures/legacy/v1/runtime/aliases_canonical.pine"),
    );
    let bars = [
        bar_ohlc(10.2, 12.4, 9.1, 11.3),
        bar_ohlc(11.1, 14.2, 10.3, 13.7),
        bar_ohlc(13.8, 15.6, 11.4, 12.2),
        bar_ohlc(12.2, 16.1, 12.0, 15.5),
        bar_ohlc(15.5, 17.7, 13.2, 14.4),
        bar_ohlc(14.3, 18.8, 14.1, 17.6),
    ];

    let legacy_result = run_historical(&legacy, &bars).expect("legacy v1 alias run");
    let canonical_result = run_historical(&canonical, &bars).expect("canonical alias run");

    assert_eq!(legacy_result, canonical_result);
}

#[test]
fn v4_input_fixture_preserves_metadata_callsites_and_overrides() {
    let legacy = compile_fixture(
        "inputs_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/inputs_legacy.pine"),
    );
    let canonical = compile_fixture(
        "inputs_canonical.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/inputs_canonical.pine"),
    );
    let legacy_inputs = input_calls(&legacy);
    let canonical_inputs = input_calls(&canonical);
    assert_eq!(legacy_inputs, canonical_inputs);
    assert_eq!(legacy_inputs.len(), 11);
    assert_eq!(
        legacy_inputs
            .iter()
            .map(|input| input.call_site_id)
            .collect::<Vec<_>>(),
        (1..=11).collect::<Vec<_>>()
    );

    let bars = [
        Bar {
            time: 1,
            open: 10.0,
            high: 12.0,
            low: 9.0,
            close: 11.0,
            volume: 1.0,
        },
        Bar {
            time: 2,
            open: 11.0,
            high: 14.0,
            low: 10.0,
            close: 13.0,
            volume: 1.0,
        },
        Bar {
            time: 3,
            open: 13.0,
            high: 15.0,
            low: 11.0,
            close: 12.0,
            volume: 1.0,
        },
        Bar {
            time: 4,
            open: 12.0,
            high: 16.0,
            low: 12.0,
            close: 15.0,
            volume: 1.0,
        },
    ];

    assert_eq!(
        run_historical(&legacy, &bars).expect("legacy default input run"),
        run_historical(&canonical, &bars).expect("canonical default input run")
    );

    let call_site = |title: &str| {
        legacy_inputs
            .iter()
            .find(|input| input.title.as_deref() == Some(title))
            .map(|input| input.call_site_id)
            .unwrap_or_else(|| panic!("missing input title {title}"))
    };
    let overrides = InputOverrides::new()
        .with_value(call_site("Length"), PineValue::Int(1))
        .with_value(call_site("Scale"), PineValue::Float(2.0))
        .with_value(call_site("Enabled"), PineValue::Bool(true))
        .with_value(call_site("Shade"), PineValue::Color(0x4CAF50))
        .with_value(call_site("Mode"), PineValue::String("SMA".to_owned()))
        .with_value(call_site("Symbol"), PineValue::String("AAPL".to_owned()))
        .with_value(call_site("Resolution"), PineValue::String("60".to_owned()))
        .with_value(
            call_site("Session"),
            PineValue::String("0930-1600".to_owned()),
        )
        .with_value(call_site("Start"), PineValue::Int(0))
        .with_value(call_site("Price"), PineValue::Float(1.0));
    let legacy_override = run_historical_with_input_overrides(&legacy, &bars, overrides.clone())
        .expect("legacy override run");
    let canonical_override = run_historical_with_input_overrides(&canonical, &bars, overrides)
        .expect("canonical override run");
    assert_eq!(legacy_override, canonical_override);
    assert_values_close(&legacy_override.plots[0].values, &[23.0, 27.0, 25.0, 31.0]);
}

#[test]
fn v4_named_input_default_fixture_matches_canonical_historical_output() {
    let legacy = compile_fixture(
        "named_input_default_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/named_input_default_legacy.pine"
        ),
    );
    let canonical = compile_fixture(
        "named_input_default_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/named_input_default_canonical.pine"
        ),
    );
    assert_eq!(input_calls(&legacy), input_calls(&canonical));

    let bars = [bar_ohlc(10.0, 12.0, 9.0, 11.0)];
    let legacy_result = run_historical(&legacy, &bars).expect("legacy named input default run");
    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical named input default run");

    assert_eq!(legacy_result, canonical_result);
    assert_values_close(&legacy_result.plots[0].values, &[7.0]);
}

#[test]
fn v4_output_fixture_matches_canonical_visual_data_and_metadata() {
    let legacy = compile_fixture(
        "outputs_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/outputs_legacy.pine"),
    );
    let canonical = compile_fixture(
        "outputs_canonical.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/outputs_canonical.pine"),
    );
    let bars = [
        bar_ohlc(10.0, 12.0, 9.0, 11.0),
        bar_ohlc(12.0, 13.0, 9.0, 10.0),
        bar_ohlc(10.0, 15.0, 10.0, 14.0),
    ];

    let legacy_result = run_historical(&legacy, &bars).expect("legacy output run");
    let canonical_result = run_historical(&canonical, &bars).expect("canonical output run");
    assert_eq!(legacy_result, canonical_result);

    assert_eq!(
        legacy_result.plots[0].style,
        PineValue::String("plot.style_columns".to_owned())
    );
    assert_eq!(
        legacy_result.plots[0].colors,
        vec![PineValue::Color(0x2196F399); bars.len()]
    );
    assert_eq!(
        legacy_result.plots[1].colors,
        vec![PineValue::Color(0xF23645CC); bars.len()]
    );
    assert_eq!(
        legacy_result.hlines[0].style,
        PineValue::String("hline.style_dotted".to_owned())
    );
    assert_eq!(
        legacy_result.fills[0].colors,
        vec![PineValue::Color(0x4CAF501A); bars.len()]
    );
    assert_eq!(
        legacy_result.bg_colors[0].values,
        vec![
            PineValue::Color(0x2196F31A),
            PineValue::Na,
            PineValue::Color(0x2196F31A),
        ]
    );
    assert_eq!(legacy_result.plots[0].metadata.offset, PineValue::Int(1));
    assert_eq!(
        legacy_result.plot_chars[0].metadata.show_last,
        PineValue::Int(2)
    );
}

#[test]
fn v4_v5_series_output_offsets_apply_the_final_value_to_complete_outputs() {
    let v4 = compile_fixture(
        "series_output_offset_legacy.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/series_output_offset_legacy.pine"
        ),
    );
    let v5 = compile_fixture(
        "v5_series_output_offset.pine",
        include_str!("../../../../tests/fixtures/runtime/v5_series_output_offset.pine"),
    );
    let canonical = compile_fixture(
        "series_output_offset_canonical.pine",
        include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/series_output_offset_canonical.pine"
        ),
    );
    let bars = [
        bar_ohlc(10.0, 12.0, 9.0, 11.0),
        bar_ohlc(12.0, 13.0, 9.0, 10.0),
        bar_ohlc(10.0, 15.0, 10.0, 14.0),
        bar_ohlc(14.0, 16.0, 11.0, 12.0),
    ];

    let canonical_result =
        run_historical(&canonical, &bars).expect("canonical final output offset run");
    for (version, program) in [(4, &v4), (5, &v5)] {
        let result = run_historical(program, &bars)
            .unwrap_or_else(|error| panic!("v{version} series output offset run: {error:?}"));
        assert_eq!(result, canonical_result, "v{version}");
        assert_eq!(result.plots[0].metadata.offset, PineValue::Int(-1));
        assert_eq!(result.plot_chars[0].metadata.offset, PineValue::Int(-1));
        assert_eq!(result.plot_shapes[0].metadata.offset, PineValue::Int(-1));
        assert_eq!(result.plot_arrows[0].metadata.offset, PineValue::Int(-1));
        assert_eq!(result.bg_colors[0].metadata.offset, PineValue::Int(-1));
        assert_eq!(result.bar_colors[0].metadata.offset, PineValue::Int(-1));
    }
}

#[test]
fn v1_output_fixture_matches_canonical_visual_data_and_metadata() {
    let legacy = compile_fixture(
        "v1_outputs_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v1/runtime/outputs_legacy.pine"),
    );
    let canonical = compile_fixture(
        "v1_outputs_canonical.pine",
        include_str!("../../../../tests/fixtures/legacy/v1/runtime/outputs_canonical.pine"),
    );
    let bars = [
        bar_ohlc(10.0, 12.0, 9.0, 11.0),
        bar_ohlc(12.0, 13.0, 9.0, 10.0),
        bar_ohlc(10.0, 15.0, 10.0, 14.0),
    ];

    let legacy_result = run_historical(&legacy, &bars).expect("legacy v1 output run");
    let canonical_result = run_historical(&canonical, &bars).expect("canonical v1 output run");
    assert_eq!(legacy_result, canonical_result);
    assert_eq!(legacy_result.plots.len(), 2);
    assert_eq!(legacy_result.plot_chars.len(), 1);
    assert_eq!(legacy_result.plot_shapes.len(), 1);
    assert_eq!(legacy_result.plot_arrows.len(), 1);
    assert_eq!(legacy_result.hlines.len(), 2);
    assert_eq!(legacy_result.fills.len(), 3);
    assert_eq!(legacy_result.bg_colors.len(), 2);
    assert_eq!(legacy_result.bar_colors.len(), 1);
}

#[test]
fn v4_dynamic_output_style_override_rejects_unknown_ordinals_at_runtime() {
    let legacy = compile_fixture(
        "outputs_legacy.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/outputs_legacy.pine"),
    );
    let style_call = input_calls(&legacy)
        .into_iter()
        .find(|input| input.title.as_deref() == Some("Plot style"))
        .expect("style input");
    let overrides = InputOverrides::new().with_value(style_call.call_site_id, PineValue::Int(99));
    let error = run_historical_with_input_overrides(&legacy, &[bar(1.0)], overrides)
        .expect_err("unknown v4 style ordinal must fail");

    assert!(
        error
            .message
            .contains("invalid Pine v4 plot style ordinal `99`")
    );
}

#[test]
fn v4_output_transparency_clamps_preserves_na_and_accepts_input_values() {
    let program = compile_fixture(
        "legacy_transparency_edges.pine",
        r#"//@version=4
study("transparency edges")
t = input(40, "Transparency")
plot(close, color=color.blue, transp=t)
plot(close, color=na, transp=40)
plot(close, color=color.blue, transp=na)
"#,
    );
    let transparency_call = input_calls(&program)
        .into_iter()
        .find(|input| input.title.as_deref() == Some("Transparency"))
        .expect("transparency input");

    let default_result = run_historical(&program, &[bar(1.0)]).expect("default transp run");
    assert_eq!(
        default_result.plots[0].colors,
        vec![PineValue::Color(0x2196F399)]
    );
    assert_eq!(default_result.plots[1].colors, vec![PineValue::Na]);
    assert_eq!(
        default_result.plots[2].colors,
        vec![PineValue::Color(0x2196F3)]
    );

    for (transp, expected) in [(-10, 0x2196F3), (120, 0x2196F300)] {
        let overrides = InputOverrides::new()
            .with_value(transparency_call.call_site_id, PineValue::Int(transp));
        let result = run_historical_with_input_overrides(&program, &[bar(1.0)], overrides)
            .expect("clamped transp run");
        assert_eq!(result.plots[0].colors, vec![PineValue::Color(expected)]);
    }
}

#[test]
fn v4_iff_eagerly_advances_both_stateful_results_while_ternary_stays_lazy() {
    let iff = compile_fixture(
        "legacy_iff_stateful.pine",
        r#"//@version=4
study("stateful iff")
plot(iff(close > open, ema(high, 2), ema(low, 2)))
"#,
    );
    let ternary = compile_fixture(
        "legacy_ternary_stateful.pine",
        r#"//@version=4
study("lazy ternary")
plot(close > open ? ema(high, 2) : ema(low, 2))
"#,
    );
    let bars = [
        bar_ohlc(1.0, 10.0, 1.0, 2.0),
        bar_ohlc(2.0, 20.0, 2.0, 3.0),
        bar_ohlc(3.0, 15.0, 5.0, 2.0),
        bar_ohlc(4.0, 14.0, 6.0, 3.0),
    ];

    let iff_result = run_historical(&iff, &bars).expect("strict iff run");
    let ternary_result = run_historical(&ternary, &bars).expect("lazy ternary run");
    assert_eq!(iff_result.plots[0].values[0], PineValue::Na);
    assert_values_close(
        &iff_result.plots[0].values[1..],
        &[15.0, 23.0 / 6.0, 95.0 / 18.0],
    );
    assert_eq!(ternary_result.plots[0].values[0], PineValue::Na);
    assert_eq!(ternary_result.plots[0].values[2], PineValue::Na);
    assert_values_close(&ternary_result.plots[0].values[1..2], &[15.0]);
    assert_values_close(&ternary_result.plots[0].values[3..], &[5.5]);
}

#[test]
fn v4_named_iff_evaluates_by_parameter_role_once() {
    let program = compile_fixture(
        "legacy_iff_named.pine",
        r#"//@version=4
study("named iff")
plot(iff(result2=lowest(low, 2), condition=close > open, result1=highest(high, 2)))
"#,
    );
    let bars = [
        bar_ohlc(1.0, 10.0, 1.0, 2.0),
        bar_ohlc(2.0, 20.0, 2.0, 3.0),
        bar_ohlc(3.0, 15.0, 5.0, 2.0),
    ];
    let result = run_historical(&program, &bars).expect("named iff run");
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Na, PineValue::Float(20.0), PineValue::Float(2.0),]
    );
}

#[test]
fn v4_offset_matches_history_for_constant_and_dynamic_offsets() {
    let legacy = compile_fixture(
        "legacy_offset.pine",
        r#"//@version=4
study("legacy offset", max_bars_back=3)
bars = input(1, "Bars")
plot(offset(close, bars))
"#,
    );
    let canonical = compile_fixture(
        "canonical_history.pine",
        r#"//@version=5
indicator("canonical history", max_bars_back=3)
bars = input.int(1, "Bars")
plot(close[bars])
"#,
    );
    assert!(legacy.history.has_dynamic_offsets);
    assert_eq!(legacy.max_bars_back, Some(3));
    let bars = [bar(10.0), bar(20.0), bar(30.0), bar(40.0)];
    assert_eq!(
        run_historical(&legacy, &bars).expect("legacy offset run"),
        run_historical(&canonical, &bars).expect("canonical history run")
    );

    let call_site_id = input_calls(&legacy)[0].call_site_id;
    let overridden = run_historical_with_input_overrides(
        &legacy,
        &bars,
        InputOverrides::new().with_value(call_site_id, PineValue::Int(2)),
    )
    .expect("overridden legacy offset run");
    assert_eq!(
        overridden.plots[0].values,
        vec![
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(10.0),
            PineValue::Float(20.0)
        ]
    );

    let error = run_historical_with_input_overrides(
        &legacy,
        &bars,
        InputOverrides::new().with_value(call_site_id, PineValue::Int(-1)),
    )
    .expect_err("negative legacy dynamic offset must use the history guard");
    assert!(
        error
            .message
            .contains("history offset must be non-negative")
    );
}

#[test]
fn v4_rsi_selects_length_and_two_series_overloads_with_formula_edges() {
    let legacy_length = compile_fixture(
        "legacy_rsi_length.pine",
        r#"//@version=4
study("legacy RSI length")
plot(rsi(close, 2))
"#,
    );
    let canonical_length = compile_fixture(
        "canonical_rsi_length.pine",
        r#"//@version=5
indicator("canonical RSI length")
plot(ta.rsi(close, 2))
"#,
    );
    let legacy_series = compile_fixture(
        "legacy_rsi_series.pine",
        r#"//@version=4
study("legacy RSI series")
plot(rsi(close, open))
"#,
    );
    let canonical_formula = compile_fixture(
        "canonical_rsi_formula.pine",
        r#"//@version=5
indicator("canonical RSI formula")
plot(100.0 - (100.0 / (1.0 + close / open)))
"#,
    );
    let bars = [
        bar_ohlc(2.0, 2.0, 2.0, 2.0),
        bar_ohlc(2.0, 4.0, 1.0, 4.0),
        bar_ohlc(2.0, 2.0, 0.0, 0.0),
        bar_ohlc(0.0, 2.0, 0.0, 2.0),
    ];

    assert_eq!(
        run_historical(&legacy_length, &bars)
            .expect("legacy length RSI")
            .plots[0]
            .values,
        run_historical(&canonical_length, &bars)
            .expect("canonical length RSI")
            .plots[0]
            .values
    );
    assert_eq!(
        run_historical(&legacy_series, &bars)
            .expect("legacy series RSI")
            .plots[0]
            .values,
        run_historical(&canonical_formula, &bars)
            .expect("canonical RSI formula")
            .plots[0]
            .values
    );
}

#[test]
fn v4_session_defaults_exclude_weekends_without_rewriting_input_strings() {
    let v4 = compile_fixture(
        "legacy_session_days.pine",
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/session_defaults_legacy.pine"),
    );
    let v5 = compile_fixture(
        "modern_session_days.pine",
        r#"//@version=5
indicator("modern session days")
s = input.session("0000-2359", "Session")
plot(na(time("D", s, "UTC")) ? 0 : 1)
plot(na(time_close("D", s, "UTC")) ? 0 : 1)
plot(s == "0000-2359" ? 1 : 0)
"#,
    );
    let dated_bar = |time| Bar {
        time,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    };
    let bars = [
        dated_bar(1_609_459_200_000),
        dated_bar(1_609_545_600_000),
        dated_bar(1_609_632_000_000),
        dated_bar(1_609_718_400_000),
    ];

    let legacy = run_historical(&v4, &bars).expect("legacy session run");
    let modern = run_historical(&v5, &bars).expect("modern session run");
    assert_values_close(&legacy.plots[0].values, &[1.0, 0.0, 0.0, 1.0]);
    assert_values_close(&legacy.plots[1].values, &[1.0, 0.0, 0.0, 1.0]);
    assert_values_close(&legacy.plots[2].values, &[1.0, 1.0, 1.0, 1.0]);
    assert_values_close(&modern.plots[0].values, &[1.0, 1.0, 1.0, 1.0]);
    assert_values_close(&modern.plots[1].values, &[1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn v4_phase6_semantics_match_incremental_and_realtime_confirmed_execution() {
    let program = compile_fixture(
        "legacy_phase6_realtime.pine",
        r#"//@version=4
study("legacy phase 6 realtime")
selected = iff(close > open, ema(high, 2), ema(low, 2))
previous = offset(close, 1)
up = max(change(close), 0)
down = -min(change(close), 0)
ratio = rsi(up, down)
plot(selected)
plot(previous)
plot(ratio)
"#,
    );
    let bars = [
        at_time(0, bar_ohlc(1.0, 10.0, 1.0, 2.0)),
        at_time(60_000, bar_ohlc(2.0, 20.0, 2.0, 3.0)),
        at_time(120_000, bar_ohlc(3.0, 15.0, 5.0, 2.0)),
        at_time(180_000, bar_ohlc(4.0, 14.0, 6.0, 3.0)),
    ];
    let historical = run_historical(&program, &bars).expect("phase 6 historical run");

    let mut incremental = RealtimeRuntime::new(&program);
    for bar in bars.iter().cloned() {
        incremental
            .update(BarUpdate::historical(bar))
            .expect("phase 6 incremental update");
    }
    assert_eq!(incremental.result(), historical);

    let mut realtime = RealtimeRuntime::new(&program);
    for bar in bars[..3].iter().cloned() {
        realtime
            .update(BarUpdate::historical(bar))
            .expect("phase 6 realtime history");
    }
    realtime
        .update(BarUpdate::forming(at_time(
            180_000,
            bar_ohlc(8.0, 30.0, 7.0, 9.0),
        )))
        .expect("phase 6 forming update");
    realtime
        .update(BarUpdate::forming(bars[3]))
        .expect("phase 6 rolled-back forming update");
    let confirmed = realtime
        .update(BarUpdate::confirmed(bars[3]))
        .expect("phase 6 confirmed update");
    assert_eq!(confirmed, historical);
}
#[test]
fn v5_numeric_typed_bool_declaration_casts_at_runtime() {
    let source = SourceFile::new(
        "v5_typed_bool.pine",
        "//@version=5\nindicator(\"v5 typed bool\")\nbool flag = close - 2\nplot(flag ? 1 : 0)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0), bar(2.0), bar(3.0)])
        .expect("runtime result");
    assert_values_close(&result.plots[0].values, &[1.0, 0.0, 1.0]);

    let v6_source = SourceFile::new(
        "v6_typed_bool.pine",
        "//@version=6\nindicator(\"v6 typed bool\")\nbool flag = close - 2\nplot(flag ? 1 : 0)\n",
    );
    let v6 = analyze_source(&v6_source);
    assert!(v6.diagnostics.iter().any(|d| d.code == "E_ASSIGN_TYPE"));
}

fn at_time(time: i64, mut bar: Bar) -> Bar {
    bar.time = time;
    bar
}
