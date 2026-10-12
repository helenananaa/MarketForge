use crate::{
    AnalysisInput, PineDialect, ScriptModeClassification, VersionOrigin, analyze_input,
    analyze_source,
};
use pine_syntax::SourceFile;

fn analyze(text: &str) -> crate::Analysis {
    analyze_source(&SourceFile::new("legacy-policy.pine", text))
}

fn diagnostic_codes(analysis: &crate::Analysis) -> Vec<&str> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

#[test]
fn inverse_trigonometric_aliases_end_at_v4() {
    for version in [3, 4] {
        let source = format!(
            "//@version={version}\nstudy(\"inverse trig\")\nplot(acos(-1) + asin(1) + atan(1))\n"
        );
        let analysis = analyze(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
    }
    let v5 = analyze("//@version=5\nindicator(\"inverse trig\")\nplot(asin(1))\n");
    assert!(diagnostic_codes(&v5).contains(&"E_UNKNOWN_FUNCTION"));
}

#[test]
fn missing_directive_selects_executable_implicit_v1_profile() {
    let analysis = analyze("study(\"Legacy\")\nplot(close)\n");

    assert_eq!(analysis.compatibility.language_version, Some(1));
    assert_eq!(
        analysis.compatibility.language_version_origin,
        VersionOrigin::ImplicitV1
    );
    assert_eq!(analysis.compatibility.dialect, Some(PineDialect::V1));
    assert_eq!(
        analysis.compatibility.script_mode,
        ScriptModeClassification::LegacyIndicator
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.hir.as_ref().and_then(|hir| hir.language_version),
        Some(1)
    );
}

#[test]
fn four_space_ternary_continuation_is_limited_to_implicit_v1_source_origin() {
    let implicit = analyze(include_str!(
        "../../../../tests/fixtures/legacy/v1/runtime/ternary_continuation_legacy.pine"
    ));
    assert!(
        implicit.diagnostics.is_empty(),
        "{:?}",
        implicit.diagnostics
    );
    assert_eq!(
        implicit.compatibility.language_version_origin,
        VersionOrigin::ImplicitV1
    );
    assert!(implicit.hir.is_some());

    for version in 1..=6 {
        let declaration = if version <= 4 {
            "study(\"explicit boundary\")"
        } else {
            "indicator(\"explicit boundary\")"
        };
        let explicit = analyze(&format!(
            "//@version={version}\n{declaration}\nvalue = close > open ? 1 :\n    0\nplot(value)\n"
        ));
        assert!(
            diagnostic_codes(&explicit).contains(&"E_PARSE_EXPR"),
            "v{version}: {:?}",
            explicit.diagnostics
        );
        assert!(explicit.hir.is_none(), "v{version}");
    }
}

#[test]
fn explicit_v1_through_v6_report_closed_dialects() {
    for version in 1..=6 {
        let declaration = if version <= 4 {
            "study(\"dialect\")"
        } else {
            "indicator(\"dialect\")"
        };
        let analysis = analyze(&format!(
            "//@version={version}\n{declaration}\nplot(close)\n"
        ));

        assert_eq!(analysis.compatibility.language_version, Some(version));
        assert_eq!(
            analysis.compatibility.language_version_origin,
            VersionOrigin::ExplicitDirective
        );
        assert_eq!(
            analysis.compatibility.dialect.map(PineDialect::version),
            Some(version)
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert_eq!(
            analysis.hir.as_ref().and_then(|hir| hir.language_version),
            Some(version)
        );
    }
}

#[test]
fn spaced_equals_version_annotations_report_explicit_dialects() {
    for version in 1..=6 {
        let declaration = if version <= 4 {
            "study(\"spaced version\")"
        } else {
            "indicator(\"spaced version\")"
        };
        let analysis = analyze(&format!(
            "//@version \t= {version}\n{declaration}\nplot(close)\n"
        ));

        assert_eq!(analysis.compatibility.language_version, Some(version));
        assert_eq!(
            analysis.compatibility.language_version_origin,
            VersionOrigin::ExplicitDirective
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_some(), "v{version}");
    }
}

#[test]
fn invalid_versions_stop_before_ordinary_semantic_analysis() {
    for version in [0, 7, u16::MAX] {
        let analysis = analyze(&format!(
            "//@version={version}\nstudy(\"invalid\")\nplot(unknown_private_name)\n"
        ));

        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LANGUAGE_VERSION_UNSUPPORTED"]
        );
        assert_eq!(analysis.compatibility.language_version, Some(version));
        assert_eq!(analysis.compatibility.dialect, None);
        assert!(analysis.hir.is_none());
    }
}

#[test]
fn v1_to_v4_strategy_declaration_and_orders_are_executable() {
    for version in 1..=4 {
        let analysis = analyze(&format!(
            "//@version={version}\nstrategy(\"legacy\", initial_capital=100000)\nstrategy.entry(\"L\", strategy.long)\n"
        ));

        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert_eq!(
            analysis.compatibility.script_mode,
            ScriptModeClassification::Strategy
        );
        assert!(analysis.hir.is_some());
    }
}

#[test]
fn v1_to_v4_strategy_reference_in_indicator_is_rejected_by_strategy_mode_check() {
    for version in 1..=4 {
        let analysis = analyze(&format!(
            "//@version={version}\nstudy(\"excluded use\")\nplot(strategy.position_size)\n"
        ));
        assert_eq!(diagnostic_codes(&analysis), vec!["E_STRATEGY_MODE"]);
    }
}

#[test]
fn legacy_modern_or_missing_declarations_are_rejected_precisely() {
    for (source, expected_mode) in [
        (
            "//@version=4\nindicator(\"wrong declaration\")\nplot(close)\n",
            ScriptModeClassification::Indicator,
        ),
        (
            "//@version=4\nplot(close)\n",
            ScriptModeClassification::Missing,
        ),
        (
            "//@version=4\nlibrary(\"not an indicator\")\n",
            ScriptModeClassification::Library,
        ),
        (
            "//@version=4\nstudy(\"mixed\")\nlibrary(\"mixed\")\n",
            ScriptModeClassification::Mixed,
        ),
    ] {
        let analysis = analyze(source);

        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LEGACY_INDICATOR_DECLARATION"]
        );
        assert_eq!(analysis.compatibility.script_mode, expected_mode);
        assert!(analysis.hir.is_none());
    }
}

