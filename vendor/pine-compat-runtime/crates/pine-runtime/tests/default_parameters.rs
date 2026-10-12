use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::{AnalysisInput, analyze_input, analyze_source};
use pine_syntax::SourceFile;

const SOURCE: &str =
    include_str!("../../../tests/fixtures/runtime/function_default_parameters.pine");

fn bars() -> Vec<Bar> {
    [10.0, 20.0, 30.0]
        .into_iter()
        .enumerate()
        .map(|(i, close)| Bar {
            time: i as i64 * 60000,
            open: close,
            high: close + 2.0,
            low: close - 2.0,
            close,
            volume: 1.0,
        })
        .collect()
}

#[test]
fn omitted_named_and_explicit_arguments_preserve_values_and_callsite_state() {
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "defaults.pine",
            SOURCE.replace("//@version=6", &format!("//@version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let input = bars();
        let mut runtime = HistoricalRuntime::new(&hir);
        runtime.append_bars(&input).unwrap();
        let expected = public_runtime_result_json(&runtime.result());
        let output: serde_json::Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(
            output["plots"][0]["values"],
            serde_json::json!([1.4, 1.4, 1.4])
        );
        assert_eq!(
            output["plots"][1]["values"],
            serde_json::json!([1.7, 1.7, 1.7])
        );
        assert_eq!(output["plots"][2]["values"], output["plots"][0]["values"]);
        assert_eq!(output["plots"][3]["values"], serde_json::json!([1, 1, 1]));
        assert_eq!(
            output["plots"][4]["values"],
            serde_json::json!([2.5, 2.5, 2.5])
        );
        assert_eq!(output["plots"][5]["values"], serde_json::json!([5, 5, 5]));
        assert_eq!(
            output["plots"][6]["values"],
            serde_json::json!([2000, 2000, 2000])
        );
        for index in [7, 8] {
            assert_eq!(
                output["plots"][index]["values"],
                serde_json::json!([1, 1, 1])
            );
        }
        for index in [9, 10] {
            assert_eq!(
                output["plots"][index]["values"],
                serde_json::json!([1.5, 1.5, 1.5])
            );
        }
        assert_eq!(
            output["plots"][11]["values"],
            serde_json::json!([10, 30, 60])
        );
        assert_eq!(output["plots"][12]["values"], serde_json::json!([1, 2, 3]));
        let mut incremental = HistoricalRuntime::new(&hir);
        for bar in &input {
            incremental.append_bar(*bar).unwrap();
        }
        assert_eq!(public_runtime_result_json(&incremental.result()), expected);
        let mut realtime = RealtimeRuntime::new(&hir);
        realtime.seed_historical(&input[..2]).unwrap();
        for close in [31.0, 29.0, 30.0] {
            let mut forming = input[2];
            forming.close = close;
            let snapshot = realtime.update(BarUpdate::forming(forming)).unwrap();
            let snapshot: serde_json::Value =
                serde_json::from_str(&public_runtime_result_json(&snapshot)).unwrap();
            assert_eq!(snapshot["plots"][11]["values"][2], 30.0 + close);
        }
        assert_eq!(
            public_runtime_result_json(&realtime.update(BarUpdate::confirmed(input[2])).unwrap()),
            expected
        );
    }
}

#[test]
fn imported_defaults_and_private_helpers_preserve_aliases_and_input_binding() {
    for version in [5, 6] {
        let root = SourceFile::new(
            "root.pine",
            format!(
                "//@version={version}\nimport test/defaults/1 as first\nimport test/defaults/1 as second\nindicator(\"imports\")\nwrapper(close) => first.previous()\nplot(wrapper(500))\nplot(second.previous(3))\nplot(first.offset(value=2))\n"
            ),
        );
        let library = SourceFile::new(
            "library.pine",
            format!(
                "//@version={version}\nlibrary(\"defaults\")\nhelper(float value, float offset=0.5) => value+offset\nexport previous(series float value=close) => value[1]\nexport offset(float value=1) => helper(value)\n"
            ),
        );
        let input =
            AnalysisInput::with_library_sources(root, vec![("test/defaults/1".into(), library)])
                .unwrap();
        let analysis = analyze_input(&input);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&hir);
        runtime.append_bars(&bars()).unwrap();
        let out: serde_json::Value =
            serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
        assert_eq!(
            out["plots"][0]["values"],
            serde_json::json!([null, 500, 500])
        );
        assert_eq!(out["plots"][1]["values"], serde_json::json!([null, 3, 3]));
        assert_eq!(
            out["plots"][2]["values"],
            serde_json::json!([2.5, 2.5, 2.5])
        );
    }
}

#[test]
fn defaults_preserve_reference_passthrough_and_pure_history_identity() {
    let source = r#"//@version=6
indicator("Default identities")
type Point
    float value
point(Point p, scale=1) => p
points(array<Point> values, scale=1) => values
wrap(Point p) => point(p)
value=wrap(Point.new(close))
values=points(array.from(value))
item=array.get(values,0)
plot(item.value)
double(series float source=close) => source * 2
max_bars_back(double(),2)
offset=input.int(1)
plot(double(close)[offset])
"#;
    let analysis = analyze_source(&SourceFile::new("identity.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&bars()).unwrap();
    let out: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    assert_eq!(out["plots"][0]["values"], serde_json::json!([10, 20, 30]));
    assert_eq!(out["plots"][1]["values"], serde_json::json!([null, 20, 40]));
}

#[test]
fn default_input_binding_is_distinct_per_call_and_follows_caller_scope() {
    let source = r#"//@version=6
indicator("Caller default binding")
read(source=close) => source
wrap(close) => read()
before=read()
plot(wrap(7))
plot(wrap(9))
close=99.0
after=read()
plot(before)
plot(after)
"#;
    let analysis = analyze_source(&SourceFile::new("caller.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&bars()).unwrap();
    let out: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    for (index, expected) in [
        (0, serde_json::json!([7, 7, 7])),
        (1, serde_json::json!([9, 9, 9])),
        (2, serde_json::json!([10, 20, 30])),
        (3, serde_json::json!([99, 99, 99])),
    ] {
        assert_eq!(out["plots"][index]["values"], expected);
    }
}
