use std::sync::Arc;

use pine_ir::{
    HirExpr, HirExprKind, HirLiteral, HirSeriesHistoryRequirement, HirSeriesMaxBarsBack, PineType,
    Qualifier, SeriesId, ValueKind,
};

use crate::*;

fn program(text: &str) -> pine_ir::HirProgram {
    let source = pine_syntax::SourceFile::new("zero-depth.pine", text);
    let analysis = pine_sema::analyze_source(&source);
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

#[test]
fn zero_depth_values_keep_evaluation_and_persistent_state_without_history_recording() {
    let hir = program(
        r#"//@version=6
indicator("zero depth")
var total = 0
items = array.new<int>()
for counter = 0 to 2
    total += 1
    array.push(items, total)
plot(close[0] + array.get(items, 2))
"#,
    );
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.set_builtin_symbols(&bar(0, 10.0), 0).unwrap();
    assert!(runtime.current_series.is_empty());
    assert!(runtime.active_series.is_empty());
    let before_steps = runtime.execution_steps_remaining;
    for statement in &hir.statements {
        assert_eq!(
            runtime.eval_stmt(statement).unwrap(),
            crate::runtime::statements::StmtControl::None
        );
    }
    assert!(runtime.execution_steps_remaining < before_steps);
    assert!(runtime.current_series.is_empty());
    assert!(runtime.active_series.is_empty());
    assert_eq!(runtime.current_series.capacity(), 0);
    let total = hir
        .symbols
        .iter()
        .find(|symbol| symbol.name == "total")
        .unwrap();
    assert_eq!(
        runtime.current_symbols.get(&total.id),
        Some(&PineValue::Int(3))
    );
    assert_eq!(
        runtime.var_store.get(&total.var_slot_id.unwrap()),
        Some(&PineValue::Int(3))
    );

    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&[bar(0, 10.0), bar(1, 10.0)]).unwrap();
    assert_eq!(
        runtime.result().plots[0].values,
        [PineValue::Float(13.0), PineValue::Float(16.0)]
    );
    assert_eq!(runtime.profile().series_values, 0);
    assert_eq!(runtime.profile().current_series_capacity, 0);
}

#[test]
fn sparse_manual_ids_preserve_bounded_unbounded_and_clamped_retention() {
    let mut hir = program("//@version=6\nindicator(\"manual\")\nplot(close)\n");
    let bounded = SeriesId(u32::MAX);
    let unbounded = SeriesId(u32::MAX - 1);
    let zero = SeriesId(u32::MAX - 2);
    let clamped = SeriesId(u32::MAX - 3);
    let absent = SeriesId(u32::MAX - 4);
    hir.series_history = vec![
        HirSeriesHistoryRequirement {
            series_id: bounded,
            max_constant_offset: 3,
            has_dynamic_offsets: false,
        },
        HirSeriesHistoryRequirement {
            series_id: unbounded,
            max_constant_offset: 0,
            has_dynamic_offsets: true,
        },
        HirSeriesHistoryRequirement {
            series_id: zero,
            max_constant_offset: 0,
            has_dynamic_offsets: false,
        },
        HirSeriesHistoryRequirement {
            series_id: clamped,
            max_constant_offset: 0,
            has_dynamic_offsets: true,
        },
    ];
    hir.series_max_bars_back = vec![HirSeriesMaxBarsBack {
        series_id: clamped,
        max_bars_back: 0,
    }];
    let prepared = PreparedProgram::new(hir.clone());
    for mut runtime in [
        HistoricalRuntime::new(&hir),
        HistoricalRuntime::from_prepared(&prepared),
    ] {
        assert_eq!(runtime.series_retention.max_depth_for(bounded), Some(3));
        assert_eq!(runtime.series_retention.max_depth_for(unbounded), None);
        assert_eq!(runtime.series_retention.max_depth_for(zero), Some(0));
        assert_eq!(runtime.series_retention.max_depth_for(clamped), Some(0));
        assert_eq!(runtime.series_retention.max_depth_for(absent), Some(0));
        let before_steps = runtime.execution_steps_remaining;
        for (id, retained) in [
            (bounded, true),
            (unbounded, true),
            (zero, false),
            (clamped, false),
            (absent, false),
        ] {
            let expr = HirExpr {
                kind: HirExprKind::Literal(HirLiteral::Float(17.0)),
                pine_type: PineType::new(Qualifier::Series, ValueKind::Float),
                series_id: Some(id),
            };
            assert_eq!(runtime.eval_expr(&expr).unwrap(), PineValue::Float(17.0));
            assert_eq!(runtime.current_series.contains_key(&id), retained);
            assert_eq!(runtime.active_series.contains(&id), retained);
        }
        assert_eq!(before_steps - runtime.execution_steps_remaining, 5);
        assert_eq!(runtime.current_series.len(), 2);
        runtime.commit_current_series().unwrap();
        assert_eq!(
            runtime.series_store.read(bounded, 1),
            PineValue::Float(17.0)
        );
        assert_eq!(
            runtime.series_store.read(unbounded, 1),
            PineValue::Float(17.0)
        );
        assert_eq!(runtime.series_store.read(clamped, 1), PineValue::Na);
    }
}

