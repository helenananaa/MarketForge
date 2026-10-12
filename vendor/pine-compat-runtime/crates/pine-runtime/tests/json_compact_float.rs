use pine_runtime::{
    Bar, BarUpdate, ListSplice, PineValue, PlotSeries, RealtimeRuntime, RuntimeChanges,
    RuntimeReplica, RuntimeResult, StrategyChanges, StrategyEquitySnapshot, StrategyOrderEvent,
    StrategyOrderFillAlertOutput, StrategyPositionSnapshot, StrategyResult, StrategyTrade,
    StreamingVisibility, into_public_runtime_result_json, public_runtime_changes_json,
    public_runtime_result_json, public_runtime_result_view_json, runtime_changes_from_json,
    runtime_result_from_json, write_public_runtime_changes_json, write_public_runtime_result_json,
    write_public_runtime_result_view_json,
};
use std::io::{self, Write};

fn assert_plot_bits(result: &RuntimeResult, expected: &[f64]) {
    assert_eq!(result.plots[0].values.len(), expected.len());
    for (actual, expected) in result.plots[0].values.iter().zip(expected) {
        assert_eq!(actual.as_f64().unwrap().to_bits(), expected.to_bits());
    }
}

#[test]
fn compact_snapshots_match_owned_borrowed_consuming_and_sink_outputs() {
    let finite = [
        f64::MAX,
        -f64::MAX,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::from_bits(1),
        -f64::from_bits(1),
        1e308,
        1e-308,
        0.0,
        -0.0,
        1.5,
        -0.25,
        1e31,
        1e-31,
    ];
    let mut result = RuntimeResult::default();
    let mut values = finite.into_iter().map(PineValue::Float).collect::<Vec<_>>();
    values.extend([f64::NAN, f64::INFINITY, f64::NEG_INFINITY].map(PineValue::Float));
    result.plots.push(PlotSeries::new(1, values));
    result.plots[0].hist_base = PineValue::Float(-f64::MAX);
    result.plots.push(PlotSeries::new(
        2,
        vec![PineValue::Tuple(vec![
            PineValue::Float(f64::MAX),
            PineValue::ChartPoint(pine_runtime::ChartPointValue::new(
                PineValue::Float(f64::from_bits(1)),
                PineValue::Int(0),
                PineValue::Float(-f64::MAX),
            )),
        ])],
    ));
    let expected = public_runtime_result_json(&result);
    assert_eq!(public_runtime_result_view_json(&result.view()), expected);
    assert_eq!(into_public_runtime_result_json(result.clone()), expected);
    for borrowed in [false, true] {
        let mut sink = Vec::new();
        if borrowed {
            write_public_runtime_result_view_json(&result.view(), &mut sink).unwrap();
        } else {
            write_public_runtime_result_json(&result, &mut sink).unwrap();
        }
        assert_eq!(sink, expected.as_bytes());
    }
    assert!(expected.contains("1.7976931348623157e+308"));
    assert!(expected.contains("-5e-324"));
    assert!(expected.contains(",0,-0,1.5,-0.25,"));
    let parsed = runtime_result_from_json(&expected).unwrap();
    let mut finite_only = parsed.clone();
    finite_only.plots[0].values.truncate(finite.len());
    assert_plot_bits(&finite_only, &finite);
    assert_eq!(
        parsed.plots[0].hist_base.as_f64().unwrap().to_bits(),
        (-f64::MAX).to_bits()
    );
    assert!(
        parsed.plots[0].values[finite.len()..]
            .iter()
            .all(PineValue::is_na)
    );
    let PineValue::Tuple(compound) = &parsed.plots[1].values[0] else {
        panic!("expected tuple")
    };
    assert_eq!(compound[0].as_f64().unwrap().to_bits(), f64::MAX.to_bits());
    let PineValue::ChartPoint(point) = &compound[1] else {
        panic!("expected chart point")
    };
    assert_eq!(point.time.as_f64().unwrap().to_bits(), 1);
    assert_eq!(
        point.price.as_f64().unwrap().to_bits(),
        (-f64::MAX).to_bits()
    );
    assert_eq!(public_runtime_result_json(&parsed), expected);
}

