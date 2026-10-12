use std::{collections::BTreeMap, sync::Arc};

use pine_ir::{CallSiteId, HirExpr, HirExprKind, HirProgram, HirStmtKind};

use crate::{
    Bar, BarUpdate, HistoricalRuntime, InputOverrides, OutputRetention, PineValue, PreparedProgram,
    RealtimeRuntime, RequestEnvironment, RuntimeResult, input_calls, public_runtime_changes_json,
    public_runtime_result_json,
};

const LEGACY: &str = "//@version=6\nindicator(\"shared rolling key\")\nplot(ta.wma(close, 2))\nplot(ta.wma(close, 2))\nplot(ta.sma(close + 1.0, 2))\nplot(ta.wma(close + 1.0, 2))\nplot(ta.sma(close + 98.0, 2))\n";

fn analyze(source: &str) -> HirProgram {
    let source = pine_syntax::SourceFile::new("ta_state_identity.pine", source);
    let analysis = pine_sema::analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

// These fixtures place every rewritten call under a top-level expression or
// declaration. Unexpected forms fail rather than silently missing a collision.
fn rewrite(expr: &mut HirExpr, id: &mut impl FnMut(&str) -> CallSiteId) {
    match &mut expr.kind {
        HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } => {
            if callee.starts_with("ta.") || callee == "fixnan" {
                *call_site_id = id(callee);
            }
            for arg in args {
                rewrite(&mut arg.value, id);
            }
        }
        HirExprKind::Unary { expr, .. } => rewrite(expr, id),
        HirExprKind::Binary { left, right, .. } => {
            rewrite(left, id);
            rewrite(right, id);
        }
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite(condition, id);
            rewrite(then_expr, id);
            rewrite(else_expr, id);
        }
        HirExprKind::Literal(_) | HirExprKind::Builtin(_) | HirExprKind::Symbol(_) => {}
        _ => panic!("unexpected fixture expression"),
    }
}

fn assign(program: &mut HirProgram, id: &mut impl FnMut(&str) -> CallSiteId) {
    for statement in &mut program.statements {
        match &mut statement.kind {
            HirStmtKind::Expr(expr) | HirStmtKind::Decl { value: expr, .. } => rewrite(expr, id),
            HirStmtKind::If { .. } => {} // Only the fixture's runtime.error guard.
            _ => panic!("unexpected fixture statement"),
        }
    }
    program.next_call_site_id = u32::MAX;
}

fn pair(source: &str, shared: CallSiteId) -> (HirProgram, HirProgram) {
    let mut collision = analyze(source);
    let mut partitioned = collision.clone();
    assign(&mut collision, &mut |_| shared);
    // Independent oracle: each callee gets its own public ID. Duplicate calls
    // of the SAME callee still share a site, retaining their invocation rules.
    let mut ids = BTreeMap::new();
    assign(&mut partitioned, &mut |callee| {
        let next = CallSiteId(10_000 + ids.len() as u32);
        *ids.entry(callee.to_owned()).or_insert(next)
    });
    (collision, partitioned)
}

fn bar(index: i64, close: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: close,
        high: close + 2.0,
        low: close - 2.0,
        close,
        volume: 3.0,
    }
}

