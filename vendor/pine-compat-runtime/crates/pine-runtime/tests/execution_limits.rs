use std::sync::Arc;

use pine_runtime::{
    Bar, BarUpdate, ChartContext, ExecutionLimits, HistoricalRuntime, InMemoryRequestDataProvider,
    RealtimeRuntime, RequestEnvironment, RequestKey, RequestTimeframe, RuntimeError,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program(body: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new(
        "limits.pine",
        format!("//@version=6\nindicator(\"limits\")\n{body}\n"),
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(time: i64, close: f64) -> Bar {
    Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn loop_limits(count: u64) -> ExecutionLimits {
    ExecutionLimits {
        max_loop_iterations_per_bar: count,
        ..ExecutionLimits::default()
    }
}

#[test]
fn growing_v6_for_boundary_returns_a_runtime_error() {
    let hir = program("int last=1\nfor i=0 to last\n    last+=1\nplot(last)");
    let error = HistoricalRuntime::new(&hir)
        .append_bar(bar(0, 1.0))
        .unwrap_err();
    assert_eq!(
        error.message,
        "for loop exceeded maximum iteration count of 100000"
    );
}

#[test]
fn nested_loops_spend_one_bar_budget() {
    let hir = program("int n=0\nfor i=0 to 9\n    for j=0 to 4\n        n+=1\nplot(n)");
    let error = HistoricalRuntime::new(&hir)
        .with_execution_limits(loop_limits(12))
        .append_bar(bar(0, 1.0))
        .unwrap_err();
    assert!(
        error
            .message
            .contains("per-bar loop iteration budget exceeded")
    );
}

#[test]
fn strategy_fill_recalculation_does_not_restore_spent_loop_allowance() {
    for recalculate in [false, true] {
        let analysis = analyze_source(&SourceFile::new(
            "fill-limits.pine",
            format!(
                "//@version=6\nstrategy(\"fill limits\",calc_on_order_fills={recalculate})\n\
                 varip int passes=0\nfor i=0 to 0\n    passes+=1\n\
                 if bar_index==0 and strategy.opentrades==0\n    strategy.entry(\"L\",strategy.long,qty=1)\nplot(passes)\n"
            ),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&hir).with_execution_limits(loop_limits(1));
        runtime.append_bar(bar(0, 10.0)).unwrap();
        let result = runtime.append_bar(bar(60_000, 11.0));
        if recalculate {
            assert!(
                result
                    .unwrap_err()
                    .message
                    .contains("per-bar loop iteration budget exceeded")
            );
        } else {
            result.unwrap();
            assert_eq!(runtime.result().plots[0].values[1].as_i64(), Some(2));
        }
    }
}

#[test]
fn foreach_families_and_loop_expressions_share_the_budget() {
    for body in [
        "int n=0\nfor x in array.from(1,2,3)\n    n+=x\nplot(n)",
        "int n=0\nm=matrix.new<int>(3,1,1)\nfor row in m\n    n+=array.size(row)\nplot(n)",
        "int n=0\nm=map.new<int,int>()\nm.put(1,1)\nm.put(2,2)\nm.put(3,3)\nfor [key,value] in m\n    n+=value\nplot(n)",
        "f()=>\n    for i=0 to 2\n        i\nplot(f())",
        "f()=>\n    int n=0\n    while n<3\n        n+=1\n        n\nplot(f())",
    ] {
        let hir = program(body);
        let error = HistoricalRuntime::new(&hir)
            .with_execution_limits(loop_limits(2))
            .append_bar(bar(0, 1.0))
            .unwrap_err();
        assert!(
            error
                .message
                .contains("per-bar loop iteration budget exceeded"),
            "{body}: {error:?}"
        );
    }
}

#[test]
fn successful_bars_reset_the_budget_and_replay_preserves_configuration() {
    let hir = program("var n=0\nfor i=0 to 2\n    n+=1\nplot(n)");
    let limits = loop_limits(3);
    let mut historical = HistoricalRuntime::new(&hir).with_execution_limits(limits);
    historical
        .append_bars(&[bar(0, 1.0), bar(60_000, 1.0)])
        .unwrap();
    assert_eq!(historical.result().plots[0].values[1].as_i64(), Some(6));
    let mut realtime = RealtimeRuntime::new(&hir).with_execution_limits(limits);
    realtime
        .seed_historical(&[bar(0, 1.0), bar(60_000, 1.0)])
        .unwrap();
    realtime.replay_historical(&[bar(0, 1.0)]).unwrap();
    assert_eq!(realtime.execution_limits(), limits);
    realtime
        .apply_update(BarUpdate::confirmed(bar(60_000, 1.0)))
        .unwrap();
    assert_eq!(realtime.result().plots[0].values[1].as_i64(), Some(6));
}

#[test]
fn expression_work_has_a_separate_configurable_budget() {
    let hir = program("plot(1+2+3+4)");
    let mut runtime = HistoricalRuntime::new(&hir).with_execution_limits(ExecutionLimits {
        max_steps_per_bar: 3,
        ..ExecutionLimits::default()
    });
    let error = runtime.append_bar(bar(0, 1.0)).unwrap_err();
    assert!(
        error
            .message
            .contains("per-bar evaluation step budget exceeded")
    );
}

fn environment(count: i64) -> RequestEnvironment {
    let timeframe = RequestTimeframe::parse("1").unwrap();
    let key = RequestKey::new("B", timeframe.clone());
    let mut provider = InMemoryRequestDataProvider::new();
    provider
        .insert(key, (0..count).map(|i| bar(i * 60_000, 1.0)).collect())
        .unwrap();
    RequestEnvironment::new(ChartContext::new("A", timeframe), Arc::new(provider))
}

#[test]
fn requested_history_does_not_get_a_new_budget_for_every_requested_bar() {
    let hir = program("plot(request.security(\"B\",\"1\",barstate.islast?math.abs(close):close))");
    let error = HistoricalRuntime::with_request_environment(&hir, environment(40))
        .with_execution_limits(ExecutionLimits {
            max_steps_per_bar: 40,
            ..ExecutionLimits::default()
        })
        .append_bar(bar(2_340_000, 1.0))
        .unwrap_err();
    assert!(
        error
            .message
            .contains("per-bar evaluation step budget exceeded")
    );
}

#[test]
fn incremental_request_evaluation_spends_parent_expression_budget() {
    let hir = program("plot(request.security(\"B\",\"1\",math.abs(close)))");
    let error = HistoricalRuntime::with_request_environment(&hir, environment(40))
        .with_execution_limits(ExecutionLimits {
            max_steps_per_bar: 40,
            ..ExecutionLimits::default()
        })
        .append_bar(bar(2_340_000, 1.0))
        .unwrap_err();
    assert!(
        error
            .message
            .contains("per-bar evaluation step budget exceeded")
    );
}

#[test]
fn bounded_same_context_checkpoints_receive_the_current_chart_allowance() {
    let hir = program(
        "plot(request.security(syminfo.tickerid,timeframe.period,math.abs(close),calc_bars_count=30))",
    );
    let mut runtime = HistoricalRuntime::new(&hir).with_execution_limits(ExecutionLimits {
        max_steps_per_bar: 32,
        ..ExecutionLimits::default()
    });
    let bars: Vec<_> = (0..30).map(|i| bar(i * 60_000, 1.0)).collect();
    runtime.append_bars(&bars).unwrap();
    assert_eq!(runtime.result().plots[0].values.len(), 30);
    assert!(
        runtime.result().plots[0]
            .values
            .iter()
            .all(|value| value.as_f64() == Some(1.0))
    );
}

#[test]
fn failed_realtime_budget_does_not_consume_the_next_update_allowance() {
    let hir =
        program("int last=2\nif close<0\n    last:=9\nint n=0\nfor i=0 to last\n    n+=1\nplot(n)");
    let mut realtime = RealtimeRuntime::new(&hir).with_execution_limits(loop_limits(3));
    realtime.seed_historical(&[bar(0, 1.0)]).unwrap();
    assert!(
        realtime
            .apply_update(BarUpdate::confirmed(bar(60_000, -1.0)))
            .is_err()
    );
    realtime
        .apply_update(BarUpdate::confirmed(bar(60_000, 1.0)))
        .unwrap();
    assert_eq!(realtime.result().plots[0].values[1].as_i64(), Some(3));
}

#[test]
fn runtime_error_text_cannot_impersonate_loop_control() {
    for message in [
        "__pine_internal_loop_break__",
        "__pine_internal_loop_continue__",
        "loop control escaped its enclosing loop",
    ] {
        for loop_body in [
            format!("for i=0 to 2\n    runtime.error(\"{message}\")"),
            format!("int i=0\nwhile i<3\n    i+=1\n    runtime.error(\"{message}\")"),
            format!("for i in array.from(1,2,3)\n    runtime.error(\"{message}\")"),
            format!("f()=>\n    runtime.error(\"{message}\")\nfor i=0 to 2\n    f()"),
        ] {
            let hir = program(&format!("{loop_body}\nplot(99)"));
            let error = HistoricalRuntime::new(&hir)
                .append_bar(bar(0, 1.0))
                .unwrap_err();
            assert_eq!(
                error,
                RuntimeError {
                    message: message.to_owned()
                }
            );
        }
    }
}
