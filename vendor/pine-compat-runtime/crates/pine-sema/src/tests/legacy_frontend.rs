use pine_ir::{
    HirExpr, HirExprKind, HirHistoryOffset, HirStmtKind, PineType, Qualifier, ValueKind,
};
use pine_syntax::{SourceFile, Span};

use crate::LegacyTranslationKind;
use crate::compatibility::{CompatibilityReport, LegacyEmulation, LegacyTranslation};
use crate::legacy::{
    CatalogValidationError, LegacyRule, LegacyRuleKind, LegacyRuleSupport, PineDialect,
    validate_catalog,
};

const TEST_RULES: &[LegacyRule] = &[
    LegacyRule {
        source_name: "iff",
        canonical_name: None,
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedExpression,
        support: LegacyRuleSupport::UnsupportedKnown {
            reason: "test deferred expression",
        },
    },
    LegacyRule {
        source_name: "security",
        canonical_name: Some("request.security"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedSecurity,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "sma",
        canonical_name: Some("ta.sma"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "tickerid",
        canonical_name: Some("syminfo.tickerid"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::ExactSymbolAlias,
        support: LegacyRuleSupport::Supported,
    },
];

fn analyze_legacy(source: &str) -> crate::Analysis {
    crate::analysis::analyze_source_with_legacy_rules(
        &SourceFile::new("legacy-test.pine", source),
        TEST_RULES,
    )
}

fn analyze_production(source: &str) -> crate::Analysis {
    crate::analyze_source(&SourceFile::new("legacy-production.pine", source))
}

fn analyze_catalog_without_admission(source: &str) -> crate::Analysis {
    crate::analysis::analyze_source_with_legacy_rules(
        &SourceFile::new("legacy-catalog-test.pine", source),
        crate::legacy::LEGACY_RULES,
    )
}

fn normalized_hir(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_production(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let mut hir = analysis.hir.expect("HIR");
    hir.language_version = None;
    // These comparisons deliberately check execution IR across different
    // source spellings/versions. Original locations are checked separately.
    hir.call_site_sources.clear();
    hir
}

fn plot_arg(analysis: &crate::Analysis) -> &HirExpr {
    analysis
        .hir
        .as_ref()
        .expect("HIR")
        .statements
        .iter()
        .find_map(|statement| match &statement.kind {
            HirStmtKind::Expr(HirExpr {
                kind: HirExprKind::Call { callee, args, .. },
                ..
            }) if callee == "plot" => args.first().map(|arg| &arg.value),
            _ => None,
        })
        .expect("plot argument")
}

fn plot_call_name(analysis: &crate::Analysis) -> &str {
    let HirExprKind::Call { callee, .. } = &plot_arg(analysis).kind else {
        panic!("plot argument is not a call");
    };
    callee
}

fn diagnostic_codes(analysis: &crate::Analysis) -> Vec<&str> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

fn assert_registered_value_namespace_versions(namespace: &str, expected: &[(&str, u16)]) {
    let prefix = format!("{namespace}.");
    let actual_names = pine_builtins::registered_value_names()
        .filter(|name| name.starts_with(&prefix))
        .collect::<std::collections::BTreeSet<_>>();
    let expected_names = expected
        .iter()
        .map(|(name, _)| *name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        actual_names, expected_names,
        "registered `{namespace}` member set changed; classify every new member explicitly"
    );
    for (name, expected_version) in expected {
        assert_eq!(
            PineDialect::qualified_builtin_min_version(name, false),
            Some(*expected_version),
            "{name}"
        );
    }
}

fn top_level_call<'a>(analysis: &'a crate::Analysis, name: &str) -> &'a HirExpr {
    analysis
        .hir
        .as_ref()
        .expect("HIR")
        .statements
        .iter()
        .find_map(|statement| match &statement.kind {
            HirStmtKind::Expr(
                expr @ HirExpr {
                    kind: HirExprKind::Call { callee, .. },
                    ..
                },
            ) if callee == name => Some(expr),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing top-level `{name}` call"))
}

#[test]
fn exact_function_alias_lowers_to_canonical_hir_and_preserves_source_span() {
    let source = "//@version=4\nplot(sma(close, 2))\n";
    let analysis = analyze_legacy(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let plot_arg = plot_arg(&analysis);
    assert!(matches!(
        &plot_arg.kind,
        HirExprKind::Call { callee, .. } if callee == "ta.sma"
    ));
    let translation = analysis
        .compatibility
        .legacy_translations
        .first()
        .expect("translation");
    let start = source.find("sma").expect("source alias");
    assert_eq!(translation.source_feature, "sma");
    assert_eq!(translation.canonical_feature, "ta.sma");
    assert_eq!(translation.kind, LegacyTranslationKind::ExactAlias);
    assert_eq!(translation.span, Span::new(start, start + "sma".len()));
    let HirExprKind::Call { call_site_id, .. } = &plot_arg.kind else {
        panic!("expected the lowered canonical call");
    };
    let origin = analysis
        .hir
        .as_ref()
        .unwrap()
        .call_site_sources
        .iter()
        .find(|entry| entry.call_site_id == *call_site_id)
        .expect("original call location");
    assert_eq!(origin.source_id, 0);
    assert!(origin.library_key.is_none());
    assert_eq!(&source[origin.start..origin.end], "sma(close, 2)");
}

#[test]
fn production_v4_study_and_first_alias_batch_match_canonical_hir() {
    let legacy = include_str!("../../../../tests/fixtures/legacy/v4/runtime/aliases_legacy.pine");
    let canonical =
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/aliases_canonical.pine");

    assert_eq!(normalized_hir(legacy), normalized_hir(canonical));

    let analysis = analyze_production(legacy);
    let translations = analysis
        .compatibility
        .legacy_translations
        .iter()
        .map(|translation| {
            (
                translation.source_feature.as_str(),
                translation.canonical_feature.as_str(),
                translation.kind,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        translations,
        vec![
            (
                "study",
                "indicator",
                LegacyTranslationKind::SignatureReshape
            ),
            ("bb", "ta.bb", LegacyTranslationKind::ExactAlias),
            ("ema", "ta.ema", LegacyTranslationKind::ExactAlias),
            ("sma", "ta.sma", LegacyTranslationKind::ExactAlias),
            (
                "crossover",
                "ta.crossover",
                LegacyTranslationKind::ExactAlias
            ),
            ("cross", "ta.cross", LegacyTranslationKind::ExactAlias),
            ("change", "ta.change", LegacyTranslationKind::ExactAlias),
            ("sqrt", "math.sqrt", LegacyTranslationKind::ExactAlias),
            ("abs", "math.abs", LegacyTranslationKind::ExactAlias),
            ("stdev", "ta.stdev", LegacyTranslationKind::ExactAlias),
            ("vwma", "ta.vwma", LegacyTranslationKind::ExactAlias),
            ("max", "math.max", LegacyTranslationKind::ExactAlias),
            ("min", "math.min", LegacyTranslationKind::ExactAlias),
            (
                "pivothigh",
                "ta.pivothigh",
                LegacyTranslationKind::ExactAlias
            ),
            ("pivotlow", "ta.pivotlow", LegacyTranslationKind::ExactAlias),
            ("atr", "ta.atr", LegacyTranslationKind::ExactAlias),
            ("avg", "math.avg", LegacyTranslationKind::ExactAlias),
            ("floor", "math.floor", LegacyTranslationKind::ExactAlias),
            ("linreg", "ta.linreg", LegacyTranslationKind::ExactAlias),
            ("stoch", "ta.stoch", LegacyTranslationKind::ExactAlias),
            ("sum", "math.sum", LegacyTranslationKind::ExactAlias),
            (
                "barssince",
                "ta.barssince",
                LegacyTranslationKind::ExactAlias
            ),
            (
                "crossunder",
                "ta.crossunder",
                LegacyTranslationKind::ExactAlias
            ),
            (
                "heikinashi",
                "ticker.heikinashi",
                LegacyTranslationKind::ExactAlias
            ),
            ("log10", "math.log10", LegacyTranslationKind::ExactAlias),
            ("macd", "ta.macd", LegacyTranslationKind::ExactAlias),
            ("sign", "math.sign", LegacyTranslationKind::ExactAlias),
            (
                "tostring",
                "str.tostring",
                LegacyTranslationKind::SignatureReshape
            ),
            (
                "tostring",
                "str.tostring",
                LegacyTranslationKind::SignatureReshape
            ),
            (
                "valuewhen",
                "ta.valuewhen",
                LegacyTranslationKind::ExactAlias
            ),
            ("cci", "ta.cci", LegacyTranslationKind::ExactAlias),
            ("ceil", "math.ceil", LegacyTranslationKind::ExactAlias),
            ("log", "math.log", LegacyTranslationKind::ExactAlias),
            ("mfi", "ta.mfi", LegacyTranslationKind::ExactAlias),
            ("mom", "ta.mom", LegacyTranslationKind::ExactAlias),
            ("pow", "math.pow", LegacyTranslationKind::ExactAlias),
            ("tr", "ta.tr", LegacyTranslationKind::SymbolAlias),
            ("tr", "ta.tr", LegacyTranslationKind::ExactAlias),
            ("obv", "ta.obv", LegacyTranslationKind::SymbolAlias),
            ("vwap", "ta.vwap", LegacyTranslationKind::SymbolAlias),
            ("vwap", "ta.vwap", LegacyTranslationKind::SignatureReshape),
            ("hma", "ta.hma", LegacyTranslationKind::ExactAlias),
            ("round", "math.round", LegacyTranslationKind::ExactAlias),
            ("rma", "ta.rma", LegacyTranslationKind::ExactAlias),
            ("wma", "ta.wma", LegacyTranslationKind::ExactAlias),
        ]
    );
}

#[test]
fn legacy_security_defaults_and_explicit_merge_values_are_versioned() {
    let v2 = analyze_legacy("//@version=2\nplot(security(\"NYSE:IBM\", \"5\", close, false))\n");
    assert!(v2.diagnostics.is_empty(), "{:?}", v2.diagnostics);
    assert_eq!(
        plot_call_name(&v2),
        "$legacy.security.gaps_off.lookahead_on"
    );

    let v3 = analyze_legacy(
        "//@version=3\nplot(security(symbol=\"NYSE:IBM\", resolution=\"5\", expression=close))\n",
    );
    assert!(v3.diagnostics.is_empty(), "{:?}", v3.diagnostics);
    assert_eq!(
        plot_call_name(&v3),
        "$legacy.security.gaps_off.lookahead_off"
    );

    let v3_explicit = analyze_legacy(
        "//@version=3\nplot(security(\"NYSE:IBM\", \"5\", close, gaps=barmerge.gaps_on, lookahead=true))\n",
    );
    assert!(
        v3_explicit.diagnostics.is_empty(),
        "{:?}",
        v3_explicit.diagnostics
    );
    assert_eq!(
        plot_call_name(&v3_explicit),
        "$legacy.security.gaps_on.lookahead_on"
    );
}

#[test]
fn production_v4_security_uses_request_expression_analysis_and_source_spanned_hir() {
    let source = "//@version=4\nstudy(\"legacy security\")\nplot(security(syminfo.tickerid, \"5\", sma(close, 2)))\n";
    let analysis = analyze_production(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let HirExprKind::Call { callee, args, .. } = &plot_arg(&analysis).kind else {
        panic!("legacy security was not lowered to a call");
    };
    assert_eq!(callee, "$legacy.security.gaps_off.lookahead_off");
    assert_eq!(args.len(), 5);
    assert_eq!(args[3].name.as_deref(), Some("$legacy_span_start"));
    assert_eq!(args[4].name.as_deref(), Some("$legacy_span_end"));
    let HirExprKind::Call { callee, .. } = &args[2].value.kind else {
        panic!("requested SMA expression was not lowered");
    };
    assert_eq!(callee, "ta.sma");
    assert!(analysis.compatibility.unsupported.is_empty());
    assert!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .any(|item| {
                item.source_feature == "security" && item.canonical_feature == "request.security"
            })
    );
}

#[test]
fn legacy_security_accepts_immutable_global_alias_graphs_and_dynamic_simple_contexts() {
    let analysis = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v1/runtime/security_aliases_legacy.pine"
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.compatibility.unsupported.is_empty());

    let synthesized_symbol = analyze_production(
        "//@version=4\nstudy(\"symbol expression\")\nmakeSymbol(prefix, name) => prefix + \":\" + name\nplot(security(makeSymbol(\"NYSE\", \"IBM\"), \"5\", close))\n",
    );
    assert!(
        synthesized_symbol.diagnostics.is_empty(),
        "{:?}",
        synthesized_symbol.diagnostics
    );
}

#[test]
fn legacy_security_accepts_pure_udfs_and_keeps_mutable_state_fail_closed() {
    let mutable = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v1/unsupported/security_mutable_alias.pine"
    ));
    assert!(diagnostic_codes(&mutable).contains(&"E_UNSUPPORTED_FEATURE"));
    assert!(
        mutable
            .compatibility
            .unsupported
            .iter()
            .any(|item| item.feature == "security")
    );

    let pure_udf = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/security_pure_udf_legacy.pine"
    ));
    assert!(
        pure_udf.diagnostics.is_empty(),
        "{:?}",
        pure_udf.diagnostics
    );
    assert!(pure_udf.compatibility.unsupported.is_empty());

    let mutable_udf = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/unsupported/security_mutable_udf.pine"
    ));
    assert!(diagnostic_codes(&mutable_udf).contains(&"E_UNSUPPORTED_FEATURE"));
    assert!(
        mutable_udf
            .compatibility
            .unsupported
            .iter()
            .any(|item| item.feature == "security")
    );

    let modern_udf = analyze_production(
        "//@version=6\nindicator(\"modern UDF request\")\ncalculate() => ta.sma(close, 2)\nplot(request.security(\"NYSE:IBM\", \"5\", calculate()))\n",
    );
    assert!(
        modern_udf.diagnostics.is_empty(),
        "{:?}",
        modern_udf.diagnostics
    );
    assert!(modern_udf.hir.is_some());
}

#[test]
fn legacy_security_accepts_same_selector_udf_local_dependencies_only() {
    let supported = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/security_udf_local_dependencies_legacy.pine"
    ));
    assert!(
        supported.diagnostics.is_empty(),
        "{:?}",
        supported.diagnostics
    );
    assert!(supported.compatibility.unsupported.is_empty());
    assert!(supported.hir.is_some());

    let mismatched = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/unsupported/security_udf_local_dependency_mismatch.pine"
    ));
    assert!(diagnostic_codes(&mismatched).contains(&"E_UNSUPPORTED_FEATURE"));
    assert!(
        mismatched
            .compatibility
            .unsupported
            .iter()
            .any(|item| item.feature == "security")
    );

    for source in [
        "//@version=4\nstudy(\"different timeframe\")\nf(src) =>\n    earlier = security(\"NYSE:IBM\", \"15\", src)\n    security(\"NYSE:IBM\", \"5\", earlier)\nplot(f(close))\n",
        "//@version=4\nstudy(\"different merge policy\")\nf(src) =>\n    earlier = security(\"NYSE:IBM\", \"5\", src, barmerge.gaps_on, barmerge.lookahead_off)\n    security(\"NYSE:IBM\", \"5\", earlier)\nplot(f(close))\n",
        "//@version=4\nstudy(\"named nested request\")\nf(src) =>\n    earlier = security(symbol=\"NYSE:IBM\", resolution=\"5\", expression=src)\n    security(\"NYSE:IBM\", \"5\", earlier)\nplot(f(close))\n",
    ] {
        let analysis = analyze_production(source);
        assert!(
            analysis
                .compatibility
                .unsupported
                .iter()
                .any(|item| item.feature == "security"),
            "{:?}",
            analysis.diagnostics
        );
    }

    let control_flow_local = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/unsupported/security_udf_control_flow_local.pine"
    ));
    assert!(diagnostic_codes(&control_flow_local).contains(&"E_UNSUPPORTED_FEATURE"));
    assert!(
        control_flow_local
            .compatibility
            .unsupported
            .iter()
            .any(|item| item.feature == "security")
    );

    let modern = analyze_production(
        "//@version=6\nindicator(\"modern local request\")\nrequested(src) =>\n    local = src + 1\n    request.security(\"NYSE:IBM\", \"5\", local)\nplot(requested(close))\n",
    );
    assert!(
        modern
            .compatibility
            .unsupported
            .iter()
            .any(|item| item.feature == "request.security"),
        "{:?}",
        modern.diagnostics
    );
}

#[test]
fn legacy_security_global_alias_nested_request_requires_matching_selector() {
    let supported = analyze_production(
        "//@version=2\nstudy(\"matching global alias\")\nacc = nz(acc[1]) + close\ninner = security(\"NYSE:IBM\", \"5\", acc)\nplot(security(\"NYSE:IBM\", \"5\", inner * 2))\n",
    );
    assert!(
        supported.diagnostics.is_empty(),
        "{:?}",
        supported.diagnostics
    );

    let mismatched = analyze_production(
        "//@version=2\nstudy(\"mismatched global alias\")\ninner = security(\"NYSE:IBM\", \"15\", close)\nplot(security(\"NYSE:IBM\", \"5\", inner * 2))\n",
    );
    assert!(diagnostic_codes(&mismatched).contains(&"E_UNSUPPORTED_FEATURE"));
}

#[test]
fn legacy_security_rejects_invalid_versioned_signatures_and_dynamic_merge_metadata() {
    let named_v2 = analyze_legacy(
        "//@version=2\nplot(security(symbol=\"NYSE:IBM\", resolution=\"5\", expression=close))\n",
    );
    assert!(diagnostic_codes(&named_v2).contains(&"E_CALL_ARG_NAME"));

    let dynamic_merge = analyze_legacy(
        "//@version=4\nflag = close > open\nplot(security(\"NYSE:IBM\", \"5\", close, flag))\n",
    );
    assert!(diagnostic_codes(&dynamic_merge).contains(&"E_LEGACY_SECURITY_MERGE"));

    let sixth_arg = analyze_legacy(
        "//@version=4\nplot(security(\"NYSE:IBM\", \"5\", close, false, false, false))\n",
    );
    assert!(diagnostic_codes(&sixth_arg).contains(&"E_CALL_ARITY"));
}

