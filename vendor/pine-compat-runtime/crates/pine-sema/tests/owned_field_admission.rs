use pine_sema::{AnalysisInput, analyze_input};
use pine_syntax::SourceFile;

const LIB: &str = r#"//@version=6
library("OwnedFields")
type Bucket
    array<float> data
method retain(array<Bucket> id, int limit, Bucket value) =>
    id.push(value)
    if id.size() > limit
        id.shift()
export collect(series float source, series bool anchor) =>
    var array<Bucket> history = array.new<Bucket>()
    var Bucket current = Bucket.new(array.new<float>())
    if anchor
        history.retain(2, current)
        current := Bucket.new(array.new<float>())
    current.data.push(source)
    0
"#;

fn analyze_library(library: &str) -> pine_sema::Analysis {
    let input = AnalysisInput::with_library_sources(
        SourceFile::new(
            "root.pine",
            "//@version=6\nimport test/lib/1 as lib\nindicator(\"admission\")\nplot(close)\n",
        ),
        vec![(
            "test/lib/1".to_owned(),
            SourceFile::new("lib.pine", library),
        )],
    )
    .unwrap();
    analyze_input(&input)
}

#[test]
fn fresh_fields_with_fresh_branch_replacement_are_admitted() {
    let result = analyze_library(LIB);
    assert!(result.hir.is_some(), "{:?}", result.diagnostics);
}

#[test]
fn constructor_binding_and_scalar_arguments_do_not_escape_references() {
    let with_scalar = LIB
        .replace(
            "    array<float> data",
            "    array<float> data\n    int stamp",
        )
        .replace(
            "Bucket.new(array.new<float>())",
            "Bucket.new(stamp=time, data=array.new<float>())",
        );
    assert!(analyze_library(&with_scalar).hir.is_some());
    for bad in [
        with_scalar.replace(
            "stamp=time, data=array.new<float>()",
            "stamp=escape(current), data=array.new<float>()",
        ),
        with_scalar.replace(
            "stamp=time, data=array.new<float>()",
            "stamp=time, data=array.new<float>(), data=borrowed",
        ),
        with_scalar.replace(
            "stamp=time, data=array.new<float>()",
            "stamp=time, missing=array.new<float>()",
        ),
        with_scalar.replace(
            "stamp=time, data=array.new<float>()",
            "data=array.new<float>(), time",
        ),
    ] {
        let result = analyze_library(&bad);
        assert!(result.hir.is_none());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "E_IMPORT_FUNCTION_SIDE_EFFECT"),
            "{:?}",
            result.diagnostics
        );
    }
}

#[test]
fn imported_array_namespace_does_not_prove_a_fresh_allocation() {
    let library = LIB
        .replace(
            "library(\"OwnedFields\")",
            "library(\"OwnedFields\")\nimport test/factory/1 as array",
        )
        .replace("array.new<float>()", "array.new_float()");
    let input = AnalysisInput::with_library_sources(
        SourceFile::new("root.pine", "//@version=6\nimport test/lib/1 as lib\nindicator(\"shadow\")\nplot(close)\n"),
        vec![("test/lib/1".to_owned(), SourceFile::new("lib.pine", library)),
            ("test/factory/1".to_owned(), SourceFile::new("factory.pine", "//@version=6\nlibrary(\"factory\")\nexport new_float() => array.new<float>()\n"))],
    ).unwrap();
    let result = analyze_input(&input);
    assert!(result.hir.is_none());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "E_IMPORT_FUNCTION_SIDE_EFFECT")
    );
}

#[test]
fn aliases_escapes_and_helper_field_replacement_keep_rejection() {
    for (label, library) in [
        ("grouped escape", LIB.replace("    current.data.push(source)", "    escape((current))\n    current.data.push(source)")),
        ("conditional alias", LIB.replace("    current.data.push(source)", "    other = anchor ? current : current\n    other.replace()\n    current.data.push(source)")),
        ("container alias", LIB.replace("    current.data.push(source)", "    other = history\n    other.replace()\n    current.data.push(source)")),
        ("implicit container receiver", LIB.replace("    current.data.push(source)", "    history.replace()\n    current.data.push(source)")),
        ("helper field replacement", LIB.replace("    id.push(value)", "    value.data := array.new<float>()\n    id.push(value)")),
        ("helper receiver replacement", LIB.replace("    id.push(value)", "    if true\n        id := borrowed\n    id.push(value)")),
        ("helper global escape", LIB.replace("    id.push(value)", "    outside := value\n    id.push(value)")),
        ("borrowed replacement", LIB.replace("        current := Bucket.new(array.new<float>())", "        current := borrowed")),
        ("return alias", LIB.replace("    current.data.push(source)", "    other = history.retain(2, current)\n    other.replace()\n    current.data.push(source)")),
        ("return escape reassignment", LIB.replace("    current.data.push(source)", "    outside := history.retain(2, current)\n    current.data.push(source)")),
        ("element alias", LIB.replace("    current.data.push(source)", "    other = history.get(0)\n    other.replace()\n    current.data.push(source)")),
        ("custom push", LIB.replace("method retain", "method push(array<float> id, float source) =>\n    0\nmethod retain")),
        ("expression field replacement", LIB.replace("    id.push(value)", "    float altered = if true\n        value.data := borrowed\n        0\n    else\n        0\n    id.push(value)")),
        ("custom size", LIB.replace("method retain", "method size(array<Bucket> id) =>\n    0\nmethod retain")),
    ] {
        let result=analyze_library(&library);
        assert!(result.diagnostics.iter().any(|d|d.code=="E_IMPORT_FUNCTION_SIDE_EFFECT"), "{label}: {:?}",result.diagnostics);
        assert!(result.hir.is_none(), "{label}");
    }
}
