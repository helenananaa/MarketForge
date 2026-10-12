use pine_ir::HirProgram;
use pine_runtime::{
    Bar, BarUpdate, PineValue, RealtimeRuntime, RuntimeResult, public_runtime_result_json,
    run_historical,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[derive(Clone, Debug)]
enum Key {
    Int(i64),
    Float(f64),
    Text(&'static str),
}

impl Key {
    fn equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::Float(left), Self::Float(right)) => left == right,
            (Self::Text(left), Self::Text(right)) => left == right,
            _ => false,
        }
    }

    fn number(&self) -> f64 {
        match self {
            Self::Int(value) => *value as f64,
            Self::Float(value) => *value,
            Self::Text("beta") => 2.0,
            Self::Text("alpha") => 1.0,
            Self::Text("界") => 3.0,
            Self::Text(value) => panic!("unexpected text key {value}"),
        }
    }
}

// This ordered Vec oracle has no hash index or tombstones. Removal drops an
// entry, so reinsertion appends the new key, including its original zero bits.
#[derive(Clone, Debug, Default)]
struct OrderedMap(Vec<(Key, f64)>);

impl OrderedMap {
    fn put(&mut self, key: Key, value: f64) {
        if let Some((_, previous)) = self.0.iter_mut().find(|(old, _)| old.equals(&key)) {
            *previous = value;
        } else {
            self.0.push((key, value));
        }
    }

    fn remove(&mut self, key: &Key) {
        if let Some(index) = self.0.iter().position(|(old, _)| old.equals(key)) {
            self.0.remove(index);
        }
    }

