use super::*;
use crate::{Bar, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program(body: &str) -> pine_ir::HirProgram {
    let source = format!("//@version=6\nindicator(\"map capacity\", max_bars_back=0)\n{body}");
    let analysis = analyze_source(&SourceFile::new("map_capacity.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(time: i64) -> Bar {
    Bar {
        time,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    }
}

#[test]
fn map_capacity_allows_existing_key_overwrites_and_rejects_one_more_pair() {
    let program = program(
        r#"var target = map.new<int, int>()
if barstate.isfirst
    for key = 0 to 49999
        map.put(target, key, key)
map.put(target, 0, -1)
if bar_index > 0
    map.put(target, 50000, -2)
plot(map.size(target))
plot(map.get(target, 0))
"#,
    );
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bar(bar(0)).unwrap();
    let result = runtime.result();
    assert_eq!(result.plots[0].values, [PineValue::Int(50000)]);
    assert_eq!(result.plots[1].values, [PineValue::Int(-1)]);
    let error = runtime.append_bar(bar(60000)).unwrap_err();
    assert_eq!(error.message, "map.put cannot exceed 50000 key-value pairs");
    let target = runtime.map_store.get(&0).unwrap();
    assert_eq!(target.entries.len(), MAX_MAP_ENTRIES);
    assert_eq!(target.entries.get(&PineValue::Int(50000)), None);
}

#[test]
fn oversized_put_all_does_not_apply_earlier_overwrites_or_change_order() {
    let program = program(
        r#"var target = map.new<int, int>()
var source = map.new<int, int>()
if barstate.isfirst
    for key = 0 to 49999
        map.put(target, key, key)
    map.put(source, 0, -1)
    map.put(source, 50000, -2)
if bar_index > 0
    map.put_all(target, source)
plot(map.size(target))
"#,
    );
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bar(bar(0)).unwrap();
    let checkpoint = runtime.clone();
    let error = runtime.append_bar(bar(60000)).unwrap_err();
    assert_eq!(
        error.message,
        "map.put_all cannot exceed 50000 key-value pairs"
    );
    for runtime in [&runtime, &checkpoint] {
        let target = &runtime.map_store.get(&0).unwrap().entries;
        assert_eq!(target.len(), MAX_MAP_ENTRIES);
        assert_eq!(target.get(&PineValue::Int(0)), Some(&PineValue::Int(0)));
        assert_eq!(target.get(&PineValue::Int(50000)), None);
        assert!(
            target.iter().enumerate().all(|(index, (key, value))| {
                key == &PineValue::Int(index as i64) && value == key
            })
        );
    }
}

#[test]
fn full_capacity_put_all_and_self_merge_allow_overwrites_in_existing_order() {
    let program = program(
        r#"target = map.new<int, int>()
source = map.new<int, int>()
for key = 0 to 49999
    map.put(target, key, key)
map.put(source, 3, -3)
map.put(source, 0, -1)
map.put_all(target, source)
map.put_all(target, target)
keys = map.keys(target)
plot(map.size(target))
plot(map.get(target, 0))
plot(map.get(target, 3))
plot(array.get(keys, 0))
plot(array.get(keys, 3))
"#,
    );
    let result = run_historical(&program, &[bar(0)]).unwrap();
    for (plot, expected) in result.plots.iter().zip([50000, -1, -3, 0, 3]) {
        assert_eq!(plot.values, [PineValue::Int(expected)]);
    }
}

#[test]
fn put_all_preflight_counts_only_new_keys_at_the_remaining_capacity_boundary() {
    let mut target: MapEntries = (0..(MAX_MAP_ENTRIES as i64 - 1))
        .map(|key| (PineValue::Int(key), PineValue::Int(key)))
        .collect::<Vec<_>>()
        .into();
    let source: MapEntries = vec![
        (PineValue::Int(0), PineValue::Int(-1)),
        (PineValue::Int(50000), PineValue::Int(-2)),
    ]
    .into();
    let program = program("plot(close)\n");
    let mut runtime = HistoricalRuntime::new(&program);
    for (id, entries) in [(0, target.clone()), (1, source)] {
        runtime.map_store.insert(
            id,
            MapStorage {
                key_kind: ArrayElementKind::Int,
                value_kind: ArrayElementKind::Int,
                entries,
            },
        );
    }
    assert!(runtime.validate_map_merge(0, 1).unwrap());
    target.put(PineValue::Int(49999), PineValue::Int(49999));
    runtime.map_store.get_mut(&0).unwrap().entries = target;
    assert!(runtime.validate_map_merge(0, 1).is_err());
}
