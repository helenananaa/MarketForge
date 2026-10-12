use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn native_v6_input_metadata_compiles_and_v5_rejects_new_parameters() {
    let source = include_str!("../../../tests/fixtures/runtime/input_active_metadata.pine");
    for version in [5, 6] {
        let source = source.replace("version=6", &format!("version={version}"));
        let analysis = analyze_source(&SourceFile::new("metadata.pine", source));
        if version == 6 {
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            assert!(analysis.hir.is_some());
        } else {
            assert!(analysis.hir.is_none());
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains("active"))
            );
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "E_CALL_ARG_TYPE")
            );
        }
    }
}

#[test]
fn v5_input_display_with_const_editable_is_admitted() {
    let source = include_str!("../../../tests/fixtures/runtime/input_active_metadata.pine")
        .replace("version=6", "version=5")
        .replace(", active=enabled", "")
        .replace("editable=enabled", "editable=false");
    let analysis = analyze_source(&SourceFile::new("v5-display.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
}

#[test]
fn nonempty_and_dynamic_program_timeframes_remain_explicitly_rejected() {
    for timeframe in ["\"60\"", "timeframe.period", "input.timeframe(\"\")", "na"] {
        let source =
            format!("//@version=6\nindicator(\"test\", timeframe={timeframe})\nplot(close)\n");
        let analysis = analyze_source(&SourceFile::new("timeframe.pine", source));
        assert!(analysis.hir.is_none());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.message.contains("indicator.timeframe")),
            "{:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn series_metadata_and_nonbool_active_remain_rejected() {
    for body in [
        "n=input.int(2,active=close>open)\nplot(n)",
        "n=input.int(2,active=1)\nplot(n)",
        "plot(close,editable=close>open)",
        "plot(close,display=close>open?display.all:display.none)",
        "plotshape(close>open,editable=close>open)",
        "a=plot(close)\nb=plot(open)\nfill(a,b,display=close>open?display.all:display.none)",
    ] {
        let source = format!("//@version=6\nindicator(\"negative\")\n{body}\n");
        let analysis = analyze_source(&SourceFile::new("negative.pine", source));
        assert!(analysis.hir.is_none(), "admitted: {body}");
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.code == "E_CALL_ARG_TYPE"),
            "{:?}",
            analysis.diagnostics
        );
    }
}
