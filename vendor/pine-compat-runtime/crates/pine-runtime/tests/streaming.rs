use std::path::PathBuf;

use pine_runtime::{
    AlertEvent, Bar, BarUpdate, OutputRetention, PineValue, RealtimeRuntime, RuntimeChanges,
    RuntimeReplica, SeriesChangeOp, StreamingVisibility,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn apply_runtime_changes(
    base: &pine_runtime::RuntimeResult,
    changes: &RuntimeChanges,
) -> pine_runtime::RuntimeResult {
    let mut replica = RuntimeReplica::new(base.clone(), changes.base_revision);
    replica.apply(changes).unwrap();
    replica.result().clone()
}

fn workspace_fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn runtime_for_source(source: &str) -> RealtimeRuntime<'static> {
    let hir = analyze_source(&SourceFile::new("streaming.pine", source))
        .hir
        .unwrap();
    RealtimeRuntime::from_program(hir)
}

#[test]
fn completed_replica_transfers_full_output_after_forming_and_confirmation() {
    let mut runtime = runtime_for_source(
        "//@version=6\nindicator(\"final snapshot\")\nplot(close)\nbgcolor(color.red)\nlabel.new(bar_index,close,\"你好\")\n",
    );
    runtime.seed_historical(&[bar(0, 1.0)]).unwrap();
    let mut replica = runtime.replica();
    for update in [
        BarUpdate::forming(bar(60000, 2.0)),
        BarUpdate::confirmed(bar(60000, 3.0)),
    ] {
        let changes = runtime.apply_update(update).unwrap();
        replica.apply(&changes).unwrap();
    }
    let expected = pine_runtime::public_runtime_result_json(&runtime.result());
    assert_eq!(
        pine_runtime::public_runtime_result_json(&replica.into_result()),
        expected
    );
}

fn runtime_for_fixture(path: &str) -> RealtimeRuntime<'static> {
    let path = workspace_fixture(path);
    let source = std::fs::read_to_string(&path).expect("fixture");
    let hir = analyze_source(&SourceFile::new(path.display().to_string(), source))
        .hir
        .unwrap();
    RealtimeRuntime::from_program(hir)
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

fn assert_visible_matches_applied(
    visible: &pine_runtime::RuntimeResult,
    changes: &RuntimeChanges,
    runtime: &RealtimeRuntime<'_>,
) -> pine_runtime::RuntimeResult {
    let applied = apply_runtime_changes(visible, changes);
    let snapshot = runtime.result();
    assert_eq!(applied, snapshot);
    let mut replica = RuntimeReplica::new(visible.clone(), changes.base_revision);
    replica.apply(changes).unwrap();
    assert!(!replica.apply(changes).unwrap());
    assert_eq!(replica.result(), &snapshot);
    applied
}

fn assert_series_is_delta(changes: &RuntimeChanges) {
    for change in &changes.series {
        let len = change
            .fields
            .values
            .len()
            .max(change.fields.closes.len())
            .max(change.fields.colors.len());
        assert!(
            len <= 1,
            "streaming series change for {:?} id {} reconstructed {} values",
            change.family,
            change.id,
            len
        );
        assert!(matches!(
            change.op,
            SeriesChangeOp::Append | SeriesChangeOp::ReplaceLast
        ));
    }
}

#[test]
fn streaming_plot_forming_and_confirm_match_snapshots() {
    let mut runtime = runtime_for_source(
        "//@version=6\nindicator(\"stream plots\")\nplot(close, color=close>10 ? color.red : color.blue)\n",
    );
    let mut visible = runtime
        .seed_historical(&[bar(0, 10.0)])
        .expect("seed should run");
    assert_eq!(visible.plots[0].values.len(), 1);
    assert_eq!(runtime.revision(), 1);

    let first = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 20.0)))
        .expect("forming update");
    assert_eq!(first.visibility, StreamingVisibility::Preview);
    assert_eq!(first.revision, 2);
    assert_series_is_delta(&first);
    assert_eq!(first.series[0].op, SeriesChangeOp::Append);
    assert_eq!(first.series[0].fields.values, vec![PineValue::Float(20.0)]);
    visible = assert_visible_matches_applied(&visible, &first, &runtime);
    assert_eq!(runtime.confirmed_result().plots[0].values.len(), 1);

    let replacement = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 5.0)))
        .expect("forming replacement");
    assert_eq!(replacement.visibility, StreamingVisibility::Preview);
    assert_eq!(replacement.revision, 3);
    assert_series_is_delta(&replacement);
    assert_eq!(replacement.series[0].op, SeriesChangeOp::ReplaceLast);
    assert_eq!(
        replacement.series[0].fields.values,
        vec![PineValue::Float(5.0)]
    );
    visible = assert_visible_matches_applied(&visible, &replacement, &runtime);

    let confirmed = runtime
        .apply_update(BarUpdate::confirmed(bar(60_000, 5.0)))
        .expect("confirmed update");
    assert_eq!(confirmed.visibility, StreamingVisibility::Confirmed);
    assert_series_is_delta(&confirmed);
    visible = assert_visible_matches_applied(&visible, &confirmed, &runtime);
    assert_eq!(visible, runtime.confirmed_result());
    assert_eq!(runtime.last_changes(), Some(&confirmed));
}