#[test]
fn user_defined_security_function_shadows_legacy_builtin() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"shadow\")\nsecurity(symbol, resolution, expression) => expression + 1\nplot(security(\"X\", \"5\", close))\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(!matches!(
        &plot_arg(&analysis).kind,
        HirExprKind::Call { callee, .. }
            if callee == "$legacy.security.gaps_off.lookahead_off"
    ));
    assert!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .all(|item| item.source_feature != "security")
    );
}

#[test]
fn v4_study_binds_historical_positions_before_canonical_validation() {
    let legacy = include_str!("../../../../tests/fixtures/legacy/v4/sema/declaration_legacy.pine");
    let canonical =
        include_str!("../../../../tests/fixtures/legacy/v4/sema/declaration_canonical.pine");

    assert_eq!(normalized_hir(legacy), normalized_hir(canonical));

    let analysis = analyze_production(legacy);
    let hir = analysis.hir.expect("legacy declaration HIR");
    assert_eq!(hir.max_bars_back, Some(12));
    assert_eq!(hir.drawing_settings.max_lines_count, Some(75));
    assert_eq!(hir.drawing_settings.max_labels_count, Some(80));
    assert_eq!(hir.drawing_settings.max_boxes_count, Some(65));
}

#[test]
fn production_v4_input_overloads_match_canonical_hir() {
    let legacy = include_str!("../../../../tests/fixtures/legacy/v4/runtime/inputs_legacy.pine");
    let canonical =
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/inputs_canonical.pine");

    assert_eq!(normalized_hir(legacy), normalized_hir(canonical));

    let analysis = analyze_production(legacy);
    let input_targets = analysis
        .compatibility
        .legacy_translations
        .iter()
        .filter(|translation| translation.source_feature == "input")
        .map(|translation| translation.canonical_feature.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        input_targets,
        vec![
            "input.int",
            "input.float",
            "input.bool",
            "input.color",
            "input.string",
            "input.symbol",
            "input.timeframe",
            "input.session",
            "input.source",
            "input.time",
            "input.price",
        ]
    );
    assert!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .filter(|translation| translation.source_feature.starts_with("input."))
            .all(|translation| translation.kind == LegacyTranslationKind::ConstantAlias)
    );
}

#[test]
fn v4_input_type_constant_survives_a_local_const_alias() {
    let source =
        include_str!("../../../../tests/fixtures/legacy/v4/sema/input_constant_alias.pine");
    let analysis = analyze_production(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .filter(|translation| translation.source_feature == "input.integer")
            .count(),
        1
    );
    assert!(analysis.hir.is_some());
}

#[test]
fn v4_input_infers_fixture_backed_scalar_and_source_overloads() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"inferred\")\ni = input(1)\nf = input(1.5)\nb = input(true)\ns = input(\"text\")\nc = input(color.red)\nsrc = input(close)\nplot(src)\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .filter(|translation| translation.source_feature == "input")
            .map(|translation| translation.canonical_feature.as_str())
            .collect::<Vec<_>>(),
        vec![
            "input.int",
            "input.float",
            "input.bool",
            "input.string",
            "input.color",
            "input.source",
        ]
    );
}

#[test]
fn v4_input_rejects_ambiguous_and_wrong_overloads_before_runtime() {
    let string_type = analyze_production(
        "//@version=4\nstudy(\"bad\")\nlength = input(3, \"Length\", type=\"input.int\")\nplot(close)\n",
    );
    assert_eq!(
        diagnostic_codes(&string_type),
        vec!["E_LEGACY_INPUT_OVERLOAD"]
    );
    assert!(string_type.hir.is_none());

    let source_confirm = analyze_production(
        "//@version=4\nstudy(\"bad\")\nsrc = input(close, \"Source\", type=input.source, confirm=true)\nplot(close)\n",
    );
    assert_eq!(diagnostic_codes(&source_confirm), vec!["E_CALL_ARG_NAME"]);
    assert!(source_confirm.hir.is_none());

    let no_defval = analyze_production(
        "//@version=4\nstudy(\"bad\")\nlength = input(title=\"Length\", type=input.integer)\nplot(close)\n",
    );
    assert_eq!(diagnostic_codes(&no_defval), vec!["E_CALL_ARITY"]);
    assert!(no_defval.hir.is_none());
}

#[test]
fn legacy_input_type_selectors_require_internal_provenance_and_cannot_leak() {
    for type_expr in ["\"$legacy-input:integer\"", "forged_type"] {
        let declaration = if type_expr == "forged_type" {
            "forged_type = \"$legacy-input:integer\"\n"
        } else {
            ""
        };
        let analysis = analyze_production(&format!(
            "//@version=4\nstudy(\"forged\")\n{declaration}length = input(3, \"Length\", type={type_expr})\nplot(close)\n"
        ));
        assert_eq!(diagnostic_codes(&analysis), vec!["E_LEGACY_INPUT_OVERLOAD"]);
        assert!(analysis.hir.is_none());
    }

    for source in [
        "//@version=4\nstudy(\"leak\")\nplot(close, title=input.integer)\n",
        "//@version=4\nstudy(\"leak alias\")\nkind = input.integer\nplot(close, title=kind)\n",
        "//@version=4\nstudy(\"leak concat\")\nplotchar(close, char=input.integer + \"\")\n",
        "//@version=4\nstudy(\"leak ternary\")\nplot(close, title=true ? input.integer : \"safe\")\n",
        "//@version=4\nstudy(\"leak transformed alias\")\nkind = input.integer + \"\"\nplot(close, title=kind)\n",
        "//@version=4\nstudy(\"leak reassignment\")\nkind = \"\"\nkind := input.integer\nplot(close, title=kind)\n",
        "//@version=4\nstudy(\"marker condition\")\nif input.integer == input.integer\n    plot(close)\n",
        "//@version=4\nstudy(\"marker condition alias\")\nflag = input.integer == input.integer\nplot(flag ? close : open)\n",
        "//@version=4\nstudy(\"udf marker condition\")\nchoose() =>\n    if input.integer == input.integer\n        1\n    else\n        2\nplot(choose())\n",
        "//@version=4\nstudy(\"tuple marker\")\n[kind, n] = [input.integer, 1]\nplot(close, title=kind)\n",
        "//@version=4\nstudy(\"reassigned marker alias\")\nkind = input.integer\nkind := \"safe\"\nalert(kind)\n",
        "//@version=4\nstudy(\"conditionally reassigned marker alias\")\nkind = input.integer\nif close > open\n    kind := \"safe\"\nalert(kind)\n",
        "//@version=4\nstudy(\"udf marker\")\nleak() => input.integer\nplot(close, title=leak())\n",
        "//@version=4\nstudy(\"nested udf marker\")\nleak() =>\n    kind = input.integer\n    kind\nrelay() => leak()\nplot(close, title=relay())\n",
    ] {
        let analysis = analyze_production(source);
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LEGACY_INPUT_CONSTANT_CONTEXT"],
            "{source}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    for source in [
        "//@version=3\nstudy(\"input result\")\nlength = input(3, \"Length\", integer)\nfast = ema(close, length)\nslow = sma(close, length)\nplot(fast + slow)\n",
        "//@version=4\nstudy(\"input result\")\nlength = input(3, \"Length\", input.integer)\nplot(sma(close, length))\n",
        "//@version=4\nstudy(\"input result through udf\")\nlength = input(3, \"Length\", input.integer)\nreadLength() => length\nplot(sma(close, readLength()))\n",
        "//@version=4\nstudy(\"reassigned input result\")\nlength = 1\nlength := input(3, \"Length\", input.integer)\nplot(sma(close, length))\n",
    ] {
        let analysis = analyze_production(source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_some());
    }
}

#[test]
fn v4_integer_input_accepts_float_metadata_without_widening_modern_input_int() {
    let legacy = analyze_production(
        "//@version=4\nstudy(\"bounds\")\nlength = input(1, \"Length\", input.integer, minval=1.0, maxval=5.0, step=1.0)\nplot(sma(close, length))\n",
    );
    assert!(legacy.diagnostics.is_empty(), "{:?}", legacy.diagnostics);
    assert!(legacy.hir.is_some());

    let modern = analyze_production(
        "//@version=5\nindicator(\"bounds\")\nlength = input.int(1, \"Length\", minval=1.0)\nplot(ta.sma(close, length))\n",
    );
    assert_eq!(diagnostic_codes(&modern), vec!["E_CALL_ARG_TYPE"]);
    assert!(modern.compatibility.legacy_translations.is_empty());
    assert!(modern.hir.is_none());
}

#[test]
fn v4_input_constants_do_not_become_global_numeric_coercions() {
    let analysis = analyze_production("//@version=4\nstudy(\"bad\")\nplot(input.integer * 2)\n");
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("numeric")),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn modern_sources_reject_v4_input_type_constants() {
    for version in [5, 6] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nindicator(\"modern\")\nkind = input.integer\nplot(close)\n"
        ));
        assert!(analysis.hir.is_none());
        assert!(analysis.compatibility.legacy_translations.is_empty());
    }
}

#[test]
fn v4_study_empty_resolution_inherits_chart_timeframe() {
    let source = "//@version=4\nstudy(\"Chart\", resolution=\"\", resolution_gaps=false, max_boxes_count=75)\nplot(close)\n";
    let analysis = analyze_production(source);

    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.compatibility.unsupported.is_empty());
    assert_eq!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .filter(|emulation| emulation.feature == "study.resolution")
            .count(),
        1
    );
    let hir = analysis.hir.expect("chart-inherited resolution HIR");
    assert_eq!(hir.drawing_settings.max_boxes_count, Some(75));
    assert!(!format!("{hir:?}").contains("resolution"));
}

#[test]
fn v4_study_nonempty_resolution_produces_one_focused_failure() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"MTF\", resolution=\"D\", resolution_gaps=true)\nplot(close)\n",
    );

    assert_eq!(diagnostic_codes(&analysis), vec!["E_UNSUPPORTED_FEATURE"]);
    assert_eq!(analysis.compatibility.unsupported.len(), 1);
    assert_eq!(
        analysis.compatibility.unsupported[0].feature,
        "study.resolution"
    );
    assert!(analysis.compatibility.legacy_translations.is_empty());
    assert!(analysis.hir.is_none());
}

#[test]
fn v4_study_dynamic_resolution_and_gaps_stay_fail_closed() {
    for source in [
        "//@version=4\nrequested = \"D\"\nstudy(\"MTF\", resolution=requested)\nplot(close)\n",
        "//@version=4\ngapPolicy = true\nstudy(\"MTF\", resolution=\"\", resolution_gaps=gapPolicy)\nplot(close)\n",
        "//@version=4\nstudy(\"MTF\", resolution_gaps=true)\nplot(close)\n",
    ] {
        let analysis = analyze_production(source);
        assert_eq!(diagnostic_codes(&analysis), vec!["E_UNSUPPORTED_FEATURE"]);
        assert_eq!(analysis.compatibility.unsupported.len(), 1);
        assert_eq!(
            analysis.compatibility.unsupported[0].feature,
            "study.resolution"
        );
        assert!(analysis.hir.is_none());
    }
}

#[test]
fn v4_study_rejects_unmapped_declaration_options_before_lowering() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"z-order\", explicit_plot_zorder=true)\nplot(close)\n",
    );

    assert_eq!(diagnostic_codes(&analysis), vec!["E_UNSUPPORTED_FEATURE"]);
    assert_eq!(
        analysis.compatibility.unsupported[0].feature,
        "study.explicit_plot_zorder"
    );
    assert!(analysis.hir.is_none());
}

#[test]
fn v4_session_calls_lower_after_versioned_default_semantics_land() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"session\")\nplot(time_close(\"D\", \"0930-1600\"))\n",
    );

    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.compatibility.unsupported.is_empty());
    assert!(matches!(
        &plot_arg(&analysis).kind,
        HirExprKind::Call { callee, .. } if callee == "time_close"
    ));
}

#[test]
fn v4_iff_lowers_to_strict_internal_select_with_source_spanned_report() {
    let source = "//@version=4\nstudy(\"iff\")\nplot(iff(close > open, high, low))\n";
    let analysis = analyze_production(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let HirExprKind::Call { callee, args, .. } = &plot_arg(&analysis).kind else {
        panic!("expected strict iff call")
    };
    assert_eq!(callee, "$legacy.iff");
    assert_eq!(
        args.iter()
            .map(|arg| arg.name.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("condition"), Some("result1"), Some("result2")]
    );
    let translation = analysis
        .compatibility
        .legacy_translations
        .iter()
        .find(|translation| translation.source_feature == "iff")
        .expect("iff translation");
    assert_eq!(translation.kind, LegacyTranslationKind::ExpressionDesugar);
    assert_eq!(translation.span.start, source.find("iff(close").unwrap());
    assert!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .any(|emulation| emulation.feature == "iff")
    );
}

#[test]
fn v4_offset_lowers_to_native_history_and_retention_requirement() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"offset\")\nplot(offset(source=close, offset=2))\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(matches!(
        &plot_arg(&analysis).kind,
        HirExprKind::History {
            offset: HirHistoryOffset::Constant(2),
            ..
        }
    ));
    let hir = analysis.hir.expect("HIR");
    assert_eq!(hir.history.max_constant_offset, 2);
    assert!(
        hir.series_history
            .iter()
            .any(|requirement| requirement.max_constant_offset == 2)
    );
}

#[test]
fn v4_rsi_overload_selection_is_type_directed() {
    let length = analyze_production(
        "//@version=4\nstudy(\"length\")\nlength=input(2)\nplot(rsi(close, length))\n",
    );
    assert!(length.diagnostics.is_empty(), "{:?}", length.diagnostics);
    assert!(matches!(
        &plot_arg(&length).kind,
        HirExprKind::Call { callee, .. } if callee == "ta.rsi"
    ));

    for second_argument in ["2.0", "bar_index"] {
        let series = analyze_production(&format!(
            "//@version=4\nstudy(\"series overload\")\nplot(rsi(close, {second_argument}))\n"
        ));
        assert!(
            series.diagnostics.is_empty(),
            "{second_argument}: {:?}",
            series.diagnostics
        );
        assert!(
            matches!(
                &plot_arg(&series).kind,
                HirExprKind::Call { callee, .. } if callee == "$legacy.rsi_series"
            ),
            "{second_argument}"
        );
    }

    let series =
        analyze_production("//@version=4\nstudy(\"series\")\nplot(rsi(x=close, y=open))\n");
    assert!(series.diagnostics.is_empty(), "{:?}", series.diagnostics);
    assert!(matches!(
        &plot_arg(&series).kind,
        HirExprKind::Call { callee, .. } if callee == "$legacy.rsi_series"
    ));
    assert!(
        series
            .compatibility
            .legacy_emulations
            .iter()
            .any(|emulation| emulation.feature == "rsi")
    );

    let invalid =
        analyze_production("//@version=4\nstudy(\"invalid\")\nplot(rsi(close, close > open))\n");
    assert_eq!(diagnostic_codes(&invalid), vec!["E_LEGACY_RSI_OVERLOAD"]);
    assert!(invalid.hir.is_none());
}

#[test]
fn focused_legacy_calls_yield_to_user_defined_functions() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"shadow\")\niff(condition, result1, result2) => result1\nplot(iff(true, close, open))\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .all(|translation| translation.source_feature != "iff")
    );
    assert!(!matches!(
        &plot_arg(&analysis).kind,
        HirExprKind::Call { callee, .. } if callee == "$legacy.iff"
    ));
}

#[test]
fn production_aliases_still_yield_to_v4_user_functions_and_lexical_values() {
    let udf = analyze_production(
        "//@version=4\nstudy(\"collision\")\nsma(source, length) => source\nplot(sma(close, 2))\n",
    );
    assert!(udf.diagnostics.is_empty(), "{:?}", udf.diagnostics);
    assert_eq!(
        udf.compatibility
            .legacy_translations
            .iter()
            .map(|translation| translation.source_feature.as_str())
            .collect::<Vec<_>>(),
        vec!["study"]
    );

    let lexical = analyze_production(
        "//@version=4\nstudy(\"collision\")\nsma = close\nplot(sma(close, 2))\n",
    );
    assert_eq!(diagnostic_codes(&lexical), vec!["E_UNKNOWN_FUNCTION"]);
    assert!(
        lexical
            .compatibility
            .legacy_translations
            .iter()
            .all(|translation| translation.source_feature != "sma")
    );
}

#[test]
fn v3_v4_udf_calls_ignore_only_later_global_legacy_alias_collisions() {
    let later_fixture = include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/udf_source_order_builtin_aliases_legacy.pine"
    );
    let earlier_fixture = include_str!(
        "../../../../tests/fixtures/legacy/v4/unsupported/udf_earlier_legacy_alias_shadow.pine"
    );
    for version in [3, 4] {
        let later = analyze_production(&later_fixture.replacen(
            "//@version=4",
            &format!("//@version={version}"),
            1,
        ));
        assert!(
            later.diagnostics.is_empty(),
            "v{version}: {:?}",
            later.diagnostics
        );
        assert!(later.hir.is_some());
        assert_eq!(
            later
                .compatibility
                .legacy_translations
                .iter()
                .filter(|translation| translation.source_feature == "rsi")
                .count(),
            2
        );

        let earlier = analyze_production(&earlier_fixture.replacen(
            "//@version=4",
            &format!("//@version={version}"),
            1,
        ));
        assert!(
            diagnostic_codes(&earlier).contains(&"E_UNKNOWN_FUNCTION"),
            "v{version}: {:?}",
            earlier.diagnostics
        );
        assert!(
            earlier
                .compatibility
                .legacy_translations
                .iter()
                .all(|translation| translation.source_feature != "sma")
        );
    }
}

