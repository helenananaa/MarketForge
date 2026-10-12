use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::{AnalysisInput, analyze_input};
use pine_syntax::SourceFile;

fn compile(root: &str, libraries: &[(&str, &str)]) -> pine_ir::HirProgram {
    let input = AnalysisInput::with_library_sources(
        SourceFile::new("root.pine", format!("//@version=6\n{root}")),
        libraries
            .iter()
            .map(|(k, s)| {
                (
                    k.to_string(),
                    SourceFile::new(*k, format!("//@version=6\n{s}")),
                )
            })
            .collect(),
    )
    .unwrap();
    let analysis = analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bars() -> Vec<Bar> {
    [1.0, 2.0, 3.0]
        .into_iter()
        .enumerate()
        .map(|(i, close)| Bar {
            time: i as i64 * 60_000,
            open: close,
            high: 10.0,
            low: 0.0,
            close,
            volume: 1.0,
        })
        .collect()
}

#[test]
fn repeated_root_aliases_keep_nested_callsite_state_independent() {
    let inner = "library(\"inner\")\nexport accumulate(float x) =>\n    var float total=0\n    total += x\n    total\n";
    let outer = "library(\"outer\")\nimport test/inner/1 as inner\nexport value(float x) => inner.accumulate(x)\n";
    let root = "import test/outer/1 as first\nimport test/outer/1 as second\nindicator(\"nested\")\nplot(first.value(close))\nplot(second.value(10))\n";
    let program = compile(root, &[("test/outer/1", outer), ("test/inner/1", inner)]);
    let input = bars();
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bars(&input).unwrap();
    let output = public_runtime_result_json(&runtime.result());
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["plots"][0]["values"], serde_json::json!([1, 3, 6]));
    assert_eq!(value["plots"][1]["values"], serde_json::json!([10, 20, 30]));
    let mut incremental = HistoricalRuntime::new(&program);
    for bar in &input {
        incremental.append_bar(*bar).unwrap();
    }
    assert_eq!(public_runtime_result_json(&incremental.result()), output);
    let mut realtime = RealtimeRuntime::new(&program);
    realtime.seed_historical(&input[..1]).unwrap();
    for close in [8.0, 2.0] {
        let mut forming = input[1];
        forming.close = close;
        realtime.update(BarUpdate::forming(forming)).unwrap();
    }
    realtime.update(BarUpdate::confirmed(input[1])).unwrap();
    let final_result = realtime.update(BarUpdate::confirmed(input[2])).unwrap();
    assert_eq!(public_runtime_result_json(&final_result), output);
}

#[test]
fn caller_alias_cannot_capture_builtin_calls_inside_library() {
    let lib = "library(\"averages\")\nexport sma(float x, int length) => x+100\nexport original(float x) => ta.sma(x,2)\n";
    let root = "import test/averages/1 as ta\nindicator(\"alias\")\nplot(ta.sma(close,2))\nplot(ta.original(close))\n";
    let program = compile(root, &[("test/averages/1", lib)]);
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bars(&bars()).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    assert_eq!(
        result["plots"][0]["values"],
        serde_json::json!([101, 102, 103])
    );
    assert_eq!(
        result["plots"][1]["values"],
        serde_json::json!([null, 1.5, 2.5])
    );
}

#[test]
fn diamond_dependency_reuses_source_without_sharing_runtime_callsites() {
    let root = "import test/a/1 as a\nindicator(\"diamond\")\nplot(a.value(close))\n";
    let a = "library(\"a\")\nimport test/b/1 as b\nimport test/c/1 as c\nexport value(float x) => b.value(x)+c.value(x)\n";
    let b = "library(\"b\")\nimport test/d/1 as d\nexport value(float x) => d.sum(x)\n";
    let c = "library(\"c\")\nimport test/d/1 as d\nexport value(float x) => d.sum(x*2)\n";
    let d =
        "library(\"d\")\nexport sum(float x) =>\n    var float total=0\n    total+=x\n    total\n";
    let program = compile(
        root,
        &[
            ("test/a/1", a),
            ("test/b/1", b),
            ("test/c/1", c),
            ("test/d/1", d),
        ],
    );
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bars(&bars()).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    assert_eq!(result["plots"][0]["values"], serde_json::json!([3, 9, 18]));
}

#[test]
fn method_body_uses_its_library_import_scope() {
    let root = "import test/outer/1 as outer\nindicator(\"method\")\np=outer.Point.new(close)\nplot(p.next())\n";
    let outer = "library(\"outer\")\nimport test/inner/1 as inner\nexport type Point\n    float x\nmethod next(Point self) => inner.value(self.x)\n";
    let inner = "library(\"inner\")\nexport value(float x) => x+1\n";
    let program = compile(root, &[("test/outer/1", outer), ("test/inner/1", inner)]);
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bars(&bars()).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    assert_eq!(result["plots"][0]["values"], serde_json::json!([2, 3, 4]));
}

#[test]
fn imported_scalar_overloads_bind_types_qualifiers_names_and_defaults() {
    let library = r#"library("overloads")
export choose(series float source, simple int length, series bool anchor, simple bool enabled=true) => source + length + (anchor and enabled ? 1.0 : 0.0)
export choose(series float source, simple int length, simple string timeframe, simple bool enabled=true) => source + length + (enabled ? str.length(timeframe) : 0)
export qualified(simple float source) => source + 1
export qualified(series float source) => source + 2
export reversed(series float source) => source + 2
export reversed(simple float source) => source + 1
export numeric(series float source) => source + 200
export numeric(series int source) => source + 100
export crossed(series float x, series int y) => x + y + 200
export crossed(series int x, series float y) => x + y + 100
export accumulate(series float source) =>
    var float total=0
    total+=source
    total
export accumulate(simple string source) =>
    var float total=0
    total+=str.length(source)
    total
"#;
    let root = r#"import test/overloads/1 as lib
indicator("overload calls")
plot(lib.choose(1.0,2,true))
plot(lib.choose(timeframe="15", source=1.0, length=2))
plot(lib.qualified(10.0))
plot(lib.qualified(close))
plot(lib.accumulate(close))
plot(lib.accumulate("ab"))
plot(lib.reversed(10.0))
plot(lib.numeric(1))
plot(lib.numeric(1.0))
plot(lib.crossed(1,1))
"#;
    let program = compile(root, &[("test/overloads/1", library)]);
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bars(&bars()).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    for (index, expected) in [
        [4, 4, 4],
        [5, 5, 5],
        [11, 11, 11],
        [3, 4, 5],
        [1, 3, 6],
        [2, 4, 6],
        [11, 11, 11],
        [101, 101, 101],
        [201, 201, 201],
        [202, 202, 202],
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            result["plots"][index]["values"],
            serde_json::json!(expected),
            "plot {index}"
        );
    }
}