fn assert_same(actual: &RuntimeResult, expected: &RuntimeResult) {
    assert_eq!(
        public_runtime_result_json(actual),
        public_runtime_result_json(expected)
    );
    for (actual, expected) in actual.plots.iter().zip(&expected.plots) {
        for (actual, expected) in actual.values.iter().zip(&expected.values) {
            match (actual, expected) {
                (PineValue::Float(a), PineValue::Float(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                _ => assert_eq!(actual, expected),
            }
        }
    }
}

#[test]
fn mixed_window_updates_do_not_corrupt_same_bar_undo() {
    let (collision, reference) = pair(LEGACY, CallSiteId(u32::MAX));
    let prepared = PreparedProgram::new(collision.clone());
    assert_eq!(prepared.hir(), &collision);
    let mut actual = HistoricalRuntime::from_prepared(&prepared);
    let mut expected = HistoricalRuntime::new(&reference);
    actual.append_bar(bar(0, 1.0)).unwrap();
    expected.append_bar(bar(0, 1.0)).unwrap();
    let result = actual.result();
    let values: Vec<_> = result
        .plots
        .iter()
        .map(|plot| plot.values[0].clone())
        .collect();
    assert_eq!(
        values,
        [
            PineValue::Na,
            PineValue::Float(1.0),
            PineValue::Na,
            PineValue::Float(5.0 / 3.0),
            PineValue::Na
        ]
    );
    assert_same(&result, &expected.result());
    for index in 1..20 {
        actual.append_bar(bar(index, index as f64 + 1.0)).unwrap();
        expected.append_bar(bar(index, index as f64 + 1.0)).unwrap();
        assert_same(&actual.result(), &expected.result());
    }
}

const STATE_FAMILIES: &str = r#"//@version=6
indicator("state families")
plot(ta.sma(bar_index % 4 == 1 ? na : close, bar_index % 3 + 2))
plot(ta.ema(close + 10, 3))
plot(ta.rma(close + 20, 3))
plot(ta.dema(close + 30, 3))
plot(ta.tema(close + 40, 3))
plot(ta.wma(close + 50, 3))
plot(ta.variance(close + 60, 3))
plot(ta.stdev(close + 70, 3))
plot(ta.cum(close + 80))
plot(ta.max(close + 90))
plot(ta.min(close + 100))
plot(ta.barssince(close > 4))
plot(ta.highest(close + 110, 3))
plot(ta.lowest(close + 120, 3))
plot(ta.crossover(close, 4.0) ? 1 : 0)
plot(ta.crossunder(close, 4.0) ? 1 : 0)
plot(ta.cross(close, 4.0) ? 1 : 0)
plot(fixnan(bar_index % 3 == 1 ? na : close + 130))
"#;

#[test]
fn sparse_and_dense_collisions_partition_windows_recurrences_extremes_and_crosses() {
    for id in [CallSiteId(0), CallSiteId(1), CallSiteId(u32::MAX)] {
        let (collision, reference) = pair(STATE_FAMILIES, id);
        // Both borrowed and prepared constructors must build the same aliases.
        let mut borrowed = HistoricalRuntime::new(&collision);
        let shared = Arc::new(collision.clone());
        let prepared = PreparedProgram::from_shared_hir(Arc::clone(&shared));
        assert_eq!(prepared.hir(), shared.as_ref());
        let mut owned = HistoricalRuntime::from_prepared(&prepared);
        let mut expected = HistoricalRuntime::new(&reference);
        for index in 0..32 {
            let close = [1.0, 6.0, 3.0, 8.0, 2.0, 4.0, -0.0, 0.1][index as usize % 8];
            for runtime in [&mut borrowed, &mut owned, &mut expected] {
                runtime.append_bar(bar(index, close)).unwrap();
            }
            assert_same(&borrowed.result(), &expected.result());
            assert_same(&owned.result(), &expected.result());
        }
    }
}

#[test]
fn forming_confirmation_failed_updates_and_replay_keep_the_same_state_partition() {
    let source = format!("{STATE_FAMILIES}\nif close < 0\n    runtime.error(\"negative\")\n");
    let (collision, reference) = pair(&source, CallSiteId(u32::MAX));
    let prepared = PreparedProgram::new(collision);
    let reference = PreparedProgram::new(reference);
    let mut actual = RealtimeRuntime::from_prepared(&prepared);
    let mut expected = RealtimeRuntime::from_prepared(&reference);
    let seed = [bar(0, 1.0), bar(1, 2.0), bar(2, 3.0)];
    for runtime in [&mut actual, &mut expected] {
        runtime.set_output_retention(OutputRetention::keep_confirmed_bars(2));
        runtime.seed_historical_without_output(&seed).unwrap();
    }
    assert_same(&actual.result(), &expected.result());
    let mut replica = actual.replica();
    for (index, close, confirmed) in [
        (3, 4.0, false),
        (3, 8.0, false),
        (3, 6.0, false),
        (3, 7.0, true),
        (4, 2.0, false),
        (4, 3.0, true),
        (5, 9.0, false),
    ] {
        let update = if confirmed {
            BarUpdate::confirmed(bar(index, close))
        } else {
            BarUpdate::forming(bar(index, close))
        };
        let changes = actual.apply_update_ref(update).unwrap();
        let other = expected.apply_update_ref(update).unwrap();
        assert_eq!(
            public_runtime_changes_json(changes),
            public_runtime_changes_json(other)
        );
        replica.apply(changes).unwrap();
        assert_same(replica.result(), &actual.result());
        assert_same(&actual.result(), &expected.result());
        let before = actual.result();
        let revision = actual.revision();
        let failed_index = if confirmed { index + 1 } else { index };
        let failure = actual
            .apply_update(BarUpdate::forming(bar(failed_index, -1.0)))
            .unwrap_err();
        let other = expected
            .apply_update(BarUpdate::forming(bar(failed_index, -1.0)))
            .unwrap_err();
        assert!(failure.message.contains("negative"), "{}", failure.message);
        assert_eq!(failure.message, other.message);
        assert_eq!(actual.revision(), revision);
        assert_same(&actual.result(), &before);
        assert_same(&actual.result(), &expected.result());
    }
    let repaired = [bar(2, 10.0), bar(3, 11.0), bar(4, 12.0)];
    actual.correct_historical(120_000, &repaired).unwrap();
    expected.correct_historical(120_000, &repaired).unwrap();
    assert_same(&actual.result(), &expected.result());
    actual.replay_historical(&seed).unwrap();
    expected.replay_historical(&seed).unwrap();
    actual
        .update_without_output(BarUpdate::forming(bar(3, 5.0)))
        .unwrap();
    expected
        .update_without_output(BarUpdate::forming(bar(3, 5.0)))
        .unwrap();
    assert_same(&actual.result(), &expected.result());
}

#[test]
fn fixnan_keeps_its_original_slot_when_colliding_with_recursive_ta_state() {
    let source = "//@version=6\nindicator(\"fixnan\")\nplot(fixnan(bar_index > 0 ? na : close + 100))\nplot(ta.ema(close, 2))\nplot(ta.cum(close))\n";
    let (collision, reference) = pair(source, CallSiteId(u32::MAX));
    let mut actual = HistoricalRuntime::new(&collision);
    let mut expected = HistoricalRuntime::new(&reference);
    actual
        .append_bars(&[bar(0, 1.0), bar(1, 2.0), bar(2, 3.0)])
        .unwrap();
    expected
        .append_bars(&[bar(0, 1.0), bar(1, 2.0), bar(2, 3.0)])
        .unwrap();
    assert_same(&actual.result(), &expected.result());
    assert_eq!(
        actual.result().plots[0].values,
        vec![PineValue::Float(101.0); 3]
    );
}

#[test]
fn prepared_hir_host_input_source_and_requested_output_ids_are_preserved() {
    let source = "//@version=6\nindicator(\"identities\")\nscale=input.float(2.0)\nplot(ta.sma(close, 2) * scale)\nplot(request.security(syminfo.tickerid, timeframe.period, ta.ema(close, 2), calc_bars_count=3))\n";
    let (collision, reference) = pair(source, CallSiteId(u32::MAX));
    let inputs = input_calls(&collision);
    let shared = Arc::new(collision.clone());
    let prepared = PreparedProgram::from_shared_hir(Arc::clone(&shared));
    assert_eq!(prepared.hir(), shared.as_ref());
    assert_eq!(input_calls(prepared.hir()), inputs);
    let overrides = InputOverrides::new().with_value(inputs[0].call_site_id, PineValue::Float(7.0));
    let mut actual = HistoricalRuntime::from_prepared_with_request_environment_and_input_overrides(
        &prepared,
        RequestEnvironment::default(),
        overrides.clone(),
    );
    let mut expected = HistoricalRuntime::with_input_overrides(&reference, overrides);
    let bars: Vec<_> = (0..8).map(|index| bar(index, index as f64 + 1.0)).collect();
    actual.append_bars(&bars).unwrap();
    expected.append_bars(&bars).unwrap();
    assert_same(&actual.result(), &expected.result());
    assert_eq!(actual.result().plots[0].values[1], PineValue::Float(10.5));
    assert!(!actual.bounded_same_context_evaluations.is_empty());
}