#[test]
fn expanded_exact_aliases_lower_across_their_declared_legacy_range() {
    for version in [1, 2, 3, 4] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nstudy(\"expanded aliases\")\ncrossed = cross(close, open)\nrounded = round(close)\nsmoothed = rma(close, 3)\nweighted = wma(close, 3)\nhigh_value = highest(high, 3)\nlow_value = lowest(low, 3)\nplot(crossed ? rounded : smoothed + weighted + high_value + low_value)\n"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.as_ref().expect("expanded alias HIR");
        for canonical in [
            "ta.cross",
            "math.round",
            "ta.rma",
            "ta.wma",
            "ta.highest",
            "ta.lowest",
        ] {
            assert!(
                hir_contains_call(hir, canonical),
                "v{version} missing canonical call {canonical}"
            );
        }
        for (source, canonical) in [
            ("cross", "ta.cross"),
            ("round", "math.round"),
            ("rma", "ta.rma"),
            ("wma", "ta.wma"),
            ("highest", "ta.highest"),
            ("lowest", "ta.lowest"),
        ] {
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == source
                        && translation.canonical_feature == canonical
                        && translation.kind == LegacyTranslationKind::ExactAlias)
            );
        }
    }
}

#[test]
fn second_exact_alias_batch_lowers_across_the_declared_legacy_range() {
    for version in [1, 2, 3, 4] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nstudy(\"second alias batch\")\nchanged = change(close)\nmagnitude = abs(close - open)\nmaximum = max(close, open)\nminimum = min(close, open)\ncrossed = crossover(close, open)\nrooted = sqrt(magnitude)\ndeviation = stdev(close, 3)\nvolume_weighted = vwma(close, 3)\nplot(crossed ? changed + magnitude + maximum + minimum + rooted + deviation + volume_weighted : 0)\n"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.as_ref().expect("second alias batch HIR");
        for canonical in [
            "ta.change",
            "math.abs",
            "math.max",
            "math.min",
            "ta.crossover",
            "math.sqrt",
            "ta.stdev",
            "ta.vwma",
        ] {
            assert!(
                hir_contains_call(hir, canonical),
                "v{version} missing canonical call {canonical}"
            );
        }
        for (source, canonical) in [
            ("change", "ta.change"),
            ("abs", "math.abs"),
            ("max", "math.max"),
            ("min", "math.min"),
            ("crossover", "ta.crossover"),
            ("sqrt", "math.sqrt"),
            ("stdev", "ta.stdev"),
            ("vwma", "ta.vwma"),
        ] {
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == source
                        && translation.canonical_feature == canonical
                        && translation.kind == LegacyTranslationKind::ExactAlias)
            );
        }
    }
}

#[test]
fn third_exact_alias_batch_lowers_across_the_declared_legacy_range() {
    for version in [1, 2, 3, 4] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nstudy(\"third alias batch\")\npivot_high = pivothigh(1, 1)\npivot_low = pivotlow(low, 1, 1)\nvolatility = atr(3)\naverage = avg(close, open)\nfloored = floor(close)\nregression = linreg(close, 3, 0)\noscillator = stoch(close, high, low, 3)\ntotal = sum(close, 3)\nplot(nz(pivot_high) + nz(pivot_low) + volatility + average + floored + regression + oscillator + total)\n"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.as_ref().expect("third alias batch HIR");
        for canonical in [
            "ta.pivothigh",
            "ta.pivotlow",
            "ta.atr",
            "math.avg",
            "math.floor",
            "ta.linreg",
            "ta.stoch",
            "math.sum",
        ] {
            assert!(
                hir_contains_call(hir, canonical),
                "v{version} missing canonical call {canonical}"
            );
        }
        for (source, canonical) in [
            ("pivothigh", "ta.pivothigh"),
            ("pivotlow", "ta.pivotlow"),
            ("atr", "ta.atr"),
            ("avg", "math.avg"),
            ("floor", "math.floor"),
            ("linreg", "ta.linreg"),
            ("stoch", "ta.stoch"),
            ("sum", "math.sum"),
        ] {
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == source
                        && translation.canonical_feature == canonical
                        && translation.kind == LegacyTranslationKind::ExactAlias)
            );
        }
    }
}

#[test]
fn third_exact_alias_batch_preserves_user_function_precedence() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"third alias collisions\")\natr(length) => length\navg(left, right) => left + right\nfloor(value) => value\nlinreg(value, length, offset) => value + length + offset\npivothigh(left, right) => left + right\npivotlow(left, right) => left - right\nstoch(value, upper, lower, length) => value + upper + lower + length\nsum(value, length) => value + length\nplot(atr(3) + avg(close, open) + floor(close) + linreg(close, 3, 0) + pivothigh(1, 1) + pivotlow(1, 1) + stoch(close, high, low, 3) + sum(close, 3))\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    for source in [
        "atr",
        "avg",
        "floor",
        "linreg",
        "pivothigh",
        "pivotlow",
        "stoch",
        "sum",
    ] {
        assert!(
            analysis
                .compatibility
                .legacy_translations
                .iter()
                .all(|translation| translation.source_feature != source),
            "unexpected fallback translation for {source}"
        );
    }
}

#[test]
fn fourth_exact_alias_batch_lowers_across_the_declared_legacy_range() {
    for version in [1, 2, 3, 4] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nstudy(\"fourth alias batch\")\nbars_since = barssince(close > open)\ncrossed_under = crossunder(close, open)\nheikin_ticker = heikinashi(\"NYSE:IBM\")\ndecimal_log = log10(close)\n[macd_line, signal_line, histogram] = macd(close, 2, 3, 2)\ndirection = sign(close - open)\nprevious = valuewhen(close > open, close, 0)\nplot(nz(bars_since) + (crossed_under ? 1 : 0) + (heikin_ticker != \"\" ? 1 : 0) + decimal_log + macd_line + signal_line + histogram + direction + nz(previous))\n"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.as_ref().expect("fourth alias batch HIR");
        for canonical in [
            "ta.barssince",
            "ta.crossunder",
            "ticker.heikinashi",
            "math.log10",
            "ta.macd",
            "math.sign",
            "ta.valuewhen",
        ] {
            assert!(
                hir_contains_call(hir, canonical),
                "v{version} missing canonical call {canonical}"
            );
        }
        for (source, canonical) in [
            ("barssince", "ta.barssince"),
            ("crossunder", "ta.crossunder"),
            ("heikinashi", "ticker.heikinashi"),
            ("log10", "math.log10"),
            ("macd", "ta.macd"),
            ("sign", "math.sign"),
            ("valuewhen", "ta.valuewhen"),
        ] {
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == source
                        && translation.canonical_feature == canonical
                        && translation.kind == LegacyTranslationKind::ExactAlias)
            );
        }
    }
}

#[test]
fn v4_tostring_reshapes_historical_parameter_names() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"legacy tostring\")\nplain = tostring(close)\nformatted = tostring(x=close, y=\"#.00\")\nplot((plain != \"\" ? 1 : 0) + (formatted != \"\" ? 1 : 0))\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(hir_contains_call(
        analysis.hir.as_ref().expect("legacy tostring HIR"),
        "str.tostring"
    ));
    assert_eq!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .filter(|translation| translation.source_feature == "tostring")
            .map(|translation| (translation.canonical_feature.as_str(), translation.kind))
            .collect::<Vec<_>>(),
        vec![
            ("str.tostring", LegacyTranslationKind::SignatureReshape),
            ("str.tostring", LegacyTranslationKind::SignatureReshape),
        ]
    );

    for version in [1, 2] {
        let unavailable = analyze_production(&format!(
            "//@version={version}\nstudy(\"tostring boundary\")\nplot(tostring(close) != \"\" ? 1 : 0)\n"
        ));
        assert_eq!(diagnostic_codes(&unavailable), vec!["E_UNKNOWN_FUNCTION"]);
        assert!(
            unavailable
                .compatibility
                .legacy_translations
                .iter()
                .all(|translation| translation.source_feature != "tostring")
        );
    }
}

#[test]
fn v3_open_close_strategy_math_and_string_aliases_are_available() {
    let analysis = analyze_production(
        "//@version=3\nstudy(\"v3 aliases\")\nplot(alma(close, 8, 0.85, 6) + exp(-1.0) + cos(1.0) + (tostring(3, \"###D\") == \"3D\" ? 1 : 0))\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
}

#[test]
fn v4_tostring_reshape_matches_canonical_hir() {
    let legacy = "//@version=4\nstudy(\"legacy tostring parity\")\nplain = tostring(close)\nformatted = tostring(x=close, y=\"#.00\")\nplot((plain != \"\" ? 1 : 0) + (formatted != \"\" ? 1 : 0))\n";
    let canonical = "//@version=5\nindicator(title=\"legacy tostring parity\")\nplain = str.tostring(close)\nformatted = str.tostring(value=close, format=\"#.00\")\nplot((plain != \"\" ? 1 : 0) + (formatted != \"\" ? 1 : 0))\n";

    assert_eq!(normalized_hir(legacy), normalized_hir(canonical));
}

#[test]
fn fourth_alias_batch_preserves_user_function_precedence() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"fourth alias collisions\")\nbarssince(condition) => condition ? 1 : 0\ncrossunder(left, right) => left < right\nheikinashi(value) => value\nlog10(value) => value\nmacd(value, fast, slow, signal) => [value, fast, slow + signal]\nsign(value) => value\ntostring(value) => \"shadowed\"\nvaluewhen(condition, value, occurrence) => condition ? value : occurrence\n[macd_line, signal_line, histogram] = macd(close, 2, 3, 2)\nrendered = tostring(close)\nplot(barssince(close > open) + (crossunder(close, open) ? 1 : 0) + (heikinashi(\"NYSE:IBM\") != \"\" ? 1 : 0) + log10(close) + macd_line + signal_line + histogram + sign(close) + (rendered != \"\" ? 1 : 0) + valuewhen(true, close, 0))\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    for source in [
        "barssince",
        "crossunder",
        "heikinashi",
        "log10",
        "macd",
        "sign",
        "tostring",
        "valuewhen",
    ] {
        assert!(
            analysis
                .compatibility
                .legacy_translations
                .iter()
                .all(|translation| translation.source_feature != source),
            "unexpected fallback translation for {source}"
        );
    }
}

#[test]
fn fifth_exact_alias_batch_lowers_across_the_declared_legacy_range() {
    for version in [1, 2, 3, 4] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nstudy(\"fifth alias batch\")\ncommodity = cci(close, 3)\nrounded_up = ceil(close)\nnatural_log = log(close)\nmoney_flow = mfi(hlc3, 3)\nmomentum = mom(close, 2)\npowered = pow(close, 2)\ntrue_range_value = tr\ntrue_range_call = tr(true)\nbalance_volume = obv\nvwap_value = vwap\nvwap_call = vwap(close)\nplot(commodity + rounded_up + natural_log + money_flow + momentum + powered + nz(true_range_value) + true_range_call + nz(balance_volume) + nz(vwap_value) + nz(vwap_call))\n"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.as_ref().expect("fifth alias batch HIR");
        for canonical in [
            "ta.cci",
            "math.ceil",
            "math.log",
            "ta.mfi",
            "ta.mom",
            "math.pow",
            "ta.tr",
            "ta.vwap",
        ] {
            assert!(
                hir_contains_call(hir, canonical),
                "v{version} missing canonical call {canonical}"
            );
        }
        for (source, canonical) in [
            ("cci", "ta.cci"),
            ("ceil", "math.ceil"),
            ("log", "math.log"),
            ("mfi", "ta.mfi"),
            ("mom", "ta.mom"),
            ("pow", "math.pow"),
            ("tr", "ta.tr"),
        ] {
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == source
                        && translation.canonical_feature == canonical
                        && translation.kind == LegacyTranslationKind::ExactAlias),
                "v{version} missing exact translation {source} -> {canonical}"
            );
        }
        for (source, canonical) in [("tr", "ta.tr"), ("obv", "ta.obv"), ("vwap", "ta.vwap")] {
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == source
                        && translation.canonical_feature == canonical
                        && translation.kind == LegacyTranslationKind::SymbolAlias),
                "v{version} missing symbol translation {source} -> {canonical}"
            );
        }
        assert!(
            analysis
                .compatibility
                .legacy_translations
                .iter()
                .any(|translation| translation.source_feature == "vwap"
                    && translation.canonical_feature == "ta.vwap"
                    && translation.kind == LegacyTranslationKind::SignatureReshape)
        );
    }
}

#[test]
fn legacy_vwap_restricts_and_reshapes_the_historical_single_source_signature() {
    let legacy = "//@version=4\nstudy(\"legacy vwap parity\")\npositional = vwap(close)\nnamed = vwap(x=hlc3)\nplot(positional + named)\n";
    let canonical = "//@version=5\nindicator(title=\"legacy vwap parity\")\npositional = ta.vwap(close)\nnamed = ta.vwap(source=hlc3)\nplot(positional + named)\n";
    assert_eq!(normalized_hir(legacy), normalized_hir(canonical));

    let invalid = analyze_production(
        "//@version=4\nstudy(\"legacy vwap boundary\")\nplot(vwap(close, true))\n",
    );
    assert_eq!(diagnostic_codes(&invalid), vec!["E_CALL_ARITY"]);
    assert!(invalid.hir.is_none());
}

#[test]
fn fifth_alias_batch_preserves_user_function_and_value_precedence() {
    let functions = analyze_production(
        "//@version=4\nstudy(\"fifth alias function collisions\")\ncci(source, length) => source\nceil(value) => value\nlog(value) => value\nmfi(source, length) => source\nmom(source, length) => source\npow(base, exponent) => base\ntr(handle_na) => handle_na ? 1.0 : 0.0\nvwap(source) => source\nplot(cci(close, 3) + ceil(close) + log(close) + mfi(close, 3) + mom(close, 2) + pow(close, 2) + tr(true) + vwap(close))\n",
    );
    assert!(
        functions.diagnostics.is_empty(),
        "{:?}",
        functions.diagnostics
    );
    for source in ["cci", "ceil", "log", "mfi", "mom", "pow", "tr", "vwap"] {
        assert!(
            functions
                .compatibility
                .legacy_translations
                .iter()
                .all(|translation| translation.source_feature != source),
            "unexpected function fallback translation for {source}"
        );
    }

    let values = analyze_production(
        "//@version=4\nstudy(\"fifth alias value collisions\")\ntr = 1.0\nobv = 2.0\nvwap = 3.0\nplot(tr + obv + vwap)\n",
    );
    assert!(values.diagnostics.is_empty(), "{:?}", values.diagnostics);
    for source in ["tr", "obv", "vwap"] {
        assert!(
            values
                .compatibility
                .legacy_translations
                .iter()
                .all(|translation| translation.source_feature != source),
            "unexpected value fallback translation for {source}"
        );
    }
}

#[test]
fn pre_v4_cross_resolves_call_and_style_constant_by_context() {
    let analysis = analyze_production(
        "//@version=3\nstudy(\"cross ambiguity\")\nplot(cross(close, open) ? 1 : 0)\nplot(close, style=cross)\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(hir_contains_call(
        analysis.hir.as_ref().expect("cross ambiguity HIR"),
        "ta.cross"
    ));
    assert!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .any(|translation| translation.source_feature == "cross"
                && translation.canonical_feature == "ta.cross"
                && translation.kind == LegacyTranslationKind::ExactAlias)
    );
    assert!(
        analysis
            .compatibility
            .legacy_translations
            .iter()
            .any(|translation| translation.source_feature == "cross"
                && translation.canonical_feature == "plot.style_cross"
                && translation.kind == LegacyTranslationKind::SymbolAlias)
    );

    let shadowed = analyze_production(
        "//@version=3\nstudy(\"cross shadowing\")\ncross(left, right) => true\nplot(cross(close, open) ? 1 : 0)\nplot(close, style=cross)\n",
    );
    assert!(
        shadowed.diagnostics.is_empty(),
        "{:?}",
        shadowed.diagnostics
    );
    assert!(!hir_contains_call(
        shadowed.hir.as_ref().expect("shadowed cross HIR"),
        "ta.cross"
    ));
    assert!(
        shadowed
            .compatibility
            .legacy_translations
            .iter()
            .all(|translation| translation.canonical_feature != "ta.cross")
    );
    assert!(
        shadowed
            .compatibility
            .legacy_translations
            .iter()
            .any(|translation| translation.source_feature == "cross"
                && translation.canonical_feature == "plot.style_cross"
                && translation.kind == LegacyTranslationKind::SymbolAlias)
    );
}

#[test]
fn modern_sources_reject_every_production_legacy_alias() {
    for version in [5, 6] {
        for alias_call in [
            "sma(close, 2)",
            "ema(close, 2)",
            "bb(close, 2, 2)",
            "change(close)",
            "cross(close, open)",
            "crossover(close, open)",
            "highest(high, 2)",
            "lowest(low, 2)",
            "rma(close, 2)",
            "round(close)",
            "sqrt(close)",
            "stdev(close, 2)",
            "vwma(close, 2)",
            "wma(close, 2)",
            "hma(close, 4)",
            "max(close, open)",
            "min(close, open)",
            "abs(close)",
            "atr(2)",
            "avg(close, open)",
            "floor(close)",
            "linreg(close, 2, 0)",
            "pivothigh(1, 1)",
            "pivotlow(1, 1)",
            "stoch(close, high, low, 2)",
            "sum(close, 2)",
            "barssince(close > open)",
            "cci(close, 2)",
            "ceil(close)",
            "crossunder(close, open)",
            "heikinashi(\"NYSE:IBM\")",
            "log(close)",
            "log10(close)",
            "macd(close, 2, 3, 2)",
            "mfi(hlc3, 2)",
            "mom(close, 2)",
            "pow(close, 2)",
            "sign(close)",
            "tostring(close)",
            "tr(true)",
            "valuewhen(close > open, close, 0)",
            "vwap(close)",
            "iff(true, close, open)",
            "offset(close, 1)",
            "rsi(close, 2)",
        ] {
            let analysis = analyze_production(&format!(
                "//@version={version}\nindicator(\"modern\")\nplot({alias_call})\n"
            ));
            assert_eq!(diagnostic_codes(&analysis), vec!["E_UNKNOWN_FUNCTION"]);
            assert!(analysis.compatibility.legacy_translations.is_empty());
        }
    }

    for version in [5, 6] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nindicator(\"modern values\")\nplot(tr + obv + vwap)\n"
        ));
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_UNKNOWN_SYMBOL", "E_UNKNOWN_SYMBOL", "E_UNKNOWN_SYMBOL"]
        );
        assert!(analysis.compatibility.legacy_translations.is_empty());
    }
}

