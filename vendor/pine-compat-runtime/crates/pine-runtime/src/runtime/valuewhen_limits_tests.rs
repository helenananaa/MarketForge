use super::*;
use crate::{
    Bar, BarUpdate, ChartContext, InMemoryRequestDataProvider, RequestEnvironment, RequestKey,
    RequestTimeframe,
};
use pine_ir::HirProgram;
use pine_sema::analyze_source;
use pine_syntax::SourceFile;
use std::sync::Arc;

fn program(body: &str) -> HirProgram {
    source(&format!(
        "//@version=6\nindicator(\"event limits\")\n{body}\n"
    ))
}

fn source(text: &str) -> HirProgram {
    let analysis = analyze_source(&SourceFile::new("event-limits.pine", text));
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

fn limits(count: usize) -> ValueWhenLimits {
    ValueWhenLimits {
        max_retained_values: Some(count),
    }
}

// Traverse actual logical histories, independently of every production counter.
fn events(runtime: &HistoricalRuntime<'_>) -> usize {
    let local: usize = runtime
        .ta_state
        .valuewhen_state
        .values()
        .map(|history| history.len())
        .sum();
    let requests: usize = runtime
        .request_evaluations
        .values()
        .map(|saved| events(saved.valuewhen_checkpoint()))
        .sum();
    let bounded: usize = runtime
        .bounded_same_context_evaluations
        .values()
        .map(|saved| events(saved))
        .sum();
    let actual = local + requests + bounded;
    assert_eq!(runtime.valuewhen_retained_values(), actual);
    actual
}

fn environment(count: usize) -> RequestEnvironment {
    let timeframe = RequestTimeframe::parse("1").unwrap();
    let mut provider = InMemoryRequestDataProvider::new();
    for symbol in ["B", "C"] {
        provider
            .insert(
                RequestKey::new(symbol, timeframe.clone()),
                (0..count)
                    .map(|index| bar(index, 10.0 + index as f64))
                    .collect(),
            )
            .unwrap();
    }
    RequestEnvironment::new(ChartContext::new("A", timeframe), Arc::new(provider))
}

#[test]
fn valuewhen_limit_is_shared_by_call_sites_and_configuration_failure_is_retryable() {
    let hir = program(
        "plot(ta.valuewhen(true,close,bar_index%3))\nplot(ta.valuewhen(true,close+1,bar_index%2))\nplot(ta.valuewhen(true,close,1))",
    );
    let mut runtime = HistoricalRuntime::new(&hir)
        .with_valuewhen_limits(limits(10))
        .unwrap();
    for index in 0..4 {
        runtime.append_bar(bar(index, index as f64)).unwrap();
    }
    assert_eq!(events(&runtime), 10);
    assert!(
        runtime
            .set_valuewhen_limits(limits(9))
            .unwrap_err()
            .message
            .starts_with("E_VALUEWHEN_BUDGET:")
    );
    assert_eq!(runtime.valuewhen_limits(), limits(10));
    assert_eq!(events(&runtime), 10);
    let mut checkpoint = runtime.clone();
    checkpoint.set_valuewhen_limits(limits(12)).unwrap();
    checkpoint.append_bar(bar(4, 4.0)).unwrap();
    assert_eq!(events(&checkpoint), 12);
    assert_eq!(events(&runtime), 10);
    assert!(
        runtime
            .append_bar(bar(4, 4.0))
            .unwrap_err()
            .message
            .starts_with("E_VALUEWHEN_BUDGET:")
    );
    assert_eq!(events(&runtime), 10);
    assert!(
        runtime
            .append_bar(bar(4, 4.0))
            .unwrap_err()
            .message
            .starts_with("E_RUNTIME_POISONED:")
    );
}

#[test]
fn valuewhen_zero_and_fixed_retention_count_net_events_including_na() {
    let zero =
        program("plot(ta.valuewhen(true,close,-1))\nplot(ta.valuewhen(true,close,4294967296))");
    let mut runtime = HistoricalRuntime::new(&zero)
        .with_valuewhen_limits(limits(0))
        .unwrap();
    for index in 0..20 {
        runtime.append_bar(bar(index, 1.0)).unwrap();
    }
    assert_eq!(events(&runtime), 0);
    let fixed = program("plot(ta.valuewhen(close!=0,close<0?na:close,1))");
    let mut runtime = HistoricalRuntime::new(&fixed)
        .with_valuewhen_limits(limits(2))
        .unwrap();
    for (index, close) in [1.0, -1.0, 0.0, 2.0, -2.0].into_iter().enumerate() {
        runtime.append_bar(bar(index, close)).unwrap();
        assert_eq!(events(&runtime), (index + 1).min(2));
    }
    assert_eq!(runtime.result().plots[0].values[4].as_f64(), Some(2.0));
    let dynamic = program(
        "occurrence=bar_index==0?4294967296:-1\nfloat missing=na\nplot(ta.valuewhen(true,missing,occurrence))",
    );
    let mut runtime = HistoricalRuntime::new(&dynamic)
        .with_valuewhen_limits(limits(1))
        .unwrap();
    runtime.append_bar(bar(0, 1.0)).unwrap();
    assert_eq!(events(&runtime), 1);
    assert!(runtime.append_bar(bar(1, 1.0)).is_err());
    assert_eq!(events(&runtime), 1);
}

#[test]
fn valuewhen_loop_events_count_each_call_and_default_preserves_unlimited_aggregate() {
    let hir = program(
        "float value=na\nfor i=0 to 2\n    value:=ta.valuewhen(true,close+i,bar_index%2)\nplot(value)",
    );
    let mut runtime = HistoricalRuntime::new(&hir)
        .with_valuewhen_limits(limits(5))
        .unwrap();
    runtime.append_bar(bar(0, 1.0)).unwrap();
    assert_eq!(events(&runtime), 3);
    assert!(runtime.append_bar(bar(1, 2.0)).is_err());
    assert_eq!(events(&runtime), 5);
    let mut unlimited = HistoricalRuntime::new(&hir);
    for index in 0..100 {
        unlimited.append_bar(bar(index, index as f64)).unwrap();
    }
    assert_eq!(unlimited.valuewhen_limits(), ValueWhenLimits::default());
    assert_eq!(events(&unlimited), 300);
}

#[test]
fn valuewhen_request_checkpoints_share_parent_allowance_and_replacement_releases_old_share() {
    let hir = program(
        "plot(ta.valuewhen(true,close,bar_index%2))\nplot(request.security(\"B\",\"1\",ta.valuewhen(true,close,bar_index%3)))\nplot(request.security(\"C\",\"1\",ta.valuewhen(true,close,bar_index%3)))",
    );
    let mut runtime = HistoricalRuntime::with_request_environment(&hir, environment(5))
        .with_valuewhen_limits(limits(12))
        .unwrap();
    runtime.append_bar(bar(4, 1.0)).unwrap();
    assert_eq!(events(&runtime), 9); // root one; two checkpoints precede bar five.
    let key = RequestKey::new("B", RequestTimeframe::parse("1").unwrap());
    runtime
        .apply_request_update(key, BarUpdate::confirmed(bar(5, 99.0)))
        .unwrap();
    runtime.append_bar(bar(5, 2.0)).unwrap();
    assert_eq!(events(&runtime), 11); // root two + B five + C four.
    let mut lower = HistoricalRuntime::with_request_environment(&hir, environment(5))
        .with_valuewhen_limits(limits(9))
        .unwrap();
    assert!(
        lower
            .append_bar(bar(4, 1.0))
            .unwrap_err()
            .message
            .starts_with("E_VALUEWHEN_BUDGET:")
    );
    assert_eq!(events(&lower), 5); // B saved four; C's fifth active event is rejected.
}

#[test]
fn valuewhen_changed_capture_replaces_checkpoint_instead_of_spending_its_old_allowance_twice() {
    let hir = program(
        "plot(ta.valuewhen(true,close,bar_index%2))\nscale=input.float(1.0)\nplot(request.security(\"B\",\"1\",ta.valuewhen(true,scale,bar_index%3)))",
    );
    let mut runtime = HistoricalRuntime::with_request_environment(&hir, environment(5))
        .with_valuewhen_limits(limits(8))
        .unwrap();
    runtime.append_bar(bar(4, 1.0)).unwrap();
    assert_eq!(events(&runtime), 5);
    assert_eq!(
        runtime
            .request_evaluations
            .values()
            .next()
            .unwrap()
            .capture_values()
            .cloned()
            .collect::<Vec<_>>(),
        vec![crate::PineValue::Float(1.0)]
    );
    // Public runtime inputs are fixed for an instance. Alter the private fixture
    // to exercise the existing changed-capture cache-rejection path explicitly.
    runtime.input_overrides.insert(
        crate::input_calls(&hir)[0].call_site_id,
        crate::PineValue::Float(2.0),
    );
    let key = RequestKey::new("B", RequestTimeframe::parse("1").unwrap());
    runtime
        .apply_request_update(key, BarUpdate::confirmed(bar(5, 99.0)))
        .unwrap();
    runtime.append_bar(bar(5, 2.0)).unwrap();
    assert_eq!(events(&runtime), 7);
    assert_eq!(runtime.result().plots[1].values[1].as_f64(), Some(2.0));
}

#[test]
fn valuewhen_full_requested_evaluators_spend_parent_allowance_even_when_discarded() {
    let hir = program(
        "plot(ta.valuewhen(true,close,bar_index%2))\nplot(request.security(\"B\",\"1\",ta.valuewhen(true,barstate.islast?close:close+1,bar_index%3)))",
    );
    let mut exact = HistoricalRuntime::with_request_environment(&hir, environment(5))
        .with_valuewhen_limits(limits(6))
        .unwrap();
    exact.append_bar(bar(4, 1.0)).unwrap();
    assert!(exact.request_evaluations.is_empty());
    assert_eq!(events(&exact), 1);
    let mut lower = HistoricalRuntime::with_request_environment(&hir, environment(5))
        .with_valuewhen_limits(limits(5))
        .unwrap();
    assert!(
        lower
            .append_bar(bar(4, 1.0))
            .unwrap_err()
            .message
            .starts_with("E_VALUEWHEN_BUDGET:")
    );
    assert_eq!(events(&lower), 1);
}

#[test]
fn valuewhen_requested_allowance_inheritance_composes_across_ancestor_levels() {
    // Nested Pine requests remain unsupported. Test transitive accounting
    // directly using real histories, without claiming that language capability.
    let hir = program("plot(ta.valuewhen(true,close,bar_index%3))");
    for (cap, succeeds) in [(11, true), (10, false)] {
        let mut parent = HistoricalRuntime::new(&hir)
            .with_valuewhen_limits(limits(cap))
            .unwrap();
        parent.append_bar(bar(0, 1.0)).unwrap();
        let mut child = parent.fork_with_request_environment(environment(5));
        child.inherit_valuewhen_budget(&parent, 0).unwrap();
        for index in 0..5 {
            child.append_bar(bar(index, 2.0)).unwrap();
        }
        let mut grandchild = child.fork_with_request_environment(environment(5));
        grandchild.inherit_valuewhen_budget(&child, 0).unwrap();
        for index in 0..4 {
            grandchild.append_bar(bar(index, 3.0)).unwrap();
        }
        let last = grandchild.append_bar(bar(4, 3.0));
        if succeeds {
            last.unwrap();
            assert_eq!(events(&grandchild), 5);
        } else {
            assert!(last.unwrap_err().message.starts_with("E_VALUEWHEN_BUDGET:"));
            assert_eq!(events(&grandchild), 4);
        }
        assert_eq!(events(&parent), 1);
        assert_eq!(events(&child), 5);
    }
}

#[test]
fn valuewhen_bounded_same_context_retains_one_child_and_replay_preserves_limit() {
    let hir = program(
        "plot(ta.valuewhen(true,close,bar_index%2))\nplot(request.security(syminfo.tickerid,timeframe.period,ta.valuewhen(true,close,bar_index%3),calc_bars_count=4))",
    );
    let mut runtime = HistoricalRuntime::new(&hir)
        .with_valuewhen_limits(limits(8))
        .unwrap();
    runtime
        .append_bars(&(0..4).map(|index| bar(index, 1.0)).collect::<Vec<_>>())
        .unwrap();
    assert_eq!(events(&runtime), 8);
    let blank = runtime.blank_for_replay();
    assert_eq!(blank.valuewhen_limits(), limits(8));
    assert_eq!(events(&blank), 0);
    let mut lower = HistoricalRuntime::new(&hir)
        .with_valuewhen_limits(limits(7))
        .unwrap();
    assert!(
        lower
            .append_bars(&(0..4).map(|index| bar(index, 1.0)).collect::<Vec<_>>())
            .is_err()
    );
    assert!(events(&lower) <= 7);
}

#[test]
fn valuewhen_strategy_fill_restore_restores_event_counter_without_refunding_work() {
    let hir = source(
        "//@version=6\nstrategy(\"event limits\",calc_on_order_fills=true)\nif bar_index==0 and strategy.opentrades==0\n    strategy.entry(\"L\",strategy.long,qty=1)\nplot(ta.valuewhen(true,close,bar_index%3))\n",
    );
    let mut runtime = HistoricalRuntime::new(&hir)
        .with_valuewhen_limits(limits(4))
        .unwrap();
    for index in 0..4 {
        runtime.append_bar(bar(index, 10.0 + index as f64)).unwrap();
        assert_eq!(events(&runtime), index + 1);
    }
    assert!(runtime.profile().strategy_recalculation_passes > 0);
    assert!(runtime.append_bar(bar(4, 14.0)).is_err());
    assert_eq!(events(&runtime), 4);
}

#[test]
fn valuewhen_counter_overflow_fails_before_publishing_counter_or_configuration() {
    let hir = program("plot(close)");
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.valuewhen_budget.local_values = usize::MAX;
    assert!(
        runtime
            .valuewhen_budget
            .replace_local_values(0, 1)
            .unwrap_err()
            .message
            .contains("count overflow")
    );
    assert_eq!(runtime.valuewhen_budget.local_values, usize::MAX);
    runtime.valuewhen_budget.external_values = 1;
    assert!(
        runtime
            .set_valuewhen_limits(limits(0))
            .unwrap_err()
            .message
            .contains("count overflow")
    );
    assert_eq!(runtime.valuewhen_limits(), ValueWhenLimits::default());
}
