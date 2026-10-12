use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pine_runtime::{
    Bar, BarUpdate, ChartContext, InMemoryRequestDataProvider, InputOverrides,
    NoRequestDataProvider, PineValue, RealtimeRuntime, RequestEnvironment, RequestKey,
    RequestTimeframe,
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

fn runtime(
    source: &str,
    chart_timeframe: &str,
    seed: Option<(RequestKey, Vec<Bar>)>,
) -> RealtimeRuntime<'static> {
    let analysis = analyze_source(&SourceFile::new("request-correction.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let provider: Arc<dyn pine_runtime::RequestDataProvider> = match seed {
        Some(stream) => Arc::new(InMemoryRequestDataProvider::from_streams([stream]).unwrap()),
        None => Arc::new(NoRequestDataProvider),
    };
    RealtimeRuntime::from_program_with_request_environment_and_input_overrides(
        analysis.hir.unwrap(),
        RequestEnvironment::new(
            ChartContext::new("A", RequestTimeframe::parse(chart_timeframe).unwrap()),
            provider,
        ),
        InputOverrides::new(),
    )
}

#[test]
fn correction_and_replay_keep_all_closed_intrabar_extras_of_the_retained_chart_bar() {
    let source = "//@version=6\nindicator(\"LTF cut\")\nvalues=request.security_lower_tf(\"B\",\"1\",close)\nplot(array.size(values))\nplot(array.sum(values))\n";
    let key = RequestKey::new("B", RequestTimeframe::parse("1").unwrap());
    let chart = [bar(0, 1.0), bar(300_000, 2.0)];
    for replay in [false, true] {
        let mut live = runtime(source, "5", None);
        let mut control = runtime(source, "5", None);
        for minute in 0..10 {
            let update = BarUpdate::confirmed(bar(minute * 60_000, (minute + 1) as f64));
            live.apply_request_update(key.clone(), update).unwrap();
            control.apply_request_update(key.clone(), update).unwrap();
        }
        live.seed_historical(&chart).unwrap();
        let owned = live.result();
        assert_eq!(
            owned.plots[0].values,
            vec![PineValue::Int(5), PineValue::Int(5)]
        );
        let replacement = if replay {
            live.replay_historical(&chart[..1]).unwrap()
        } else {
            live.correct_historical(300_000, &[]).unwrap()
        };
        control.seed_historical(&chart[..1]).unwrap();
        assert_eq!(replacement.plots, control.result().plots);
        assert_eq!(replacement.plots[0].values, vec![PineValue::Int(5)]);
        assert_eq!(replacement.plots[1].values, vec![PineValue::Float(15.0)]);
        assert_eq!(owned.plots[0].values.len(), 2);
        // The discarded second period must be admissible again after correction.
        live.apply_request_update(key.clone(), BarUpdate::confirmed(bar(300_000, 6.0)))
            .unwrap();
    }
}

#[test]
fn correction_drops_unclosed_higher_timeframe_confirmed_tail() {
    let source = "//@version=6\nindicator(\"HTF cut\")\nplot(request.security(\"B\",\"5\",barstate.islast?close:0))\n";
    let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
    let chart: Vec<_> = (0..10).map(|minute| bar(minute * 60_000, 1.0)).collect();
    let mut live = runtime(source, "1", None);
    live.apply_request_update(key.clone(), BarUpdate::confirmed(bar(0, 10.0)))
        .unwrap();
    live.apply_request_update(key.clone(), BarUpdate::confirmed(bar(300_000, 20.0)))
        .unwrap();
    live.seed_historical(&chart).unwrap();
    let corrected = live.correct_historical(420_000, &[]).unwrap();
    let mut control = runtime(source, "1", None);
    control
        .apply_request_update(key.clone(), BarUpdate::confirmed(bar(0, 10.0)))
        .unwrap();
    control.seed_historical(&chart[..7]).unwrap();
    assert_eq!(corrected.plots, control.result().plots);
    assert_eq!(corrected.plots[0].values[4], PineValue::Float(10.0));
    live.apply_request_update(key, BarUpdate::confirmed(bar(300_000, 20.0)))
        .unwrap();
}

#[test]
fn repeated_replay_cannot_use_a_discarded_early_successor_as_a_close_boundary() {
    let source = "//@version=6\nindicator(\"early successor\")\nplot(request.security(\"B\",\"5\",barstate.islast?close:0))\n";
    let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
    let seed = Some((key.clone(), vec![bar(-300_000, 10.0)]));
    let chart = [bar(0, 1.0), bar(60_000, 1.0)];
    let mut live = runtime(source, "1", seed.clone());
    for (time, close) in [(0, 20.0), (60_000, 30.0)] {
        live.apply_request_update(key.clone(), BarUpdate::confirmed(bar(time, close)))
            .unwrap();
    }
    live.seed_historical(&chart).unwrap();
    let first = live.replay_historical(&chart).unwrap();
    let second = live.replay_historical(&chart).unwrap();
    let mut control = runtime(source, "1", seed);
    control.seed_historical(&chart).unwrap();
    assert_eq!(first.plots, control.result().plots);
    assert_eq!(second.plots, first.plots);
    assert_eq!(first.plots[0].values.last(), Some(&PineValue::Float(10.0)));
}

fn date(year: i32, month: u32, day: u32) -> i64 {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

#[test]
fn monthly_request_correction_uses_calendar_close_including_leap_february() {
    let source = "//@version=6\nindicator(\"month cut\")\nplot(request.security(\"B\",\"M\",barstate.islast?close:0))\n";
    let key = RequestKey::new("B", RequestTimeframe::parse("M").unwrap());
    for (previous, current, next, chart, cut, retained_current) in [
        (
            date(2023, 12, 1),
            date(2024, 1, 1),
            date(2024, 2, 1),
            vec![
                date(2024, 1, 29),
                date(2024, 1, 30),
                date(2024, 1, 31),
                date(2024, 2, 1),
            ],
            date(2024, 1, 31),
            false,
        ),
        (
            date(2023, 12, 1),
            date(2024, 1, 1),
            date(2024, 2, 1),
            vec![
                date(2024, 1, 29),
                date(2024, 1, 30),
                date(2024, 1, 31),
                date(2024, 2, 1),
            ],
            date(2024, 2, 1),
            true,
        ),
        (
            date(2024, 1, 1),
            date(2024, 2, 1),
            date(2024, 3, 1),
            vec![date(2024, 2, 28), date(2024, 2, 29), date(2024, 3, 1)],
            date(2024, 3, 1),
            true,
        ),
    ] {
        let seed = Some((key.clone(), vec![bar(previous, 10.0)]));
        let mut live = runtime(source, "D", seed.clone());
        live.apply_request_update(key.clone(), BarUpdate::confirmed(bar(current, 20.0)))
            .unwrap();
        live.apply_request_update(key.clone(), BarUpdate::confirmed(bar(next, 30.0)))
            .unwrap();
        let chart: Vec<_> = chart.into_iter().map(|time| bar(time, 1.0)).collect();
        live.seed_historical(&chart).unwrap();
        let corrected = live.correct_historical(cut, &[]).unwrap();
        let mut control = runtime(source, "D", seed);
        if retained_current {
            control
                .apply_request_update(key.clone(), BarUpdate::confirmed(bar(current, 20.0)))
                .unwrap();
        }
        let prefix = chart.partition_point(|bar| bar.time < cut);
        control.seed_historical(&chart[..prefix]).unwrap();
        assert_eq!(corrected.plots, control.result().plots, "cut={cut}");
        assert_eq!(
            corrected.plots[0].values.last(),
            Some(&PineValue::Float(if retained_current {
                20.0
            } else {
                10.0
            }))
        );
    }
}

#[test]
fn replay_keeps_a_forming_request_that_has_not_closed_by_the_chart_boundary() {
    let source =
        "//@version=6\nindicator(\"forming request\")\nplot(request.security(\"B\",\"5\",close))\n";
    let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
    let mut live = runtime(source, "1", Some((key.clone(), vec![bar(0, 10.0)])));
    live.apply_request_update(key, BarUpdate::forming(bar(300_000, 20.0)))
        .unwrap();
    let chart: Vec<_> = (0..7).map(|minute| bar(minute * 60_000, 1.0)).collect();
    live.seed_historical(&chart).unwrap();
    live.replay_historical(&chart).unwrap();
    live.apply_update(BarUpdate::forming(bar(420_000, 1.0)))
        .unwrap();
    assert_eq!(
        live.result().plots[0].values.last(),
        Some(&PineValue::Float(20.0))
    );
}

#[test]
fn replay_execution_failure_preserves_forming_state_and_subsequent_replica_changes() {
    let source = "//@version=6\nindicator(\"replay fail\")\nvar n=0\nn+=1\nif close<0\n    runtime.error(\"bad close\")\nplot(n)\nplot(close)\n";
    let mut live = runtime(source, "1", None);
    live.seed_historical(&[bar(0, 1.0)]).unwrap();
    live.apply_update(BarUpdate::forming(bar(60_000, 2.0)))
        .unwrap();
    let mut replica = live.replica();
    let before = live.result();
    let revision = live.revision();
    let last_changes = live.last_changes().cloned();
    assert_eq!(
        live.replay_historical(&[bar(0, -1.0)]).unwrap_err().message,
        "bad close"
    );
    assert_eq!(live.result(), before);
    assert_eq!(live.revision(), revision);
    assert_eq!(live.last_changes(), last_changes.as_ref());
    let changes = live
        .apply_update(BarUpdate::confirmed(bar(60_000, 3.0)))
        .unwrap();
    assert!(replica.apply(&changes).unwrap());
    assert_eq!(replica.result(), &live.result());
    assert_eq!(
        live.result().plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(2)]
    );
}