#[test]
fn sparse_drawing_identities_survive_retention_and_forming_replacement() {
    let mut runtime = runtime_for_source("//@version=6\nindicator(\"sparse drawings\", max_labels_count=500)\nvar label pinned = label.new(bar_index, close, \"pinned\")\nlabel.new(bar_index, close, \"bar\")\nif barstate.islast\n    label.set_text(pinned, str.tostring(close))\n")
        .with_output_retention(OutputRetention::keep_confirmed_bars(8));
    let seed: Vec<_> = (0..256).map(|index| bar(index * 60_000, 10.0)).collect();
    let mut visible = runtime.seed_historical(&seed).unwrap();
    assert!(visible.labels.iter().any(|label| label.id == 1));
    assert!(visible.labels.iter().any(|label| label.id > 250));
    for index in 256..260 {
        for close in [20.0, 21.0] {
            let changes = runtime
                .apply_update(BarUpdate::forming(bar(index * 60_000, close)))
                .unwrap();
            visible = assert_visible_matches_applied(&visible, &changes, &runtime);
        }
        let changes = runtime
            .apply_update(BarUpdate::confirmed(bar(index * 60_000, 22.0)))
            .unwrap();
        visible = assert_visible_matches_applied(&visible, &changes, &runtime);
        assert!(visible.labels.iter().any(|label| label.id == 1));
    }
}

#[test]
fn streaming_shapes_candles_colors_and_fills_match_snapshots() {
    let mut runtime = runtime_for_source(
        r#"//@version=6
indicator("stream families")
plotshape(close > 10)
plotcandle(open, high, low, close)
bgcolor(close > 10 ? color.red : color.blue)
barcolor(close > 10 ? color.green : color.orange)
h1 = hline(10)
h2 = hline(20)
fill(h1, h2, color=close > 10 ? color.red : color.navy)
"#,
    );
    let history: Vec<Bar> = (0..64)
        .map(|index| bar(index as i64 * 60_000, 5.0 + index as f64))
        .collect();
    let mut visible = runtime.seed_historical(&history).expect("family seed");
    assert_eq!(visible.plot_shapes[0].values.len(), 64);
    assert_eq!(visible.plot_candles[0].closes.len(), 64);
    assert_eq!(visible.bg_colors[0].values.len(), 64);
    assert_eq!(visible.fills[0].colors.len(), 64);

    let forming = runtime
        .apply_update(BarUpdate::forming(bar(64 * 60_000, 80.0)))
        .expect("family forming");
    assert_series_is_delta(&forming);
    visible = assert_visible_matches_applied(&visible, &forming, &runtime);
    assert_eq!(visible.plot_shapes[0].values.len(), 65);
    assert_eq!(visible.fills[0].colors.len(), 65);

    let replacement = runtime
        .apply_update(BarUpdate::forming(bar(64 * 60_000, 1.0)))
        .expect("family replacement");
    assert_series_is_delta(&replacement);
    visible = assert_visible_matches_applied(&visible, &replacement, &runtime);

    let confirmed = runtime
        .apply_update(BarUpdate::confirmed(bar(64 * 60_000, 1.0)))
        .expect("family confirm");
    visible = assert_visible_matches_applied(&visible, &confirmed, &runtime);
    assert_eq!(visible, runtime.confirmed_result());
}