#[test]
fn hma_alias_is_pine_v4_only() {
    let v4 = analyze_production("//@version=4\nstudy(\"legacy hma\")\nplot(hma(close, 4))\n");
    assert!(v4.diagnostics.is_empty(), "{:?}", v4.diagnostics);
    assert!(hir_contains_call(
        v4.hir.as_ref().expect("legacy hma HIR"),
        "ta.hma"
    ));

    let v3 = analyze_production("//@version=3\nstudy(\"legacy hma\")\nplot(hma(close, 4))\n");
    assert_eq!(diagnostic_codes(&v3), vec!["E_UNKNOWN_FUNCTION"]);
}

#[test]
fn exact_alias_applies_across_only_its_declared_version_range() {
    for directive in [None, Some(2), Some(3), Some(4)] {
        let source = match directive {
            Some(version) => format!("//@version={version}\nplot(sma(close, 2))\n"),
            None => "plot(sma(close, 2))\n".to_owned(),
        };
        let analysis = analyze_legacy(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert_eq!(analysis.compatibility.legacy_translations.len(), 1);
    }

    let v4_symbol = analyze_legacy("//@version=4\nplot(tickerid == \"\" ? 1 : 0)\n");
    assert_eq!(diagnostic_codes(&v4_symbol), vec!["E_UNKNOWN_SYMBOL"]);
    assert!(v4_symbol.compatibility.legacy_translations.is_empty());
}

#[test]
fn user_defined_function_wins_over_exact_alias() {
    let analysis =
        analyze_legacy("//@version=4\nsma(source, length) => source\nplot(sma(close, 2))\n");
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.compatibility.legacy_translations.is_empty());
    assert!(!matches!(
        &plot_arg(&analysis).kind,
        HirExprKind::Call { callee, .. } if callee == "ta.sma"
    ));
}

#[test]
fn lexical_value_prevents_function_alias_fallback() {
    let analysis = analyze_legacy("//@version=4\nsma = close\nplot(sma(close, 2))\n");
    assert_eq!(diagnostic_codes(&analysis), vec!["E_UNKNOWN_FUNCTION"]);
    assert!(analysis.compatibility.legacy_translations.is_empty());
}

#[test]
fn exact_symbol_alias_is_fallback_and_lowers_to_canonical_builtin() {
    let source = "//@version=3\nplot(tickerid == \"\" ? 1 : 0)\n";
    let analysis = analyze_legacy(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let HirExprKind::Ternary { condition, .. } = &plot_arg(&analysis).kind else {
        panic!("expected ternary plot expression")
    };
    let HirExprKind::Binary { left, .. } = &condition.kind else {
        panic!("expected string comparison")
    };
    assert!(matches!(
        &left.kind,
        HirExprKind::Builtin(name) if name == "syminfo.tickerid"
    ));
    let translation = analysis
        .compatibility
        .legacy_translations
        .first()
        .expect("translation");
    assert_eq!(translation.kind, LegacyTranslationKind::SymbolAlias);
    assert_eq!(translation.span.start, source.find("tickerid").unwrap());
}

#[test]
fn v4_label_style_aliases_lower_to_canonical_dynamic_enums() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"legacy label styles\", overlay=true)\nupStyle = label.style_labelup\ndownStyle = label.style_labeldown\nstyle = close >= open ? upStyle : downStyle\nid = label.new(bar_index, high, \"legacy\", style=style)\nlabel.set_style(id, style)\nplot(close)\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
    for (source, canonical) in [
        ("label.style_labelup", "label.style_label_up"),
        ("label.style_labeldown", "label.style_label_down"),
    ] {
        assert!(
            analysis
                .compatibility
                .legacy_translations
                .iter()
                .any(|translation| translation.source_feature == source
                    && translation.canonical_feature == canonical
                    && translation.kind == LegacyTranslationKind::SymbolAlias),
            "missing translation {source} -> {canonical}: {:?}",
            analysis.compatibility.legacy_translations
        );
    }

    for version in [5, 6] {
        let modern = analyze_production(&format!(
            "//@version={version}\nindicator(\"modern label styles\")\nplot(label.style_labelup == label.style_labeldown ? close : open)\n"
        ));
        assert!(
            !modern.diagnostics.is_empty()
                && diagnostic_codes(&modern)
                    .iter()
                    .all(|code| *code == "E_UNSUPPORTED_FEATURE"),
            "v{version}: {:?}",
            modern.diagnostics
        );
        assert!(modern.hir.is_none());
        assert!(modern.compatibility.legacy_translations.is_empty());
    }
}

#[test]
fn v4_string_input_options_bound_drawing_enum_values() {
    let bounded = analyze_production(
        "//@version=4\nstudy(\"bounded drawing input\")\nstyle = input(line.style_solid, \"Style\", input.string, false, [line.style_solid, line.style_dashed])\nline.new(bar_index, low, bar_index + 1, high, style=style)\nplot(close)\n",
    );
    assert!(bounded.diagnostics.is_empty(), "{:?}", bounded.diagnostics);
    assert!(bounded.hir.is_some());

    let unbounded = analyze_production(
        "//@version=4\nstudy(\"unbounded drawing input\")\nstyle = input(line.style_solid, \"Style\", input.string)\nline.new(bar_index, low, bar_index + 1, high, style=style)\nplot(close)\n",
    );
    assert!(
        unbounded.diagnostics.is_empty(),
        "{:?}",
        unbounded.diagnostics
    );
    assert!(unbounded.hir.is_some());
}

#[test]
fn v4_plot_style_accepts_proven_string_domains() {
    let ternary = analyze_production(
        "//@version=4\nstudy(\"dynamic plot style\")\nstyle = bar_index % 2 == 0 ? plot.style_line : plot.style_histogram\nplot(close, style=style)\n",
    );
    assert!(ternary.diagnostics.is_empty(), "{:?}", ternary.diagnostics);
    assert!(ternary.hir.is_some());

    let bounded = analyze_production(
        "//@version=4\nstudy(\"bounded plot style\")\nstyle = input(plot.style_line, \"Style\", input.string, false, [plot.style_line, plot.style_histogram])\nplot(close, style=style)\n",
    );
    assert!(bounded.diagnostics.is_empty(), "{:?}", bounded.diagnostics);
    assert!(bounded.hir.is_some());

    let unbounded = analyze_production(
        "//@version=4\nstudy(\"unbounded plot style\")\nstyle = input(plot.style_line, \"Style\", input.string)\nplot(close, style=style)\n",
    );
    assert_eq!(
        diagnostic_codes(&unbounded),
        vec!["E_LEGACY_OUTPUT_ARGUMENT"]
    );
    assert!(unbounded.hir.is_none());

    let series_int = analyze_production(
        "//@version=4\nstudy(\"series int plot style\")\nplot(close, style=bar_index)\n",
    );
    assert_eq!(
        diagnostic_codes(&series_int),
        vec!["E_LEGACY_OUTPUT_ARGUMENT"]
    );
    assert!(series_int.hir.is_none());

    let hline_domain = analyze_production(
        "//@version=4\nstudy(\"dynamic hline style\")\nstyle = close > open ? hline.style_solid : hline.style_dotted\nhline(1, linestyle=style)\nplot(close)\n",
    );
    assert!(
        hline_domain.diagnostics.is_empty(),
        "{:?}",
        hline_domain.diagnostics
    );
    assert!(hline_domain.hir.is_some());
}

#[test]
fn lexical_symbol_wins_over_symbol_alias() {
    let analysis =
        analyze_legacy("//@version=3\ntickerid = \"local\"\nplot(tickerid == \"local\" ? 1 : 0)\n");
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.compatibility.legacy_translations.is_empty());
}

#[test]
fn unsupported_known_and_unknown_calls_remain_distinct() {
    let unsupported = analyze_legacy("//@version=4\nplot(iff(true, close, open))\n");
    assert_eq!(
        diagnostic_codes(&unsupported),
        vec!["E_UNSUPPORTED_FEATURE"]
    );
    assert_eq!(unsupported.compatibility.unsupported[0].feature, "iff");

    let unknown = analyze_legacy("//@version=4\nplot(mystery(close))\n");
    assert_eq!(diagnostic_codes(&unknown), vec!["E_UNKNOWN_FUNCTION"]);
    assert!(unknown.compatibility.unsupported.is_empty());
}

