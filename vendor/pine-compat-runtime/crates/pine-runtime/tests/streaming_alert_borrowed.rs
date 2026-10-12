use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use pine_runtime::{
    AlertEvent, Bar, BarUpdate, ChartContext, EventAction, InputOverrides, OutputRetention,
    RealtimeRuntime, RealtimeUpdateContext, RequestDataError, RequestDataProvider,
    RequestEnvironment, RequestKey, RequestTimeframe, RuntimeChanges, public_runtime_changes_json,
    public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

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

fn runtime(source: &str) -> RealtimeRuntime<'static> {
    let analysis = analyze_source(&SourceFile::new("borrowed-alerts.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    RealtimeRuntime::from_program(analysis.hir.unwrap())
}

fn event(id: u32, index: usize, message: String) -> AlertEvent {
    AlertEvent {
        id,
        bar_index: index,
        time: index as i64 * 60_000,
        message,
        source: "alert".to_owned(),
    }
}

fn messages(id: u32, index: usize, count: usize, reverse: bool) -> Vec<AlertEvent> {
    let payload = "汉🙂".repeat(256);
    (0..count)
        .map(|item| {
            let suffix = if reverse {
                (count - 1 - item).to_string()
            } else if item % 3 == 0 {
                "重复".to_owned()
            } else {
                item.to_string()
            };
            event(id, index, format!("{payload}{suffix}"))
        })
        .collect()
}

fn assert_delta(
    changes: &RuntimeChanges,
    removed: impl IntoIterator<Item = AlertEvent>,
    added: impl IntoIterator<Item = AlertEvent>,
) {
    let expected: Vec<_> = removed
        .into_iter()
        .map(|event| (EventAction::Remove, event))
        .chain(added.into_iter().map(|event| (EventAction::Add, event)))
        .collect();
    assert_eq!(changes.alerts.len(), expected.len());
    for (actual, (action, event)) in changes.alerts.iter().zip(expected) {
        assert_eq!(actual.action, action);
        assert_eq!(actual.event, event);
    }
}

#[test]
fn borrowed_alert_deltas_preserve_duplicates_order_and_closed_history_across_leaves() {
    let source = "//@version=6\nindicator(\"shared alerts\")\npayload = str.repeat(\"汉🙂\", 256)\nn = int(close)\nif n > 0\n    for i = 0 to n - 1\n        suffix = open < 0 ? str.tostring(n - 1 - i) : i % 3 == 0 ? \"重复\" : str.tostring(i)\n        alert(payload + suffix, alert.freq_all)\nplot(close)\n";
    let mut runtime = runtime(source);
    let history = [bar(0, 257.0), bar(60_000, 257.0), bar(120_000, 257.0)];
    runtime.seed_historical_without_output(&history).unwrap();
    let id = runtime.result_view().alerts.iter().next().unwrap().id;
    let mut confirmed: Vec<_> = (0..3)
        .flat_map(|index| messages(id, index, 257, false))
        .collect();
    assert_eq!(runtime.result().alerts, confirmed);
    let mut replica = runtime.replica();
    let mut previous = Vec::new();
    for (count, reverse, kind, remove_from, add_from) in [
        (257, false, 0, 0, 0),
        (257, false, 0, 257, 257),
        (129, false, 0, 129, 129),
        (260, false, 0, 129, 129),
        (259, true, 0, 0, 0),
        (259, true, 1, 259, 259),
    ] {
        let mut input = bar(180_000, count as f64);
        if reverse {
            input.open = -1.0;
            input.low = -1.0;
        }
        let update = if kind == 0 {
            BarUpdate::forming(input)
        } else {
            BarUpdate::confirmed(input)
        };
        let current = messages(id, 3, count, reverse);
        let changes = runtime.apply_update_ref(update).unwrap();
        assert_delta(
            changes,
            previous[remove_from..].iter().rev().cloned(),
            current[add_from..].iter().cloned(),
        );
        assert!(replica.apply(changes).unwrap());
        assert!(!replica.apply(changes).unwrap());
        let mut expected = confirmed.clone();
        expected.extend(current.clone());
        assert_eq!(runtime.result().alerts, expected);
        assert_eq!(replica.result(), &runtime.result());
        if kind == 0 {
            assert_eq!(runtime.confirmed_result().alerts, confirmed);
        } else {
            confirmed = expected;
            assert_eq!(runtime.confirmed_result().alerts, confirmed);
        }
        previous = current;
    }
    let next = messages(id, 4, 2, false);
    let changes = runtime
        .apply_update_ref(BarUpdate::forming(bar(240_000, 2.0)))
        .unwrap();
    assert_delta(changes, [], next.clone());
    replica.apply(changes).unwrap();
    let changes = runtime
        .apply_update_ref(BarUpdate::confirmed(bar(240_000, 0.0)))
        .unwrap();
    assert_delta(changes, next.into_iter().rev(), []);
    replica.apply(changes).unwrap();
    assert_eq!(runtime.result().alerts, confirmed);
    assert_eq!(replica.result(), &runtime.result());
    let last = messages(id, 5, 1, false);
    let changes = runtime
        .apply_update_ref(BarUpdate::historical(bar(300_000, 1.0)))
        .unwrap();
    assert_delta(changes, [], last.clone());
    replica.apply(changes).unwrap();
    confirmed.extend(last);
    assert_eq!(runtime.result().alerts, confirmed);
    assert_eq!(replica.result(), &runtime.result());
}

#[test]
fn trimmed_alert_cursors_follow_retention_and_preview_replacement() {
    let source = "//@version=6\nindicator(\"retained alerts\")\nif close > 0\n    for i = 0 to 128\n        alert(\"重复汉🙂\", alert.freq_all)\nplot(close)\n";
    for keep in [0, 1, 2] {
        let mut runtime =
            runtime(source).with_output_retention(OutputRetention::keep_confirmed_bars(keep));
        let history: Vec<_> = (0..5).map(|index| bar(index * 60_000, 1.0)).collect();
        runtime.seed_historical_without_output(&history).unwrap();
        let mut replica = runtime.replica();
        for index in 5..8 {
            for update in [
                BarUpdate::forming(bar(index * 60_000, 1.0)),
                BarUpdate::forming(bar(index * 60_000, 0.0)),
                BarUpdate::forming(bar(index * 60_000, 1.0)),
                BarUpdate::confirmed(bar(index * 60_000, 1.0)),
            ] {
                let changes = runtime.apply_update_ref(update).unwrap();
                assert!(
                    changes
                        .alerts
                        .iter()
                        .all(|item| item.event.bar_index >= index as usize - 1)
                );
                replica.apply(changes).unwrap();
                assert!(!replica.apply(changes).unwrap());
                assert_eq!(
                    public_runtime_result_json(replica.result()),
                    public_runtime_result_json(&runtime.result())
                );
                assert!(
                    runtime
                        .result()
                        .alerts
                        .iter()
                        .all(|event| event.bar_index >= runtime.display_origin())
                );
            }
        }
        // Changing retention immediately replaces the old cursor. Later deltas
        // must not revisit an expired closed alert prefix.
        runtime.set_output_retention(OutputRetention::keep_confirmed_bars(0));
        replica = runtime.replica();
        let changes = runtime
            .apply_update_ref(BarUpdate::forming(bar(480_000, 1.0)))
            .unwrap();
        assert_eq!(changes.alerts.len(), 129);
        assert!(
            changes
                .alerts
                .iter()
                .all(|item| item.action == EventAction::Add && item.event.bar_index == 8)
        );
        replica.apply(changes).unwrap();
        assert_eq!(replica.result(), &runtime.result());
    }
}

#[test]
fn borrowed_and_owned_update_apis_have_identical_bytes_and_independent_owned_payloads() {
    let source = "//@version=6\nindicator(\"borrowed bytes\")\nvarip int n = 0\nn += 1\nif barstate.isrealtime\n    for i = 0 to 128\n        alert(str.repeat(\"数据🙂\", 64) + str.tostring(close) + str.tostring(n), alert.freq_all)\nplot(timenow)\nplot(n)\n";
    let mut borrowed = runtime(source);
    let mut owned = runtime(source);
    for runtime in [&mut borrowed, &mut owned] {
        runtime
            .seed_historical_with_execution_times_without_output(&[bar(0, 1.0)], &[1_000])
            .unwrap();
    }
    for (index, update, context) in [
        (
            0,
            BarUpdate::forming(bar(60_000, 2.0)),
            RealtimeUpdateContext {
                execution_time: Some(61_000),
                opening_update: Some(true),
            },
        ),
        (
            1,
            BarUpdate::forming(bar(60_000, 3.0)),
            RealtimeUpdateContext {
                execution_time: Some(62_000),
                opening_update: None,
            },
        ),
        (
            2,
            BarUpdate::confirmed(bar(60_000, 4.0)),
            RealtimeUpdateContext {
                execution_time: Some(63_000),
                opening_update: Some(false),
            },
        ),
    ] {
        let expected = if index == 1 {
            owned
                .apply_update_with_execution_time(update, context.execution_time.unwrap())
                .unwrap()
        } else {
            owned.apply_update_with_context(update, context).unwrap()
        };
        let actual = if index == 1 {
            borrowed
                .apply_update_with_execution_time_ref(update, context.execution_time.unwrap())
                .unwrap()
        } else {
            borrowed
                .apply_update_with_context_ref(update, context)
                .unwrap()
        };
        assert_eq!(
            public_runtime_changes_json(actual),
            public_runtime_changes_json(&expected)
        );
        let cached_address = actual as *const RuntimeChanges;
        assert_eq!(
            cached_address,
            borrowed.last_changes().unwrap() as *const RuntimeChanges
        );
        assert_eq!(owned.last_changes(), Some(&expected));
        assert_ne!(
            expected.alerts[0].event.message.as_ptr(),
            owned.last_changes().unwrap().alerts[0]
                .event
                .message
                .as_ptr()
        );
        assert_eq!(
            public_runtime_result_json(&borrowed.result()),
            public_runtime_result_json(&owned.result())
        );
    }
    let mut escaped = owned.last_changes().unwrap().clone();
    escaped.alerts[0].event.message.clear();
    assert!(
        !owned.last_changes().unwrap().alerts[0]
            .event
            .message
            .is_empty()
    );
}

struct FaultableProvider {
    bars: Vec<Bar>,
    fail: AtomicBool,
}

impl RequestDataProvider for FaultableProvider {
    fn bars<'a>(&'a self, key: &RequestKey) -> Result<&'a [Bar], RequestDataError> {
        if self.fail.load(Ordering::Relaxed) {
            Err(RequestDataError::DuplicateBars { time: -999 })
        } else if key.symbol() == "R" {
            Ok(&self.bars)
        } else {
            Err(RequestDataError::MissingData {
                symbol: key.symbol().to_owned(),
                timeframe: key.timeframe().value().to_owned(),
            })
        }
    }
}