    fn get(&self, key: &Key) -> Option<f64> {
        self.0
            .iter()
            .find(|(old, _)| old.equals(key))
            .map(|(_, value)| *value)
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    fn consecutive(count: usize) -> Self {
        Self(
            (0..count)
                .map(|key| (Key::Int(key as i64), key as f64))
                .collect(),
        )
    }
}

fn program(source: &str) -> HirProgram {
    let analysis = analyze_source(&SourceFile::new("map-remove-lookup.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("map remove HIR")
}

fn bar(index: usize, close: f64, volume: f64) -> Bar {
    Bar {
        time: index as i64 * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume,
    }
}

fn append(expected: &mut [Vec<Option<f64>>], values: &[Option<f64>]) {
    assert_eq!(expected.len(), values.len());
    for (plot, value) in expected.iter_mut().zip(values) {
        plot.push(*value);
    }
}

fn assert_outputs(result: &RuntimeResult, expected: &[Vec<Option<f64>>]) {
    assert_eq!(result.plots.len(), expected.len());
    for (plot_index, (plot, values)) in result.plots.iter().zip(expected).enumerate() {
        assert_eq!(plot.values.len(), values.len(), "plot {plot_index} length");
        for (index, (actual, expected)) in plot.values.iter().zip(values).enumerate() {
            match (actual, expected) {
                (PineValue::Float(actual), Some(expected)) => assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "plot {plot_index} at {index}"
                ),
                (PineValue::Na, None) => {}
                _ => panic!("plot {plot_index} at {index}: {actual:?}, expected {expected:?}"),
            }
        }
    }
}

#[test]
fn map_remove_public_key_kinds_preserve_reinsert_order_and_zero_bits() {
    let input = [
        bar(0, -0.0, 1.0),
        bar(1, 0.0, 1.0),
        bar(2, -0.0, 1.0),
        bar(3, 0.0, 1.0),
    ];
    let mut floats = OrderedMap::default();
    let mut integers = OrderedMap::default();
    let mut strings = OrderedMap::default();
    let mut expected = vec![Vec::new(); 15];
    for (index, sample) in input.iter().enumerate() {
        if index == 0 {
            for (key, value) in [(sample.close, 10.0), (1.5, 15.0), (-2.25, 22.5)] {
                floats.put(Key::Float(key), value);
            }
            for key in [7, -3, 0] {
                integers.put(Key::Int(key), key as f64);
            }
            for (key, value) in [("beta", 2.0), ("alpha", 1.0), ("界", 3.0)] {
                strings.put(Key::Text(key), value);
            }
        }
        floats.remove(&Key::Float(9.0));
        if index > 0 {
            floats.remove(&Key::Float(-sample.close));
            floats.put(Key::Float(sample.close), 100.0 + index as f64);
        }
        integers.remove(&Key::Int(-3));
        integers.remove(&Key::Int(999));
        integers.put(Key::Int(-3), 200.0 + index as f64);
        strings.remove(&Key::Text("alpha"));
        strings.remove(&Key::Text("missing"));
        strings.put(Key::Text("alpha"), 300.0 + index as f64);
        let mut values = Vec::new();
        for (map, key) in [
            (&floats, Key::Float(sample.close)),
            (&integers, Key::Int(-3)),
            (&strings, Key::Text("alpha")),
        ] {
            values.push(Some(map.0.len() as f64));
            values.extend(map.0.iter().map(|(key, _)| Some(key.number())));
            values.push(map.get(&key));
        }
        append(&mut expected, &values);
    }
    for version in [5, 6] {
        let hir = program(&format!(
            r#"//@version={version}
indicator("map remove key order")
var floats=map.new<float,float>()
var integers=map.new<int,float>()
var strings=map.new<string,float>()
if barstate.isfirst
    map.put(floats,close,10.0)
    map.put(floats,1.5,15.0)
    map.put(floats,-2.25,22.5)
    map.put(integers,7,7.0)
    map.put(integers,-3,-3.0)
    map.put(integers,0,0.0)
    map.put(strings,"beta",2.0)
    map.put(strings,"alpha",1.0)
    map.put(strings,"界",3.0)
map.remove(floats,9.0)
if bar_index>0
    map.remove(floats,-close)
    map.put(floats,close,100.0+bar_index)
map.remove(integers,-3)
map.remove(integers,999)
map.put(integers,-3,200.0+bar_index)
map.remove(strings,"alpha")
map.remove(strings,"missing")
map.put(strings,"alpha",300.0+bar_index)
fk=map.keys(floats)
ik=map.keys(integers)
sk=map.keys(strings)
plot(map.size(floats)*1.0)
plot(array.get(fk,0))
plot(array.get(fk,1))
plot(array.get(fk,2))
plot(map.get(floats,close))
plot(map.size(integers)*1.0)
plot(array.get(ik,0)*1.0)
plot(array.get(ik,1)*1.0)
plot(array.get(ik,2)*1.0)
plot(map.get(integers,-3))
plot(map.size(strings)*1.0)
plot(array.get(sk,0)=="beta"?2.0:array.get(sk,0)=="alpha"?1.0:3.0)
plot(array.get(sk,1)=="beta"?2.0:array.get(sk,1)=="alpha"?1.0:3.0)
plot(array.get(sk,2)=="beta"?2.0:array.get(sk,2)=="alpha"?1.0:3.0)
plot(map.get(strings,"alpha"))
"#,
        ));
        assert_outputs(&run_historical(&hir, &input).unwrap(), &expected);
    }
}

#[test]
fn map_remove_same_site_observes_supported_key_effects_and_fresh_slots() {
    let input = [bar(0, 0.25, 1.0), bar(1, 4.0, 1.0), bar(2, -2.0, 1.0)];
    let mut expected = vec![Vec::new(); 9];
    for sample in &input {
        let mut map = OrderedMap::default();
        let mut sizes = 0;
        let mut counter = 0;
        for call in 0..4 {
            map.clear();
            for key in 0..257 {
                map.put(Key::Int(key), key as f64);
            }
            // Production reaches its tombstone-compaction boundary here; the
            // independent Vec model only removes entries in original order.
            for key in 0..130 {
                map.remove(&Key::Int(key));
            }
            let target = if call % 2 == 0 { 130 } else { 999 };
            counter += 1;
            map.remove(&Key::Int(target));
            sizes += map.0.len();
            map.put(Key::Int(target), sample.close + counter as f64);
        }
        append(
            &mut expected,
            &[
                Some(sizes as f64),
                Some(counter as f64),
                Some(map.0.len() as f64),
                Some(map.0[0].0.number()),
                Some(map.0[1].0.number()),
                Some(map.0.last().unwrap().0.number()),
                map.get(&Key::Int(130)),
                map.get(&Key::Int(999)),
                map.get(&Key::Int(0)),
            ],
        );
    }
    for version in [5, 6] {
        let hir = program(&format!(
            r#"//@version={version}
indicator("map remove argument effects")
key_expr(c,i)=>
    array.set(c,0,array.get(c,0)+1)
    i
var m=map.new<int,float>()
counter=array.new_int(1,0)
sizes=0
for i=0 to 3
    map.clear(m)
    for key=0 to 256
        map.put(m,key,key)
    for key=0 to 129
        map.remove(m,key)
    target=i%2==0?130:999
    map.remove(m,key_expr(counter,target))
    sizes+=map.size(m)
    map.put(m,target,close+array.get(counter,0))
keys=map.keys(m)
plot(sizes*1.0)
plot(array.get(counter,0)*1.0)
plot(map.size(m)*1.0)
plot(array.get(keys,0)*1.0)
plot(array.get(keys,1)*1.0)
plot(array.get(keys,array.size(keys)-1)*1.0)
plot(map.get(m,130))
plot(map.get(m,999))
plot(map.get(m,0))
"#,
        ));
        assert_outputs(&run_historical(&hir, &input).unwrap(), &expected);
    }
}

fn advance_paged(
    plain: &mut OrderedMap,
    sticky: &mut OrderedMap,
    saved: &OrderedMap,
    index: usize,
    close: f64,
) -> Vec<Option<f64>> {
    if index == 1 {
        for key in 128..256 {
            plain.put(Key::Int(key), key as f64);
            sticky.put(Key::Int(key), key as f64);
        }
    }
    let full = plain.clone();
    let tail = if index == 0 { 128 } else { 256 };
    if index <= 1 {
        plain.put(Key::Int(tail), tail as f64);
        sticky.put(Key::Int(tail), tail as f64);
    }
    let boundary = plain.clone();
    for map in [&mut *plain, &mut *sticky] {
        map.remove(&Key::Int(127));
        if index > 0 {
            map.remove(&Key::Int(128));
            map.remove(&Key::Int(256));
        }
        map.remove(&Key::Int(999));
        map.put(Key::Int(127), close);
        map.put(Key::Int(120), map.get(&Key::Int(120)).unwrap() + close);
    }
    let mut live_copy = plain.clone();
    live_copy.remove(&Key::Int(120));
    live_copy.remove(&Key::Int(127));
    live_copy.put(Key::Int(127), -close);
    vec![
        Some(plain.0.len() as f64),
        Some(sticky.0.len() as f64),
        Some(full.0.len() as f64),
        Some(boundary.0.len() as f64),
        Some(saved.0.len() as f64),
        plain.get(&Key::Int(120)),
        sticky.get(&Key::Int(120)),
        saved.get(&Key::Int(120)),
        boundary.get(&Key::Int(127)),
        plain.get(&Key::Int(127)),
        live_copy.get(&Key::Int(120)),
        live_copy.get(&Key::Int(127)),
        Some(plain.0.last().unwrap().0.number()),
        Some(boundary.0.last().unwrap().0.number()),
        plain.get(&Key::Int(256)),
    ]
}

#[test]
fn map_remove_paged_copies_and_varip_follow_realtime_updates() {
    let hir = program(
        r#"//@version=6
indicator("map remove paged snapshots")
var plain=map.new<int,float>()
var alias=plain
if barstate.isfirst
    for key=0 to 127
        map.put(plain,key,key)
var saved=map.copy(plain)
varip sticky=map.copy(plain)
if bar_index==1
    for key=128 to 255
        map.put(alias,key,key)
        map.put(sticky,key,key)
full=map.copy(plain)
tail=bar_index==0?128:256
if bar_index<=1
    map.put(alias,tail,tail)
    map.put(sticky,tail,tail)
boundary=map.copy(plain)
map.remove(alias,127)
map.remove(sticky,127)
if bar_index>0
    map.remove(alias,128)
    map.remove(alias,256)
    map.remove(sticky,128)
    map.remove(sticky,256)
map.remove(alias,999)
map.remove(sticky,999)
map.put(alias,127,close)
map.put(sticky,127,close)
map.put(alias,120,map.get(alias,120)+close)
map.put(sticky,120,map.get(sticky,120)+close)
live_copy=map.copy(plain)
map.remove(live_copy,120)
map.remove(live_copy,127)
map.put(live_copy,127,-close)
keys=map.keys(plain)
boundary_keys=map.keys(boundary)
plot(map.size(plain)*1.0)
plot(map.size(sticky)*1.0)
plot(map.size(full)*1.0)
plot(map.size(boundary)*1.0)
plot(map.size(saved)*1.0)
plot(map.get(plain,120))
plot(map.get(sticky,120))
plot(map.get(saved,120))
plot(map.get(boundary,127))
plot(map.get(plain,127))
plot(map.get(live_copy,120))
plot(map.get(live_copy,127))
plot(array.get(keys,array.size(keys)-1)*1.0)
plot(array.get(boundary_keys,array.size(boundary_keys)-1)*1.0)
plot(map.get(plain,256))
"#,
    );
    let saved = OrderedMap::consecutive(128);
    let mut plain = saved.clone();
    let mut sticky = saved.clone();
    let mut expected = vec![Vec::new(); 15];
    append(
        &mut expected,
        &advance_paged(&mut plain, &mut sticky, &saved, 0, 1.0),
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.seed_historical(&[bar(0, 1.0, 1.0)]).unwrap();
    assert_outputs(&runtime.result(), &expected);
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    for close in [2.0, 3.0] {
        let mut branch = plain.clone();
        let mut visible = expected.clone();
        append(
            &mut visible,
            &advance_paged(&mut branch, &mut sticky, &saved, 1, close),
        );
        runtime
            .apply_update(BarUpdate::forming(bar(1, close, 1.0)))
            .unwrap();
        assert_outputs(&runtime.result(), &visible);
        assert_eq!(runtime.confirmed_bar_count(), 1);
        assert_eq!(
            public_runtime_result_json(&runtime.confirmed_result()),
            confirmed
        );
    }
    append(
        &mut expected,
        &advance_paged(&mut plain, &mut sticky, &saved, 1, 4.0),
    );
    runtime
        .apply_update(BarUpdate::confirmed(bar(1, 4.0, 1.0)))
        .unwrap();
    assert_outputs(&runtime.result(), &expected);
    for (index, close) in [(2, 5.0), (3, -2.0)] {
        append(
            &mut expected,
            &advance_paged(&mut plain, &mut sticky, &saved, index, close),
        );
        runtime
            .update_without_output(BarUpdate::confirmed(bar(index, close, 1.0)))
            .unwrap();
        assert_outputs(&runtime.result(), &expected);
    }
    assert_outputs(&runtime.confirmed_result(), &expected);
    assert_eq!(runtime.confirmed_bar_count(), 4);
}

fn advance_atomic(map: &mut OrderedMap, close: f64) -> [Option<f64>; 6] {
    map.remove(&Key::Int(130));
    map.remove(&Key::Int(999));
    map.put(Key::Int(130), close);
    map.put(Key::Int(0), map.get(&Key::Int(0)).unwrap() + close);
    [
        Some(map.0.len() as f64),
        map.get(&Key::Int(130)),
        map.get(&Key::Int(0)),
        map.get(&Key::Int(129)),
        Some(map.0.last().unwrap().0.number()),
        map.get(&Key::Int(999)),
    ]
}

#[test]
fn map_remove_failed_forming_updates_and_future_commits_are_atomic() {
    let hir = program(
        r#"//@version=6
indicator("map remove rollback",max_bars_back=0)
var m=map.new<int,float>()
if barstate.isfirst
    for key=0 to 256
        map.put(m,key,key)
map.remove(m,130)
map.remove(m,999)
map.put(m,130,close)
map.put(m,0,map.get(m,0)+close)
if volume>10
    for key=0 to 129
        map.remove(m,key)
    runtime.error("map remove compaction rollback")
if volume<0
    runtime.error("map remove rollback")
keys=map.keys(m)
plot(map.size(m)*1.0)
plot(map.get(m,130))
plot(map.get(m,0))
plot(map.get(m,129))
plot(array.get(keys,array.size(keys)-1)*1.0)
plot(map.get(m,999))
"#,
    );
    let mut map = OrderedMap::consecutive(257);
    let mut expected = vec![Vec::new(); 6];
    append(&mut expected, &advance_atomic(&mut map, 1.0));
    let mut input = vec![bar(0, 1.0, 1.0)];
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.seed_historical(&input).unwrap();
    assert_outputs(&runtime.result(), &expected);
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    for close in [2.0, 3.0] {
        let mut branch = map.clone();
        let mut visible = expected.clone();
        append(&mut visible, &advance_atomic(&mut branch, close));
        runtime
            .update_without_output(BarUpdate::forming(bar(1, close, 1.0)))
            .unwrap();
        assert_outputs(&runtime.result(), &visible);
        assert_eq!(
            public_runtime_result_json(&runtime.confirmed_result()),
            confirmed
        );
    }
    // Both errors occur after removal and reinsertion. The first also reaches
    // compaction after removing a prefix, before the transaction is rejected.
    for (close, volume, message) in [
        (9.0, 11.0, "map remove compaction rollback"),
        (7.0, -1.0, "map remove rollback"),
    ] {
        let before = public_runtime_result_json(&runtime.result());
        let profile = runtime.profile();
        let confirmed_profile = runtime.confirmed_profile();
        let revision = runtime.revision();
        let changes = runtime.last_changes().cloned();
        let error = runtime
            .apply_update(BarUpdate::forming(bar(1, close, volume)))
            .unwrap_err();
        assert_eq!(error.message, message);
        assert_eq!(runtime.profile(), profile);
        assert_eq!(runtime.confirmed_profile(), confirmed_profile);
        assert_eq!(runtime.revision(), revision);
        assert_eq!(runtime.last_changes(), changes.as_ref());
        assert_eq!(runtime.confirmed_bar_count(), 1);
        assert_eq!(public_runtime_result_json(&runtime.result()), before);
        assert_eq!(
            public_runtime_result_json(&runtime.confirmed_result()),
            confirmed
        );
    }
    let sample = bar(1, 4.0, 1.0);
    let mut branch = map.clone();
    let mut visible = expected.clone();
    append(&mut visible, &advance_atomic(&mut branch, sample.close));
    runtime.apply_update(BarUpdate::forming(sample)).unwrap();
    assert_outputs(&runtime.result(), &visible);
    append(&mut expected, &advance_atomic(&mut map, sample.close));
    input.push(sample);
    runtime.apply_update(BarUpdate::confirmed(sample)).unwrap();
    assert_outputs(&runtime.result(), &expected);
    for (index, close) in [(2, -2.0), (3, 0.5)] {
        let sample = bar(index, close, 1.0);
        input.push(sample);
        append(&mut expected, &advance_atomic(&mut map, close));
        runtime
            .update_without_output(BarUpdate::confirmed(sample))
            .unwrap();
        assert_outputs(&runtime.result(), &expected);
    }
    assert_outputs(&runtime.confirmed_result(), &expected);
    assert_eq!(runtime.confirmed_bar_count(), input.len());
    assert_eq!(
        public_runtime_result_json(&runtime.result()),
        public_runtime_result_json(&run_historical(&hir, &input).unwrap())
    );
}
