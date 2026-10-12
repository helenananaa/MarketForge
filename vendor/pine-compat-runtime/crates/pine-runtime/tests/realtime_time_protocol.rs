use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, RuntimeChanges, public_runtime_result_json};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program() -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new(
        "time-protocol.pine",
        "//@version=6\nindicator(\"time protocol\")\nvarip int n=0\nif barstate.isnew\n    n:=0\nn+=1\nif close<0\n    runtime.error(\"rejected execution\")\nplot(n)\nplot(time)\n",
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

type State = (
    String,
    String,
    Option<RuntimeChanges>,
    u64,
    usize,
    Option<i64>,
    Option<i64>,
);

fn state(runtime: &RealtimeRuntime<'_>) -> State {
    (
        public_runtime_result_json(&runtime.result()),
        public_runtime_result_json(&runtime.confirmed_result()),
        runtime.last_changes().cloned(),
        runtime.revision(),
        runtime.confirmed_bar_count(),
        runtime.last_confirmed_bar_time(),
        runtime.forming_bar_time(),
    )
}

#[test]
fn mismatched_observations_preserve_intrabar_state_and_cached_delta() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .seed_historical_without_output(&[bar(0, 1.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(60_000, 1.0)))
        .unwrap();
    let before = state(&runtime);
    for update in [
        BarUpdate::forming(bar(120_000, 1.0)),
        BarUpdate::confirmed(bar(120_000, 1.0)),
    ] {
        assert!(
            runtime
                .apply_update(update)
                .unwrap_err()
                .message
                .contains("does not match forming time")
        );
        assert_eq!(state(&runtime), before);
    }
    // A rejected observation must not consume another varip execution.
    runtime
        .apply_update(BarUpdate::forming(bar(60_000, 2.0)))
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values.last().unwrap().as_i64(),
        Some(2)
    );
    runtime
        .apply_update(BarUpdate::confirmed(bar(60_000, 3.0)))
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values.last().unwrap().as_i64(),
        Some(3)
    );
    assert_eq!(runtime.forming_bar_time(), None);
    let confirmed = state(&runtime);
    for update in [
        BarUpdate::forming(bar(60_000, 1.0)),
        BarUpdate::confirmed(bar(0, 1.0)),
        BarUpdate::historical(bar(-1, 1.0)),
    ] {
        assert!(
            runtime
                .update_without_output(update)
                .unwrap_err()
                .message
                .contains("must be later than confirmed time")
        );
        assert_eq!(state(&runtime), confirmed);
    }
}

#[test]
fn historical_seed_preflights_the_whole_batch_and_existing_prefix() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir);
    let empty = state(&runtime);
    for bars in [
        vec![bar(0, 1.0), bar(0, 1.0)],
        vec![bar(60_000, 1.0), bar(0, 1.0)],
    ] {
        assert!(
            runtime
                .seed_historical_without_output(&bars)
                .unwrap_err()
                .message
                .contains("strictly increasing")
        );
        assert_eq!(state(&runtime), empty);
    }
    runtime
        .seed_historical_without_output(&[bar(0, 1.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(60_000, 1.0)))
        .unwrap();
    let before = state(&runtime);
    for bars in [vec![bar(0, 1.0)], vec![bar(60_000, 1.0), bar(30_000, 1.0)]] {
        assert!(runtime.seed_historical_without_output(&bars).is_err());
        assert_eq!(state(&runtime), before);
    }
    runtime
        .seed_historical_without_output(&[bar(60_000, 1.0)])
        .unwrap();
    assert_eq!(runtime.forming_bar_time(), None);
    assert_eq!(runtime.last_confirmed_bar_time(), Some(60_000));
    runtime
        .update_without_output(BarUpdate::forming(bar(120_000, 1.0)))
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values.last().unwrap().as_i64(),
        Some(1)
    );
}

#[test]
fn replay_and_correction_reset_time_protocol_only_after_valid_execution() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .seed_historical_without_output(&[bar(0, 1.0), bar(60_000, 1.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(120_000, 1.0)))
        .unwrap();
    let before = state(&runtime);
    assert!(
        runtime
            .replay_historical_without_output(&[bar(0, 1.0), bar(0, 1.0)])
            .is_err()
    );
    assert_eq!(state(&runtime), before);
    assert!(
        runtime
            .correct_historical_without_output(60_000, &[bar(60_000, 1.0), bar(30_000, 1.0)])
            .is_err()
    );
    assert_eq!(state(&runtime), before);
    assert!(
        runtime
            .replay_historical_without_output(&[bar(0, -1.0)])
            .is_err()
    );
    assert_eq!(state(&runtime), before);
    runtime
        .correct_historical_without_output(60_000, &[bar(90_000, 1.0)])
        .unwrap();
    assert_eq!(runtime.last_confirmed_bar_time(), Some(90_000));
    assert_eq!(runtime.forming_bar_time(), None);
    runtime
        .update_without_output(BarUpdate::forming(bar(120_000, 1.0)))
        .unwrap();
    runtime
        .replay_historical_without_output(&[bar(-60_000, 1.0)])
        .unwrap();
    assert_eq!(runtime.last_confirmed_bar_time(), Some(-60_000));
    assert_eq!(runtime.forming_bar_time(), None);
    runtime
        .update_without_output(BarUpdate::confirmed(bar(0, 1.0)))
        .unwrap();
}

#[test]
fn execution_failure_does_not_advance_the_confirmed_timestamp() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .seed_historical_without_output(&[bar(0, 1.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(60_000, 1.0)))
        .unwrap();
    let before = state(&runtime);
    assert_eq!(
        runtime
            .update_without_output(BarUpdate::confirmed(bar(60_000, -1.0)))
            .unwrap_err()
            .message,
        "rejected execution"
    );
    assert_eq!(state(&runtime), before);
    runtime
        .update_without_output(BarUpdate::confirmed(bar(60_000, 1.0)))
        .unwrap();
    assert_eq!(runtime.last_confirmed_bar_time(), Some(60_000));
    assert_eq!(
        runtime.result().plots[0].values.last().unwrap().as_i64(),
        Some(2)
    );
}

#[test]
fn historical_discovery_discards_the_speculative_bar_without_carrying_varip() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .seed_historical_without_output(&[bar(0, 1.0)])
        .unwrap();
    for _ in 0..2 {
        runtime
            .update_without_output(BarUpdate::forming(bar(60_000, 1.0)))
            .unwrap();
    }
    runtime
        .update_without_output(BarUpdate::historical(bar(120_000, 1.0)))
        .unwrap();
    assert_eq!(runtime.last_confirmed_bar_time(), Some(120_000));
    assert_eq!(runtime.forming_bar_time(), None);
    assert_eq!(
        runtime.result().plots[0].values.last().unwrap().as_i64(),
        Some(1)
    );
    runtime
        .update_without_output(BarUpdate::forming(bar(180_000, 1.0)))
        .unwrap();
}
