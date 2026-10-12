use pine_ir::{
    CallSiteId, HirCallArg, HirExpr, HirExprKind, HirLiteral, HirProgram, HirStmtKind, PineType,
    Qualifier, ValueKind,
};

use crate::{Bar, HistoricalRuntime, PineValue};

fn program(text: &str) -> HirProgram {
    let analysis = pine_sema::analyze_source(&pine_syntax::SourceFile::new("lengths.pine", text));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn program_and_source() -> (HirProgram, HirExpr) {
    // Calls below are assembled by hand, so declare the implicit previous-bar
    // dependencies that semantic analysis would record for source-level TA calls.
    let program = program(
        "//@version=6\nindicator(\"lengths\")\nplot(close)\nplot(close[1])\nplot(high[1])\nplot(low[1])\nplot(volume[1])\n",
    );
    let HirStmtKind::Expr(expr) = &program.statements[1].kind else {
        panic!("expected plot");
    };
    let HirExprKind::Call { args, .. } = &expr.kind else {
        panic!("expected plot call");
    };
    let source = args[0].value.clone();
    (program, source)
}

fn bar(index: i64, close: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: 10.0,
        high: close + 2.0,
        low: close - 2.0,
        close,
        volume: 100.0,
    }
}

fn literal(value: HirLiteral, kind: ValueKind) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Literal(value),
        pine_type: PineType::new(Qualifier::Const, kind),
        series_id: None,
    }
}

fn int(value: i64) -> HirExpr {
    literal(HirLiteral::Int(value), ValueKind::Int)
}

fn float(value: f64) -> HirExpr {
    literal(HirLiteral::Float(value), ValueKind::Float)
}

fn arg(value: HirExpr) -> HirCallArg {
    HirCallArg { name: None, value }
}

fn source_length(source: &HirExpr, length: i64) -> Vec<HirCallArg> {
    vec![arg(source.clone()), arg(int(length))]
}

fn oversized_calls(
    source: &HirExpr,
    length: i64,
) -> Vec<(&'static str, Vec<HirCallArg>, PineValue)> {
    let mut calls: Vec<_> = [
        "ta.sma",
        "ta.ema",
        "ta.rma",
        "ta.rsi",
        "ta.wma",
        "ta.hma",
        "ta.cci",
        "ta.cog",
        "ta.vwma",
        "ta.mfi",
        "ta.cmo",
        "ta.change",
        "ta.mom",
        "ta.roc",
        "ta.rci",
        "ta.range",
        "ta.dev",
        "ta.median",
        "ta.mode",
        "ta.percentrank",
    ]
    .into_iter()
    .map(|name| (name, source_length(source, length), PineValue::Na))
    .collect();
    let three_na = PineValue::Tuple(vec![PineValue::Na; 3]);
    for (name, expected) in [("ta.bb", three_na.clone()), ("ta.bbw", PineValue::Na)] {
        let mut args = source_length(source, length);
        args.push(arg(float(2.0)));
        calls.push((name, args, expected));
    }
    for name in ["ta.correlation", "ta.covariance"] {
        calls.push((
            name,
            vec![arg(source.clone()), arg(source.clone()), arg(int(length))],
            PineValue::Na,
        ));
    }
    for name in ["ta.variance", "ta.stdev"] {
        let mut args = source_length(source, length);
        args.push(arg(literal(HirLiteral::Bool(true), ValueKind::Bool)));
        calls.push((name, args, PineValue::Na));
    }
    for name in [
        "ta.percentile_nearest_rank",
        "ta.percentile_linear_interpolation",
    ] {
        let mut args = source_length(source, length);
        args.push(arg(float(50.0)));
        calls.push((name, args, PineValue::Na));
    }
    let mut alma = source_length(source, length);
    alma.extend([arg(float(0.85)), arg(float(6.0))]);
    calls.push(("ta.alma", alma, PineValue::Na));
    let mut linreg = source_length(source, length);
    linreg.push(arg(int(0)));
    calls.push(("ta.linreg", linreg, PineValue::Na));
    calls.push((
        "ta.stoch",
        vec![
            arg(source.clone()),
            arg(float(20.0)),
            arg(float(0.0)),
            arg(int(length)),
        ],
        PineValue::Na,
    ));
    calls.push(("ta.wpr", vec![arg(int(length))], PineValue::Na));
    for name in ["ta.rising", "ta.falling"] {
        calls.push((name, source_length(source, length), PineValue::Bool(false)));
    }
    calls.push((
        "ta.macd",
        vec![
            arg(source.clone()),
            arg(int(length)),
            arg(int(length)),
            arg(int(length)),
        ],
        three_na,
    ));
    calls
}

