use super::*;

#[test]
fn modern_requests_admit_pure_scalar_string_transformations() {
    for expression in [
        "str.length(str.tostring(close))",
        "str.length(str.upper(str.tostring(close)))",
        "str.length(str.lower(str.tostring(close)))",
        "str.contains(str.tostring(close), \".\") ? 1 : 0",
        "str.startswith(str.tostring(close), \"1\") ? 1 : 0",
        "str.endswith(str.tostring(close), \"0\") ? 1 : 0",
        "str.pos(str.tostring(close), \".\")",
        "str.length(str.substring(str.tostring(close), 0, 1))",
        "str.length(str.trim(str.tostring(close)))",
        "str.length(str.repeat(str.tostring(close), 2))",
        "str.length(str.replace(str.tostring(close), \".\", \"_\"))",
        "str.tonumber(str.replace_all(str.tostring(close), \".\", \"\"))",
        "str.tonumber(str.tostring(close))",
        "str.length(str.format(\"value={0}\", close))",
        r#"str.length(str.match(str.tostring(close), close % 2 == 0 ? "[0-9]+" : "[0-9]+\\.[0-9]+"))"#,
        "str.length(str.format_time(time, \"yyyy-MM-dd\", \"UTC\"))",
    ] {
        let source = format!(
            "//@version=6\nindicator(\"scalar strings\")\nplot(request.security(\"B\", \"5\", {expression}))\n"
        );
        let analysis = analyze(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{expression}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_some(), "{expression}");
    }
}

#[test]
fn modern_string_request_udf_preserves_scalar_locals_and_endpoint_reads() {
    let source = "//@version=6\nindicator(\"string UDF\")\nf(float value) =>\n    string result = str.tostring(value)\n    if barstate.islast\n        result := str.tostring(value[1])\n    str.tonumber(result)\nplot(request.security(\"B\", \"5\", f(close)))\n";
    let analysis = analyze(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
}

#[test]
fn string_results_do_not_admit_collection_arguments_or_mutable_captures() {
    for body in [
        "float value = close\nvalue := close + 1\nplot(request.security(\"B\", \"5\", str.tonumber(str.tostring(value))))",
        "var string held = str.tostring(close)\nplot(request.security(\"B\", \"5\", str.tonumber(held)))",
        "values = array.from(close)\nplot(request.security(\"B\", \"5\", str.length(str.format(\"{0}\", values))))",
        "plot(request.security(\"B\", \"5\", str.length(str.format(\"{0}\", array.from(close)))))",
        "plot(request.security(\"B\", \"5\", str.length(str.tostring(array.from(close)))))",
        "f() =>\n    values = array.from(close)\n    str.length(str.format(\"{0}\", values))\nplot(request.security(\"B\", \"5\", f()))",
        "plot(request.security(\"B\", \"5\", str.length(str.format(\"{0}\", str.split(str.tostring(close), \".\")))))",
        "type Box\n    float value\nbox = Box.new(close)\nplot(request.security(\"B\", \"5\", str.length(str.tostring(box))))",
        "drawing = line.new(bar_index, close, bar_index + 1, close)\nplot(request.security(\"B\", \"5\", str.length(str.format(\"{0}\", drawing))))",
        "plot(request.security(\"B\", \"5\", str.tonumber(str.tostring(request.security(\"C\", \"5\", close)))))",
    ] {
        let source = format!("//@version=6\nindicator(\"capture guard\")\n{body}\n");
        let analysis = analyze(&source);
        assert!(analysis.hir.is_none(), "{body}");
        assert!(
            analysis.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "E_UNSUPPORTED_FEATURE"
                    && diagnostic.message.contains("request.security")
            }),
            "{body}: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn legacy_security_keeps_its_existing_scalar_call_subset() {
    let analysis = analyze(
        "//@version=4\nstudy(\"legacy strings\")\nplot(security(\"B\", \"5\", str.tonumber(str.tostring(close))))\n",
    );
    assert!(analysis.hir.is_none());
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E_UNSUPPORTED_FEATURE" && diagnostic.message.contains("security")
    }));
}
