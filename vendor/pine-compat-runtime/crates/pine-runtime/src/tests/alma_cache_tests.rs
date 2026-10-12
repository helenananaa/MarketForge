use std::{collections::VecDeque, sync::Arc};

use pine_ir::{
    CallSiteId, HirCallArg, HirExpr, HirExprKind, HirLiteral, HirProgram, PineType, Qualifier,
    ValueKind,
};

use super::*;

#[derive(Clone, Copy)]
struct Parameters {
    length: i64,
    offset: Option<f64>,
    sigma: Option<f64>,
    floor: bool,
}

fn parameters(length: i64) -> Parameters {
    Parameters {
        length,
        offset: Some(0.85),
        sigma: Some(6.0),
        floor: false,
    }
}

// Independent event model: every valid ALMA invocation consumes one sample,
// including repeated invocations at the same callsite within one bar. It owns
// ordinary VecDeque storage and recomputes every exp, without cache helpers.
#[derive(Clone, Default)]
struct DirectAlma {
    values: VecDeque<Option<f64>>,
}

impl DirectAlma {
    fn observe(&mut self, source: Option<f64>, p: Parameters) -> PineValue {
        if p.length <= 0 {
            return PineValue::Na;
        }
        let (Some(offset), Some(sigma)) = (p.offset, p.sigma) else {
            return PineValue::Na;
        };
        if sigma <= 0.0 || !offset.is_finite() || !sigma.is_finite() {
            return PineValue::Na;
        }
        let Ok(length) = usize::try_from(p.length) else {
            return PineValue::Na;
        };
        while self.values.len() >= length {
            self.values.pop_front();
        }
        self.values
            .push_back(source.filter(|value| value.is_finite()));
        if self.values.len() != length || self.values.iter().any(Option::is_none) {
            return PineValue::Na;
        }
        let mut center = offset * (length as f64 - 1.0);
        if p.floor {
            center = center.floor();
        }
        let scale = length as f64 / sigma;
        if scale == 0.0 || !scale.is_finite() {
            return PineValue::Na;
        }
        // Frozen pre-cache formula and operation order. In particular, ALMA
        // starts both accumulators at positive zero, unlike std Float Sum.
        let mut numerator = 0.0;
        let mut denominator = 0.0;
        for (index, source) in self.values.iter().enumerate() {
            let distance = index as f64 - center;
            let weight = (-(distance * distance) / (2.0 * scale * scale)).exp();
            numerator += source.expect("ready direct window") * weight;
            denominator += weight;
        }
        if denominator == 0.0 || !denominator.is_finite() {
            return PineValue::Na;
        }
        let value = numerator / denominator;
        if value.is_finite() {
            PineValue::Float(value)
        } else {
            PineValue::Na
        }
    }
}