#[test]
fn global_zero_max_bars_back_keeps_dynamic_reads_and_miss_diagnostics() {
    let mut hir =
        program("//@version=6\nindicator(\"clamped\")\nplot(na(close[bar_index]) ? 1 : 0)\n");
    hir.max_bars_back = Some(0);
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&[bar(0, 10.0), bar(1, 11.0), bar(2, 12.0)])
        .unwrap();
    let result = runtime.result();
    assert_eq!(
        result.plots[0].values,
        [PineValue::Int(0), PineValue::Int(1), PineValue::Int(1)]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "W_HISTORY_MAX_BARS_BACK")
    );
    let profile = runtime.profile();
    assert_eq!(profile.series_values, 0);
    assert_eq!(profile.current_series_capacity, 0);
    assert_eq!(profile.history_dynamic_retention_misses, 2);
    assert_eq!(profile.history_max_bars_back, Some(0));
    assert!(runtime.active_series.is_empty());
}

#[test]
fn repeated_assignments_and_conditional_udfs_keep_committed_source_history() {
    let hir = program(
        r#"//@version=6
indicator("history")
carry(s) => s[1]
var source = 0.0
float previous = na
for counter = 0 to 2
    source := close + counter
    previous := source[1]
float conditional = na
if bar_index % 2 == 0
    conditional := carry(close)
plot(previous)
plot(source)
plot(conditional)
"#,
    );
    let input: Vec<_> = (0..6)
        .map(|index| bar(index, 10.0 + index as f64))
        .collect();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&input).unwrap();
    let result = runtime.result();
    assert_eq!(
        result.plots[0].values,
        [
            PineValue::Na,
            PineValue::Float(12.0),
            PineValue::Float(13.0),
            PineValue::Float(14.0),
            PineValue::Float(15.0),
            PineValue::Float(16.0)
        ]
    );
    assert_eq!(
        result.plots[1].values,
        [
            PineValue::Float(12.0),
            PineValue::Float(13.0),
            PineValue::Float(14.0),
            PineValue::Float(15.0),
            PineValue::Float(16.0),
            PineValue::Float(17.0)
        ]
    );
    assert_eq!(
        result.plots[2].values,
        [
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(10.0),
            PineValue::Na,
            PineValue::Float(12.0),
            PineValue::Na
        ]
    );
}

#[test]
fn requested_history_still_commits_its_retained_source() {
    let hir = program(
        "//@version=6\nindicator(\"request history\")\nplot(request.security(\"B\", \"1\", close[1]))\n",
    );
    let timeframe = RequestTimeframe::parse("1").unwrap();
    let requested: Vec<_> = (0..6)
        .map(|index| bar(index, 100.0 + index as f64))
        .collect();
    let provider = InMemoryRequestDataProvider::from_streams([(
        RequestKey::new("B", timeframe.clone()),
        requested,
    )])
    .unwrap();
    let environment =
        RequestEnvironment::new(ChartContext::new("A", timeframe), Arc::new(provider));
    let mut runtime = HistoricalRuntime::with_request_environment(&hir, environment);
    runtime
        .append_bars(
            &(0..6)
                .map(|index| bar(index, 10.0 + index as f64))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values,
        [
            PineValue::Na,
            PineValue::Float(100.0),
            PineValue::Float(101.0),
            PineValue::Float(102.0),
            PineValue::Float(103.0),
            PineValue::Float(104.0)
        ]
    );
}

#[test]
fn zero_depth_collection_roots_survive_gc_and_varip_forming_updates() {
    let hir = program(
        r#"//@version=6
indicator("collection roots")
varip array<int> saved = array.new<int>()
if barstate.isnew
    array.clear(saved)
array.push(saved, int(close))
plot(array.size(saved))
plot(array.get(saved, array.size(saved) - 1))
temporary = array.new<int>(16, int(close))
"#,
    );
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&(0..2050).map(|index| bar(index, 10.0)).collect::<Vec<_>>())
        .unwrap();
    runtime.collection_gc_next_id = 0;
    runtime.collect_temporary_collections();
    assert_eq!(runtime.array_store.len(), 2);
    assert_eq!(
        runtime.result().plots[1].values.last(),
        Some(&PineValue::Int(10))
    );
    assert_eq!(runtime.profile().series_values, 0);
    assert_eq!(runtime.profile().current_series_capacity, 0);

    let mut live = RealtimeRuntime::new(&hir);
    live.seed_historical(&[bar(0, 10.0)]).unwrap();
    for (close, count) in [(11.0, 1), (12.0, 2), (13.0, 3)] {
        let result = live.update(BarUpdate::forming(bar(1, close))).unwrap();
        assert_eq!(result.plots[0].values.last(), Some(&PineValue::Int(count)));
        assert_eq!(
            result.plots[1].values.last(),
            Some(&PineValue::Int(close as i64))
        );
    }
}