#[test]
fn streaming_dense_orders_match_snapshots_across_history_lengths() {
    let source = r#"//@version=6
strategy("stream fills dense")
if close >= 10
    strategy.entry("L", strategy.long, qty=1)
if close < 10
    strategy.close("L")
plotshape(close)
"#;
    for history_len in [8_usize, 64, 256] {
        let mut runtime = runtime_for_source(source);
        let history: Vec<Bar> = (0..history_len)
            .map(|index| {
                let close = if index % 2 == 0 { 12.0 } else { 8.0 };
                bar(index as i64 * 60_000, close)
            })
            .collect();
        let mut visible = runtime
            .seed_historical(&history)
            .expect("dense strategy seed");
        let forming = runtime
            .apply_update(BarUpdate::forming(bar(history_len as i64 * 60_000, 12.0)))
            .expect("dense forming");
        visible = assert_visible_matches_applied(&visible, &forming, &runtime);
        let confirmed = runtime
            .apply_update(BarUpdate::confirmed(bar(history_len as i64 * 60_000, 12.0)))
            .expect("dense confirm");
        visible = assert_visible_matches_applied(&visible, &confirmed, &runtime);
        assert_eq!(visible, runtime.confirmed_result());
        assert_eq!(visible.plot_shapes[0].values.len(), history_len + 1);
    }
}

#[test]
fn streaming_label_create_mutate_and_delete_match_snapshots() {
    let mut runtime = runtime_for_fixture("tests/fixtures/realtime/label_rollback.pine");
    let mut visible = runtime
        .update(BarUpdate::historical(bar(0, 1.0)))
        .expect("historical seed");
    assert_eq!(visible.labels.len(), 1);

    let forming = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 2.0)))
        .expect("forming labels");
    assert!(
        forming
            .drawings
            .iter()
            .any(|change| change.action.as_str() == "add")
    );
    assert!(
        forming
            .drawings
            .iter()
            .any(|change| change.action.as_str() == "setTail")
    );
    visible = assert_visible_matches_applied(&visible, &forming, &runtime);
    assert_eq!(visible.labels.len(), 2);

    let replacement = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 3.0)))
        .expect("replacement labels");
    visible = assert_visible_matches_applied(&visible, &replacement, &runtime);
    assert_eq!(visible.labels.len(), 2);
    assert!(!visible.labels[0].snapshots[1].exists);

    let confirmed = runtime
        .apply_update(BarUpdate::confirmed(bar(60_000, 4.0)))
        .expect("confirmed labels");
    assert!(
        confirmed
            .drawings
            .iter()
            .any(|change| change.action.as_str() == "delete")
    );
    visible = assert_visible_matches_applied(&visible, &confirmed, &runtime);
    assert_eq!(visible.labels.len(), 1);
    assert_eq!(visible, runtime.confirmed_result());
}

#[test]
fn streaming_alerts_and_strategy_fills_do_not_duplicate_on_reread() {
    let mut alerts = runtime_for_source(
        r#"//@version=6
indicator("stream alerts")
if close > 2
    alert("high")
plot(close)
"#,
    );
    let mut visible = alerts.seed_historical(&[bar(0, 1.0)]).expect("alert seed");
    let mut alert_replica = alerts.replica();
    let forming = alerts
        .apply_update(BarUpdate::forming(bar(60_000, 3.0)))
        .expect("alert forming");
    assert!(
        forming
            .alerts
            .iter()
            .any(|change| change.action.as_str() == "add")
    );
    visible = assert_visible_matches_applied(&visible, &forming, &alerts);
    assert_eq!(visible.alerts.len(), 1);
    let reread = alerts.last_changes().cloned().expect("last alert changes");
    alert_replica.apply(&forming).unwrap();
    assert!(!alert_replica.apply(&reread).unwrap());
    assert_eq!(alert_replica.result(), &visible);
    assert_eq!(visible.alerts.len(), 1);

    let quiet = alerts
        .apply_update(BarUpdate::forming(bar(60_000, 1.0)))
        .expect("alert rollback");
    assert!(
        quiet
            .alerts
            .iter()
            .any(|change| change.action.as_str() == "remove")
    );
    visible = assert_visible_matches_applied(&visible, &quiet, &alerts);
    assert!(visible.alerts.is_empty());

    let mut strategy = runtime_for_source(
        r#"//@version=6
strategy("stream fills")
if bar_index == 1
    strategy.entry("L", strategy.long, qty=1)
plot(close)
"#,
    );
    let mut strategy_visible = strategy
        .seed_historical(&[bar(0, 1.0)])
        .expect("strategy seed");
    let forming = strategy
        .apply_update(BarUpdate::forming(bar(60_000, 2.0)))
        .expect("strategy forming");
    strategy_visible = assert_visible_matches_applied(&strategy_visible, &forming, &strategy);
    let mut repeated = RuntimeReplica::new(strategy_visible.clone(), strategy.revision());
    let confirmed = strategy
        .apply_update(BarUpdate::confirmed(bar(60_000, 2.0)))
        .expect("strategy confirm");
    strategy_visible = assert_visible_matches_applied(&strategy_visible, &confirmed, &strategy);
    let orders = strategy_visible
        .strategy
        .as_ref()
        .map(|item| item.orders.len())
        .unwrap_or(0);
    let fills = strategy_visible
        .strategy
        .as_ref()
        .map(|item| item.alerts.len())
        .unwrap_or(0);
    repeated.apply(&confirmed).unwrap();
    assert!(!repeated.apply(&confirmed).unwrap());
    let replayed = repeated.result().clone();
    assert_eq!(
        replayed.strategy.as_ref().map(|item| item.orders.len()),
        Some(orders)
    );
    assert_eq!(
        replayed.strategy.as_ref().map(|item| item.alerts.len()),
        Some(fills)
    );
    assert_eq!(replayed, strategy.result());
}