#[test]
fn huge_window_lengths_keep_invalid_return_shapes_without_becoming_short_windows() {
    let (program, source) = program_and_source();
    for length in [i64::from(u32::MAX), 4_294_967_296, 4_294_967_297, i64::MAX] {
        for (callee, args, expected) in oversized_calls(&source, length) {
            let mut runtime = HistoricalRuntime::new(&program);
            runtime
                .append_bars(&[bar(0, 10.0), bar(1, 12.0), bar(2, 11.0)])
                .unwrap();
            assert_eq!(
                runtime
                    .eval_call(callee, CallSiteId(u32::MAX), &args)
                    .unwrap(),
                expected,
                "{callee}, length={length}"
            );
        }
    }
}

#[test]
fn representable_large_lengths_are_not_subject_to_an_arbitrary_cap() {
    let (program, source) = program_and_source();
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bar(bar(0, 10.0)).unwrap();
    let site = CallSiteId(u32::MAX);
    let args = source_length(&source, i64::from(u32::MAX));
    assert_eq!(
        runtime.eval_call("ta.sma", site, &args).unwrap(),
        PineValue::Na
    );
    let window = runtime.ta_state.rolling_windows.values().next().unwrap();
    assert_eq!(window.values.len(), 1);
    assert_eq!(window.sum, 10.0);
}

#[test]
fn huge_lengths_still_evaluate_later_arguments_before_returning_na() {
    let (program, source) = program_and_source();
    let errors = self::program(
        "//@version=6\nindicator(\"error argument\")\nruntime.error(\"late argument was evaluated\")\n",
    );
    let HirStmtKind::Expr(error) = &errors.statements[1].kind else {
        panic!("expected error call");
    };
    let length = 4_294_967_296;
    let mut cases = Vec::new();
    for callee in [
        "ta.bb",
        "ta.bbw",
        "ta.linreg",
        "ta.variance",
        "ta.stdev",
        "ta.percentile_nearest_rank",
        "ta.percentile_linear_interpolation",
    ] {
        let mut args = source_length(&source, length);
        args.push(arg(error.clone()));
        cases.push((callee, args));
    }
    let mut alma = source_length(&source, length);
    alma.extend([arg(float(0.85)), arg(float(6.0)), arg(error.clone())]);
    cases.push(("ta.alma", alma));
    for callee in ["ta.pivothigh", "ta.pivotlow"] {
        cases.push((
            callee,
            vec![arg(source.clone()), arg(int(length)), arg(error.clone())],
        ));
    }
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bar(bar(0, 10.0)).unwrap();
    for (callee, args) in cases {
        assert_eq!(
            runtime
                .eval_call(callee, CallSiteId(u32::MAX), &args)
                .unwrap_err()
                .message,
            "late argument was evaluated",
            "{callee}"
        );
    }
    assert!(runtime.ta_state.rolling_windows.is_empty());
}

#[test]
fn pivot_zero_sides_remain_legal_and_huge_combined_lengths_do_not_wrap() {
    let (program, source) = program_and_source();
    for callee in ["ta.pivothigh", "ta.pivotlow"] {
        let mut runtime = HistoricalRuntime::new(&program);
        runtime.append_bar(bar(0, 10.0)).unwrap();
        let site = CallSiteId(u32::MAX);
        assert_eq!(
            runtime
                .eval_call(
                    callee,
                    site,
                    &[arg(source.clone()), arg(int(0)), arg(int(0))]
                )
                .unwrap(),
            PineValue::Float(10.0)
        );
        for (left, right) in [
            (i64::from(u32::MAX), 0),
            (0, i64::from(u32::MAX)),
            (i64::from(u32::MAX), 1),
            (4_294_967_296, 0),
            (0, 4_294_967_296),
            (i64::MAX, i64::MAX),
        ] {
            #[cfg(target_pointer_width = "32")]
            let before = runtime.ta_state.rolling_windows.clone();
            let args = [arg(source.clone()), arg(int(left)), arg(int(right))];
            assert_eq!(
                runtime.eval_call(callee, site, &args).unwrap(),
                PineValue::Na
            );
            #[cfg(target_pointer_width = "32")]
            assert_eq!(
                runtime.ta_state.rolling_windows, before,
                "{callee}, left={left}, right={right}"
            );
        }
    }
}

