use pine_runtime::{RequestArgument, host_requirements, host_requirements_json, run_historical};
use pine_sema::{AnalysisInput, analyze_input, analyze_source};
use pine_syntax::SourceFile;

#[test]
fn modern_merge_options_and_calendar_relation_are_reported_without_execution() {
    let hir = program(
        "//@version=6\nindicator(\"merge inventory\")\ng=barmerge.gaps_on\na=g\nplot(request.security(\"OTHER\",\"M\",close,gaps=a,lookahead=barmerge.lookahead_on))\nplot(request.security(\"OTHER\",\"M\",close))\n",
    );
    let report = host_requirements(&hir);
    assert_eq!(report.schema_version, 2);
    assert_eq!(report.requests[0].gaps, "gapsOn");
    assert_eq!(report.requests[0].lookahead, "lookaheadOn");
    assert_eq!(
        report.requests[0].timeframe_relation,
        "sameOrLowerOrHigherIntegerMultipleExceptCalendarMonths"
    );
    assert_eq!(report.requests[1].gaps, "gapsOff");
    assert_eq!(report.requests[1].lookahead, "lookaheadOff");
}

fn program(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("requirements.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

#[test]
fn indicator_inventory_has_no_broker_or_external_input_obligation() {
    let hir = program("//@version=6\nindicator(\"plain\")\nplot(close)\n");
    let report = host_requirements(&hir);
    assert!(report.account.is_none());
    assert!(report.requests.is_empty());
    assert_eq!(report.execution.clock, "notUsed");
    assert_eq!(report.chart.defaults.min_move, 1);
    assert_eq!(report.chart.defaults.price_scale, 100);
    assert_eq!(report.chart.defaults.quantity_precision, 0);
    assert!(report.chart.defaults.synthetic);
    assert!(!report.chart.defaults.symbol_lookup);
    assert_eq!(host_requirements_json(&hir), host_requirements_json(&hir));
}

#[test]
fn inventory_preserves_conditional_clock_and_current_context_requests() {
    let hir = program(
        "//@version=6\nindicator(\"conditional\")\nplot(false ? timenow : 7)\nplot(request.security(syminfo.tickerid, timeframe.period, close))\n",
    );
    let report = host_requirements(&hir);
    assert_eq!(report.execution.clock, "explicitTimestampWhenEvaluated");
    assert_eq!(
        report.requests[0].symbol,
        RequestArgument::CurrentContextSymbol
    );
    assert_eq!(
        report.requests[0].timeframe,
        RequestArgument::CurrentContextTimeframe
    );
    let bars = [pine_runtime::Bar {
        time: 0,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    }];
    let result = run_historical(&hir, &bars).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(7.0));
}

#[test]
fn modern_request_context_and_dynamic_history_are_discovered() {
    let hir = program(
        "//@version=6\nindicator(\"requests\")\ns=input.symbol(\"OTHER\")\nplot(request.security(\"OTHER\", \"60\", close))\nplot(request.security(syminfo.tickerid,timeframe.period,close))\nplot(close[int(request.security(\"FIXED\",\"5\",close))])\n",
    );
    let report = host_requirements(&hir);
    assert_eq!(report.requests.len(), 3);
    assert_eq!(report.input_call_site_ids.len(), 1);
    assert!(
        report
            .requests
            .iter()
            .any(|r| r.symbol == RequestArgument::Literal("OTHER".into()))
    );
    assert!(
        report
            .requests
            .iter()
            .any(|r| r.symbol == RequestArgument::CurrentContextSymbol
                && r.timeframe == RequestArgument::CurrentContextTimeframe)
    );
    assert!(
        report
            .requests
            .iter()
            .any(|r| r.symbol == RequestArgument::Literal("FIXED".into())
                && r.timeframe == RequestArgument::Literal("5".into()))
    );
}

#[test]
fn strategy_inventory_exposes_optional_fallbacks_and_account_limits() {
    let hir = program(
        "//@version=6\nstrategy(\"host\",use_bar_magnifier=true,calc_on_every_tick=true,calc_on_order_fills=true,process_orders_on_close=true)\nstrategy.risk.max_intraday_loss(10,strategy.percent_of_equity)\nplot(syminfo.mincontract)\n",
    );
    let report = host_requirements(&hir);
    assert_eq!(report.account.unwrap().point_value, 1);
    assert_eq!(
        report.execution.magnifier,
        "historicalIntrabarsOrReportedStandardOhlcFallback"
    );
    assert_eq!(
        report.execution.session_windows,
        "hostWindowAndTradingDayIdsOrUtcFallback"
    );
    assert!(
        report.execution.calc_on_every_tick
            && report.execution.calc_on_order_fills
            && report.execution.process_orders_on_close
    );
    assert_eq!(report.chart.symbol_metadata, ["syminfo.mincontract"]);
}

#[test]
fn imported_executable_request_is_discovered_without_invoking_a_provider() {
    let input = AnalysisInput::with_library_sources(
        SourceFile::new("root.pine", "//@version=6\nimport Test/Feeds/1 as feeds\nindicator(\"imported\")\nplot(feeds.value())\n"),
        vec![("Test/Feeds/1".into(),SourceFile::new("feeds.pine", "//@version=6\nlibrary(\"Feeds\")\nexport value() => request.security(\"REMOTE\",\"60\",close)\n"))],
    ).unwrap();
    let analysis = analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let report = host_requirements(&analysis.hir.unwrap());
    assert_eq!(report.requests.len(), 1);
    assert_eq!(
        report.requests[0].symbol,
        RequestArgument::Literal("REMOTE".into())
    );
}

#[test]
fn legacy_runtime_request_arguments_are_not_mislabeled_as_literal_defaults() {
    let hir = program(
        "//@version=4\nstudy(\"legacy\")\ns=input(\"OTHER\")\nplot(security(s, \"60\", close))\n",
    );
    assert_eq!(
        host_requirements(&hir).requests[0].symbol,
        RequestArgument::RuntimeExpression
    );
}

#[test]
fn discovery_does_not_expand_modern_request_admission() {
    let expr = "request.security(s, \"60\", math.random(0, 1, 7))";
    let source =
        format!("//@version=6\nindicator(\"boundary\")\ns=input.symbol(\"OTHER\")\nplot({expr})\n");
    let analysis = analyze_source(&SourceFile::new("boundary.pine", source));
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.code == "E_UNSUPPORTED_FEATURE")
    );
}