#[test]
fn spaced_version_comment_selects_modern_dialect() {
    // Preserve the original source while correcting its former implicit-v1 classification.
    let analysis = analyze("// @version=6\nindicator(\"still implicit v1\")\nplot(close)\n");
    assert!(diagnostic_codes(&analysis).is_empty());
    assert_eq!(
        analysis.compatibility.script_mode,
        ScriptModeClassification::Indicator
    );
    assert!(analysis.hir.is_some());
}

#[test]
fn user_defined_study_call_does_not_satisfy_legacy_indicator_admission() {
    let analysis =
        analyze("//@version=4\nstudy(title) => title\nstudy(\"not a declaration\")\nplot(close)\n");

    assert_eq!(
        diagnostic_codes(&analysis),
        vec!["E_LEGACY_INDICATOR_DECLARATION"]
    );
    assert_eq!(
        analysis.compatibility.script_mode,
        ScriptModeClassification::Missing
    );
    assert!(analysis.hir.is_none());
}

#[test]
fn modern_v5_v6_modes_keep_existing_paths_and_reject_study_alias() {
    for version in [5, 6] {
        for declaration in ["indicator(\"modern\")", "strategy(\"modern\")"] {
            let analysis = analyze(&format!(
                "//@version={version}\n{declaration}\nplot(close)\n"
            ));
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            assert!(analysis.hir.is_some());
        }

        let analysis = analyze(&format!(
            "//@version={version}\nstudy(\"not modern\")\nplot(close)\n"
        ));
        assert_eq!(diagnostic_codes(&analysis), vec!["E_UNKNOWN_FUNCTION"]);
        assert_eq!(
            analysis.compatibility.script_mode,
            ScriptModeClassification::LegacyIndicator
        );
    }
}

#[test]
fn newer_root_can_import_older_library_version() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nindicator(\"root\")\nimport user/lib/1 as lib\nplot(lib.value)\n",
    );
    let library = SourceFile::new(
        "lib.pine",
        "//@version=5\nlibrary(\"lib\")\nexport value = 1\n",
    );
    let input = AnalysisInput::with_library_sources(root, vec![("user/lib/1".to_owned(), library)])
        .expect("analysis input");

    let analysis = analyze_input(&input);

    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
}

#[test]
fn older_root_cannot_import_newer_library_version() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=5\nindicator(\"root\")\nimport user/lib/1 as lib\nplot(lib.value)\n",
    );
    let library = SourceFile::new(
        "lib.pine",
        "//@version=6\nlibrary(\"lib\")\nexport value = 1\n",
    );
    let input = AnalysisInput::with_library_sources(root, vec![("user/lib/1".to_owned(), library)])
        .expect("analysis input");

    let analysis = analyze_input(&input);
    assert_eq!(
        diagnostic_codes(&analysis),
        vec!["E_LANGUAGE_VERSION_CONFLICT"]
    );
    assert!(analysis.hir.is_none());
}

#[test]
fn matching_root_and_library_versions_remain_executable() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nindicator(\"root\")\nimport user/lib/1 as lib\nplot(lib.value)\n",
    );
    let library = SourceFile::new(
        "lib.pine",
        "//@version=6\nlibrary(\"lib\")\nexport value = 1\n",
    );
    let input = AnalysisInput::with_library_sources(root, vec![("user/lib/1".to_owned(), library)])
        .expect("analysis input");

    let analysis = analyze_input(&input);

    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
}
