//! Public collection semantics across directory branches and realtime rollback.
use pine_ir::HirProgram;
use pine_runtime::{
    Bar, BarUpdate, PineValue, RealtimeRuntime, RuntimeResult, public_runtime_result_json,
    run_historical,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program(version: u8, source: &str) -> HirProgram {
    let source = format!("//@version={version}\n{source}");
    let analysis = analyze_source(&SourceFile::new("paged_directory.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(time: i64, close: f64, volume: f64) -> Bar {
    Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume,
    }
}

const SOURCE: &str = r#"indicator("paged directory", max_bars_back=0)
var values=array.new_float(10000,0.0)
var cells=matrix.new<float>(100,100,0.0)
var mapping=map.new<int,float>()
if barstate.isfirst
    for key=0 to 9999
        map.put(mapping,key,0.0)
index=int(close)
previous=matrix.copy(cells)
alias=values
array.set(alias,index,close/4.0)
if index==4095
    matrix.set(cells,40,95,close/8.0)
else if index==4096
    matrix.set(cells,40,96,close/8.0)
else if index==8191
    matrix.set(cells,81,91,close/8.0)
else if index==8192
    matrix.set(cells,81,92,close/8.0)
else if index==127
    matrix.set(cells,1,27,close/8.0)
else if index==128
    matrix.set(cells,1,28,close/8.0)
else if index==32
    matrix.set(cells,0,32,close/8.0)
else if index==9999
    matrix.set(cells,99,99,close/8.0)
map.remove(mapping,index)
map.put(mapping,index,close/2.0)
if volume<0
    runtime.error("directory branch rollback")
plot(array.sum(values))
plot(matrix.sum(cells))
plot(matrix.sum(previous))
plot(array.sum(map.values(mapping)))
plot(array.get(map.keys(mapping),9999)*1.0)
plot(array.get(values,4095)+array.get(values,4096)+array.get(values,8191)+array.get(values,8192))
"#;

#[derive(Clone)]
struct Model {
    values: Vec<f64>,
    cells: Vec<f64>,
    mapping: Vec<(usize, f64)>,
}

impl Model {
    fn new() -> Self {
        Self {
            values: vec![0.0; 10000],
            cells: vec![0.0; 10000],
            mapping: (0..10000).map(|key| (key, 0.0)).collect(),
        }
    }
    fn apply(&mut self, close: f64) -> [f64; 6] {
        let index = close as usize;
        let previous = self.cells.iter().sum();
        self.values[index] = close / 4.0;
        self.cells[index] = close / 8.0;
        self.mapping.retain(|(key, _)| *key != index);
        self.mapping.push((index, close / 2.0));
        [
            self.values.iter().sum(),
            self.cells.iter().sum(),
            previous,
            self.mapping.iter().map(|(_, value)| value).sum(),
            index as f64,
            [4095, 4096, 8191, 8192]
                .iter()
                .map(|index| self.values[*index])
                .sum(),
        ]
    }
}

fn assert_last(result: &RuntimeResult, expected: &[f64]) {
    assert_eq!(result.plots.len(), expected.len());
    for (plot, expected) in result.plots.iter().zip(expected) {
        let Some(PineValue::Float(actual)) = plot.values.last() else {
            panic!("expected Float: {:?}", plot.values.last())
        };
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[test]
fn array_map_matrix_directory_branches_preserve_realtime_and_ordered_outputs() {
    for version in [5, 6] {
        let program = program(version, SOURCE);
        let mut runtime = RealtimeRuntime::new(&program);
        let mut model = Model::new();
        let mut bars = Vec::new();
        for (time, close) in [4095.0, 4096.0, 8191.0, 8192.0].into_iter().enumerate() {
            let input = bar(time as i64, close, 1.0);
            bars.push(input);
            let expected = model.apply(close);
            runtime.apply_update(BarUpdate::historical(input)).unwrap();
            assert_last(&runtime.result(), &expected);
        }
        let confirmed = public_runtime_result_json(&runtime.confirmed_result());
        for close in [127.0, 128.0] {
            let expected = model.clone().apply(close);
            runtime
                .apply_update(BarUpdate::forming(bar(4, close, 1.0)))
                .unwrap();
            assert_last(&runtime.result(), &expected);
            assert_eq!(
                public_runtime_result_json(&runtime.confirmed_result()),
                confirmed
            );
        }
        let before = public_runtime_result_json(&runtime.result());
        let profile = runtime.profile();
        let revision = runtime.revision();
        let last_changes = runtime.last_changes().cloned();
        let error = runtime
            .apply_update(BarUpdate::forming(bar(4, 32.0, -1.0)))
            .unwrap_err();
        assert_eq!(error.message, "directory branch rollback");
        assert_eq!(public_runtime_result_json(&runtime.result()), before);
        assert_eq!(
            public_runtime_result_json(&runtime.confirmed_result()),
            confirmed
        );
        assert_eq!(runtime.profile(), profile);
        assert_eq!(runtime.revision(), revision);
        assert_eq!(runtime.last_changes(), last_changes.as_ref());
        for (time, close) in [(4, 9999.0), (5, 4095.0), (6, 4096.0)] {
            let input = bar(time, close, 1.0);
            let expected = model.apply(close);
            runtime.apply_update(BarUpdate::confirmed(input)).unwrap();
            bars.push(input);
            assert_last(&runtime.result(), &expected);
        }
        let batch = run_historical(&program, &bars).unwrap();
        assert_eq!(
            public_runtime_result_json(&runtime.result()),
            public_runtime_result_json(&batch)
        );
    }
}

#[test]
fn large_varip_collections_follow_scalar_intrabar_persistence_after_failed_updates() {
    let source = r#"indicator("varip directory", max_bars_back=0)
varip scalar=0.0
varip values=array.new_float(6000,0.0)
varip cells=matrix.new<float>(60,100,0.0)
varip mapping=map.new<int,float>()
if barstate.isfirst
    for key=0 to 5999
        map.put(mapping,key,0.0)
scalar+=1.0
array.set(values,4096,array.get(values,4096)+1.0)
matrix.set(cells,40,96,matrix.get(cells,40,96)+1.0)
map.put(mapping,4096,map.get(mapping,4096)+1.0)
if volume<0
    runtime.error("varip directory rollback")
plot(array.get(values,4096)-scalar)
plot(matrix.get(cells,40,96)-scalar)
plot(map.get(mapping,4096)-scalar)
"#;
    let program = program(6, source);
    let mut runtime = RealtimeRuntime::new(&program);
    runtime
        .apply_update(BarUpdate::historical(bar(0, 1.0, 1.0)))
        .unwrap();
    for time in 1..4 {
        for close in [2.0, 3.0] {
            runtime
                .apply_update(BarUpdate::forming(bar(time, close, 1.0)))
                .unwrap();
            assert_last(&runtime.result(), &[0.0; 3]);
        }
        let before = public_runtime_result_json(&runtime.result());
        let profile = runtime.profile();
        assert_eq!(
            runtime
                .apply_update(BarUpdate::forming(bar(time, 7.0, -1.0)))
                .unwrap_err()
                .message,
            "varip directory rollback"
        );
        assert_eq!(public_runtime_result_json(&runtime.result()), before);
        assert_eq!(runtime.profile(), profile);
        runtime
            .apply_update(BarUpdate::confirmed(bar(time, 4.0, 1.0)))
            .unwrap();
        assert_last(&runtime.result(), &[0.0; 3]);
    }
}