#[test]
fn streaming_preserves_var_varip_failure_isolation_and_caller_independence() {
    let mut runtime = runtime_for_source(
        r#"//@version=6
indicator("stream var")
var float regular = 0.0
varip float intrabar = 0.0
regular += 1.0
intrabar += 1.0
plot(regular)
plot(intrabar)
"#,
    );
    let mut visible = runtime.seed_historical(&[bar(0, 1.0)]).expect("var seed");
    let first = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 2.0)))
        .expect("var forming");
    visible = assert_visible_matches_applied(&visible, &first, &runtime);
    assert_eq!(
        visible.plots[0].values,
        vec![PineValue::Float(1.0), PineValue::Float(2.0)]
    );
    assert_eq!(
        visible.plots[1].values,
        vec![PineValue::Float(1.0), PineValue::Float(2.0)]
    );

    let second = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 3.0)))
        .expect("varip persist");
    visible = assert_visible_matches_applied(&visible, &second, &runtime);
    assert_eq!(visible.plots[0].values.last(), Some(&PineValue::Float(2.0)));
    assert_eq!(visible.plots[1].values.last(), Some(&PineValue::Float(3.0)));
    assert_eq!(
        runtime.confirmed_result().plots[0].values,
        vec![PineValue::Float(1.0)]
    );

    let handed = runtime.result();
    let handed_json = pine_runtime::public_runtime_result_json(&handed);
    let mut caller = apply_runtime_changes(&visible, &second);
    caller.plots[0].values[0] = PineValue::Float(999.0);
    caller.alerts.push(AlertEvent {
        id: 99,
        bar_index: 0,
        time: 0,
        message: "caller".to_owned(),
        source: "caller".to_owned(),
    });
    assert_eq!(
        pine_runtime::public_runtime_result_json(&runtime.result()),
        handed_json
    );

    let mut failing = runtime_for_source(
        r#"//@version=6
indicator("last-bar resource limit")
if bar_index >= 2
    while true
        x = close
plot(close)
"#,
    );
    let seed = failing
        .seed_historical(&[bar(60_000, 1.0), bar(120_000, 2.0)])
        .expect("prefix");
    let seed_json = pine_runtime::public_runtime_result_json(&seed);
    let before = pine_runtime::public_runtime_result_json(&failing.result());
    let revision = failing.revision();
    let error = failing
        .apply_update(BarUpdate::forming(bar(180_000, 3.0)))
        .expect_err("iteration ceiling");
    assert!(error.message.contains("exceeded maximum iteration"));
    assert_eq!(failing.revision(), revision);
    assert_eq!(
        pine_runtime::public_runtime_result_json(&failing.result()),
        before
    );
    assert_eq!(
        pine_runtime::public_runtime_result_json(&failing.confirmed_result()),
        seed_json
    );
    assert_eq!(pine_runtime::public_runtime_result_json(&seed), seed_json);
}

