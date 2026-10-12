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

// An independent ordered list: overwrites retain the first key and its slot.
// Numeric equality deliberately treats the two float zero signs as one key.
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
    let analysis = analyze_source(&SourceFile::new("map-put-lookup.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("map put HIR")
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
fn map_put_public_key_kinds_preserve_order_and_original_zero_bits() {
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
        floats.put(Key::Float(sample.close), 100.0 + index as f64);
        integers.put(Key::Int(-3), 200.0 + index as f64);
        strings.put(Key::Text("alpha"), 300.0 + index as f64);
        let mut values = Vec::new();
        for (map, updated_slot) in [(&floats, 0), (&integers, 1), (&strings, 1)] {
            values.push(Some(map.0.len() as f64));
            values.extend(map.0.iter().map(|(key, _)| Some(key.number())));
            values.push(Some(map.0[updated_slot].1));
        }
        append(&mut expected, &values);
    }
    for version in [5, 6] {
        let hir = program(&format!(
            r#"//@version={version}
indicator("map key order")
var floats = map.new<float,float>()
var integers = map.new<int,float>()
var strings = map.new<string,float>()
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
map.put(floats,close,100.0+bar_index)
map.put(integers,-3,200.0+bar_index)
map.put(strings,"alpha",300.0+bar_index)
fk=map.keys(floats)
ik=map.keys(integers)
sk=map.keys(strings)
plot(map.size(floats)*1.0)
plot(array.get(fk,0))
plot(array.get(fk,1))
plot(array.get(fk,2))
plot(array.get(map.values(floats),0))
plot(map.size(integers)*1.0)
plot(array.get(ik,0)*1.0)
plot(array.get(ik,1)*1.0)
plot(array.get(ik,2)*1.0)
plot(array.get(map.values(integers),1))
plot(map.size(strings)*1.0)
plot(array.get(sk,0)=="beta"?2.0:0.0)
plot(array.get(sk,1)=="alpha"?1.0:0.0)
plot(array.get(sk,2)=="界"?3.0:0.0)
plot(array.get(map.values(strings),1))
"#,
        ));
        assert_outputs(&run_historical(&hir, &input).unwrap(), &expected);
    }
}

#[test]
fn map_put_same_site_locates_after_key_and_value_side_effects() {
    let input = [bar(0, 0.25, 1.0), bar(1, 4.0, 1.0), bar(2, -2.0, 1.0)];
    let mut expected = vec![Vec::new(); 10];
    for sample in &input {
        let mut map = OrderedMap::default();
        let mut total = 0.0;
        let mut counter = 0;
        for call in 0..4 {
            // Supported top-level mutations alternate a target in slot two
            // with an absent target that must append into slot three.
            map.clear();
            map.put(Key::Int(40), 40.0);
            map.put(Key::Int(50), 50.0);
            if call % 2 == 0 {
                map.put(Key::Int(call), 20.0);
            } else {
                map.put(Key::Int(30), 30.0);
            }
            // Key and value UDFs mutate an array counter, not the map: the
            // current analyzer rejects Map mutators inside UDF bodies.
            counter += 1;
            let seen = counter;
            counter += 10;
            map.put(Key::Int(call), sample.close + seen as f64);
            total += map.get(&Key::Int(call)).unwrap();
        }
        append(
            &mut expected,
            &[
                Some(total),
                Some(map.0.len() as f64),
                Some(map.0[0].0.number()),
                Some(map.0[1].0.number()),
                Some(map.0[2].0.number()),
                Some(map.0[3].0.number()),
                Some(map.0[1].1),
                Some(map.0[2].1),
                Some(map.0[3].1),
                Some(counter as f64),
            ],
        );
    }
    for version in [5, 6] {
        let hir = program(&format!(
            r#"//@version={version}
indicator("map argument mutation")
key_expr(c,i)=>
    array.set(c,0,array.get(c,0)+1)
    i
value_expr(c,x)=>
    seen=array.get(c,0)
    array.set(c,0,seen+10)
    x+seen
var m=map.new<int,float>()
counter=array.new_int(1,0)
total=0.0
for i=0 to 3
    map.clear(m)
    map.put(m,40,40.0)
    map.put(m,50,50.0)
    if i%2==0
        map.put(m,i,20.0)
    else
        map.put(m,30,30.0)
    map.put(m,key_expr(counter,i),value_expr(counter,close))
    total+=map.get(m,i)
keys=map.keys(m)
values=map.values(m)
plot(total)
plot(map.size(m)*1.0)
plot(array.get(keys,0)*1.0)
plot(array.get(keys,1)*1.0)
plot(array.get(keys,2)*1.0)
plot(array.get(keys,3)*1.0)
plot(array.get(values,1))
plot(array.get(values,2))
plot(array.get(values,3))
plot(array.get(counter,0)*1.0)
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
    let boundary = plain.clone();
    let key = if index == 0 { 128 } else { 255 + index as i64 };
    let value = if index == 0 { 128.0 } else { close };
    plain.put(Key::Int(key), value);
    sticky.put(Key::Int(key), value);
    plain.put(Key::Int(127), plain.get(&Key::Int(127)).unwrap() + close);
    sticky.put(Key::Int(127), sticky.get(&Key::Int(127)).unwrap() + close);
    let mut live_copy = plain.clone();
    live_copy.put(Key::Int(127), -close);
    live_copy.put(Key::Int(key), -close * 2.0);
    vec![
        Some(plain.0.len() as f64),
        Some(sticky.0.len() as f64),
        Some(boundary.0.len() as f64),
        Some(saved.0.len() as f64),
        plain.get(&Key::Int(127)),
        sticky.get(&Key::Int(127)),
        boundary.get(&Key::Int(127)),
        saved.get(&Key::Int(127)),
        live_copy.get(&Key::Int(127)),
        plain.get(&Key::Int(key)),
        Some(plain.0.last().unwrap().0.number()),
        Some(boundary.0.last().unwrap().0.number()),
        saved.get(&Key::Int(128)),
        live_copy.get(&Key::Int(key)),
    ]
}

#[test]
fn map_put_paged_copies_and_varip_follow_realtime_updates() {
    let hir = program(
        r#"//@version=6
indicator("map paged snapshots")
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
boundary=map.copy(plain)
key=bar_index==0?128:255+bar_index
value=bar_index==0?128.0:close
map.put(alias,key,value)
map.put(sticky,key,value)
map.put(alias,127,map.get(alias,127)+close)
map.put(sticky,127,map.get(sticky,127)+close)
live_copy=map.copy(plain)
map.put(live_copy,127,-close)
map.put(live_copy,key,-close*2.0)
keys=map.keys(plain)
boundary_keys=map.keys(boundary)
plot(map.size(plain)*1.0)
plot(map.size(sticky)*1.0)
plot(map.size(boundary)*1.0)
plot(map.size(saved)*1.0)
plot(map.get(plain,127))
plot(map.get(sticky,127))
plot(map.get(boundary,127))
plot(map.get(saved,127))
plot(map.get(live_copy,127))
plot(map.get(plain,key))
plot(array.get(keys,array.size(keys)-1)*1.0)
plot(array.get(boundary_keys,array.size(boundary_keys)-1)*1.0)
plot(map.get(saved,128))
plot(map.get(live_copy,key))
"#,
    );
    let saved = OrderedMap::consecutive(128);
    let mut plain = saved.clone();
    let mut sticky = saved.clone();
    let mut expected = vec![Vec::new(); 14];
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
            .apply_update(BarUpdate::confirmed(bar(index, close, 1.0)))
            .unwrap();
        assert_outputs(&runtime.result(), &expected);
    }
    assert_outputs(&runtime.confirmed_result(), &expected);
    assert_eq!(runtime.confirmed_bar_count(), 4);
}

fn advance_full(map: &mut OrderedMap, close: f64) -> [Option<f64>; 4] {
    map.put(Key::Int(0), map.get(&Key::Int(0)).unwrap() + close);
    map.put(
        Key::Int(49_999),
        map.get(&Key::Int(49_999)).unwrap() + close,
    );
    [
        Some(map.0.len() as f64),
        map.get(&Key::Int(49_999)),
        map.get(&Key::Int(0)),
        Some(if map.get(&Key::Int(50_000)).is_some() {
            1.0
        } else {
            0.0
        }),
    ]
}

#[test]
fn map_put_full_capacity_failures_and_future_commits_are_atomic() {
    let hir = program(
        r#"//@version=6
indicator("map capacity rollback",max_bars_back=0)
var m=map.new<int,float>()
if barstate.isfirst
    for key=0 to 49999
        map.put(m,key,key)
map.put(m,0,map.get(m,0)+close)
map.put(m,volume>10?50000:49999,map.get(m,49999)+close)
if volume<0
    runtime.error("map put rollback")
plot(map.size(m)*1.0)
plot(map.get(m,49999))
plot(map.get(m,0))
plot(map.contains(m,50000)?1.0:0.0)
"#,
    );
    // Initial unique entries are built directly, rather than performing an
    // artificial quadratic sequence of list lookups for this capacity case.
    let mut map = OrderedMap::consecutive(50_000);
    let mut expected = vec![Vec::new(); 4];
    append(&mut expected, &advance_full(&mut map, 1.0));
    let mut input = vec![bar(0, 1.0, 1.0)];
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.seed_historical(&input).unwrap();
    assert_outputs(&runtime.result(), &expected);
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    for close in [2.0, 3.0] {
        let mut branch = map.clone();
        let mut visible = expected.clone();
        append(&mut visible, &advance_full(&mut branch, close));
        runtime
            .update_without_output(BarUpdate::forming(bar(1, close, 1.0)))
            .unwrap();
        assert_outputs(&runtime.result(), &visible);
        assert_eq!(runtime.confirmed_bar_count(), 1);
        assert_eq!(
            public_runtime_result_json(&runtime.confirmed_result()),
            confirmed
        );
    }
    for (close, volume, message) in [
        (9.0, 11.0, "map.put cannot exceed 50000 key-value pairs"),
        (7.0, -1.0, "map put rollback"),
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
    append(&mut visible, &advance_full(&mut branch, sample.close));
    runtime.apply_update(BarUpdate::forming(sample)).unwrap();
    assert_outputs(&runtime.result(), &visible);
    append(&mut expected, &advance_full(&mut map, sample.close));
    input.push(sample);
    runtime.apply_update(BarUpdate::confirmed(sample)).unwrap();
    assert_outputs(&runtime.result(), &expected);
    for (index, close) in [(2, -2.0), (3, 0.5)] {
        let sample = bar(index, close, 1.0);
        input.push(sample);
        append(&mut expected, &advance_full(&mut map, close));
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