#[test]
fn lower_timeframe_request_is_reported_as_host_data_obligation() {
    let hir = program(
        "//@version=6\nindicator(\"lower timeframe\")\ns=input.symbol(\"OTHER\")\na=request.security_lower_tf(s, \"30S\", close)\nplot(array.size(a))\n",
    );
    let report = host_requirements(&hir);
    assert_eq!(report.requests.len(), 1);
    let request = &report.requests[0];
    assert_eq!(request.function, "request.security_lower_tf");
    assert_eq!(request.symbol, RequestArgument::RuntimeExpression);
    assert_eq!(request.timeframe, RequestArgument::Literal("30S".into()));
    assert_eq!(request.timeframe_relation, "sameOrLower");
    assert_eq!(request.gaps, "notApplicable");
    assert_eq!(request.lookahead, "notApplicable");
}

#[test]
fn security_timeframe_inventory_matches_executable_modern_and_legacy_relations() {
    use std::sync::Arc;

    use pine_runtime::{
        Bar, ChartContext, HistoricalRuntime, InMemoryRequestDataProvider, RequestEnvironment,
        RequestKey, RequestTimeframe,
    };

    for version in [4, 5, 6] {
        let declaration = if version == 4 { "study" } else { "indicator" };
        let function = if version == 4 {
            "security"
        } else {
            "request.security"
        };
        // A three-minute request is lower than a five-minute chart without
        // being an integer divisor. Six minutes is a forbidden higher ratio;
        // calendar months retain their explicit boundary-based exception.
        for (timeframe, accepted) in [
            ("3", true),
            ("5", true),
            ("10", true),
            ("6", false),
            ("M", true),
        ] {
            let hir = program(&format!(
                "//@version={version}\n{declaration}(\"relation\")\nplot({function}(\"REMOTE\", \"{timeframe}\", close))\n"
            ));
            let report = host_requirements(&hir);
            assert_eq!(
                report.requests[0].timeframe_relation,
                "sameOrLowerOrHigherIntegerMultipleExceptCalendarMonths"
            );
            let mut provider = InMemoryRequestDataProvider::new();
            provider
                .insert(
                    RequestKey::new("REMOTE", RequestTimeframe::parse(timeframe).unwrap()),
                    vec![Bar {
                        time: 0,
                        open: 1.0,
                        high: 1.0,
                        low: 1.0,
                        close: 1.0,
                        volume: 1.0,
                    }],
                )
                .unwrap();
            let env = RequestEnvironment::new(
                ChartContext::new("CHART", RequestTimeframe::parse("5").unwrap()),
                Arc::new(provider),
            );
            let mut runtime = HistoricalRuntime::with_request_environment(&hir, env);
            let outcome = runtime.append_bars(&[Bar {
                time: 0,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            }]);
            assert_eq!(
                outcome.is_ok(),
                accepted,
                "v{version} {timeframe}: {outcome:?}"
            );
            if timeframe == "3" {
                outcome.unwrap();
                assert_eq!(runtime.result().plots[0].values[0].as_f64(), Some(1.0));
            } else if !accepted {
                assert!(outcome.unwrap_err().message.contains("integer multiple"));
            }
        }
    }
}