#[test]
fn compatibility_update_still_returns_complete_snapshot() {
    let mut runtime = runtime_for_source("//@version=6\nindicator(\"compat\")\nplot(close)\n");
    let seed = runtime.seed_historical(&[bar(0, 10.0)]).expect("seed");
    assert_eq!(seed.plots[0].values.len(), 1);
    let forming = runtime
        .update(BarUpdate::forming(bar(60_000, 20.0)))
        .expect("compat forming");
    assert_eq!(forming.plots[0].values.len(), 2);
    assert!(runtime.last_changes().is_none());
    let streaming = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 21.0)))
        .expect("stream after compat");
    assert_series_is_delta(&streaming);
    assert_eq!(streaming.series[0].op, SeriesChangeOp::ReplaceLast);
    assert_eq!(
        apply_runtime_changes(&forming, &streaming),
        runtime.result()
    );
}

#[test]
fn display_retention_keeps_compute_and_strategy_and_trims_replica() {
    let indicator = "//@version=6\nindicator(\"retain\")\nplot(close + close[20])\n";
    let strategy = r#"//@version=6
strategy("retain trades")
if close >= 10
    strategy.entry("L", strategy.long, qty=1)
if close < 10
    strategy.close("L")
plot(close)
"#;
    let history: Vec<Bar> = (0..32)
        .map(|index| {
            let close = if index % 2 == 0 { 12.0 } else { 8.0 };
            bar(index as i64 * 60_000, close)
        })
        .collect();
    let mut full = runtime_for_source(indicator);
    let full_seed = full.seed_historical(&history).expect("full seed");
    let mut trimmed = runtime_for_source(indicator)
        .with_output_retention(OutputRetention::keep_confirmed_bars(8));
    let trimmed_seed = trimmed.seed_historical(&history).expect("trimmed seed");
    assert_eq!(trimmed.display_origin(), 24);
    assert_eq!(trimmed_seed.plots[0].values.len(), 8);
    assert_eq!(
        trimmed_seed.plots[0].values,
        full_seed.plots[0].values[24..].to_vec()
    );

    let mut replica = trimmed.replica();
    assert_eq!(replica.retained_from(), 24);
    let forming = trimmed
        .apply_update(BarUpdate::forming(bar(32 * 60_000, 12.0)))
        .expect("retained forming");
    assert_eq!(forming.retained_from, 24);
    assert!(replica.apply(&forming).unwrap());
    assert_eq!(replica.result().plots[0].values.len(), 9);
    assert_eq!(replica.result(), &trimmed.result());

    let confirmed = trimmed
        .apply_update(BarUpdate::confirmed(bar(32 * 60_000, 12.0)))
        .expect("retained confirm");
    assert_eq!(confirmed.retained_from, 25);
    assert!(replica.apply(&confirmed).unwrap());
    assert_eq!(replica.retained_from(), 25);
    assert_eq!(replica.result().plots[0].values.len(), 8);
    assert_eq!(replica.result(), &trimmed.result());
    full.apply_update(BarUpdate::confirmed(bar(32 * 60_000, 12.0)))
        .expect("full confirm");
    assert_eq!(
        trimmed.result().plots[0].values,
        full.result().plots[0].values[25..].to_vec()
    );

    let mut full_strategy = runtime_for_source(strategy);
    let mut trimmed_strategy =
        runtime_for_source(strategy).with_output_retention(OutputRetention::keep_confirmed_bars(8));
    full_strategy
        .seed_historical(&history)
        .expect("strategy seed");
    trimmed_strategy
        .seed_historical(&history)
        .expect("trimmed strategy seed");
    full_strategy
        .apply_update(BarUpdate::confirmed(bar(32 * 60_000, 12.0)))
        .expect("strategy confirm");
    trimmed_strategy
        .apply_update(BarUpdate::confirmed(bar(32 * 60_000, 12.0)))
        .expect("trimmed strategy confirm");
    assert_eq!(
        trimmed_strategy
            .result()
            .strategy
            .as_ref()
            .map(|item| item.trades.clone()),
        full_strategy
            .result()
            .strategy
            .as_ref()
            .map(|item| item.trades.clone())
    );
    assert_eq!(trimmed_strategy.result().plots[0].values.len(), 8);
}

#[test]
fn display_retention_accepts_schema_2_as_untrimmed_origin() {
    let mut runtime = runtime_for_source("//@version=6\nindicator(\"schema2\")\nplot(close)\n");
    runtime.seed_historical(&[bar(0, 1.0)]).unwrap();
    let mut replica = runtime.replica();
    let mut changes = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 2.0)))
        .unwrap();
    changes.schema_version = 2;
    changes.retained_from = 99;
    assert!(replica.apply(&changes).unwrap());
    assert_eq!(replica.retained_from(), 0);
    assert_eq!(replica.result().plots[0].values.len(), 2);
}