#[test]
fn seeded_ema_and_macd_keep_huge_lengths_on_the_float_recurrence_path() {
    let (program, _) = program_and_source();
    for callee in ["ta.ema", "ta.macd"] {
        let mut runtime = HistoricalRuntime::new(&program);
        runtime.append_bar(bar(0, 10.0)).unwrap();
        let site = CallSiteId(u32::MAX);
        let mut initial = source_length(&float(10.0), 1);
        if callee == "ta.macd" {
            initial.extend([arg(int(1)), arg(int(1))]);
        }
        let initial_expected = if callee == "ta.macd" {
            PineValue::Tuple(vec![PineValue::Float(0.0); 3])
        } else {
            PineValue::Float(10.0)
        };
        assert_eq!(
            runtime.eval_call(callee, site, &initial).unwrap(),
            initial_expected
        );
        runtime.append_bar(bar(1, 20.0)).unwrap();
        let before = runtime.ta_state.rolling_windows.clone();
        let alpha = 2.0 / (i64::MAX as f64 + 1.0);
        let fast = alpha * 20.0 + (1.0 - alpha) * 10.0;
        let mut huge = source_length(&float(20.0), i64::MAX);
        let expected = if callee == "ta.macd" {
            huge.extend([arg(int(1)), arg(int(1))]);
            let macd = fast - 20.0;
            PineValue::Tuple(vec![
                PineValue::Float(macd),
                PineValue::Float(macd),
                PineValue::Float(0.0),
            ])
        } else {
            PineValue::Float(fast)
        };
        assert_eq!(runtime.eval_call(callee, site, &huge).unwrap(), expected);
        assert_eq!(runtime.ta_state.rolling_windows, before);
    }
    for callee in ["ta.dema", "ta.tema"] {
        let mut runtime = HistoricalRuntime::new(&program);
        assert_eq!(
            runtime
                .eval_call(
                    callee,
                    CallSiteId(u32::MAX),
                    &source_length(&float(10.0), i64::MAX)
                )
                .unwrap(),
            PineValue::Float(10.0)
        );
        assert!(runtime.ta_state.rolling_windows.is_empty());
    }
}

#[cfg(target_pointer_width = "32")]
#[test]
fn unrepresentable_lengths_leave_existing_windows_unchanged() {
    let (program, source) = program_and_source();
    for ((callee, small, _), (_, oversized, expected)) in oversized_calls(&source, 2)
        .into_iter()
        .zip(oversized_calls(&source, 4_294_967_296))
    {
        let mut runtime = HistoricalRuntime::new(&program);
        runtime.append_bars(&[bar(0, 10.0), bar(1, 12.0)]).unwrap();
        let site = CallSiteId(u32::MAX);
        runtime.eval_call(callee, site, &small).unwrap();
        let before = runtime.ta_state.rolling_windows.clone();
        assert_eq!(
            runtime.eval_call(callee, site, &oversized).unwrap(),
            expected,
            "{callee}"
        );
        assert_eq!(runtime.ta_state.rolling_windows, before, "{callee}");
    }
    let mut runtime = HistoricalRuntime::new(&program);
    let site = CallSiteId(u32::MAX);
    assert_eq!(runtime.wilder_rma(site, 0, None, Some(10.0), 1), Some(10.0));
    let before = runtime.ta_state.rolling_windows.clone();
    assert_eq!(
        runtime.wilder_rma(site, 0, Some(10.0), Some(20.0), 4_294_967_296),
        None
    );
    assert_eq!(runtime.ta_state.rolling_windows, before);
}

#[cfg(target_pointer_width = "32")]
#[test]
fn small_windows_resume_from_preserved_history_after_unrepresentable_lengths() {
    let (program, _) = program_and_source();
    for (callee, expected) in [("ta.wma", 80.0 / 3.0), ("ta.variance", 25.0)] {
        let mut runtime = HistoricalRuntime::new(&program);
        let site = CallSiteId(u32::MAX);
        for (index, source) in [10.0, 20.0].into_iter().enumerate() {
            runtime.append_bar(bar(index as i64, source)).unwrap();
            runtime
                .eval_call(callee, site, &source_length(&float(source), 2))
                .unwrap();
        }
        let before = runtime.ta_state.rolling_windows.clone();
        assert_eq!(
            runtime
                .eval_call(callee, site, &source_length(&float(999.0), 4_294_967_296))
                .unwrap(),
            PineValue::Na
        );
        assert_eq!(runtime.ta_state.rolling_windows, before);
        runtime.append_bar(bar(2, 30.0)).unwrap();
        assert_eq!(
            runtime
                .eval_call(callee, site, &source_length(&float(30.0), 2))
                .unwrap(),
            PineValue::Float(expected)
        );
    }
}
