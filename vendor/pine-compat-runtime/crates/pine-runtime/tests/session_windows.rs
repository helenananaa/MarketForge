use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, SessionWindowIds, SessionWindowInput,
    session_window_input_from_bars,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program() -> pine_ir::HirProgram {
    let source = SourceFile::new(
        "session_windows.pine",
        "//@version=6\nstrategy(\"session\", calc_on_every_tick=true)\nstrategy.risk.max_intraday_filled_orders(2)\nstrategy.entry(\"L\", strategy.long)\nplot(strategy.position_size)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

fn bar(index: usize) -> Bar {
    Bar {
        time: (index as i64 + 1) * 60_000,
        open: 10.0,
        high: 10.0,
        low: 10.0,
        close: 10.0,
        volume: 1.0,
    }
}

fn windows(indices: &[usize], name: &str) -> SessionWindowInput {
    session_window_input_from_bars(
        indices
            .iter()
            .map(|&index| {
                (
                    index,
                    SessionWindowIds {
                        window_id: name.to_owned(),
                        trading_day_id: name.to_owned(),
                    },
                )
            })
            .collect(),
    )
    .expect("windows")
}

fn forming_runtime(program: &pine_ir::HirProgram) -> RealtimeRuntime<'_> {
    let mut runtime = RealtimeRuntime::new(program)
        .with_session_windows(windows(&[0, 1], "day"))
        .expect("input");
    runtime.seed_historical(&[bar(0)]).expect("seed");
    runtime.update(BarUpdate::forming(bar(1))).expect("forming");
    runtime
}

#[test]
fn session_batch_preflight_rejects_missing_tail_without_executing_prefix() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program)
        .with_session_windows(windows(&[0, 2], "day"))
        .expect("input");
    let before = runtime.result();
    let error = runtime
        .append_bars(&[bar(0), bar(1), bar(2)])
        .expect_err("missing bar 1");
    assert!(error.message.contains("E_SESSION_COVERAGE"));
    assert_eq!(runtime.result(), before);
    runtime
        .extend_session_windows(windows(&[1], "day"))
        .expect("repair missing coverage");
    runtime
        .append_bars(&[bar(0), bar(1), bar(2)])
        .expect("retry");
}

#[test]
fn session_append_failure_is_retryable_and_extensions_match_batch() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program)
        .with_session_windows(windows(&[0], "day"))
        .expect("input");
    runtime.append_bar(bar(0)).expect("first");
    let before = runtime.result();
    assert!(runtime.append_bar(bar(1)).is_err());
    assert_eq!(runtime.result(), before);
    runtime
        .extend_session_windows(windows(&[1, 2], "day"))
        .expect("extend");
    runtime.append_bars(&[bar(1), bar(2)]).expect("append");
    let mut batch = HistoricalRuntime::new(&program)
        .with_session_windows(windows(&[0, 1, 2], "day"))
        .expect("input");
    batch.append_bars(&[bar(0), bar(1), bar(2)]).expect("batch");
    assert_eq!(runtime.result(), batch.result());
}

#[test]
fn session_history_cannot_be_replaced_removed_or_extended_with_conflicting_ids() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program)
        .with_session_windows(windows(&[0], "day"))
        .expect("input");
    runtime.append_bar(bar(0)).expect("first");
    for replacement in [
        windows(&[0, 1], "changed"),
        windows(&[], "day"),
        windows(&[1], "day"),
    ] {
        let error = runtime
            .clone()
            .with_session_windows(replacement)
            .err()
            .expect("reject replacement");
        assert!(error.message.contains("E_SESSION_HISTORY_CHANGED"));
    }
    let before = runtime.session_windows().clone();
    let error = runtime
        .extend_session_windows(windows(&[0, 1], "changed"))
        .expect_err("reject extension");
    assert!(error.message.contains("E_SESSION_HISTORY_CHANGED"));
    assert_eq!(runtime.session_windows(), &before);
    runtime
        .extend_session_windows(windows(&[0, 1], "day"))
        .expect("idempotent history");
    runtime = runtime
        .with_session_windows(windows(&[0, 1, 2], "day"))
        .expect("unchanged prefix");
    runtime.append_bar(bar(1)).expect("second");
}

#[test]
fn session_mode_cannot_be_enabled_after_utc_execution() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bar(bar(0)).expect("UTC");
    for input in [windows(&[0, 1], "day"), windows(&[1], "day")] {
        assert!(runtime.clone().with_session_windows(input.clone()).is_err());
        assert!(runtime.extend_session_windows(input).is_err());
    }
    runtime
        .extend_session_windows(windows(&[], "day"))
        .expect("empty no-op");
}

#[test]
fn session_realtime_extensions_preserve_forming_and_confirmed_state() {
    let program = program();
    let mut runtime = RealtimeRuntime::new(&program)
        .with_session_windows(windows(&[0], "day"))
        .expect("input");
    runtime.seed_historical(&[bar(0)]).expect("seed");
    let confirmed = runtime.confirmed_result();
    assert!(runtime.update(BarUpdate::forming(bar(1))).is_err());
    assert_eq!(runtime.result(), confirmed);
    runtime
        .extend_session_windows(windows(&[1], "day"))
        .expect("extend");
    let forming = runtime.update(BarUpdate::forming(bar(1))).expect("forming");
    let error = runtime
        .extend_session_windows(windows(&[1, 2], "changed"))
        .expect_err("forming is locked");
    assert!(error.message.contains("E_SESSION_HISTORY_CHANGED"));
    assert_eq!(runtime.result(), forming);
    assert_eq!(runtime.confirmed_result(), confirmed);
    assert!(
        forming_runtime(&program)
            .with_session_windows(windows(&[0], "day"))
            .is_err()
    );
    runtime
        .extend_session_windows(windows(&[0, 1, 2], "day"))
        .expect("extend while forming");
    assert_eq!(runtime.result(), forming);
    let repeated = runtime
        .update(BarUpdate::forming(bar(1)))
        .expect("replacement tick");
    assert_eq!(repeated, forming);
    runtime
        .update(BarUpdate::confirmed(bar(1)))
        .expect("confirm");
    runtime.update(BarUpdate::confirmed(bar(2))).expect("next");
    let mut control = RealtimeRuntime::new(&program)
        .with_session_windows(windows(&[0, 1, 2], "day"))
        .expect("input");
    control.seed_historical(&[bar(0)]).expect("seed");
    control.update(BarUpdate::forming(bar(1))).expect("forming");
    control
        .update(BarUpdate::forming(bar(1)))
        .expect("forming replacement");
    control
        .update(BarUpdate::confirmed(bar(1)))
        .expect("confirm");
    control.update(BarUpdate::confirmed(bar(2))).expect("next");
    assert_eq!(runtime.result(), control.result());
}

#[test]
fn session_realtime_builder_updates_both_snapshots_without_rewriting_forming_ids() {
    let program = program();
    let mut runtime = RealtimeRuntime::new(&program)
        .with_session_windows(windows(&[0, 1], "day"))
        .expect("input");
    runtime.seed_historical(&[bar(0)]).expect("seed");
    runtime.update(BarUpdate::forming(bar(1))).expect("forming");
    assert!(
        forming_runtime(&program)
            .with_session_windows(windows(&[0, 1], "changed"))
            .is_err()
    );
    runtime = runtime
        .with_session_windows(windows(&[0, 1, 2], "day"))
        .expect("extend replacement");
    runtime
        .update(BarUpdate::confirmed(bar(1)))
        .expect("confirm");
    runtime.update(BarUpdate::confirmed(bar(2))).expect("next");
}