fn assert_preserved(
    runtime: &RealtimeRuntime<'_>,
    before: &str,
    revision: u64,
    cache: &str,
    pointers: (*const RuntimeChanges, Option<*const u8>),
) {
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(
        public_runtime_changes_json(runtime.last_changes().unwrap()),
        cache
    );
    assert_eq!(
        runtime.last_changes().unwrap() as *const RuntimeChanges,
        pointers.0
    );
    assert_eq!(
        runtime
            .last_changes()
            .unwrap()
            .alerts
            .first()
            .map(|event| event.event.message.as_ptr()),
        pointers.1
    );
}

#[test]
fn borrowed_request_updates_preserve_cache_on_none_and_roll_back_feed_and_execution_errors() {
    let source = "//@version=6\nindicator(\"request borrow\")\nx = request.security(\"R\", \"1\", close)\nif x < 0\n    runtime.error(\"bad request\")\nif barstate.isrealtime\n    for i = 0 to 256\n        alert(str.repeat(\"汉🙂\", 128) + str.tostring(x), alert.freq_all)\nplot(x)\n";
    let analysis = analyze_source(&SourceFile::new("borrowed-request.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let provider = Arc::new(FaultableProvider {
        bars: vec![bar(0, 1.0)],
        fail: AtomicBool::new(false),
    });
    let environment = RequestEnvironment::new(
        ChartContext::new("C", RequestTimeframe::parse("1").unwrap()),
        provider.clone(),
    );
    let mut runtime = RealtimeRuntime::from_program_with_request_environment_and_input_overrides(
        analysis.hir.unwrap(),
        environment,
        InputOverrides::new(),
    );
    let key = RequestKey::new("R", RequestTimeframe::parse("1").unwrap());
    runtime
        .seed_historical_without_output(&[bar(0, 1.0)])
        .unwrap();
    assert!(
        runtime
            .apply_request_update_ref(key.clone(), BarUpdate::forming(bar(60_000, 2.0)))
            .unwrap()
            .is_none()
    );
    runtime
        .apply_update_ref(BarUpdate::forming(bar(60_000, 5.0)))
        .unwrap();
    let before = public_runtime_result_json(&runtime.result());
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    let revision = runtime.revision();
    let cache = public_runtime_changes_json(runtime.last_changes().unwrap());
    let pointer = (
        runtime.last_changes().unwrap() as *const RuntimeChanges,
        runtime
            .last_changes()
            .unwrap()
            .alerts
            .first()
            .map(|event| event.event.message.as_ptr()),
    );
    for update in [
        BarUpdate::forming(bar(120_000, 3.0)),
        BarUpdate::confirmed(bar(0, 3.0)),
    ] {
        assert!(
            runtime
                .apply_request_update_ref(key.clone(), update)
                .is_err()
        );
        assert_preserved(&runtime, &before, revision, &cache, pointer);
    }
    provider.fail.store(true, Ordering::Relaxed);
    assert!(
        runtime
            .apply_request_update_ref(key.clone(), BarUpdate::forming(bar(60_000, 3.0)))
            .unwrap_err()
            .message
            .contains("-999")
    );
    provider.fail.store(false, Ordering::Relaxed);
    assert_preserved(&runtime, &before, revision, &cache, pointer);
    assert_eq!(
        runtime
            .apply_request_update_ref(key.clone(), BarUpdate::forming(bar(60_000, -1.0)))
            .unwrap_err()
            .message,
        "bad request"
    );
    assert_preserved(&runtime, &before, revision, &cache, pointer);
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        confirmed
    );
    let mut replica = runtime.replica();
    let changes = runtime
        .apply_request_update_ref(key.clone(), BarUpdate::forming(bar(60_000, 3.0)))
        .unwrap()
        .unwrap();
    assert_eq!(changes.alerts.len(), 514);
    assert!(
        changes.alerts[..257]
            .iter()
            .all(|item| item.action == EventAction::Remove && item.event.message.ends_with('2'))
    );
    assert!(
        changes.alerts[257..]
            .iter()
            .all(|item| item.action == EventAction::Add && item.event.message.ends_with('3'))
    );
    replica.apply(changes).unwrap();
    let pointer = changes as *const RuntimeChanges;
    assert_eq!(
        runtime.last_changes().unwrap() as *const RuntimeChanges,
        pointer
    );
    assert_eq!(replica.result(), &runtime.result());
    let changes = runtime
        .apply_update_ref(BarUpdate::confirmed(bar(60_000, 5.0)))
        .unwrap();
    replica.apply(changes).unwrap();
    assert_eq!(replica.result(), &runtime.result());
    let cache = public_runtime_changes_json(runtime.last_changes().unwrap());
    let pointer = (
        runtime.last_changes().unwrap() as *const RuntimeChanges,
        runtime
            .last_changes()
            .unwrap()
            .alerts
            .first()
            .map(|event| event.event.message.as_ptr()),
    );
    let revision = runtime.revision();
    let before = public_runtime_result_json(&runtime.result());
    assert!(
        runtime
            .apply_request_update_ref(key, BarUpdate::confirmed(bar(60_000, 3.0)))
            .unwrap()
            .is_none()
    );
    assert_preserved(&runtime, &before, revision, &cache, pointer);
}

