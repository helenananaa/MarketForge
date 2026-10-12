use std::sync::Arc;

use pine_runtime::{
    Bar, BarUpdate, ChartContext, InMemoryRequestDataProvider, RealtimeRuntime, RequestEnvironment,
    RequestKey, RequestTimeframe, ValueWhenLimits,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program(body: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new(
        "valuewhen-limits.pine",
        format!("//@version=6\nindicator(\"limits\")\n{body}\n"),
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(index: usize, close: f64) -> Bar {
    Bar {
        time: index as i64 * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn limits(cap: usize) -> ValueWhenLimits {
    ValueWhenLimits {
        max_retained_values: Some(cap),
    }
}

#[test]
fn failed_forming_or_confirmation_preserves_selection_workspace_for_retry() {
    let hir = program("plot(ta.median(close,8))\nplot(ta.valuewhen(close>50,close,bar_index%3))");
    for failed_update in [
        BarUpdate::forming(bar(8, 60.0)),
        BarUpdate::confirmed(bar(8, 60.0)),
    ] {
        let mut runtime = RealtimeRuntime::new(&hir)
            .with_valuewhen_limits(limits(0))
            .unwrap();
        runtime
            .seed_historical_without_output(&(0..8).map(|i| bar(i, 10.0)).collect::<Vec<_>>())
            .unwrap();
        let mut replica = runtime.replica();
        let first = runtime
            .apply_update(BarUpdate::forming(bar(8, 20.0)))
            .unwrap();
        replica.apply(&first).unwrap();
        let before = runtime.result();
        let profile = runtime.profile();
        let confirmed_profile = runtime.confirmed_profile();
        let changes = runtime.last_changes().cloned();
        let revision = runtime.revision();
        assert!(
            runtime
                .apply_update(failed_update)
                .unwrap_err()
                .message
                .starts_with("E_VALUEWHEN_BUDGET:")
        );
        assert_eq!(runtime.result(), before);
        assert_eq!(runtime.last_changes(), changes.as_ref());
        assert_eq!(runtime.revision(), revision);
        assert_eq!(runtime.valuewhen_retained_values(), 0);
        assert_eq!(runtime.confirmed_profile(), confirmed_profile);
        assert_eq!(
            runtime.profile(),
            profile,
            "failed candidate lost reusable selection capacity"
        );
        let retry = runtime
            .apply_update(BarUpdate::forming(bar(8, 20.0)))
            .unwrap();
        replica.apply(&retry).unwrap();
        assert_eq!(replica.result(), &runtime.result());
        assert_eq!(runtime.profile(), profile);
    }
}

#[test]
fn forming_replacement_and_confirmation_count_one_state_and_replay_keeps_configuration() {
    let hir = program("plot(ta.valuewhen(close>0,close,bar_index%3))");
    let mut runtime = RealtimeRuntime::new(&hir)
        .with_valuewhen_limits(limits(4))
        .unwrap();
    runtime
        .seed_historical_without_output(
            &(0..3).map(|i| bar(i, 10.0 + i as f64)).collect::<Vec<_>>(),
        )
        .unwrap();
    let mut replica = runtime.replica();
    for close in [99.0, 100.0, 101.0] {
        let changes = runtime
            .apply_update(BarUpdate::forming(bar(3, close)))
            .unwrap();
        replica.apply(&changes).unwrap();
        assert_eq!(replica.result(), &runtime.result());
        assert_eq!(runtime.valuewhen_retained_values(), 4);
        assert_eq!(runtime.confirmed_valuewhen_retained_values(), 3);
    }
    let previous = runtime.result();
    let confirmed = runtime.confirmed_result();
    let revision = runtime.revision();
    let changes = runtime.last_changes().cloned();
    assert!(runtime.set_valuewhen_limits(limits(3)).is_err());
    assert_eq!(runtime.valuewhen_limits(), limits(4));
    assert_eq!(runtime.result(), previous);
    assert_eq!(runtime.confirmed_result(), confirmed);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.last_changes(), changes.as_ref());
    let changes = runtime
        .apply_update(BarUpdate::confirmed(bar(3, 101.0)))
        .unwrap();
    replica.apply(&changes).unwrap();
    assert_eq!(replica.result(), &runtime.result());
    assert_eq!(runtime.confirmed_valuewhen_retained_values(), 4);
    let previous = runtime.result();
    let revision = runtime.revision();
    assert!(
        runtime
            .apply_update(BarUpdate::forming(bar(4, 102.0)))
            .unwrap_err()
            .message
            .starts_with("E_VALUEWHEN_BUDGET:")
    );
    assert_eq!(runtime.result(), previous);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.valuewhen_retained_values(), 4);
    runtime
        .update_without_output(BarUpdate::forming(bar(4, -1.0)))
        .unwrap();
    runtime
        .update_without_output(BarUpdate::confirmed(bar(4, -1.0)))
        .unwrap();
    assert_eq!(runtime.confirmed_valuewhen_retained_values(), 4);
    runtime
        .replay_historical(&(0..2).map(|i| bar(i, 10.0 + i as f64)).collect::<Vec<_>>())
        .unwrap();
    assert_eq!(runtime.valuewhen_limits(), limits(4));
    assert_eq!(runtime.valuewhen_retained_values(), 2);
}

#[test]
fn partial_multi_callsite_failure_does_not_publish_state_or_poison_a_retry() {
    let hir = program(
        "plot(ta.valuewhen(close>0,close,bar_index%3))\nplot(ta.valuewhen(close>0,close+1,bar_index%2))",
    );
    let mut runtime = RealtimeRuntime::new(&hir)
        .with_valuewhen_limits(limits(3))
        .unwrap();
    runtime
        .seed_historical_without_output(&[bar(0, 10.0)])
        .unwrap();
    let previous = runtime.result();
    let revision = runtime.revision();
    assert!(
        runtime
            .apply_update(BarUpdate::forming(bar(1, 99.0)))
            .is_err()
    );
    assert_eq!(runtime.result(), previous);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.valuewhen_retained_values(), 2);
    runtime
        .apply_update(BarUpdate::forming(bar(1, -1.0)))
        .unwrap();
    runtime
        .apply_update(BarUpdate::confirmed(bar(1, -1.0)))
        .unwrap();
    assert_eq!(runtime.valuewhen_retained_values(), 2);
    assert_eq!(runtime.confirmed_valuewhen_retained_values(), 2);
}

#[test]
fn request_update_budget_failure_rolls_back_feed_preview_revision_and_delta() {
    let hir = program(
        "plot(ta.median(close,2))\nplot(ta.valuewhen(true,close,bar_index%3))\nplot(request.security(\"B\",\"1\",ta.valuewhen(close>0,close,bar_index%3)))",
    );
    let timeframe = RequestTimeframe::parse("1").unwrap();
    let key = RequestKey::new("B", timeframe.clone());
    let mut provider = InMemoryRequestDataProvider::new();
    provider
        .insert(
            key.clone(),
            (0..5)
                .map(|index| bar(index, 10.0 + index as f64))
                .collect(),
        )
        .unwrap();
    let environment =
        RequestEnvironment::new(ChartContext::new("A", timeframe), Arc::new(provider));
    let mut runtime = RealtimeRuntime::with_request_environment(&hir, environment)
        .with_valuewhen_limits(limits(7))
        .unwrap();
    runtime
        .seed_historical_without_output(&[bar(4, 1.0)])
        .unwrap();
    let mut replica = runtime.replica();
    replica
        .apply(
            &runtime
                .apply_update(BarUpdate::forming(bar(5, 2.0)))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(runtime.confirmed_valuewhen_retained_values(), 5);
    assert_eq!(runtime.valuewhen_retained_values(), 6);
    let profile = runtime.profile();
    assert!(profile.rolling_window_value_capacity > profile.rolling_window_values);
    let previous = runtime.result();
    let confirmed = runtime.confirmed_result();
    let revision = runtime.revision();
    let changes = runtime.last_changes().cloned();
    assert!(
        runtime
            .apply_request_update(key.clone(), BarUpdate::forming(bar(5, 99.0)))
            .unwrap_err()
            .message
            .starts_with("E_VALUEWHEN_BUDGET:")
    );
    assert_eq!(runtime.result(), previous);
    assert_eq!(runtime.confirmed_result(), confirmed);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.last_changes(), changes.as_ref());
    assert_eq!(runtime.valuewhen_retained_values(), 6);
    // Rollback clones can compact the two-sample history. The reused selection
    // workspace must survive in addition to those retained input samples.
    assert!(
        runtime.profile().rolling_window_value_capacity > runtime.profile().rolling_window_values
    );
    let changes = runtime
        .apply_request_update(key.clone(), BarUpdate::forming(bar(5, 0.0)))
        .unwrap()
        .unwrap();
    replica.apply(&changes).unwrap();
    assert_eq!(replica.result(), &runtime.result());
    assert_eq!(runtime.valuewhen_retained_values(), 7);
    assert_eq!(runtime.confirmed_valuewhen_retained_values(), 5);
    let previous = runtime.result();
    let revision = runtime.revision();
    assert!(
        runtime
            .apply_request_update(key, BarUpdate::forming(bar(5, 99.0)))
            .is_err()
    );
    assert_eq!(runtime.result(), previous);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.valuewhen_retained_values(), 7);
    assert!(
        runtime.profile().rolling_window_value_capacity > runtime.profile().rolling_window_values
    );
}