#[test]
fn timenow_is_a_host_backed_series_int_in_legacy_and_modern_dialects() {
    for (version, declaration) in [(4, "study"), (6, "indicator")] {
        let source = format!(
            "//@version={version}\n{declaration}(\"clock\")\nelapsed = timenow - time\ninside = elapsed <= 60000 and elapsed > 0\nplot(inside ? 1 : 0)\n"
        );
        let analysis = analyze_production(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert!(analysis.compatibility.unsupported.is_empty());
        let hir = analysis
            .hir
            .expect("timenow should lower to executable HIR");
        let symbol = hir.timenow_symbol.expect("timenow symbol metadata");
        let symbol = hir
            .symbols
            .iter()
            .find(|candidate| candidate.id == symbol)
            .expect("lowered timenow symbol");
        assert_eq!(symbol.name, "timenow");
        assert_eq!(
            symbol.pine_type,
            pine_ir::PineType::new(pine_ir::Qualifier::Series, pine_ir::ValueKind::Int)
        );
    }
}

#[test]
fn modern_dialects_do_not_activate_legacy_aliases() {
    for version in [5, 6] {
        let source = format!("//@version={version}\nindicator(\"modern\")\nplot(sma(close, 2))\n");
        let analysis = crate::analysis::analyze_source_with_legacy_rules(
            &SourceFile::new("modern-control.pine", source),
            TEST_RULES,
        );
        assert_eq!(diagnostic_codes(&analysis), vec!["E_UNKNOWN_FUNCTION"]);
        assert!(analysis.compatibility.legacy_translations.is_empty());
    }
}

#[test]
fn late_canonical_namespace_is_rejected_in_legacy_source() {
    let analysis = analyze_legacy("//@version=4\nplot(ta.sma(close, 2))\n");
    assert_eq!(
        diagnostic_codes(&analysis),
        vec!["E_LEGACY_VERSION_FEATURE"]
    );
    assert!(analysis.compatibility.legacy_translations.is_empty());
    assert!(analysis.hir.is_none());
}

#[test]
fn source_builtin_names_are_gated_by_legacy_dialect_before_registry_lookup() {
    for source in [
        "//@version=2\nstudy(\"no arrays\")\nvalues = array.new_float()\nplot(close)\n",
        "//@version=2\nstudy(\"no ta namespace\")\nplot(ta.sma(close, 2))\n",
        "//@version=3\nstudy(\"no color namespace\")\nplot(close, color=color.red)\n",
        "//@version=4\nstudy(\"no ta namespace\")\nplot(ta.sma(close, 2))\n",
    ] {
        let analysis = analyze_production(source);
        assert!(
            diagnostic_codes(&analysis).contains(&"E_LEGACY_VERSION_FEATURE"),
            "{:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    let v4_qualified_control = analyze_production(
        "//@version=4\nstudy(\"v4 namespace\")\nplot(close, color=color.new(color.red, 10))\n",
    );
    assert!(
        v4_qualified_control.diagnostics.is_empty(),
        "{:?}",
        v4_qualified_control.diagnostics
    );
    assert!(v4_qualified_control.hir.is_some());
}

#[test]
fn qualified_builtin_version_inventory_covers_registered_namespaces() {
    let missing_call_names = pine_builtins::PHASE_1_BUILTINS
        .iter()
        .map(|signature| signature.name)
        .filter(|name| name.contains('.'))
        .filter(|name| PineDialect::qualified_builtin_min_version(name, true).is_none())
        .collect::<Vec<_>>();
    assert!(
        missing_call_names.is_empty(),
        "unclassified registered qualified calls: {missing_call_names:?}"
    );

    let missing_value_names = pine_builtins::registered_value_names()
        .filter(|name| name.contains('.'))
        .filter(|name| PineDialect::qualified_builtin_min_version(name, false).is_none())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        missing_value_names.is_empty(),
        "unclassified registered qualified values: {missing_value_names:?}"
    );

    for (name, is_call, expected) in [
        ("barstate.isfirst", false, 1),
        ("barstate.isconfirmed", false, 3),
        ("barstate.islastconfirmedhistory", false, 4),
        ("currency.USD", false, 1),
        ("currency.BTC", false, 5),
        ("strategy.long", false, 1),
        ("strategy.opentrades.capital_held", false, 5),
        ("strategy.closedtrades.first_index", false, 6),
        ("strategy.account_currency", false, 5),
        ("strategy.closedtrades.entry_price", true, 5),
        ("strategy.convert_to_account", true, 5),
        ("barmerge.gaps_on", false, 3),
        ("location.abovebar", false, 1),
        ("shape.circle", false, 1),
        ("scale.left", false, 2),
        ("size.normal", false, 3),
        ("adjustment.none", false, 3),
        ("session.regular", false, 3),
        ("session.ismarket", false, 4),
        ("session.isfirstbar_regular", false, 5),
        ("alert.freq_all", false, 4),
        ("array.new_float", true, 4),
        ("array.first", true, 5),
        ("box.set_xloc", true, 6),
        ("label.set_text_formatting", true, 6),
        ("box.set_text_formatting", true, 6),
        ("table.cell_set_text_formatting", true, 6),
        ("color.new", true, 4),
        ("dayofweek.monday", false, 4),
        ("display.all", false, 4),
        ("display.pane", false, 5),
        ("input.integer", false, 4),
        ("order.ascending", false, 4),
        ("position.top_left", false, 4),
        ("syminfo.mintick", false, 3),
        ("syminfo.tickerid", false, 4),
        ("syminfo.country", false, 5),
        ("syminfo.main_tickerid", false, 6),
        ("timeframe.period", false, 4),
        ("timeframe.isticks", false, 5),
        ("timeframe.main_period", false, 6),
        ("input.int", true, 5),
        ("syminfo.prefix", true, 5),
        ("timeframe.change", true, 5),
        ("font.family_default", false, 5),
        ("ta.sma", true, 5),
        ("ta.rci", true, 6),
        ("math.pi", false, 4),
        ("text.align_left", false, 4),
        ("text.wrap_auto", false, 5),
        ("text.format_bold", false, 6),
        ("currency.BDT", false, 6),
    ] {
        assert_eq!(
            PineDialect::qualified_builtin_min_version(name, is_call),
            Some(expected),
            "{name}"
        );
    }

    assert_registered_value_namespace_versions(
        "barstate",
        &[
            ("barstate.isfirst", 1),
            ("barstate.islast", 1),
            ("barstate.islastconfirmedhistory", 4),
            ("barstate.isnew", 1),
            ("barstate.isconfirmed", 3),
            ("barstate.ishistory", 1),
            ("barstate.isrealtime", 1),
        ],
    );
    assert_registered_value_namespace_versions(
        "session",
        &[
            ("session.extended", 3),
            ("session.regular", 3),
            ("session.ismarket", 4),
            ("session.ispremarket", 4),
            ("session.ispostmarket", 4),
            ("session.isfirstbar", 5),
            ("session.islastbar", 5),
            ("session.isfirstbar_regular", 5),
            ("session.islastbar_regular", 5),
        ],
    );
    assert_registered_value_namespace_versions(
        "syminfo",
        &[
            ("syminfo.basecurrency", 4),
            ("syminfo.currency", 4),
            ("syminfo.description", 4),
            ("syminfo.country", 5),
            ("syminfo.industry", 5),
            ("syminfo.main_tickerid", 6),
            ("syminfo.prefix", 3),
            ("syminfo.root", 3),
            ("syminfo.session", 3),
            ("syminfo.sector", 5),
            ("syminfo.ticker", 4),
            ("syminfo.tickerid", 4),
            ("syminfo.timezone", 3),
            ("syminfo.type", 4),
            ("syminfo.volumetype", 5),
            ("syminfo.mintick", 3),
            ("syminfo.mincontract", 6),
            ("syminfo.pointvalue", 3),
            ("syminfo.minmove", 5),
            ("syminfo.pricescale", 5),
        ],
    );
    assert_registered_value_namespace_versions(
        "timeframe",
        &[
            ("timeframe.period", 4),
            ("timeframe.main_period", 6),
            ("timeframe.isticks", 5),
            ("timeframe.isseconds", 4),
            ("timeframe.isminutes", 4),
            ("timeframe.isintraday", 4),
            ("timeframe.isdaily", 4),
            ("timeframe.isweekly", 4),
            ("timeframe.ismonthly", 4),
            ("timeframe.isdwm", 4),
            ("timeframe.multiplier", 4),
        ],
    );
    assert_registered_value_namespace_versions(
        "display",
        &[
            ("display.all", 4),
            ("display.none", 4),
            ("display.pane", 5),
            ("display.price_scale", 5),
            ("display.status_line", 5),
            ("display.data_window", 5),
            ("display.pine_screener", 6),
        ],
    );
    assert_registered_value_namespace_versions(
        "text",
        &[
            ("text.align_left", 4),
            ("text.align_center", 4),
            ("text.align_right", 4),
            ("text.align_top", 4),
            ("text.align_bottom", 4),
            ("text.wrap_none", 5),
            ("text.wrap_auto", 5),
            ("text.format_none", 6),
            ("text.format_bold", 6),
            ("text.format_italic", 6),
        ],
    );
    assert_registered_value_namespace_versions(
        "math",
        &[
            ("math.e", 4),
            ("math.pi", 4),
            ("math.phi", 4),
            ("math.rphi", 4),
        ],
    );
    assert_registered_value_namespace_versions(
        "currency",
        &[
            ("currency.AUD", 1),
            ("currency.BDT", 6),
            ("currency.BHD", 6),
            ("currency.BRL", 6),
            ("currency.BTC", 5),
            ("currency.CAD", 1),
            ("currency.CHF", 1),
            ("currency.CLP", 6),
            ("currency.CNY", 6),
            ("currency.COP", 6),
            ("currency.CZK", 6),
            ("currency.DKK", 6),
            ("currency.EGP", 6),
            ("currency.ETH", 5),
            ("currency.EUR", 1),
            ("currency.GBP", 1),
            ("currency.HKD", 1),
            ("currency.HUF", 6),
            ("currency.IDR", 6),
            ("currency.ILS", 6),
            ("currency.INR", 5),
            ("currency.ISK", 6),
            ("currency.JPY", 1),
            ("currency.KES", 6),
            ("currency.KRW", 5),
            ("currency.KWD", 6),
            ("currency.LKR", 6),
            ("currency.MAD", 6),
            ("currency.MXN", 6),
            ("currency.MYR", 5),
            ("currency.NGN", 6),
            ("currency.NONE", 1),
            ("currency.NOK", 1),
            ("currency.NZD", 1),
            ("currency.PEN", 6),
            ("currency.PHP", 6),
            ("currency.PKR", 6),
            ("currency.PLN", 6),
            ("currency.QAR", 6),
            ("currency.RON", 6),
            ("currency.RSD", 6),
            ("currency.RUB", 1),
            ("currency.SAR", 6),
            ("currency.SEK", 1),
            ("currency.SGD", 1),
            ("currency.THB", 6),
            ("currency.TND", 6),
            ("currency.TRY", 1),
            ("currency.TWD", 6),
            ("currency.USD", 1),
            ("currency.USDT", 5),
            ("currency.VES", 6),
            ("currency.VND", 6),
            ("currency.ZAR", 1),
        ],
    );
}

#[test]
fn mixed_qualified_namespace_versions_preserve_legacy_members_and_gate_later_ones() {
    let v2 = analyze_production(
        "//@version=2\nstudy(\"v2 qualified options\")\nlegacyOptions = location.abovebar == location.abovebar and shape.circle == shape.circle and scale.left == scale.left and currency.USD == currency.USD\nplot(legacyOptions ? close : open)\n",
    );
    assert!(v2.diagnostics.is_empty(), "{:?}", v2.diagnostics);
    assert!(v2.hir.is_some());

    let v3 = analyze_production(
        "//@version=3\nstudy(\"v3 qualified options\")\nlegacyOptions = size.normal == size.normal and adjustment.none == adjustment.none and session.regular == session.regular and barstate.isconfirmed\nplot(legacyOptions ? syminfo.mintick : syminfo.pointvalue)\n",
    );
    assert!(v3.diagnostics.is_empty(), "{:?}", v3.diagnostics);
    assert!(v3.hir.is_some());

    for (version, name) in [
        (2, "adjustment.none"),
        (2, "size.normal"),
        (2, "barstate.islastconfirmedhistory"),
        (4, "session.isfirstbar_regular"),
        (4, "syminfo.main_tickerid"),
        (4, "syminfo.country"),
        (4, "timeframe.main_period"),
        (4, "currency.BTC"),
        (4, "display.pane"),
        (5, "syminfo.main_tickerid"),
        (5, "syminfo.mincontract"),
        (5, "timeframe.main_period"),
        (5, "text.format_bold"),
        (5, "currency.BDT"),
        (5, "strategy.closedtrades.first_index"),
    ] {
        let declaration = if version <= 4 {
            "study(\"late qualified value\")"
        } else {
            "indicator(\"late qualified value\")"
        };
        let analysis = analyze_production(&format!(
            "//@version={version}\n{declaration}\nvalue = {name}\nplot(close)\n"
        ));
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LEGACY_VERSION_FEATURE"],
            "v{version} {name}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    let v4 = analyze_production(
        "//@version=4\nstudy(\"v4 mixed namespaces\")\na = session.ismarket\nb = syminfo.tickerid\nc = timeframe.period\nd = display.all\ne = text.align_left\nf = math.pi\nplot(a ? close : open)\n",
    );
    assert!(v4.diagnostics.is_empty(), "{:?}", v4.diagnostics);
    assert!(v4.hir.is_some());

    let v5 = analyze_production(
        "//@version=5\nstrategy(\"v5 mixed namespaces\")\na = session.isfirstbar_regular\nb = syminfo.country\nc = currency.BTC\nd = display.pane\ne = text.align_left\nf = timeframe.isticks\ng = strategy.opentrades.capital_held\nplot(a ? close : open)\n",
    );
    assert!(v5.diagnostics.is_empty(), "{:?}", v5.diagnostics);
    assert!(v5.hir.is_some());

    let v6 = analyze_production(
        "//@version=6\nstrategy(\"v6 mixed namespaces\")\na = syminfo.main_tickerid\nb = syminfo.mincontract\nc = timeframe.main_period\nd = text.format_bold\ne = currency.BDT\nf = strategy.closedtrades.first_index\ng = ta.rci(close, 2)\nplot(close)\n",
    );
    assert!(v6.diagnostics.is_empty(), "{:?}", v6.diagnostics);
    assert!(v6.hir.is_some());

    let v5_box_set_xloc = analyze_production(
        "//@version=5\nindicator(\"late box set_xloc\")\nbox.set_xloc(na, 0, 1, xloc.bar_index)\nplot(close)\n",
    );
    assert_eq!(
        diagnostic_codes(&v5_box_set_xloc),
        vec!["E_LEGACY_VERSION_FEATURE"]
    );
    assert!(v5_box_set_xloc.hir.is_none());

    let v6_additions = analyze_production(
        "//@version=6\nstrategy(\"v6 exact additions\")\nid = box.new(bar_index, high, bar_index + 1, low)\nbox.set_xloc(id, bar_index - 1, bar_index + 1, xloc.bar_index)\nplot(strategy.opentrades.capital_held + strategy.closedtrades.first_index)\n",
    );
    assert!(
        v6_additions.diagnostics.is_empty(),
        "{:?}",
        v6_additions.diagnostics
    );
    assert!(v6_additions.hir.is_some());
}

#[test]
fn drawing_v6_calls_and_parameters_do_not_leak_into_v5() {
    for source in [
        "//@version=5\nindicator(\"late label setter\")\nlabel.set_text_formatting(na, 1)\n",
        "//@version=5\nindicator(\"late box setter\")\nbox.set_text_formatting(na, 1)\n",
        "//@version=5\nindicator(\"late table setter\")\ntable.cell_set_text_formatting(na, 0, 0, 1)\n",
        "//@version=5\nindicator(\"late box xloc\")\nbox.set_xloc(na, 0, 1, xloc.bar_index)\n",
        "//@version=5\nindicator(\"late label method\")\nid = label.new(bar_index, high)\nid.set_text_formatting(1)\n",
        "//@version=5\nindicator(\"late box method\")\nid = box.new(bar_index, high, bar_index + 1, low)\nid.set_text_formatting(1)\n",
        "//@version=5\nindicator(\"late table method\")\nid = table.new(position.top_right, 1, 1)\nid.cell_set_text_formatting(0, 0, 1)\n",
        "//@version=5\nindicator(\"late box xloc method\")\nid = box.new(bar_index, high, bar_index + 1, low)\nid.set_xloc(0, 1, xloc.bar_index)\n",
        "//@version=5\nindicator(\"late label parameter\")\nlabel.new(bar_index, high, text_formatting=1)\n",
        "//@version=5\nindicator(\"late box parameter\")\nbox.new(bar_index, high, bar_index + 1, low, text_formatting=1)\n",
        "//@version=5\nindicator(\"late table parameter\")\nid = table.new(position.top_right, 1, 1)\ntable.cell(id, 0, 0, \"x\", text_formatting=1)\n",
        "//@version=5\nindicator(\"late positional parameter\")\nlabel.new(bar_index, high, \"x\", xloc.bar_index, yloc.price, color.red, label.style_none, color.white, size.normal, text.align_left, \"\", font.family_default, false, 1)\n",
        "//@version=5\nindicator(\"late integer size\")\nlabel.new(bar_index, high, size=12)\n",
    ] {
        let analysis = analyze_production(source);
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LEGACY_VERSION_FEATURE"],
            "{source}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    let v6 = analyze_production(
        "//@version=6\nindicator(\"v6 drawing additions\")\nlabel_id = label.new(bar_index, high, text_formatting=1, size=12)\nlabel_id.set_text_formatting(2)\nbox_id = box.new(bar_index, high, bar_index + 1, low, text_formatting=1, text_size=12)\nbox_id.set_text_formatting(2)\nbox_id.set_xloc(bar_index, bar_index + 1, xloc.bar_index)\ntable_id = table.new(position.top_right, 1, 1)\ntable.cell(table_id, 0, 0, \"x\", text_formatting=1, text_size=12)\ntable_id.cell_set_text_formatting(0, 0, 2)\nplot(close)\n",
    );
    assert!(v6.diagnostics.is_empty(), "{:?}", v6.diagnostics);
    assert!(v6.hir.is_some());
}

#[test]
fn drawing_v5_overloads_and_parameters_do_not_leak_into_v4() {
    let named_cases = [
        (
            "label point overload",
            "//@version=4\nlabel.new(point=na)\nplot(close)\n",
        ),
        (
            "label text_font_family",
            "//@version=4\nlabel.new(bar_index, high, text_font_family=\"font.family_default\")\nplot(close)\n",
        ),
        (
            "label force_overlay",
            "//@version=4\nlabel.new(bar_index, high, force_overlay=false)\nplot(close)\n",
        ),
        (
            "line point overload",
            "//@version=4\nline.new(first_point=na, second_point=na)\nplot(close)\n",
        ),
        (
            "line force_overlay",
            "//@version=4\nline.new(bar_index, low, bar_index + 1, high, force_overlay=false)\nplot(close)\n",
        ),
        (
            "box point overload",
            "//@version=4\nbox.new(top_left=na, bottom_right=na)\nplot(close)\n",
        ),
        (
            "box text",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, text=\"x\")\nplot(close)\n",
        ),
        (
            "box text_size",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, text_size=size.small)\nplot(close)\n",
        ),
        (
            "box text_color",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, text_color=color.white)\nplot(close)\n",
        ),
        (
            "box text_halign",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, text_halign=text.align_left)\nplot(close)\n",
        ),
        (
            "box text_valign",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, text_valign=text.align_top)\nplot(close)\n",
        ),
        (
            "box text_wrap",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, text_wrap=\"text.wrap_none\")\nplot(close)\n",
        ),
        (
            "box text_font_family",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, text_font_family=\"font.family_default\")\nplot(close)\n",
        ),
        (
            "box force_overlay",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, force_overlay=false)\nplot(close)\n",
        ),
        (
            "table force_overlay",
            "//@version=4\ntable.new(position.top_right, 1, 1, force_overlay=false)\nplot(close)\n",
        ),
        (
            "table cell tooltip",
            "//@version=4\nid = table.new(position.top_right, 1, 1)\ntable.cell(id, 0, 0, \"x\", tooltip=\"tip\")\nplot(close)\n",
        ),
        (
            "table cell text_font_family",
            "//@version=4\nid = table.new(position.top_right, 1, 1)\ntable.cell(id, 0, 0, \"x\", text_font_family=\"font.family_default\")\nplot(close)\n",
        ),
    ];
    let positional_cases = [
        (
            "label point overload positional",
            "//@version=4\nchart.point p = na\nlabel.new(p)\nplot(close)\n",
        ),
        (
            "label text_font_family positional",
            "//@version=4\nlabel.new(bar_index, high, \"x\", xloc.bar_index, yloc.price, color.blue, label.style_none, color.white, size.normal, text.align_left, \"tip\", \"font.family_default\")\nplot(close)\n",
        ),
        (
            "line point overload positional",
            "//@version=4\nchart.point p = na\nchart.point q = na\nline.new(p, q)\nplot(close)\n",
        ),
        (
            "line force_overlay positional",
            "//@version=4\nline.new(bar_index, low, bar_index + 1, high, xloc.bar_index, extend.none, color.blue, line.style_solid, 1, false)\nplot(close)\n",
        ),
        (
            "box point overload positional",
            "//@version=4\nchart.point p = na\nchart.point q = na\nbox.new(p, q)\nplot(close)\n",
        ),
        (
            "box text positional",
            "//@version=4\nbox.new(bar_index, high, bar_index + 1, low, color.blue, 1, line.style_solid, extend.none, xloc.bar_index, color.white, \"x\")\nplot(close)\n",
        ),
        (
            "table force_overlay positional",
            "//@version=4\ntable.new(position.top_right, 1, 1, color.black, color.black, 1, color.black, 1, false)\nplot(close)\n",
        ),
        (
            "table cell tooltip positional",
            "//@version=4\nid = table.new(position.top_right, 1, 1)\ntable.cell(id, 0, 0, \"x\", 0, 0, color.white, text.align_left, text.align_top, size.small, color.black, \"tip\")\nplot(close)\n",
        ),
    ];

    for (case, source) in named_cases.into_iter().chain(positional_cases) {
        let v4_source = source.replacen(
            "//@version=4\n",
            "//@version=4\nstudy(\"drawing v5 gate\")\n",
            1,
        );
        let v4 = analyze_production(&v4_source);
        assert_eq!(
            diagnostic_codes(&v4),
            vec!["E_LEGACY_VERSION_FEATURE"],
            "{case}: {:?}",
            v4.diagnostics
        );
        assert!(v4.hir.is_none(), "{case}");

        let v5_source = source.replacen(
            "//@version=4\n",
            "//@version=5\nindicator(\"drawing v5 gate\")\n",
            1,
        );
        let v5 = analyze_production(&v5_source);
        assert!(v5.diagnostics.is_empty(), "{case}: {:?}", v5.diagnostics);
        assert!(v5.hir.is_some(), "{case}");
    }
}

#[test]
fn strategy_introspection_namespace_starts_at_its_registered_version() {
    for source in [
        "//@version=4\nstudy(\"late strategy value\")\nvalue = strategy.account_currency\nplot(close)\n",
        "//@version=4\nstudy(\"late strategy call\")\nstrategy.closedtrades.entry_price(0)\nplot(close)\n",
    ] {
        let analysis = analyze_catalog_without_admission(source);
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LEGACY_VERSION_FEATURE"],
            "{source}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    let v5 = analyze_production(
        "//@version=5\nstrategy(\"v5 strategy introspection\")\nconverted = strategy.convert_to_account(close)\nentry = strategy.closedtrades.entry_price(0)\nplot(converted + entry + strategy.avg_trade)\n",
    );
    assert!(v5.diagnostics.is_empty(), "{:?}", v5.diagnostics);
    assert!(v5.hir.is_some());
}

#[test]
fn builtin_method_syntax_starts_in_v5_without_moving_v4_namespace_calls() {
    let direct = analyze_production(
        "//@version=4\nstudy(\"direct drawing call\")\nlabel.set_x(na, bar_index)\nplot(close)\n",
    );
    assert!(direct.diagnostics.is_empty(), "{:?}", direct.diagnostics);
    assert!(direct.hir.is_some());

    let method = analyze_production(
        "//@version=4\nstudy(\"drawing method\")\nid = label.new(bar_index, high)\nid.set_x(bar_index)\nplot(close)\n",
    );
    assert_eq!(diagnostic_codes(&method), vec!["E_LEGACY_VERSION_FEATURE"]);
    assert!(method.hir.is_none());

    let array_direct = analyze_production(
        "//@version=4\nstudy(\"direct array call\")\nvalues = array.new_float()\narray.push(values, close)\nplot(array.size(values))\n",
    );
    assert!(
        array_direct.diagnostics.is_empty(),
        "{:?}",
        array_direct.diagnostics
    );
    assert!(array_direct.hir.is_some());

    let array_method = analyze_production(
        "//@version=4\nstudy(\"array method\")\nvalues = array.new_float()\nvalues.push(close)\nplot(array.size(values))\n",
    );
    assert_eq!(
        diagnostic_codes(&array_method),
        vec!["E_LEGACY_VERSION_FEATURE"]
    );
    assert!(array_method.hir.is_none());

    for source in [
        "//@version=4\nstudy(\"array call-result method\")\nvalues = array.new_float()\nplot(array.copy(values).size())\n",
        "//@version=4\nstudy(\"array call-result mutation\")\nvalues = array.new_float()\narray.copy(values).push(close)\nplot(close)\n",
    ] {
        let analysis = analyze_production(source);
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LEGACY_VERSION_FEATURE"],
            "{source}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    let array_call_result_v5 = analyze_production(
        "//@version=5\nindicator(\"array call-result methods\")\nvalues = array.new_float()\narray.copy(values).push(close)\nplot(array.copy(values).size())\n",
    );
    assert!(
        array_call_result_v5.diagnostics.is_empty(),
        "{:?}",
        array_call_result_v5.diagnostics
    );
    assert!(array_call_result_v5.hir.is_some());
}

#[test]
fn v3_requires_the_legacy_tickerid_alias_instead_of_the_v4_syminfo_spelling() {
    let qualified = analyze_production(
        "//@version=3\nstudy(\"qualified tickerid\")\nsyminfo.tickerid\nplot(close)\n",
    );
    assert_eq!(
        diagnostic_codes(&qualified),
        vec!["E_LEGACY_VERSION_FEATURE"]
    );
    assert!(qualified.hir.is_none());

    let alias = analyze_production(
        "//@version=3\nstudy(\"legacy tickerid\")\nvalue = tickerid\nplot(close)\n",
    );
    assert!(alias.diagnostics.is_empty(), "{:?}", alias.diagnostics);
    assert!(alias.compatibility.legacy_translations.iter().any(|item| {
        item.source_feature == "tickerid" && item.canonical_feature == "syminfo.tickerid"
    }));
    assert!(alias.hir.is_some());
}