#[test]
fn historical_replay_replaces_confirmed_history_and_discards_forming() {
    let mut runtime = runtime_for_source("//@version=6\nindicator(\"replay\")\nplot(close)\n");
    runtime
        .seed_historical(&[bar(0, 10.0), bar(60_000, 20.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(120_000, 30.0)))
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values,
        vec![
            PineValue::Float(10.0),
            PineValue::Float(20.0),
            PineValue::Float(30.0)
        ]
    );
    let replayed = runtime
        .replay_historical(&[bar(0, 11.0), bar(60_000, 21.0)])
        .unwrap();
    assert_eq!(
        replayed.plots[0].values,
        vec![PineValue::Float(11.0), PineValue::Float(21.0)]
    );
    assert_eq!(runtime.result(), replayed);
    assert_eq!(runtime.confirmed_result(), replayed);
    assert!(runtime.last_changes().is_none());

    let mut control = runtime_for_source("//@version=6\nindicator(\"replay\")\nplot(close)\n");
    let expected = control
        .seed_historical(&[bar(0, 11.0), bar(60_000, 21.0)])
        .unwrap();
    assert_eq!(replayed.plots[0].values, expected.plots[0].values);
}

#[test]
fn historical_replay_failure_restores_forming_and_revision() {
    let mut runtime =
        runtime_for_source("//@version=6\nindicator(\"replay fail\")\nplot(timenow)\n");
    runtime
        .seed_historical_with_execution_times(&[bar(0, 10.0)], &[1_000])
        .unwrap();
    runtime
        .apply_update_with_execution_time(BarUpdate::forming(bar(60_000, 11.0)), 2_000)
        .unwrap();
    let before = runtime.result();
    let revision = runtime.revision();
    let error = runtime
        .replay_historical_with_execution_times(&[bar(0, 12.0)], &[1_000, 2_000])
        .expect_err("count mismatch");
    assert!(error.message.contains("execution timestamp count"));
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.result(), before);
    assert_eq!(
        runtime.result().plots[0].values.last(),
        Some(&PineValue::Int(2_000))
    );
}

#[test]
fn historical_replay_requires_replica_reset_and_keeps_request_feed() {
    let source = r#"//@version=6
indicator("replay request")
plot(request.security("NYSE:IBM", timeframe.period, close))
"#;
    let key = pine_runtime::RequestKey::new("NYSE:IBM", pine_runtime::RequestTimeframe::default());
    let environment = pine_runtime::RequestEnvironment::new(
        pine_runtime::ChartContext::default(),
        std::sync::Arc::new(
            pine_runtime::InMemoryRequestDataProvider::from_streams([(
                key.clone(),
                vec![bar(0, 20.0)],
            )])
            .expect("provider"),
        ),
    );
    let hir = analyze_source(&SourceFile::new("replay-request.pine", source))
        .hir
        .unwrap();
    let mut runtime = RealtimeRuntime::from_program_with_request_environment_and_input_overrides(
        hir,
        environment,
        pine_runtime::InputOverrides::new(),
    );
    runtime.seed_historical(&[bar(0, 5.0)]).unwrap();
    let mut replica = runtime.replica();
    runtime
        .apply_request_update(key.clone(), BarUpdate::forming(bar(60_000, 99.0)))
        .unwrap();
    let forming = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 6.0)))
        .unwrap();
    assert!(replica.apply(&forming).unwrap());
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Float(20.0), PineValue::Float(99.0)]
    );

    let replayed = runtime.replay_historical(&[bar(0, 5.0)]).unwrap();
    assert_eq!(replayed.plots[0].values, vec![PineValue::Float(20.0)]);
    assert_ne!(replica.revision(), runtime.revision());
    assert!(!replica.apply(&forming).unwrap());
    let next = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 6.0)))
        .unwrap();
    assert!(replica.apply(&next).is_err());
    replica.reset_with_retained_from(replayed, next.base_revision, runtime.display_origin());
    assert!(replica.apply(&next).unwrap());
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Float(20.0), PineValue::Float(99.0)]
    );
}