#[test]
fn borrowed_chart_failures_preserve_last_delta_and_snapshot_operations_clear_it() {
    let source = "//@version=6\nindicator(\"cache lifecycle\")\nif close < 0\n    runtime.error(\"bad close\")\nif barstate.isrealtime\n    alert(\"cached\", alert.freq_all)\nplot(close)\n";
    let mut runtime = runtime(source);
    runtime
        .seed_historical_without_output(&[bar(0, 1.0)])
        .unwrap();
    runtime
        .apply_update_ref(BarUpdate::forming(bar(60_000, 2.0)))
        .unwrap();
    let revision = runtime.revision();
    let before = public_runtime_result_json(&runtime.result());
    let cache = public_runtime_changes_json(runtime.last_changes().unwrap());
    let pointer = (
        runtime.last_changes().unwrap() as *const RuntimeChanges,
        runtime
            .last_changes()
            .unwrap()
            .alerts
            .first()
            .map(|event| event.event.message.as_ptr()),
    );
    assert_eq!(
        runtime
            .apply_update_ref(BarUpdate::forming(bar(60_000, -1.0)))
            .unwrap_err()
            .message,
        "bad close"
    );
    assert_preserved(&runtime, &before, revision, &cache, pointer);
    runtime
        .update_without_output(BarUpdate::forming(bar(60_000, 3.0)))
        .unwrap();
    assert!(runtime.last_changes().is_none());
    runtime
        .apply_update_ref(BarUpdate::confirmed(bar(60_000, 4.0)))
        .unwrap();
    assert!(runtime.last_changes().is_some());
    runtime
        .seed_historical_without_output(&[bar(120_000, 5.0)])
        .unwrap();
    assert!(runtime.last_changes().is_none());
    runtime
        .apply_update_ref(BarUpdate::forming(bar(180_000, 6.0)))
        .unwrap();
    runtime
        .replay_historical_without_output(&[bar(0, 7.0)])
        .unwrap();
    assert!(runtime.last_changes().is_none());
}