fn assert_bits(actual: &PineValue, expected: &PineValue) {
    match (actual, expected) {
        (PineValue::Float(actual), PineValue::Float(expected)) => {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        _ => assert_eq!(actual, expected),
    }
}

fn assert_plot(actual: &RuntimeResult, plot: usize, expected: &[PineValue]) {
    assert_eq!(actual.plots[plot].values.len(), expected.len());
    for (actual, expected) in actual.plots[plot].values.iter().zip(expected) {
        assert_bits(actual, expected);
    }
}

fn program(text: &str) -> HirProgram {
    let analysis = analyze_source(&pine_syntax::SourceFile::new("alma_cache.pine", text));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

fn timed_bar(index: usize, close: f64) -> Bar {
    Bar {
        time: index as i64 * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn expr(literal: HirLiteral, kind: ValueKind) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Literal(literal),
        pine_type: PineType::new(Qualifier::Const, kind),
        series_id: None,
    }
}

fn numeric(value: Option<f64>) -> HirExpr {
    value.map_or_else(
        || HirExpr {
            kind: HirExprKind::Builtin("na".to_owned()),
            pine_type: PineType::new(Qualifier::Const, ValueKind::Na),
            series_id: None,
        },
        |value| expr(HirLiteral::Float(value), ValueKind::Float),
    )
}

fn arguments(source: Option<f64>, p: Parameters) -> Vec<HirCallArg> {
    [
        numeric(source),
        expr(HirLiteral::Int(p.length), ValueKind::Int),
        numeric(p.offset),
        numeric(p.sigma),
        expr(HirLiteral::Bool(p.floor), ValueKind::Bool),
    ]
    .into_iter()
    .map(|value| HirCallArg { name: None, value })
    .collect()
}

fn evaluate(
    runtime: &mut HistoricalRuntime<'_>,
    oracle: &mut DirectAlma,
    site: CallSiteId,
    source: Option<f64>,
    p: Parameters,
) -> PineValue {
    let actual = runtime
        .eval_call("ta.alma", site, &arguments(source, p))
        .unwrap();
    assert_bits(&actual, &oracle.observe(source, p));
    actual
}

fn sample(index: usize) -> Option<f64> {
    match index % 11 {
        0 => Some(-0.0),
        1 => Some(0.0),
        2 => Some(f64::from_bits(1)),
        3 => Some(-f64::from_bits(1)),
        4 => Some(1e16),
        5 => Some(1.0),
        6 => Some(-1e16),
        7 => Some(0.1),
        8 => Some(-0.3),
        _ => Some((index % 97) as f64 / 8.0 - 5.0),
    }
}

#[test]
fn alma_weights_match_direct_exp_bits_for_extreme_finite_parameters() {
    let hir = program("indicator(\"direct ALMA\")\nplot(close)\n");
    for length in [1, 2, 3, 127, 128, 129, 257, 3000] {
        for (offset, sigma) in [
            (-0.0, 6.0),
            (0.0, 6.0),
            (0.85, 6.0),
            (-1.0, 2.0),
            (1.5, 2.0),
            (f64::from_bits(1), 6.0),
            (f64::MAX, 6.0),
            (-f64::MAX, 6.0),
            (0.85, f64::from_bits(1)),
            (0.85, f64::MIN_POSITIVE),
            (0.85, f64::MAX),
        ] {
            for floor in [false, true] {
                let p = Parameters {
                    length,
                    offset: Some(offset),
                    sigma: Some(sigma),
                    floor,
                };
                let mut runtime = HistoricalRuntime::new(&hir);
                let mut oracle = DirectAlma::default();
                for index in 0..length as usize + 4 {
                    evaluate(
                        &mut runtime,
                        &mut oracle,
                        CallSiteId(u32::MAX),
                        sample(index),
                        p,
                    );
                }
            }
        }
    }
    for source in [-0.0, 0.0, f64::MAX, -f64::MAX] {
        let mut runtime = HistoricalRuntime::new(&hir);
        let mut oracle = DirectAlma::default();
        for _ in 0..8 {
            let result = evaluate(
                &mut runtime,
                &mut oracle,
                CallSiteId(u32::MAX),
                Some(source),
                parameters(3),
            );
            if source == 0.0 && matches!(result, PineValue::Float(_)) {
                assert_bits(&result, &PineValue::Float(0.0));
            }
        }
    }
}

#[test]
fn alma_public_dynamic_lengths_and_loops_consume_every_invocation() {
    let hir = program(
        "indicator(\"ALMA call events\")\nlength = bar_index % 5 == 0 ? 129 : bar_index % 5 == 1 ? 2 : 4\nsource = bar_index % 17 == 0 ? na : close\nplot(ta.alma(source, length, 0.85, 6))\nfloat value = na\nfor step = 0 to 2\n    value := ta.alma(close + step * 0.25, 4, 0.25, 2, true)\nplot(value)\n",
    );
    let bars = (0..280)
        .map(|index| timed_bar(index, sample(index).unwrap()))
        .collect::<Vec<_>>();
    let result = run_historical(&hir, &bars).unwrap();
    let mut dynamic = DirectAlma::default();
    let mut repeated = DirectAlma::default();
    let mut expected_dynamic = Vec::new();
    let mut expected_repeated = Vec::new();
    for (index, bar) in bars.iter().enumerate() {
        let length = match index % 5 {
            0 => 129,
            1 => 2,
            _ => 4,
        };
        expected_dynamic
            .push(dynamic.observe((index % 17 != 0).then_some(bar.close), parameters(length)));
        let mut value = PineValue::Na;
        for step in 0..3 {
            value = repeated.observe(
                Some(bar.close + step as f64 * 0.25),
                Parameters {
                    offset: Some(0.25),
                    sigma: Some(2.0),
                    floor: true,
                    ..parameters(4)
                },
            );
        }
        expected_repeated.push(value);
    }
    assert_plot(&result, 0, &expected_dynamic);
    assert_plot(&result, 1, &expected_repeated);
    // Three same-site calls already make this second-bar window ready.
    assert!(matches!(expected_repeated[1], PineValue::Float(_)));
}

#[test]
fn alma_same_site_parameter_switches_and_missing_samples_preserve_bits() {
    let hir = program("indicator(\"ALMA parameter switches\")\nplot(close)\n");
    let mut runtime = HistoricalRuntime::new(&hir);
    let mut oracle = DirectAlma::default();
    let site = CallSiteId(u32::MAX);
    for p in [
        parameters(3),
        Parameters {
            offset: Some(-0.0),
            ..parameters(3)
        },
        Parameters {
            offset: Some(0.0),
            ..parameters(3)
        },
        Parameters {
            offset: Some(0.25),
            ..parameters(3)
        },
        Parameters {
            sigma: Some(2.0),
            ..parameters(3)
        },
        Parameters {
            floor: true,
            ..parameters(3)
        },
        parameters(129),
        parameters(2),
        Parameters {
            offset: None,
            ..parameters(2)
        },
        Parameters {
            sigma: None,
            ..parameters(2)
        },
        Parameters {
            sigma: Some(-0.0),
            ..parameters(2)
        },
        Parameters {
            sigma: Some(-1.0),
            ..parameters(2)
        },
        Parameters {
            offset: Some(f64::INFINITY),
            ..parameters(2)
        },
        Parameters {
            sigma: Some(f64::NAN),
            ..parameters(2)
        },
        parameters(3),
    ] {
        for index in 0..p.length.max(3) as usize + 4 {
            evaluate(&mut runtime, &mut oracle, site, sample(index), p);
        }
        // Same key with an NA source must still consume the NA sample and
        // recover only after it actually leaves the ordinary event window.
        for source in [None, Some(f64::INFINITY), Some(1.25), Some(2.5), Some(5.0)] {
            evaluate(&mut runtime, &mut oracle, site, source, p);
        }
    }
    let checkpoint = runtime.clone();
    let checkpoint_oracle = oracle.clone();
    for offset in [0.85, 0.125, 0.85] {
        evaluate(
            &mut runtime,
            &mut oracle,
            site,
            Some(7.25),
            Parameters {
                offset: Some(offset),
                ..parameters(3)
            },
        );
    }
    let mut restored = checkpoint;
    let mut restored_oracle = checkpoint_oracle;
    for index in 0..6 {
        evaluate(
            &mut restored,
            &mut restored_oracle,
            site,
            sample(index),
            parameters(3),
        );
    }
}

#[test]
fn alma_not_ready_huge_lengths_leave_weight_storage_unallocated() {
    let hir = program("indicator(\"ALMA huge lengths\")\nplot(close)\n");
    for length in [
        0,
        -1,
        i64::from(u32::MAX),
        4_294_967_296,
        4_294_967_297,
        i64::MAX,
    ] {
        let mut runtime = HistoricalRuntime::new(&hir);
        let mut oracle = DirectAlma::default();
        for _ in 0..3 {
            let value = evaluate(
                &mut runtime,
                &mut oracle,
                CallSiteId(u32::MAX),
                Some(1.0),
                parameters(length),
            );
            assert_eq!(value, PineValue::Na);
            assert_eq!(runtime.alma_weights.capacity(), 0);
        }
        for index in 0..6 {
            evaluate(
                &mut runtime,
                &mut oracle,
                CallSiteId(u32::MAX),
                sample(index),
                parameters(2),
            );
        }
    }
}

#[test]
fn alma_realtime_rollback_replica_and_failed_updates_preserve_bits() {
    let hir = program(
        "indicator(\"ALMA forming cache\")\nvalue = ta.alma(close, 4, 0.85, 6)\nif close < 0\n    runtime.error(\"reject forming after ALMA\")\nplot(value)\n",
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    let seed = [timed_bar(0, 1.0), timed_bar(1, 2.0), timed_bar(2, 4.0)];
    runtime.seed_historical_without_output(&seed).unwrap();
    let mut confirmed = DirectAlma::default();
    let mut expected = seed
        .iter()
        .map(|bar| confirmed.observe(Some(bar.close), parameters(4)))
        .collect::<Vec<_>>();
    let mut replica = runtime.replica();
    let mut snapshots = Vec::new();
    // No seed window is ready: first forming is pending, the replacement can
    // construct weights, and subsequent replacements may reuse that kernel.
    for close in [8.0, 16.0, 32.0] {
        let mut preview = confirmed.clone();
        let mut preview_values = expected.clone();
        preview_values.push(preview.observe(Some(close), parameters(4)));
        let changes = runtime
            .apply_update_ref(BarUpdate::forming(timed_bar(3, close)))
            .unwrap();
        replica.apply(changes).unwrap();
        assert_plot(&runtime.result(), 0, &preview_values);
        assert_plot(replica.result(), 0, &preview_values);
        snapshots.push((runtime.result(), preview_values));
        assert_plot(&runtime.confirmed_result(), 0, &expected);
    }
    let before = public_runtime_result_json(&runtime.result());
    let before_changes = public_runtime_changes_json(runtime.last_changes().unwrap());
    let revision = runtime.revision();
    assert_eq!(
        runtime
            .apply_update_ref(BarUpdate::forming(timed_bar(3, -1.0)))
            .unwrap_err()
            .message,
        "reject forming after ALMA"
    );
    assert_eq!(runtime.revision(), revision);
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    assert_eq!(
        public_runtime_changes_json(runtime.last_changes().unwrap()),
        before_changes
    );
    let mut preview = confirmed.clone();
    let mut preview_values = expected.clone();
    preview_values.push(preview.observe(Some(4.0), parameters(4)));
    replica
        .apply(
            runtime
                .apply_update_ref(BarUpdate::forming(timed_bar(3, 4.0)))
                .unwrap(),
        )
        .unwrap();
    assert_plot(replica.result(), 0, &preview_values);
    expected.push(confirmed.observe(Some(5.0), parameters(4)));
    replica
        .apply(
            runtime
                .apply_update_ref(BarUpdate::confirmed(timed_bar(3, 5.0)))
                .unwrap(),
        )
        .unwrap();
    assert_plot(&runtime.confirmed_result(), 0, &expected);
    assert_plot(replica.result(), 0, &expected);
    let mut preview = confirmed.clone();
    let mut preview_values = expected.clone();
    preview_values.push(preview.observe(Some(-0.0), parameters(4)));
    runtime
        .update_without_output(BarUpdate::forming(timed_bar(4, -0.0)))
        .unwrap();
    assert_plot(&runtime.result(), 0, &preview_values);
    for (snapshot, expected) in snapshots {
        assert_plot(&snapshot, 0, &expected);
    }
    runtime.replay_historical_without_output(&seed).unwrap();
    assert_plot(&runtime.result(), 0, &expected[..seed.len()]);
    let mut cold = DirectAlma::default();
    for bar in &seed {
        cold.observe(Some(bar.close), parameters(4));
    }
    let mut replay_values = expected[..seed.len()].to_vec();
    replay_values.push(cold.observe(Some(9.0), parameters(4)));
    runtime
        .update(BarUpdate::forming(timed_bar(3, 9.0)))
        .unwrap();
    assert_plot(&runtime.result(), 0, &replay_values);
}

#[test]
fn alma_request_and_chart_contexts_keep_independent_exact_bits() {
    let hir = program(
        "indicator(\"ALMA contexts\")\n[remote, floored] = request.security(\"REMOTE\", timeframe.period, [ta.alma(close, 4, 0.85, 6), ta.alma(close, 4, 0.25, 2, true)])\nplot(remote)\nplot(floored)\nplot(ta.alma(close, 4, 0.85, 6))\n",
    );
    let remote = (0..12)
        .map(|index| timed_bar(index, (index * index + 3) as f64 / 8.0))
        .collect::<Vec<_>>();
    let provider = InMemoryRequestDataProvider::from_streams([(
        RequestKey::new("REMOTE", RequestTimeframe::default()),
        remote.clone(),
    )])
    .unwrap();
    let mut runtime = HistoricalRuntime::with_request_environment(
        &hir,
        RequestEnvironment::new(ChartContext::default(), Arc::new(provider)),
    );
    let mut request = DirectAlma::default();
    let mut floored = DirectAlma::default();
    let mut chart = DirectAlma::default();
    let mut expected = [Vec::new(), Vec::new(), Vec::new()];
    for (index, remote) in remote.iter().enumerate().take(8) {
        let close = sample(index).unwrap();
        runtime.append_bar(timed_bar(index, close)).unwrap();
        expected[0].push(request.observe(Some(remote.close), parameters(4)));
        expected[1].push(floored.observe(
            Some(remote.close),
            Parameters {
                offset: Some(0.25),
                sigma: Some(2.0),
                floor: true,
                ..parameters(4)
            },
        ));
        expected[2].push(chart.observe(Some(close), parameters(4)));
        for (plot, expected) in expected.iter().enumerate() {
            assert_plot(&runtime.result(), plot, expected);
        }
    }
    let checkpoint = runtime.clone();
    let snapshot = runtime.result();
    for (index, remote) in remote.iter().enumerate().take(10).skip(8) {
        runtime
            .append_bar(timed_bar(index, 100.0 + index as f64))
            .unwrap();
        expected[0].push(request.observe(Some(remote.close), parameters(4)));
        expected[1].push(floored.observe(
            Some(remote.close),
            Parameters {
                offset: Some(0.25),
                sigma: Some(2.0),
                floor: true,
                ..parameters(4)
            },
        ));
        expected[2].push(chart.observe(Some(100.0 + index as f64), parameters(4)));
    }
    for (plot, expected) in expected.iter().enumerate() {
        assert_plot(&runtime.result(), plot, expected);
        assert_plot(&checkpoint.result(), plot, &expected[..8]);
        assert_plot(&snapshot, plot, &expected[..8]);
    }
}