#[test]
fn pre_v3_named_arguments_are_limited_to_annotations() {
    let annotation = analyze_production(
        "//@version=2\nstudy(title=\"named annotations\")\nperiod = input(defval=\"D\", title=\"Timeframe\", type=resolution)\nplot(series=close, title=\"Close\")\nalertcondition(condition=close > open, title=\"Rise\", message=\"rise\")\nalertcondition(close < open, \"Fall\", \"fall\")\n",
    );
    assert!(
        annotation.diagnostics.is_empty(),
        "{:?}",
        annotation.diagnostics
    );
    assert!(annotation.hir.is_some());

    for source in [
        "//@version=2\nstudy(\"named sma\")\nplot(sma(source=close, length=2))\n",
        "//@version=2\nstudy(\"named security\")\nplot(security(symbol=\"NYSE:IBM\", resolution=\"5\", expression=close))\n",
        "//@version=2\nstudy(\"named udf\")\npass(value) => value\nplot(pass(value=close))\n",
    ] {
        let analysis = analyze_production(source);
        assert!(
            diagnostic_codes(&analysis).contains(&"E_CALL_ARG_NAME"),
            "{:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    let shadowed_annotation = analyze_catalog_without_admission(
        "//@version=2\nstudy(value) => value\nplot(study(value=close))\n",
    );
    assert!(
        diagnostic_codes(&shadowed_annotation).contains(&"E_CALL_ARG_NAME"),
        "{:?}",
        shadowed_annotation.diagnostics
    );
    assert!(shadowed_annotation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E_CALL_ARG_NAME"
            && diagnostic.message.contains("user-defined function `study`")
    }));
    assert!(shadowed_annotation.hir.is_none());

    let shadowed_alertcondition = analyze_catalog_without_admission(
        "//@version=2\nalertcondition(value) => value\nplot(alertcondition(value=close))\n",
    );
    assert!(
        diagnostic_codes(&shadowed_alertcondition).contains(&"E_FUNCTION_NAME"),
        "{:?}",
        shadowed_alertcondition.diagnostics
    );
    assert!(shadowed_alertcondition.hir.is_none());

    for source in [
        "//@version=1\nstudy(\"v1 alertcondition\")\nalertcondition(close > open, \"Rise\", \"rise\")\n",
        "//@version=1\nstudy(\"v1 named alertcondition\")\nalertcondition(condition=close > open, title=\"Rise\", message=\"rise\")\n",
    ] {
        let analysis = analyze_production(source);
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_LEGACY_VERSION_FEATURE"]
        );
        assert!(analysis.hir.is_none());
    }

    let v3_control = analyze_production(
        "//@version=3\nstudy(title=\"v3 named builtins\")\nlength = input(defval=3)\nplot(sma(source=close, length=length))\n",
    );
    assert!(
        v3_control.diagnostics.is_empty(),
        "{:?}",
        v3_control.diagnostics
    );
    assert!(v3_control.hir.is_some());
}

#[test]
fn legacy_udf_calls_require_positional_arguments_through_v4() {
    for version in [1, 2, 3, 4] {
        let analysis = analyze_production(&format!(
            "//@version={version}\nstudy(\"named legacy udf\")\npass(value) => value\nplot(pass(value=close))\n"
        ));
        assert_eq!(diagnostic_codes(&analysis), vec!["E_CALL_ARG_NAME"]);
        assert!(
            analysis.diagnostics[0]
                .message
                .contains("user-defined function `pass`")
        );
        assert!(analysis.hir.is_none());
    }

    let modern = analyze_production(
        "//@version=5\nindicator(\"named modern udf\")\npass(value) => value\nplot(pass(value=close))\n",
    );
    assert!(modern.diagnostics.is_empty(), "{:?}", modern.diagnostics);
    assert!(modern.hir.is_some());
}

#[test]
fn production_and_synthetic_catalogs_validate_against_canonical_registries() {
    let production_errors = validate_catalog(crate::legacy::LEGACY_RULES);
    assert!(production_errors.is_empty(), "{production_errors:?}");
    assert!(validate_catalog(TEST_RULES).is_empty());

    const INVALID: &[LegacyRule] = &[LegacyRule {
        source_name: "bad",
        canonical_name: Some("ta.does_not_exist"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    }];
    let errors = validate_catalog(INVALID);
    assert!(
        matches!(errors.as_slice(), [CatalogValidationError(message)] if message.contains("not registered"))
    );

    const BAD_SYMBOL: &[LegacyRule] = &[LegacyRule {
        source_name: "bad_symbol",
        canonical_name: Some("syminfo.does_not_exist"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::ExactSymbolAlias,
        support: LegacyRuleSupport::Supported,
    }];
    assert!(
        validate_catalog(BAD_SYMBOL)
            .iter()
            .any(|error| error.0.contains("canonical symbol"))
    );

    const MODERN_LEAK: &[LegacyRule] = &[LegacyRule {
        source_name: "leak",
        canonical_name: Some("ta.sma"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V5,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    }];
    assert!(
        validate_catalog(MODERN_LEAK)
            .iter()
            .any(|error| error.0.contains("modern dialects"))
    );

    const OVERLAP: &[LegacyRule] = &[
        LegacyRule {
            source_name: "same",
            canonical_name: Some("ta.sma"),
            min_version: PineDialect::V1,
            max_version: PineDialect::V3,
            kind: LegacyRuleKind::ExactFunctionAlias,
            support: LegacyRuleSupport::Supported,
        },
        LegacyRule {
            source_name: "same",
            canonical_name: Some("ta.ema"),
            min_version: PineDialect::V3,
            max_version: PineDialect::V4,
            kind: LegacyRuleKind::ExactFunctionAlias,
            support: LegacyRuleSupport::Supported,
        },
    ];
    assert!(
        validate_catalog(OVERLAP)
            .iter()
            .any(|error| error.0.contains("overlapping"))
    );
}

#[test]
fn legacy_report_normalization_is_stable_and_deduplicated() {
    let first = LegacyTranslation {
        source_feature: "sma".to_owned(),
        canonical_feature: "ta.sma".to_owned(),
        kind: LegacyTranslationKind::ExactAlias,
        span: Span::new(20, 23),
    };
    let earlier = LegacyTranslation {
        source_feature: "tickerid".to_owned(),
        canonical_feature: "syminfo.tickerid".to_owned(),
        kind: LegacyTranslationKind::SymbolAlias,
        span: Span::new(10, 18),
    };
    let emulation = LegacyEmulation {
        feature: "session".to_owned(),
        behavior: "legacy default".to_owned(),
        span: Span::new(30, 37),
    };
    let mut report = CompatibilityReport {
        legacy_translations: vec![first.clone(), earlier.clone(), first],
        legacy_emulations: vec![emulation.clone(), emulation],
        ..CompatibilityReport::default()
    };
    crate::legacy::normalize_legacy_report(&mut report);
    assert_eq!(report.legacy_translations.len(), 2);
    assert_eq!(report.legacy_translations[0], earlier);
    assert_eq!(report.legacy_translations[1].source_feature, "sma");
    assert_eq!(report.legacy_emulations.len(), 1);
}

#[test]
fn v4_output_binder_accepts_historical_signatures_and_retains_hidden_semantics() {
    let source = r#"//@version=4
study("v4 outputs")
selectedStyle = input(1)
p1 = plot(close, "p1", color.blue, 2, selectedStyle, true, 40, 0, 1, true, true, 3, display.all)
p2 = plot(open)
plotchar(close > open, "char", "X", location.abovebar, color.red, 25)
plotshape(close > open, "shape", shape.circle, location.belowbar, color.green, 10)
plotarrow(close - open, "arrow", color.green, color.red, 35)
plotbar(open, high, low, close, "bars", color.blue)
plotcandle(open, high, low, close, "candles", color.blue, color.gray)
h1 = hline(1, "one", color.gray, 1)
h2 = hline(2)
fill(p1, p2, color.blue, 55)
fill(hline1=h1, hline2=h2, color=color.red)
bgcolor(color.blue, 45, title="bg")
barcolor(color.red, title="bars")
"#;
    let analysis = analyze_production(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let plot = top_level_call(&analysis, "plotchar");
    let HirExprKind::Call { args, .. } = &plot.kind else {
        unreachable!()
    };
    assert!(
        args.iter()
            .any(|arg| { arg.name.as_deref() == Some(pine_ir::LEGACY_TRANSPARENCY_ARG) })
    );

    let translations = analysis
        .compatibility
        .legacy_translations
        .iter()
        .filter(|translation| translation.kind == LegacyTranslationKind::OutputAdaptation)
        .collect::<Vec<_>>();
    assert_eq!(translations.len(), 8);
    assert!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .any(|item| { item.feature == "plot.transp" })
    );
    assert!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .any(|item| { item.feature == "plot.numeric_style" })
    );
    assert!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .any(|item| { item.feature == "hline.numeric_style" })
    );
}

#[test]
fn v4_plotshape_tolerates_identical_duplicate_literal_transparency_only() {
    let accepted = analyze_production(
        "//@version=4\nstudy(\"legacy\")\nplotshape(close > open, transp=0, color=color.green, transp=0)\n",
    );
    assert!(
        accepted.diagnostics.is_empty(),
        "{:?}",
        accepted.diagnostics
    );
    assert!(accepted.hir.is_some());

    for source in [
        "//@version=4\nstudy(\"conflict\")\nplotshape(close > open, transp=0, transp=10)\n",
        "//@version=4\nstudy(\"other argument\")\nplotshape(close > open, color=color.red, color=color.red)\n",
    ] {
        let analysis = analyze_production(source);
        assert!(
            diagnostic_codes(&analysis).contains(&"E_CALL_ARG_DUPLICATE"),
            "{source}: {:?}",
            analysis.diagnostics
        );
    }
    let modern = analyze_production(
        "//@version=5\nindicator(\"modern\")\nplotshape(close > open, transp=0, transp=0)\n",
    );
    assert!(diagnostic_codes(&modern).contains(&"E_CALL_ARG_NAME"));
}

#[test]
fn v4_v5_series_output_offsets_use_the_final_value_while_v3_v6_stay_strict() {
    for (version, source) in [
        (
            4,
            include_str!(
                "../../../../tests/fixtures/legacy/v4/runtime/series_output_offset_legacy.pine"
            ),
        ),
        (
            5,
            include_str!("../../../../tests/fixtures/runtime/v5_series_output_offset.pine"),
        ),
    ] {
        let analysis = analyze_production(source);
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_some());
        assert_eq!(
            analysis
                .compatibility
                .legacy_emulations
                .iter()
                .filter(|emulation| {
                    emulation.feature == format!("v{version}.series_output_offset")
                })
                .count(),
            6
        );
    }

    let v3 = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v3/unsupported/series_output_offset.pine"
    ));
    assert_eq!(
        diagnostic_codes(&v3)
            .into_iter()
            .filter(|code| *code == "E_CALL_ARG_TYPE")
            .count(),
        2
    );
    assert!(v3.hir.is_none());
    assert!(v3.compatibility.legacy_emulations.is_empty());

    let v6 = analyze_production(include_str!(
        "../../../../tests/fixtures/sema/unsupported_v6_series_output_offset.pine"
    ));
    assert_eq!(
        diagnostic_codes(&v6)
            .into_iter()
            .filter(|code| *code == "E_CALL_ARG_TYPE")
            .count(),
        6
    );
    assert!(v6.hir.is_none());
    assert!(v6.compatibility.legacy_emulations.is_empty());
}

#[test]
fn v1_v3_output_binder_accepts_the_documented_pre_v4_signatures() {
    for version_header in ["", "//@version=2\n", "//@version=3\n"] {
        let source = format!(
            r#"{version_header}study("pre-v4 outputs")
p1 = plot(close, "p1", blue, 2, columns, true, 40, 0, 1, true, true, 3)
p2 = plot(open)
plotchar(close > open, "char", "X", location.abovebar, red, transp=25, offset=1, text="Up", textcolor=white, editable=false, show_last=2)
plotshape(close < open, "shape", shape.circle, location.belowbar, green, transp=35, offset=-1, text="Dn", textcolor=white, editable=true, show_last=3)
plotarrow(close - open, "arrow", green, red, 45, 1, 5, 20, false, 2)
plotbar(open, high, low, close, "bars", blue, false, 2)
plotcandle(open, high, low, close, "candles", blue, gray, true, 3, black)
h1 = hline(10, "ten", gray, dotted, 2, false)
h2 = hline(20)
fill(p1, p2, green, 80, "plot fill", false, 2)
fill(h1, h2, red, 70, "hline fill", true)
bgcolor(close > open ? blue : na, 80, 1, false, 2, "background")
barcolor(close > open ? green : red, -1, false, 2, "bars")
"#
        );
        let analysis = analyze_production(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{version_header:?}: {:?}",
            analysis.diagnostics
        );
        assert!(
            analysis
                .compatibility
                .legacy_emulations
                .iter()
                .any(|item| item.feature == "plotchar.transp"),
            "{version_header:?}"
        );
        assert!(
            analysis
                .compatibility
                .legacy_emulations
                .iter()
                .any(|item| item.feature == "fill.transp"),
            "{version_header:?}"
        );
        assert!(
            analysis
                .compatibility
                .legacy_translations
                .iter()
                .any(|item| {
                    item.source_feature == "plotshape"
                        && item.kind == LegacyTranslationKind::OutputAdaptation
                }),
            "{version_header:?}"
        );
    }
}