#[test]
fn compact_realtime_changes_preserve_preview_confirmed_drawings_and_replica_bits() {
    let source = pine_syntax::SourceFile::new(
        "compact.pine",
        "//@version=6\nindicator(\"compact\")\nplot(close)\nlabel.new(bar_index,close)\nline.new(bar_index,close,bar_index+1,close)\n",
    );
    let analysis = pine_sema::analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    let bar = |index: i64, close| Bar {
        time: index * 60000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    };
    runtime.seed_historical(&[bar(0, f64::MAX)]).unwrap();
    let seed = public_runtime_result_view_json(&runtime.result_view());
    let mut replica =
        RuntimeReplica::new(runtime_result_from_json(&seed).unwrap(), runtime.revision());
    let mut confirmed_values = vec![f64::MAX];
    for (index, value) in [
        -f64::MAX,
        1e-308,
        f64::from_bits(1),
        -f64::from_bits(1),
        -0.0,
    ]
    .into_iter()
    .enumerate()
    {
        let confirmed_before = public_runtime_result_view_json(&runtime.confirmed_result_view());
        for (confirmed, current) in [(false, value), (false, -value), (true, value)] {
            let update = if confirmed {
                BarUpdate::confirmed(bar(index as i64 + 1, current))
            } else {
                BarUpdate::forming(bar(index as i64 + 1, current))
            };
            let changes = runtime.apply_update(update).unwrap();
            let wire = public_runtime_changes_json(&changes);
            let mut sink = Vec::new();
            write_public_runtime_changes_json(&changes, &mut sink).unwrap();
            assert_eq!(sink, wire.as_bytes());
            let decoded = runtime_changes_from_json(&wire).unwrap();
            assert!(replica.apply(&decoded).unwrap());
            assert!(!replica.apply(&decoded).unwrap());
            let visible = public_runtime_result_view_json(&runtime.result_view());
            assert_eq!(public_runtime_result_json(replica.result()), visible);
            assert_eq!(public_runtime_result_json(&runtime.result()), visible);
            let mut visible_values = confirmed_values.clone();
            visible_values.push(current);
            assert_plot_bits(replica.result(), &visible_values);
            if confirmed {
                confirmed_values.push(current);
                assert_eq!(
                    public_runtime_result_view_json(&runtime.confirmed_result_view()),
                    visible
                );
            } else {
                assert_eq!(
                    public_runtime_result_view_json(&runtime.confirmed_result_view()),
                    confirmed_before
                );
            }
            assert_plot_bits(&runtime.confirmed_result(), &confirmed_values);
        }
    }
}

fn strategy(value: f64) -> StrategyResult {
    StrategyResult {
        orders: vec![StrategyOrderEvent {
            id: "order".into(),
            bar_index: 0,
            time: 0,
            direction: "long".into(),
            qty: value,
            price: value,
        }],
        trades: vec![StrategyTrade {
            id: "trade".into(),
            exit_id: "exit".into(),
            entry_bar_index: 0,
            exit_bar_index: 1,
            entry_time: 0,
            exit_time: 1,
            entry_price: value,
            exit_price: value,
            qty: value,
            profit: value,
        }],
        position: vec![StrategyPositionSnapshot {
            bar_index: 0,
            size: value,
            avg_price: Some(value),
        }],
        equity: vec![StrategyEquitySnapshot {
            bar_index: 0,
            cash: value,
            market_value: value,
            equity: value,
            net_profit: value,
        }],
        alerts: vec![StrategyOrderFillAlertOutput {
            id: "alert".into(),
            bar_index: 0,
            time: 0,
            direction: "long".into(),
            qty: value,
            price: value,
            entry_id: None,
            exit_id: None,
            message: "fill".into(),
        }],
        diagnostics: Vec::new(),
    }
}