#[test]
fn historical_correct_from_keeps_prefix_and_replays_suffix() {
    let source = "//@version=6\nindicator(\"correct\")\nplot(close)\n";
    let mut runtime = runtime_for_source(source);
    runtime
        .seed_historical(&[bar(0, 10.0), bar(60_000, 20.0), bar(120_000, 30.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(180_000, 40.0)))
        .unwrap();
    let corrected = runtime
        .correct_historical(60_000, &[bar(60_000, 21.0), bar(120_000, 31.0)])
        .unwrap();
    assert_eq!(
        corrected.plots[0].values,
        vec![
            PineValue::Float(10.0),
            PineValue::Float(21.0),
            PineValue::Float(31.0)
        ]
    );
    assert_eq!(runtime.confirmed_bar_count(), 3);
    assert_eq!(runtime.last_confirmed_bar_time(), Some(120_000));
    assert!(runtime.last_changes().is_none());

    let mut control = runtime_for_source(source);
    let expected = control
        .seed_historical(&[bar(0, 10.0), bar(60_000, 21.0), bar(120_000, 31.0)])
        .unwrap();
    assert_eq!(corrected.plots[0].values, expected.plots[0].values);
}

#[test]
fn historical_correct_truncates_and_failed_cut_is_atomic() {
    let mut runtime = runtime_for_source("//@version=6\nindicator(\"truncate\")\nplot(close)\n");
    runtime
        .seed_historical(&[bar(0, 10.0), bar(60_000, 20.0), bar(120_000, 30.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(180_000, 40.0)))
        .unwrap();
    let before = runtime.result();
    let revision = runtime.revision();
    let error = runtime
        .correct_historical(240_000, &[bar(240_000, 50.0)])
        .expect_err("after last confirmed");
    assert!(error.message.contains("E_HISTORY_CORRECT"));
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.result(), before);

    let truncated = runtime.correct_historical(60_000, &[]).unwrap();
    assert_eq!(truncated.plots[0].values, vec![PineValue::Float(10.0)]);
    assert_eq!(runtime.confirmed_bar_count(), 1);
}

#[test]
fn historical_correct_trims_request_feed_so_islast_does_not_see_dropped_bars() {
    let source = r#"//@version=6
indicator("correct request islast")
plot(request.security("NYSE:IBM", timeframe.period, barstate.islast ? close : 0))
"#;
    let key = pine_runtime::RequestKey::new("NYSE:IBM", pine_runtime::RequestTimeframe::default());
    let environment = pine_runtime::RequestEnvironment::new(
        pine_runtime::ChartContext::default(),
        std::sync::Arc::new(
            pine_runtime::InMemoryRequestDataProvider::from_streams([(
                key.clone(),
                vec![bar(0, 10.0)],
            )])
            .expect("provider"),
        ),
    );
    let hir = analyze_source(&SourceFile::new("correct-request.pine", source))
        .hir
        .unwrap();
    let mut runtime = RealtimeRuntime::from_program_with_request_environment_and_input_overrides(
        hir,
        environment,
        pine_runtime::InputOverrides::new(),
    );
    runtime
        .apply_request_update(key.clone(), BarUpdate::confirmed(bar(60_000, 20.0)))
        .unwrap();
    runtime
        .apply_request_update(key, BarUpdate::confirmed(bar(120_000, 30.0)))
        .unwrap();
    runtime
        .seed_historical(&[bar(0, 1.0), bar(60_000, 2.0), bar(120_000, 3.0)])
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Int(0), PineValue::Int(0), PineValue::Float(30.0)]
    );
    let corrected = runtime.correct_historical(120_000, &[]).unwrap();
    assert_eq!(
        corrected.plots[0].values,
        vec![PineValue::Int(0), PineValue::Float(20.0)]
    );
}

#[test]
fn historical_seed_rejects_mixed_execution_clocks() {
    let mut runtime = runtime_for_source("//@version=6\nindicator(\"clock mix\")\nplot(timenow)\n");
    runtime
        .seed_historical_with_execution_times(&[bar(0, 10.0)], &[1_000])
        .unwrap();
    let before = runtime.result();
    let error = runtime
        .seed_historical(&[bar(60_000, 11.0)])
        .expect_err("clocks required");
    assert!(error.message.contains("E_HISTORY_CLOCK"));
    assert_eq!(runtime.result(), before);
    assert_eq!(runtime.confirmed_bar_count(), 1);
}