#[test]
fn v1_v3_output_binder_rejects_later_only_parameters() {
    for version_header in ["", "//@version=2\n", "//@version=3\n"] {
        for call in [
            "plotshape(true, display=na)",
            "plotchar(true, display=na)",
            "plotarrow(1, display=na)",
            "plotbar(open, high, low, close, display=na)",
            "plotcandle(open, high, low, close, display=na)",
        ] {
            let source = format!("{version_header}study(\"invalid\")\n{call}\n");
            let analysis = analyze_production(&source);
            assert!(
                diagnostic_codes(&analysis).contains(&"E_CALL_ARG_NAME"),
                "{version_header:?}: {call}: {:?}",
                analysis.diagnostics
            );
            assert!(analysis.hir.is_none(), "{version_header:?}: {call}");
        }

        let source = format!(
            "{version_header}study(\"invalid fill\")\np1 = plot(close)\np2 = plot(open)\nfill(p1, p2, fillgaps=true)\n"
        );
        let analysis = analyze_production(&source);
        assert!(
            diagnostic_codes(&analysis).contains(&"E_CALL_ARG_NAME"),
            "{version_header:?}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none(), "{version_header:?}");
    }
}

#[test]
fn v4_output_binder_rejects_unsupported_arguments_and_invalid_legacy_values() {
    for (call, expected_code) in [
        ("plot(close, force_overlay=true)", "E_CALL_ARG_NAME"),
        ("plot(close, transp=close)", "E_LEGACY_OUTPUT_ARGUMENT"),
        ("plot(close, style=9)", "E_LEGACY_OUTPUT_ARGUMENT"),
        (
            "plot(close, style=\"invented\")",
            "E_LEGACY_OUTPUT_ARGUMENT",
        ),
        ("hline(1, linestyle=3)", "E_LEGACY_OUTPUT_ARGUMENT"),
    ] {
        let source = format!("//@version=4\nstudy(\"invalid\")\n{call}\n");
        let analysis = analyze_production(&source);
        assert!(
            diagnostic_codes(&analysis).contains(&expected_code),
            "{call}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none(), "{call}");
    }

    let mixed_fill = analyze_production(
        "//@version=4\nstudy(\"invalid fill\")\np = plot(close)\nh = hline(1)\nfill(p, h)\n",
    );
    assert_eq!(
        diagnostic_codes(&mixed_fill),
        vec!["E_LEGACY_OUTPUT_ARGUMENT"]
    );
}

#[test]
fn legacy_output_compatibility_does_not_weaken_modern_unique_types() {
    for version in [5, 6] {
        for call in ["plot(close, style=1)", "hline(1, linestyle=1)"] {
            let source = format!("//@version={version}\nindicator(\"modern\")\n{call}\n");
            let analysis = analyze_production(&source);
            assert!(!analysis.diagnostics.is_empty(), "{version}: {call}");
            assert!(analysis.compatibility.legacy_translations.is_empty());
            assert!(analysis.compatibility.legacy_emulations.is_empty());
        }
    }
    let deprecated_v5 = analyze_production(
        "//@version=5\nindicator(\"deprecated transparency\")\nplot(close, transp=40)\n",
    );
    assert!(deprecated_v5.diagnostics.is_empty());
    assert!(deprecated_v5.hir.is_some());
    let removed_v6 = analyze_production(
        "//@version=6\nindicator(\"removed transparency\")\nplot(close, transp=40)\n",
    );
    assert!(diagnostic_codes(&removed_v6).contains(&"E_CALL_ARG_NAME"));
}

#[test]
fn v4_strategy_entry_accepts_boolean_directions_without_weakening_v5_v6() {
    let legacy = analyze_production(
        "//@version=4\nstrategy(\"legacy direction\")\nstrategy.entry(\"L\", true)\nstrategy.entry(\"S\", false)\n",
    );
    assert!(legacy.diagnostics.is_empty(), "{:?}", legacy.diagnostics);
    assert!(legacy.hir.is_some());

    for version in [5, 6] {
        let source = format!(
            "//@version={version}\nstrategy(\"modern direction\")\nstrategy.entry(\"L\", true)\n"
        );
        let analysis = analyze_production(&source);
        assert!(
            diagnostic_codes(&analysis).contains(&"E_CALL_ARG_TYPE"),
            "{version}: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn v3_core_fixture_lowers_names_constants_declaration_and_na_to_canonical_hir() {
    let source = include_str!("../../../../tests/fixtures/legacy/v3/runtime/core_legacy.pine");
    let analysis = analyze_production(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.as_ref().expect("v3 core HIR");
    let value = hir
        .symbols
        .iter()
        .find(|symbol| symbol.name == "value")
        .expect("inferred v3 value symbol");
    assert_eq!(
        value.pine_type,
        PineType::new(Qualifier::Series, ValueKind::Float)
    );

    let translations = analysis
        .compatibility
        .legacy_translations
        .iter()
        .map(|translation| {
            (
                translation.source_feature.as_str(),
                translation.canonical_feature.as_str(),
            )
        })
        .collect::<Vec<_>>();
    for expected in [
        ("study", "indicator"),
        ("integer", "input.int"),
        ("input", "input.int"),
        ("ema", "ta.ema"),
        ("sma", "ta.sma"),
        ("red", "color.red"),
        ("color", "color.new"),
        ("histogram", "plot.style_histogram"),
        ("n", "bar_index"),
        ("interval", "timeframe.multiplier"),
        ("blue", "color.blue"),
        ("gray", "color.gray"),
        ("dotted", "hline.style_dotted"),
    ] {
        assert!(translations.contains(&expected), "missing {expected:?}");
    }
    assert_eq!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .filter(|emulation| emulation.feature == "v3.untyped_na")
            .count(),
        1
    );
}

#[test]
fn selected_pre_v4_constants_resolve_only_through_v3() {
    let aliases = [
        ("aqua", "color.aqua"),
        ("black", "color.black"),
        ("blue", "color.blue"),
        ("fuchsia", "color.fuchsia"),
        ("gray", "color.gray"),
        ("green", "color.green"),
        ("lime", "color.lime"),
        ("maroon", "color.maroon"),
        ("navy", "color.navy"),
        ("olive", "color.olive"),
        ("orange", "color.orange"),
        ("purple", "color.purple"),
        ("red", "color.red"),
        ("silver", "color.silver"),
        ("teal", "color.teal"),
        ("white", "color.white"),
        ("yellow", "color.yellow"),
        ("area", "plot.style_area"),
        ("areabr", "plot.style_areabr"),
        ("circles", "plot.style_circles"),
        ("columns", "plot.style_columns"),
        ("cross", "plot.style_cross"),
        ("histogram", "plot.style_histogram"),
        ("line", "plot.style_line"),
        ("linebr", "plot.style_linebr"),
        ("stepline", "plot.style_stepline"),
        ("dashed", "hline.style_dashed"),
        ("dotted", "hline.style_dotted"),
        ("solid", "hline.style_solid"),
        ("sunday", "dayofweek.sunday"),
        ("monday", "dayofweek.monday"),
        ("tuesday", "dayofweek.tuesday"),
        ("wednesday", "dayofweek.wednesday"),
        ("thursday", "dayofweek.thursday"),
        ("friday", "dayofweek.friday"),
        ("saturday", "dayofweek.saturday"),
        ("period", "timeframe.period"),
        ("isdaily", "timeframe.isdaily"),
        ("isdwm", "timeframe.isdwm"),
        ("isintraday", "timeframe.isintraday"),
        ("isminutes", "timeframe.isminutes"),
        ("isseconds", "timeframe.isseconds"),
        ("ismonthly", "timeframe.ismonthly"),
        ("isweekly", "timeframe.isweekly"),
        ("interval", "timeframe.multiplier"),
        ("ticker", "syminfo.ticker"),
        ("tickerid", "syminfo.tickerid"),
        ("n", "bar_index"),
        ("bool", "input.bool"),
        ("float", "input.float"),
        ("integer", "input.int"),
        ("resolution", "input.timeframe"),
        ("session", "input.session"),
        ("source", "input.source"),
        ("string", "input.string"),
        ("symbol", "input.symbol"),
    ];

    for version in 1..=3 {
        for (alias, canonical) in aliases {
            if alias == "isseconds" && version < 3 {
                continue;
            }
            let analysis = analyze_catalog_without_admission(&format!(
                "//@version={version}\nvalue = {alias}\n"
            ));
            assert!(
                analysis.diagnostics.is_empty(),
                "v{version} {alias}: {:?}",
                analysis.diagnostics
            );
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == alias
                        && translation.canonical_feature == canonical)
            );
        }
    }

    for version in 4..=6 {
        for (alias, _) in aliases {
            let analysis = analyze_catalog_without_admission(&format!(
                "//@version={version}\nvalue = {alias}\n"
            ));
            assert!(!analysis.diagnostics.is_empty(), "v{version} {alias}");
            assert!(analysis.compatibility.legacy_translations.is_empty());
        }
    }
}

#[test]
fn v3_color_helper_is_versioned_and_user_symbols_still_shadow_other_aliases() {
    for version in 1..=3 {
        let analysis = analyze_catalog_without_admission(&format!(
            "//@version={version}\nshade = color(red, 50)\n"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        assert!(
            analysis
                .compatibility
                .legacy_translations
                .iter()
                .any(|translation| translation.source_feature == "color"
                    && translation.canonical_feature == "color.new")
        );
    }

    let shadowed = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v3/sema/shadowing.pine"
    ));
    assert!(
        shadowed.diagnostics.is_empty(),
        "{:?}",
        shadowed.diagnostics
    );
    for name in [
        "ema",
        "red",
        "n",
        "interval",
        "integer",
        "histogram",
        "dotted",
        "monday",
        "period",
        "isdaily",
        "ticker",
        "tickerid",
    ] {
        assert!(
            shadowed
                .compatibility
                .legacy_translations
                .iter()
                .all(|translation| translation.source_feature != name),
            "shadowed {name} was translated"
        );
    }
}

#[test]
fn v3_declaration_input_and_output_signatures_stay_version_specific() {
    let adapted = analyze_production(
        "//@version=3\nstudy(\"adapted output\")\nplot(close, color=red, style=5, transp=25)\n",
    );
    assert!(adapted.diagnostics.is_empty(), "{:?}", adapted.diagnostics);
    for feature in ["plot.transp", "plot.numeric_style"] {
        assert!(
            adapted
                .compatibility
                .legacy_emulations
                .iter()
                .any(|item| { item.feature == feature && item.behavior.contains("Pine v3") })
        );
    }

    for source in [
        "//@version=3\nstudy(\"too wide\", \"wide\", false, 2, true)\nplot(close)\n",
        "//@version=3\nstudy(\"format\", format=format.price)\nplot(close)\n",
        "//@version=3\nstudy(\"input\")\nx=input(1, tooltip=\"new\")\nplot(x)\n",
        "//@version=3\nstudy(\"plot\")\nplot(close, display=display.all)\n",
        include_str!(
            "../../../../tests/fixtures/legacy/v3/unsupported/later_signature_arguments.pine"
        ),
    ] {
        let analysis = analyze_production(source);
        assert!(!analysis.diagnostics.is_empty(), "{source}");
        assert!(analysis.hir.is_none());
    }

    let v4 = analyze_production(
        "//@version=4\nstudy(\"v4\", format=format.price)\nx=input(1, tooltip=\"ok\")\nplot(x, display=display.all)\n",
    );
    assert!(v4.diagnostics.is_empty(), "{:?}", v4.diagnostics);
}

#[test]
fn v3_untyped_na_infers_only_one_stable_scalar_type() {
    let scalar = analyze_production(
        r#"//@version=3
study("v3 scalar na")
i = na
i := 1
f = na
f := close
b = na
b := true
s = na
s := "text"
c = na
c := red
plot(i + f + (b ? 1 : 0))
"#,
    );
    assert!(scalar.diagnostics.is_empty(), "{:?}", scalar.diagnostics);
    let hir = scalar.hir.expect("scalar v3 HIR");
    for (name, expected) in [
        ("i", PineType::new(Qualifier::Const, ValueKind::Int)),
        ("f", PineType::new(Qualifier::Series, ValueKind::Float)),
        ("b", PineType::new(Qualifier::Const, ValueKind::Bool)),
        ("s", PineType::new(Qualifier::Const, ValueKind::String)),
        ("c", PineType::new(Qualifier::Const, ValueKind::Color)),
    ] {
        assert_eq!(
            hir.symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .expect(name)
                .pine_type,
            expected,
            "{name}"
        );
    }

    for (source, expected) in [
        (
            include_str!(
                "../../../../tests/fixtures/legacy/v3/unsupported/untyped_na_unresolved.pine"
            ),
            vec!["E_LEGACY_V3_NA_INFERENCE"],
        ),
        (
            include_str!(
                "../../../../tests/fixtures/legacy/v3/unsupported/untyped_na_collection.pine"
            ),
            vec!["E_LEGACY_VERSION_FEATURE", "E_LEGACY_V3_NA_INFERENCE"],
        ),
        (
            include_str!(
                "../../../../tests/fixtures/legacy/v3/unsupported/untyped_na_conflict.pine"
            ),
            vec!["E_LEGACY_V3_NA_INFERENCE"],
        ),
    ] {
        let analysis = analyze_production(source);
        assert_eq!(
            diagnostic_codes(&analysis),
            expected,
            "{source}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_none());
    }

    for source in [
        "//@version=4\nstudy(\"strict v4\")\nvalue=na\nvalue:=close\nplot(close)\n",
        "//@version=6\nindicator(\"strict v6\")\nvalue=na\nvalue:=close\nplot(close)\n",
    ] {
        let analysis = analyze_production(source);
        assert!(!analysis.diagnostics.is_empty());
        assert!(
            diagnostic_codes(&analysis)
                .iter()
                .all(|code| *code != "E_LEGACY_V3_NA_INFERENCE")
        );
        assert!(analysis.compatibility.legacy_emulations.is_empty());
    }
}

#[test]
fn v2_declaration_graph_preserves_symbol_identity_and_stable_current_order() {
    let analysis = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v2/runtime/core_legacy.pine"
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.as_ref().expect("v2 HIR");
    let symbol_names = hir
        .symbols
        .iter()
        .map(|symbol| (symbol.id, symbol.name.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let declaration_names = hir
        .statements
        .iter()
        .filter_map(|statement| match statement.kind {
            HirStmtKind::Decl { symbol, .. } => symbol_names.get(&symbol).copied(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let later = declaration_names
        .iter()
        .position(|name| *name == "laterCurrent")
        .expect("later declaration");
    let consumer = declaration_names
        .iter()
        .position(|name| *name == "currentForward")
        .expect("forward consumer");
    assert!(later < consumer, "{declaration_names:?}");

    let self_symbol = hir
        .symbols
        .iter()
        .find(|symbol| symbol.name == "selfSeries")
        .expect("self symbol");
    assert!(self_symbol.series_id.is_some());
    let self_decl = hir
        .statements
        .iter()
        .find_map(|statement| match &statement.kind {
            HirStmtKind::Decl { symbol, value } if *symbol == self_symbol.id => Some(value),
            _ => None,
        })
        .expect("self declaration");
    assert!(hir_expr_contains_history_symbol(self_decl, self_symbol.id));

    for feature in [
        "v2.self_reference",
        "v2.forward_reference",
        "v2.bool_arithmetic",
        "v2.numeric_to_bool",
    ] {
        assert!(
            analysis
                .compatibility
                .legacy_emulations
                .iter()
                .any(|emulation| emulation.feature == feature),
            "missing {feature}"
        );
    }
    assert!(hir_contains_call(hir, "float"));
    assert!(hir_contains_call(hir, "bool"));
}

#[test]
fn v1_v2_allow_legacy_input_inside_udf_without_relaxing_v3() {
    let body = "study(\"local input\")\nf() =>\n    factor = input(0.5)\n    factor\nplot(f())\n";
    for source in [body.to_owned(), format!("//@version=2\n{body}")] {
        let analysis = analyze_production(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert!(analysis.hir.is_some());
    }
    let v3 = analyze_production(&format!("//@version=3\n{body}"));
    assert!(v3.hir.is_none());
    assert!(
        v3.compatibility
            .unsupported
            .iter()
            .any(|feature| feature.feature == "function_side_effect")
    );
}

#[test]
fn v2_declaration_graph_treats_positive_const_expression_offsets_as_history() {
    let analysis = analyze_production(
        "//@version=2\nstudy(\"const history offset\")\nselfSeries = nz(selfSeries[1 + 0]) + close\nplot(selfSeries)\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
    assert!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .any(|emulation| emulation.feature == "v2.self_reference")
    );
}

#[test]
fn v1_v2_declaration_graph_keeps_source_order_input_prerequisites_outside_reordering() {
    let implicit = include_str!(
        "../../../../tests/fixtures/legacy/v1/runtime/graph_source_order_prerequisite_legacy.pine"
    );
    for source in [implicit.to_owned(), format!("//@version=2\n{implicit}")] {
        let analysis = analyze_production(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis
            .hir
            .as_ref()
            .expect("source-order prerequisite HIR");
        let symbol_names = hir
            .symbols
            .iter()
            .map(|symbol| (symbol.id, symbol.name.as_str()))
            .collect::<std::collections::HashMap<_, _>>();
        let declarations = hir
            .statements
            .iter()
            .filter_map(|statement| match statement.kind {
                HirStmtKind::Decl { symbol, .. } => symbol_names.get(&symbol).copied(),
                _ => None,
            })
            .collect::<Vec<_>>();
        let length = declarations
            .iter()
            .position(|name| *name == "length")
            .expect("length declaration");
        let delta = declarations
            .iter()
            .position(|name| *name == "delta")
            .expect("delta declaration");
        let trend = declarations
            .iter()
            .position(|name| *name == "trend")
            .expect("trend declaration");
        let direction = declarations
            .iter()
            .position(|name| *name == "direction")
            .expect("direction declaration");
        assert!(
            length < delta && delta < trend && trend < direction,
            "{declarations:?}"
        );
        assert!(hir_contains_call(hir, "ta.rising"));
        assert!(hir_contains_call(hir, "ta.falling"));
    }

    let barrier = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v2/unsupported/forward_reference_unsafe_initializer_barrier.pine"
    ));
    assert_eq!(
        diagnostic_codes(&barrier),
        vec!["E_LEGACY_FORWARD_REFERENCE_UNSAFE"]
    );
    assert!(barrier.hir.is_none());
}

#[test]
fn rising_and_falling_aliases_are_exact_and_legacy_only() {
    for version in 1..=4 {
        let analysis = analyze_production(&format!(
            "//@version={version}\nstudy(\"legacy trend aliases\")\nup = rising(close, 2)\ndown = falling(close, 2)\nplot(up ? 1 : down ? -1 : 0)\n"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.as_ref().expect("legacy trend alias HIR");
        for (source, canonical) in [("rising", "ta.rising"), ("falling", "ta.falling")] {
            assert!(hir_contains_call(hir, canonical));
            assert!(
                analysis
                    .compatibility
                    .legacy_translations
                    .iter()
                    .any(|translation| translation.source_feature == source
                        && translation.canonical_feature == canonical
                        && translation.kind == LegacyTranslationKind::ExactAlias)
            );
        }
    }

    for version in 5..=6 {
        let analysis = analyze_production(&format!(
            "//@version={version}\nindicator(\"modern trend aliases\")\nplot(rising(close, 2) ? 1 : falling(close, 2) ? -1 : 0)\n"
        ));
        assert_eq!(
            diagnostic_codes(&analysis),
            vec!["E_UNKNOWN_FUNCTION", "E_UNKNOWN_FUNCTION"]
        );
        assert!(analysis.hir.is_none());
        assert!(analysis.compatibility.legacy_translations.is_empty());
    }
}

#[test]
fn v2_declaration_graph_rejects_cycles_barriers_unsafe_calls_and_limits_once() {
    for (source, expected) in [
        (
            include_str!("../../../../tests/fixtures/legacy/v2/unsupported/reference_cycle.pine"),
            "E_LEGACY_REFERENCE_CYCLE",
        ),
        (
            include_str!(
                "../../../../tests/fixtures/legacy/v2/unsupported/forward_reference_barrier.pine"
            ),
            "E_LEGACY_FORWARD_REFERENCE_UNSAFE",
        ),
        (
            include_str!("../../../../tests/fixtures/legacy/v2/unsupported/unsafe_graph_call.pine"),
            "E_LEGACY_REFERENCE_GRAPH_UNSAFE",
        ),
    ] {
        let analysis = analyze_production(source);
        assert_eq!(diagnostic_codes(&analysis), vec![expected]);
        assert!(analysis.hir.is_none());
    }

    let mut oversized = String::from(
        "//@version=2\nstudy(\"oversized graph\")\nfirst = node256 + 1\nnode0 = close\n",
    );
    for index in 1..=256 {
        oversized.push_str(&format!("node{index} = node{} + 1\n", index - 1));
    }
    oversized.push_str("plot(first)\n");
    let analysis = analyze_production(&oversized);
    assert_eq!(
        diagnostic_codes(&analysis),
        vec!["E_LEGACY_REFERENCE_GRAPH_LIMIT"]
    );
    assert!(analysis.hir.is_none());
}

#[test]
fn v2_declaration_graph_enforces_the_edge_limit_independently() {
    let mut oversized = String::from("//@version=2\nstudy(\"oversized edge graph\")\n");
    oversized.push_str("first = ");
    for index in 1..=100 {
        if index > 1 {
            oversized.push_str(" + ");
        }
        oversized.push_str(&format!("node{index}"));
    }
    oversized.push('\n');
    oversized.push_str("node0 = close\n");
    for index in 1..=100 {
        oversized.push_str(&format!("node{index} = node0"));
        for dependency in 1..index {
            oversized.push_str(&format!(" + node{dependency}"));
        }
        oversized.push('\n');
    }
    oversized.push_str("plot(first)\n");

    let analysis = analyze_production(&oversized);
    assert_eq!(
        diagnostic_codes(&analysis),
        vec!["E_LEGACY_REFERENCE_GRAPH_LIMIT"]
    );
    assert!(analysis.hir.is_none());
}

#[test]
fn v1_v3_integer_division_is_truncated_across_the_complete_expression() {
    for version in [1, 2, 3] {
        let source = format!(
            "//@version={version}\nstudy(\"Legacy integer division\")\nlength = input(5, title=\"Length\")\nlag = length / 2\nweighted(source, window) => wma(source, window)\nplot(wma(close, lag))\nplot(weighted(close, length / 2))\nplot(length / 2)\nplot(close[lag])\nplot(close[5 / 2])\n"
        );
        let analysis = analyze_production(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        assert_eq!(
            analysis
                .compatibility
                .legacy_emulations
                .iter()
                .filter(|emulation| { emulation.feature == format!("v{version}.integer_division") })
                .count(),
            4
        );
        let hir = analysis.hir.as_ref().expect("legacy integer division HIR");
        assert!(hir_contains_call(hir, "int"));
        let ordinary_division = hir
            .statements
            .iter()
            .filter_map(|statement| match &statement.kind {
                HirStmtKind::Expr(HirExpr {
                    kind: HirExprKind::Call { callee, args, .. },
                    ..
                }) if callee == "plot" => args.first().map(|arg| &arg.value),
                _ => None,
            })
            .nth(2)
            .expect("ordinary division plot");
        assert_eq!(
            ordinary_division.pine_type,
            PineType::new(Qualifier::Input, ValueKind::Int)
        );
        assert!(matches!(
            ordinary_division.kind,
            HirExprKind::Call { ref callee, .. } if callee == "int"
        ));
    }
}

#[test]
fn v4_integer_division_preserves_input_fractions_and_truncates_constants() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"v4 division\")\nlength = input(5)\nplot(length / 2)\nplot(5 / 2)\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .filter(|emulation| emulation.feature == "v4.integer_division")
            .count(),
        1
    );
    let hir = analysis.hir.as_ref().expect("v4 division HIR");
    let plots = hir
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            HirStmtKind::Expr(HirExpr {
                kind: HirExprKind::Call { callee, args, .. },
                ..
            }) if callee == "plot" => args.first().map(|arg| &arg.value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        plots[0].pine_type,
        PineType::new(Qualifier::Input, ValueKind::Float)
    );
    assert_eq!(
        plots[1].pine_type,
        PineType::new(Qualifier::Const, ValueKind::Int)
    );
}

#[test]
fn v4_ema_length_accepts_integer_operand_quotient_in_public_hull_pattern() {
    let analysis = analyze_production(
        "//@version=4\nstudy(\"Hull EMA length\")\nlength = input(55)\nehma(src, len) => ema(2 * ema(src, len / 2) - ema(src, len), round(sqrt(len)))\nplot(ehma(close, length))\nplot(length / 2)\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
    assert!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .any(|emulation| emulation.feature == "v4.ema_division_length")
    );
}

#[test]
fn v5_integer_division_depends_on_const_qualifiers() {
    let analysis = analyze_production(include_str!(
        "../../../../tests/fixtures/runtime/v5_const_integer_division.pine"
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .filter(|emulation| emulation.feature == "v5.integer_division")
            .count(),
        3
    );

    let hir = analysis.hir.as_ref().expect("v5 integer division HIR");
    let plot_values = hir
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            HirStmtKind::Expr(HirExpr {
                kind: HirExprKind::Call { callee, args, .. },
                ..
            }) if callee == "plot" => args.first().map(|arg| &arg.value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(plot_values.len(), 5);
    assert_eq!(
        plot_values[0].pine_type,
        PineType::new(Qualifier::Const, ValueKind::Int)
    );
    assert_eq!(plot_values[1].pine_type, plot_values[0].pine_type);
    assert_eq!(
        plot_values[2].pine_type,
        PineType::new(Qualifier::Input, ValueKind::Float)
    );
    assert_eq!(
        plot_values[3].pine_type,
        PineType::new(Qualifier::Series, ValueKind::Float)
    );
    assert!(matches!(
        plot_values[4].kind,
        HirExprKind::History {
            offset: HirHistoryOffset::Constant(2),
            ..
        }
    ));

    let const_udf = analyze_production(
        "//@version=5\nindicator(\"v5 const UDF division\")\nhalf(value) => value / 2\nplot(half(5))\n",
    );
    assert!(
        const_udf.diagnostics.is_empty(),
        "{:?}",
        const_udf.diagnostics
    );
    assert!(
        const_udf
            .compatibility
            .legacy_emulations
            .iter()
            .any(|emulation| emulation.feature == "v5.integer_division")
    );

    let input_udf = analyze_production(
        "//@version=5\nindicator(\"v5 input UDF division\")\nvalue = input.int(5)\nhalf(argument) => argument / 2\nplot(half(value))\n",
    );
    assert!(
        input_udf.diagnostics.is_empty(),
        "{:?}",
        input_udf.diagnostics
    );
    assert!(
        input_udf
            .compatibility
            .legacy_emulations
            .iter()
            .all(|emulation| emulation.feature != "v5.integer_division")
    );

    let input_history = analyze_production(
        "//@version=5\nindicator(\"v5 input history division\")\ndivisor = input.int(2)\nplot(close[5 / divisor])\n",
    );
    assert!(
        input_history.diagnostics.is_empty(),
        "{:?}",
        input_history.diagnostics
    );
    assert!(
        input_history
            .compatibility
            .legacy_emulations
            .iter()
            .all(|emulation| emulation.feature != "v5.integer_division")
    );
}

#[test]
fn integer_division_rejects_float_operands_and_nonconst_modern_qualifiers() {
    let legacy_float = analyze_production(
        "//@version=4\nstudy(\"float lengths\")\nlength=input(5)\nplot(sma(close, length / 2.0))\nplot(sma(close, 5.0))\n",
    );
    assert_eq!(
        diagnostic_codes(&legacy_float),
        vec!["E_CALL_ARG_TYPE", "E_CALL_ARG_TYPE"]
    );
    assert!(
        legacy_float
            .compatibility
            .legacy_emulations
            .iter()
            .all(|emulation| !emulation.feature.contains("integer_division"))
    );

    let legacy_wma = analyze_production(
        "//@version=4\nstudy(\"wma float lengths\")\nlength=input(5)\nplot(wma(close, length / 2.0))\nplot(wma(close, 5.0))\n",
    );
    assert!(
        legacy_wma.diagnostics.is_empty(),
        "{:?}",
        legacy_wma.diagnostics
    );

    for version in [5, 6] {
        let modern = analyze_production(&format!(
            "//@version={version}\nindicator(\"modern division\")\nlength=input.int(5)\nplot(ta.wma(close, length / 2))\n"
        ));
        assert!(
            modern.diagnostics.is_empty(),
            "v{version}: {:?}",
            modern.diagnostics
        );
        assert!(
            modern
                .compatibility
                .legacy_emulations
                .iter()
                .all(|emulation| !emulation.feature.contains("integer_division"))
        );
    }
}

#[test]
fn pre_v6_numeric_builtin_bool_arguments_lower_through_bool_casts() {
    let v1 = analyze_production(
        "//@version=1\nstudy(\"numeric bool call\")\nsignal=close-open\nplot(valuewhen(signal, close, 0))\n",
    );
    assert!(v1.diagnostics.is_empty(), "{:?}", v1.diagnostics);
    assert_eq!(
        v1.compatibility
            .legacy_emulations
            .iter()
            .filter(|emulation| emulation.feature == "v1.numeric_to_bool")
            .count(),
        1
    );
    assert!(hir_contains_call(
        v1.hir.as_ref().expect("v1 numeric bool HIR"),
        "bool"
    ));

    for version in [2, 3, 4] {
        let source = include_str!(
            "../../../../tests/fixtures/legacy/v4/runtime/numeric_bool_call_arguments_legacy.pine"
        )
        .replacen("//@version=4", &format!("//@version={version}"), 1);
        let analysis = analyze_production(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        assert_eq!(
            analysis
                .compatibility
                .legacy_emulations
                .iter()
                .filter(|emulation| { emulation.feature == format!("v{version}.numeric_to_bool") })
                .count(),
            2
        );
        assert!(hir_contains_call(
            analysis.hir.as_ref().expect("legacy numeric bool HIR"),
            "bool"
        ));
    }

    let v5 = analyze_production(
        "//@version=5\nindicator(\"numeric bool calls\")\nsignal=bar_index==2?na:close-open-1\nplot(ta.valuewhen(signal, close, 0))\nalertcondition(signal, \"Nonzero\", \"Nonzero\")\nplot(ta.alma(close, 9, 0.85, 6, input.int(1)))\n",
    );
    assert!(v5.diagnostics.is_empty(), "{:?}", v5.diagnostics);
    assert_eq!(
        v5.compatibility
            .legacy_emulations
            .iter()
            .filter(|emulation| emulation.feature == "v5.numeric_to_bool")
            .count(),
        3
    );
    assert!(hir_contains_call(
        v5.hir.as_ref().expect("v5 numeric bool HIR"),
        "bool"
    ));

    let v5_series_for_simple = analyze_production(
        "//@version=5\nindicator(\"series simple bool\")\nplot(ta.alma(close, 9, 0.85, 6, close))\n",
    );
    assert_eq!(
        diagnostic_codes(&v5_series_for_simple),
        vec!["E_CALL_ARG_TYPE"]
    );
    assert!(v5_series_for_simple.hir.is_none());
}

#[test]
fn v6_keeps_numeric_builtin_bool_arguments_strict() {
    let analysis = analyze_production(
        "//@version=6\nindicator(\"numeric bool calls\")\nsignal=bar_index==2?na:close-open-1\nplot(ta.valuewhen(signal, close, 0))\nalertcondition(signal, \"Nonzero\", \"Nonzero\")\nplot(ta.alma(close, 9, 0.85, 6, input.int(1)))\n",
    );
    assert_eq!(
        diagnostic_codes(&analysis),
        vec!["E_CALL_ARG_TYPE", "E_CALL_ARG_TYPE", "E_CALL_ARG_TYPE"]
    );
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .compatibility
            .legacy_emulations
            .iter()
            .all(|emulation| emulation.feature != "v6.numeric_to_bool")
    );
}

#[test]
fn v2_bool_arithmetic_and_pre_v6_numeric_conditions_keep_version_boundaries() {
    let v2 = analyze_production(
        "//@version=2\nstudy(\"coercions\")\nb=close>open\nplot(b + true)\nplot((close-open) ? 1 : 0)\n",
    );
    assert!(v2.diagnostics.is_empty(), "{:?}", v2.diagnostics);
    assert!(hir_contains_call(v2.hir.as_ref().expect("v2 HIR"), "float"));
    assert!(hir_contains_call(v2.hir.as_ref().expect("v2 HIR"), "bool"));

    let v3_bool = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v3/unsupported/bool_arithmetic.pine"
    ));
    assert_eq!(diagnostic_codes(&v3_bool), vec!["E_OPERATOR_TYPE"]);
    let v3_numeric =
        analyze_production("//@version=3\nstudy(\"numeric condition\")\nplot(close ? 1 : 0)\n");
    assert!(
        v3_numeric.diagnostics.is_empty(),
        "{:?}",
        v3_numeric.diagnostics
    );
    assert!(hir_contains_call(
        v3_numeric.hir.as_ref().expect("v3 numeric condition HIR"),
        "bool"
    ));

    let v6 = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v6/unsupported/numeric_condition.pine"
    ));
    assert_eq!(diagnostic_codes(&v6), vec!["E_CONDITION_TYPE"]);
    assert!(v6.hir.is_none());
}

#[test]
fn v1_v2_bool_numeric_comparisons_lower_through_float_casts() {
    let legacy_fixture = include_str!(
        "../../../../tests/fixtures/legacy/v2/runtime/bool_numeric_comparisons_legacy.pine"
    );
    for version in [1, 2] {
        let analysis = analyze_production(&legacy_fixture.replacen(
            "//@version=2",
            &format!("//@version={version}"),
            1,
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "v{version}: {:?}",
            analysis.diagnostics
        );
        assert!(hir_contains_call(
            analysis.hir.as_ref().expect("legacy comparison HIR"),
            "float"
        ));
        assert_eq!(
            analysis
                .compatibility
                .legacy_emulations
                .iter()
                .filter(|emulation| { emulation.feature == format!("v{version}.bool_arithmetic") })
                .count(),
            4
        );
    }

    let v3 = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v3/unsupported/bool_numeric_comparison.pine"
    ));
    assert_eq!(
        diagnostic_codes(&v3)
            .into_iter()
            .filter(|code| *code == "E_OPERATOR_TYPE")
            .count(),
        4
    );
    assert!(v3.hir.is_none());
}

#[test]
fn v4_function_final_statements_and_reference_side_effects_are_supported() {
    let legacy = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/udf_final_statements_legacy.pine"
    ));
    let canonical = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/udf_final_statements_canonical.pine"
    ));

    assert!(
        legacy.diagnostics.is_empty(),
        "v4: {:?}",
        legacy.diagnostics
    );
    assert!(
        canonical.diagnostics.is_empty(),
        "v6: {:?}",
        canonical.diagnostics
    );
    assert!(legacy.hir.is_some());
    assert!(canonical.hir.is_some());

    let legacy_source = include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/udf_final_statements_legacy.pine"
    );
    for version in [5, 6] {
        let current = analyze_production(
            &legacy_source
                .replacen("//@version=4", &format!("//@version={version}"), 1)
                .replacen(
                    "study(\"Legacy v4 UDF final statements\")",
                    "indicator(\"Current UDF final statements\")",
                    1,
                ),
        );
        assert!(
            current.diagnostics.is_empty(),
            "v{version}: {:?}",
            current.diagnostics
        );
        assert!(current.hir.is_some(), "v{version}");
    }

    let side_effects = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/udf_reference_side_effects_legacy.pine"
    ));
    assert_eq!(
        diagnostic_codes(&side_effects)
            .iter()
            .filter(|code| matches!(**code, "E_FUNCTION_RETURN" | "E_LOOP_RETURN"))
            .count(),
        0,
        "{:?}",
        side_effects.diagnostics
    );
    assert!(
        side_effects.diagnostics.is_empty(),
        "{:?}",
        side_effects.diagnostics
    );
    assert!(side_effects.hir.is_some());

    let canonical_side_effects = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/udf_reference_side_effects_canonical.pine"
    ));
    assert!(
        canonical_side_effects.diagnostics.is_empty(),
        "{:?}",
        canonical_side_effects.diagnostics
    );
    assert!(canonical_side_effects.hir.is_some());

    let line_setters_source =
        include_str!("../../../../tests/fixtures/legacy/v4/runtime/udf_line_setters_legacy.pine");
    let line_setters = analyze_production(line_setters_source);
    assert!(
        line_setters.diagnostics.is_empty(),
        "{:?}",
        line_setters.diagnostics
    );
    assert!(line_setters.hir.is_some());

    let canonical_line_setters = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/runtime/udf_line_setters_canonical.pine"
    ));
    assert!(
        canonical_line_setters.diagnostics.is_empty(),
        "{:?}",
        canonical_line_setters.diagnostics
    );
    assert!(canonical_line_setters.hir.is_some());

    let v3_line_setters =
        analyze_production(&line_setters_source.replacen("//@version=4", "//@version=3", 1));
    assert!(
        !v3_line_setters.compatibility.unsupported.is_empty(),
        "{:?}",
        v3_line_setters.diagnostics
    );
    assert!(v3_line_setters.hir.is_none());

    for version in [5, 6] {
        let modern = analyze_production(
            &line_setters_source
                .replacen("//@version=4", &format!("//@version={version}"), 1)
                .replacen(
                    "study(\"Legacy v4 UDF line setters\"",
                    "indicator(\"Current UDF line setters\"",
                    1,
                ),
        );
        assert!(
            modern.diagnostics.is_empty(),
            "v{version}: {:?}",
            modern.diagnostics
        );
        assert!(modern.hir.is_some(), "v{version}");
    }

    let focused_boundary = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v4/unsupported/udf_other_reference_side_effects.pine"
    ));
    assert_eq!(
        focused_boundary
            .compatibility
            .unsupported
            .iter()
            .filter(|feature| feature.feature == "function_side_effect")
            .count(),
        3,
        "{:?}",
        focused_boundary.compatibility.unsupported
    );
    assert!(focused_boundary.hir.is_none());
}