#[test]
fn modern_input_request_selector_remains_a_host_obligation() {
    let hir = program(
        "//@version=6\nindicator(\"selector\")\ns=input.symbol(\"OTHER\")\nplot(request.security(s,\"60\",close))\n",
    );
    assert_eq!(
        host_requirements(&hir).requests[0].symbol,
        RequestArgument::RuntimeExpression
    );
}

#[test]
fn root_input_and_request_locations_are_original_utf8_byte_ranges() {
    let source = "//@version=6\n// 中文位置\nindicator(\"source\")\ns=input.symbol(\"REMOTE\")\nplot(request.security(\"REMOTE\",\"60\",close))\n";
    let report = host_requirements(&program(source));
    assert_eq!(report.call_sites.len(), 2);
    for call in &report.call_sites {
        let location = call.source.as_ref().unwrap();
        assert_eq!(location.source_id, 0);
        assert!(location.library_key.is_none());
        let text = &source[location.start..location.end];
        if report.input_call_site_ids.contains(&call.call_site_id) {
            assert_eq!(text, "input.symbol(\"REMOTE\")");
        } else {
            assert_eq!(text, "request.security(\"REMOTE\",\"60\",close)");
        }
    }
}

#[test]
fn aliased_and_transitive_calls_keep_physical_library_identity() {
    let leaf = "//@version=6\n// 原始库\nlibrary(\"Leaf\")\nexport value() => request.security(\"REMOTE\",\"60\",close)\n";
    let parent = "//@version=6\nimport Test/Leaf/1 as leaf\nlibrary(\"Parent\")\nexport value() => leaf.value()\n";
    let input = AnalysisInput::with_library_sources(
        SourceFile::new("same.pine", "//@version=6\nimport Test/Leaf/1 as left\nimport Test/Leaf/1 as right\nimport Test/Parent/1 as parent\nindicator(\"origins\")\nplot(left.value())\nplot(right.value())\nplot(parent.value())\n"),
        vec![
            ("Test/Parent/1".into(), SourceFile::new("same.pine", parent)),
            ("Test/Leaf/1".into(), SourceFile::new("same.pine", leaf)),
        ],
    ).unwrap();
    let analysis = analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let report = host_requirements(&analysis.hir.unwrap());
    assert_eq!(report.requests.len(), 3);
    assert_eq!(report.call_sites.len(), 3);
    for call in &report.call_sites {
        let source = call.source.as_ref().unwrap();
        assert_eq!(
            source.source_id, 1,
            "physical IDs follow sorted library sources"
        );
        assert_eq!(source.library_key.as_deref(), Some("Test/Leaf/1"));
        assert_eq!(
            &leaf[source.start..source.end],
            "request.security(\"REMOTE\",\"60\",close)"
        );
    }
}

#[test]
fn legacy_security_location_is_not_replaced_with_the_canonical_callee_text() {
    let source = "//@version=4\nstudy(\"origin\")\nplot(security(\"REMOTE\",\"60\",close))\n";
    let report = host_requirements(&program(source));
    assert_eq!(report.call_sites.len(), 1);
    let location = report.call_sites[0].source.as_ref().unwrap();
    assert_eq!(
        &source[location.start..location.end],
        "security(\"REMOTE\",\"60\",close)"
    );
}

#[test]
fn absent_hir_provenance_is_explicitly_unavailable() {
    let mut hir = program(
        "//@version=6\nindicator(\"manual\")\nplot(request.security(\"REMOTE\",\"60\",close))\n",
    );
    hir.call_site_sources.clear();
    let report = host_requirements(&hir);
    assert_eq!(report.requests.len(), 1);
    assert_eq!(report.call_sites.len(), 1);
    assert!(report.call_sites[0].source.is_none());
    let json: serde_json::Value = serde_json::from_str(&host_requirements_json(&hir)).unwrap();
    assert!(json["callSites"][0]["source"].is_null());
}

#[test]
fn caller_request_argument_does_not_acquire_the_inlined_library_origin() {
    let root = "//@version=6\nimport Test/Identity/1 as lib\nindicator(\"caller\")\nplot(lib.value(request.security(\"REMOTE\",\"60\",close)))\n";
    let input = AnalysisInput::with_library_sources(
        SourceFile::new("root.pine", root),
        vec![(
            "Test/Identity/1".into(),
            SourceFile::new(
                "library.pine",
                "//@version=6\nlibrary(\"Identity\")\nexport value(float x) => x\n",
            ),
        )],
    )
    .unwrap();
    let analysis = analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let report = host_requirements(&analysis.hir.unwrap());
    assert_eq!(report.requests.len(), 1);
    assert_eq!(report.call_sites.len(), 1);
    let source = report.call_sites[0].source.as_ref().unwrap();
    assert_eq!(source.source_id, 0);
    assert!(source.library_key.is_none());
    assert_eq!(
        &root[source.start..source.end],
        "request.security(\"REMOTE\",\"60\",close)"
    );
}