fn assert_strategy_bits(strategy: &StrategyResult, expected: f64) {
    for value in [
        strategy.orders[0].qty,
        strategy.orders[0].price,
        strategy.trades[0].entry_price,
        strategy.trades[0].exit_price,
        strategy.trades[0].qty,
        strategy.trades[0].profit,
        strategy.position[0].size,
        strategy.position[0].avg_price.unwrap(),
        strategy.equity[0].cash,
        strategy.equity[0].market_value,
        strategy.equity[0].equity,
        strategy.equity[0].net_profit,
        strategy.alerts[0].qty,
        strategy.alerts[0].price,
    ] {
        assert_eq!(value.to_bits(), expected.to_bits());
    }
}

#[test]
fn compact_strategy_fields_match_snapshot_delta_and_replica_bits() {
    for value in [
        f64::MAX,
        -f64::MAX,
        f64::from_bits(1),
        -f64::from_bits(1),
        -0.0,
        1.25,
    ] {
        let strategy = strategy(value);
        let result = RuntimeResult {
            strategy: Some(strategy.clone()),
            ..RuntimeResult::default()
        };
        let wire = public_runtime_result_json(&result);
        let decoded = runtime_result_from_json(&wire).unwrap();
        assert_strategy_bits(decoded.strategy.as_ref().unwrap(), value);
        assert_eq!(public_runtime_result_json(&decoded), wire);
        let mut changes = RuntimeChanges::new(1, StreamingVisibility::Confirmed);
        changes.strategy = Some(StrategyChanges {
            orders: Some(ListSplice {
                start: 0,
                items: strategy.orders,
            }),
            trades: Some(ListSplice {
                start: 0,
                items: strategy.trades,
            }),
            alerts: Some(ListSplice {
                start: 0,
                items: strategy.alerts,
            }),
            position: Some(ListSplice {
                start: 0,
                items: strategy.position,
            }),
            equity: Some(ListSplice {
                start: 0,
                items: strategy.equity,
            }),
            diagnostics: None,
        });
        let decoded = runtime_changes_from_json(&public_runtime_changes_json(&changes)).unwrap();
        let mut replica = RuntimeReplica::new(RuntimeResult::default(), 0);
        replica.apply(&decoded).unwrap();
        assert_strategy_bits(replica.result().strategy.as_ref().unwrap(), value);
        assert_eq!(public_runtime_result_json(replica.result()), wire);
    }
}

#[test]
fn public_sinks_propagate_failure_inside_compact_snapshot_and_delta_numbers() {
    struct Sink {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes.len() == self.limit {
                return Err(io::Error::new(io::ErrorKind::StorageFull, "full"));
            }
            let count = bytes.len().min(3).min(self.limit - self.bytes.len());
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut result = RuntimeResult::default();
    result
        .plots
        .push(PlotSeries::new(1, vec![PineValue::Float(f64::MAX)]));
    let mut changes = RuntimeChanges::new(1, StreamingVisibility::Confirmed);
    changes.series.push(pine_runtime::SeriesChange {
        family: pine_runtime::SeriesFamily::Plot,
        id: 1,
        op: pine_runtime::SeriesChangeOp::Append,
        start: 0,
        fields: pine_runtime::SeriesFields {
            values: vec![PineValue::Float(f64::MAX)],
            ..pine_runtime::SeriesFields::default()
        },
        header: None,
    });
    for delta in [false, true] {
        let expected = if delta {
            public_runtime_changes_json(&changes)
        } else {
            public_runtime_result_json(&result)
        };
        let limit = expected.find("1.797693").unwrap() + 11;
        let mut sink = Sink {
            bytes: Vec::new(),
            limit,
        };
        let error = if delta {
            write_public_runtime_changes_json(&changes, &mut sink)
        } else {
            write_public_runtime_result_json(&result, &mut sink)
        }
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert_eq!(sink.bytes, expected.as_bytes()[..limit]);
    }
}