#[test]
fn implicit_v1_matches_explicit_v2_only_for_the_claimed_shared_profile() {
    let v1 = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v1/runtime/shared_v1.pine"
    ));
    let v2 = analyze_production(include_str!(
        "../../../../tests/fixtures/legacy/v2/runtime/shared_v2.pine"
    ));
    assert!(v1.diagnostics.is_empty(), "{:?}", v1.diagnostics);
    assert!(v2.diagnostics.is_empty(), "{:?}", v2.diagnostics);
    assert_eq!(
        v1.compatibility.language_version_origin,
        crate::VersionOrigin::ImplicitV1
    );
    let mut v1_hir = v1.hir.expect("v1 HIR");
    let mut v2_hir = v2.hir.expect("v2 HIR");
    v1_hir.language_version = None;
    v2_hir.language_version = None;
    // The explicit version directive shifts source byte ranges; this control
    // compares only the already-claimed shared execution profile.
    v1_hir.call_site_sources.clear();
    v2_hir.call_site_sources.clear();
    assert_eq!(v1_hir, v2_hir);
}

fn hir_expr_contains_history_symbol(expr: &HirExpr, symbol: pine_ir::SymbolId) -> bool {
    match &expr.kind {
        HirExprKind::History { expr, .. } => {
            matches!(expr.kind, HirExprKind::Symbol(candidate) if candidate == symbol)
                || hir_expr_contains_history_symbol(expr, symbol)
        }
        HirExprKind::Unary { expr, .. } => hir_expr_contains_history_symbol(expr, symbol),
        HirExprKind::Binary { left, right, .. } => {
            hir_expr_contains_history_symbol(left, symbol)
                || hir_expr_contains_history_symbol(right, symbol)
        }
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            hir_expr_contains_history_symbol(condition, symbol)
                || hir_expr_contains_history_symbol(then_expr, symbol)
                || hir_expr_contains_history_symbol(else_expr, symbol)
        }
        HirExprKind::Call { args, .. } => args
            .iter()
            .any(|arg| hir_expr_contains_history_symbol(&arg.value, symbol)),
        _ => false,
    }
}

fn hir_contains_call(hir: &pine_ir::HirProgram, expected: &str) -> bool {
    hir.statements
        .iter()
        .any(|statement| match &statement.kind {
            HirStmtKind::Expr(expr)
            | HirStmtKind::Decl { value: expr, .. }
            | HirStmtKind::TupleDecl { value: expr, .. }
            | HirStmtKind::Reassign { value: expr, .. } => hir_expr_contains_call(expr, expected),
            _ => false,
        })
}

fn hir_expr_contains_call(expr: &HirExpr, expected: &str) -> bool {
    match &expr.kind {
        HirExprKind::Call { callee, args, .. } => {
            callee == expected
                || args
                    .iter()
                    .any(|arg| hir_expr_contains_call(&arg.value, expected))
        }
        HirExprKind::Unary { expr, .. } | HirExprKind::History { expr, .. } => {
            hir_expr_contains_call(expr, expected)
        }
        HirExprKind::Binary { left, right, .. } => {
            hir_expr_contains_call(left, expected) || hir_expr_contains_call(right, expected)
        }
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            hir_expr_contains_call(condition, expected)
                || hir_expr_contains_call(then_expr, expected)
                || hir_expr_contains_call(else_expr, expected)
        }
        _ => false,
    }
}
