use std::{
    fs,
    path::{Path, PathBuf},
};

use pine_runtime::{
    Bar, HistoricalRuntime, MagnifierChartBarInput, magnifier_input_from_groups, run_historical,
};
use pine_sema::{Analysis, AnalysisInput, analyze_input, analyze_source};
use pine_syntax::SourceFile;

fn workspace_fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

#[test]
fn runtime_fixtures_match_incremental_append_execution() {
    let undefined_cases = include_str!("../../../tests/fixtures/undefined_udt_access.tsv")
        .lines()
        .skip(1)
        .map(|line| {
            line.split_once('\t')
                .expect("undefined-object case mapping")
        })
        .collect::<Vec<_>>();
    for (original, guarded) in &undefined_cases {
        assert!(workspace_fixture(&format!("tests/fixtures/runtime/{original}")).exists());
        assert!(workspace_fixture(&format!("tests/fixtures/runtime/{guarded}")).exists());
    }
    let fixtures_dir = workspace_fixture("tests/fixtures/runtime");
    let default_bars = load_bars(&workspace_fixture("tests/fixtures/runtime/bars.csv"));
    let strategy_exit_loss_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_loss_bars.csv",
    ));
    let strategy_exit_profit_short_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_profit_short_bars.csv",
    ));
    let strategy_exit_profit_loss_interactions_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_profit_loss_interactions_bars.csv",
    ));
    let strategy_exit_bracket_loss_profit_loss_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_bracket_loss_profit_loss_bars.csv",
    ));
    let strategy_exit_bracket_mixed_pairs_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_bracket_mixed_pairs_bars.csv",
    ));
    let strategy_exit_bracket_replacement_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_bracket_replacement_bars.csv",
    ));
    let strategy_exit_bracket_both_hit_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_bracket_both_hit_bars.csv",
    ));
    let trailing_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_trailing_bars.csv",
    ));
    let trailing_short_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_trailing_short_bars.csv",
    ));
    let strategy_entry_stop_short_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_entry_stop_short_bars.csv",
    ));
    let strategy_entry_stop_limit_short_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_entry_stop_limit_short_bars.csv",
    ));
    let strategy_margin_call_short_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_margin_call_short_bars.csv",
    ));
    let strategy_close_entries_rule_any_exit_from_entry_short_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_close_entries_rule_any_exit_from_entry_short_bars.csv",
    ));
    let reservation_trailing_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_reservation_trailing_bars.csv",
    ));
    let reservation_trailing_mixed_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_reservation_trailing_mixed_bars.csv",
    ));
    let reservation_trailing_host_parity_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_exit_reservation_trailing_host_parity_bars.csv",
    ));
    let strategy_pyramiding_exit_omitted_profit_persistent_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_pyramiding_exit_omitted_profit_persistent_from_entries_bars.csv",
    ));
    let strategy_pyramiding_exit_omitted_loss_persistent_bars = load_bars(&workspace_fixture(
        "tests/fixtures/runtime/strategy_pyramiding_exit_omitted_loss_persistent_from_entries_bars.csv",
    ));
    let strategy_pyramiding_exit_omitted_loss_profit_bracket_persistent_bars = load_bars(
        &workspace_fixture(
            "tests/fixtures/runtime/strategy_pyramiding_exit_omitted_loss_profit_bracket_persistent_from_entries_bars.csv",
        ),
    );
    let strategy_pyramiding_exit_omitted_stop_profit_bracket_persistent_bars = load_bars(
        &workspace_fixture(
            "tests/fixtures/runtime/strategy_pyramiding_exit_omitted_stop_profit_bracket_persistent_from_entries_bars.csv",
        ),
    );
    let strategy_pyramiding_exit_omitted_loss_limit_bracket_persistent_bars = load_bars(
        &workspace_fixture(
            "tests/fixtures/runtime/strategy_pyramiding_exit_omitted_loss_limit_bracket_persistent_from_entries_bars.csv",
        ),
    );
    let strategy_pyramiding_exit_omitted_stop_limit_bracket_persistent_bars = load_bars(
        &workspace_fixture(
            "tests/fixtures/runtime/strategy_pyramiding_exit_omitted_stop_limit_bracket_persistent_from_entries_bars.csv",
        ),
    );
    let mut checked = 0;
    let mut failures = Vec::new();

    for entry in fs::read_dir(&fixtures_dir).expect("runtime fixture dir should be readable") {
        let path = entry.expect("fixture entry should be readable").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("pine") {
            continue;
        }

        let text = fs::read_to_string(&path).expect("fixture should be readable");
        let has_latest_known_bar_state = text.contains("barstate.islast")
            || text.contains("barstate.islastconfirmedhistory")
            || text.contains("session.islastbar")
            || text.contains("session.islastbar_regular")
            || text.contains("last_bar_index")
            || text.contains("last_bar_time")
            || text.contains("chart.right_visible_bar_time");
        let analysis = analyze_fixture(&path, text);
        assert!(
            analysis.diagnostics.is_empty(),
            "{} diagnostics: {:?}",
            path.display(),
            analysis.diagnostics
        );
        let hir = analysis.hir.expect("runtime fixture should lower to HIR");
        if undefined_cases
            .iter()
            .any(|(name, _)| Some(*name) == path.file_name().and_then(|name| name.to_str()))
        {
            // These original sources dereference a missing object.
            // Retain and compare their native error semantics; guarded siblings
            // below exercise successful historical/incremental history reads.
            let full_error = run_historical(&hir, &default_bars).unwrap_err();
            let mut incremental = HistoricalRuntime::new(&hir);
            let (at, incremental_error) = default_bars
                .iter()
                .copied()
                .enumerate()
                .find_map(|(i, bar)| incremental.append_bar(bar).err().map(|error| (i, error)))
                .expect("original fixture must fail incrementally");
            let full_at = (0..default_bars.len())
                .find(|i| run_historical(&hir, &default_bars[..=*i]).is_err())
                .unwrap();
            assert_eq!(at, full_at, "{} failure timing", path.display());
            assert!(
                full_error.message.contains("E_UDT_NA_FIELD"),
                "{}: {}",
                path.display(),
                full_error.message
            );
            assert_eq!(full_error.message, incremental_error.message);
            checked += 1;
            continue;
        }
        let bars = match path.file_name().and_then(|name| name.to_str()) {
            Some("strategy_exit_loss.pine") => &strategy_exit_loss_bars,
            Some("strategy_exit_profit_short.pine") => &strategy_exit_profit_short_bars,
            Some("strategy_exit_profit_loss_interactions.pine") => {
                &strategy_exit_profit_loss_interactions_bars
            }
            Some("strategy_exit_bracket_loss_profit_loss_fill.pine") => {
                &strategy_exit_bracket_loss_profit_loss_bars
            }
            Some("strategy_exit_bracket_mixed_pairs.pine") => {
                &strategy_exit_bracket_mixed_pairs_bars
            }
            Some("strategy_exit_bracket_replacement.pine") => {
                &strategy_exit_bracket_replacement_bars
            }
            Some("strategy_exit_bracket_both_hit.pine") => &strategy_exit_bracket_both_hit_bars,
            Some("strategy_entry_stop_short.pine") | Some("strategy_order_stop_short.pine") => {
                &strategy_entry_stop_short_bars
            }
            Some("strategy_entry_stop_limit_short.pine")
            | Some("strategy_order_stop_limit_short.pine") => &strategy_entry_stop_limit_short_bars,
            Some("strategy_margin_call_short.pine") => &strategy_margin_call_short_bars,
            Some("strategy_close_entries_rule_any_exit_from_entry_short.pine")
            | Some("strategy_close_entries_rule_any_exit_same_id_partial_short.pine") => {
                &strategy_close_entries_rule_any_exit_from_entry_short_bars
            }
            Some("strategy_exit_trail_price_fill_short.pine") => &trailing_short_bars,
            Some("strategy_exit_trail_points_fill_short.pine") => &trailing_short_bars,
            Some("strategy_exit_active_entry_trail_points_attachment.pine") => &trailing_bars,
            Some("strategy_exit_active_entry_stop_profit_bracket.pine") => &trailing_bars,
            Some("strategy_exit_active_entry_loss_limit_bracket.pine") => &trailing_bars,
            Some("strategy_exit_active_entry_loss_profit_bracket.pine") => &trailing_bars,
            Some("strategy_exit_omitted_trailing_replacement.pine") => &trailing_bars,
            Some("strategy_exit_qty_trailing_partial.pine") => &trailing_bars,
            Some("strategy_exit_qty_percent_trailing_partial.pine") => &trailing_bars,
            Some("strategy_exit_reservation_qty_trailing_price_multi.pine") => {
                &reservation_trailing_bars
            }
            Some("strategy_exit_reservation_qty_trailing_points_multi.pine") => {
                &reservation_trailing_bars
            }
            Some("strategy_exit_reservation_qty_trailing_replacement.pine") => {
                &reservation_trailing_bars
            }
            Some("strategy_exit_reservation_qty_trailing_clamp.pine") => &reservation_trailing_bars,
            Some("strategy_exit_reservation_trailing_state.pine") => &reservation_trailing_bars,
            Some("strategy_exit_reservation_qty_percent_trailing_multi.pine") => {
                &reservation_trailing_bars
            }
            Some("strategy_exit_reservation_qty_mixed_trailing_multi.pine") => {
                &reservation_trailing_bars
            }
            Some("strategy_exit_reservation_qty_percent_trailing_replacement.pine") => {
                &reservation_trailing_bars
            }
            Some("strategy_exit_reservation_qty_percent_trailing_clamp.pine") => {
                &reservation_trailing_bars
            }
            Some("strategy_exit_reservation_trailing_host_parity.pine") => {
                &reservation_trailing_host_parity_bars
            }
            Some("strategy_pyramiding_exit_omitted_profit_persistent_from_entries.pine") => {
                &strategy_pyramiding_exit_omitted_profit_persistent_bars
            }
            Some("strategy_pyramiding_exit_omitted_loss_persistent_from_entries.pine") => {
                &strategy_pyramiding_exit_omitted_loss_persistent_bars
            }
            Some(
                "strategy_pyramiding_exit_omitted_loss_profit_bracket_persistent_from_entries.pine",
            ) => &strategy_pyramiding_exit_omitted_loss_profit_bracket_persistent_bars,
            Some(
                "strategy_pyramiding_exit_omitted_stop_profit_bracket_persistent_from_entries.pine",
            ) => &strategy_pyramiding_exit_omitted_stop_profit_bracket_persistent_bars,
            Some(
                "strategy_pyramiding_exit_omitted_loss_limit_bracket_persistent_from_entries.pine",
            ) => &strategy_pyramiding_exit_omitted_loss_limit_bracket_persistent_bars,
            Some(
                "strategy_pyramiding_exit_omitted_stop_limit_bracket_persistent_from_entries.pine",
            ) => &strategy_pyramiding_exit_omitted_stop_limit_bracket_persistent_bars,
            Some("strategy_exit_reservation_trailing_single_downside_order.pine") => {
                &reservation_trailing_mixed_bars
            }
            Some("strategy_exit_reservation_trailing_bracket_downside_order.pine") => {
                &reservation_trailing_mixed_bars
            }
            Some("strategy_exit_reservation_trailing_mixed_side_precedence.pine") => {
                &reservation_trailing_mixed_bars
            }
            Some("strategy_exit_reservation_trailing_activation_mixed_fill.pine") => {
                &reservation_trailing_mixed_bars
            }
            Some("strategy_exit_reservation_trailing_replacement_mixed.pine") => {
                &reservation_trailing_mixed_bars
            }
            Some("strategy_exit_reservation_trailing_mixed_state.pine") => {
                &reservation_trailing_mixed_bars
            }
            _ => &default_bars,
        };

        let full = match run_historical(&hir, bars) {
            Ok(result) => result,
            Err(error) => {
                failures.push(format!("{}: {}", path.display(), error.message));
                continue;
            }
        };
        let mut runtime = HistoricalRuntime::new(&hir);
        if has_latest_known_bar_state {
            for result in runtime.historical_dataset(bars).unwrap() {
                result.expect("known-dataset step execution should succeed");
            }
        } else {
            for bar in bars.iter().copied() {
                runtime
                    .append_bar(bar)
                    .expect("append execution should succeed");
            }
        }
        let incremental = runtime.result();

        assert_eq!(
            incremental,
            full,
            "{} incremental result should match full recomputation",
            path.display()
        );
        checked += 1;
    }

    assert!(
        failures.is_empty(),
        "unexpected runtime failures:\n{}",
        failures.join("\n")
    );
    assert!(checked >= 7, "expected runtime fixtures to be checked");
}

#[test]
fn for_in_fixtures_match_incremental_append_execution() {
    let bars = load_bars(&workspace_fixture("tests/fixtures/runtime/bars.csv"));

    for fixture in [
        "tests/fixtures/runtime/for_in.pine",
        "tests/fixtures/runtime/for_in_float.pine",
        "tests/fixtures/runtime/for_in_bool.pine",
        "tests/fixtures/runtime/for_in_string.pine",
        "tests/fixtures/runtime/for_in_color.pine",
        "tests/fixtures/runtime/for_in_control_flow.pine",
        "tests/fixtures/runtime/for_in_mutation.pine",
        "tests/fixtures/runtime/for_in_stateful.pine",
        "tests/fixtures/runtime/for_in_zero_iteration.pine",
    ] {
        assert_fixture_matches_incremental_append_execution(fixture, &bars);
    }
}

#[test]
fn matrix_history_fixtures_match_incremental_append_execution() {
    let bars = load_bars(&workspace_fixture("tests/fixtures/runtime/bars.csv"));

    for fixture in [
        "tests/fixtures/runtime/matrix_history.pine",
        "tests/fixtures/runtime/matrix_history_shape.pine",
        "tests/fixtures/runtime/matrix_dynamic_history.pine",
    ] {
        assert_fixture_matches_incremental_append_execution(fixture, &bars);
    }
}

#[test]
fn legacy_v4_outputs_match_incremental_append_execution() {
    let bars = load_bars(&workspace_fixture("tests/fixtures/runtime/bars.csv"));
    assert_fixture_matches_incremental_append_execution(
        "tests/fixtures/legacy/v4/runtime/outputs_legacy.pine",
        &bars,
    );
}

fn assert_fixture_matches_incremental_append_execution(fixture: &str, bars: &[Bar]) {
    let path = workspace_fixture(fixture);
    let text = fs::read_to_string(&path).expect("fixture should be readable");
    let analysis = analyze_fixture(&path, text);
    assert!(
        analysis.diagnostics.is_empty(),
        "{} diagnostics: {:?}",
        path.display(),
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("runtime fixture should lower to HIR");

    let full = run_historical(&hir, bars).expect("full execution should succeed");
    let mut runtime = HistoricalRuntime::new(&hir);
    for bar in bars.iter().copied() {
        runtime
            .append_bar(bar)
            .expect("append execution should succeed");
    }

    assert_eq!(
        runtime.result(),
        full,
        "{} incremental result should match full recomputation",
        path.display()
    );
}

fn analyze_fixture(path: &Path, text: String) -> Analysis {
    let source = SourceFile::new(path.display().to_string(), text.clone());
    if text.contains("import user/transitive_outer/1") {
        let libraries = ["inner", "outer"]
            .into_iter()
            .map(|name| {
                let path = workspace_fixture(&format!(
                    "tests/fixtures/libraries/transitive_{name}_lib.pine"
                ));
                (
                    format!("user/transitive_{name}/1"),
                    SourceFile::new(
                        path.display().to_string(),
                        fs::read_to_string(path).unwrap(),
                    ),
                )
            })
            .collect();
        return analyze_input(&AnalysisInput::with_library_sources(source, libraries).unwrap());
    }
    let library = if text.contains("import test/scalar_overloads/1") {
        Some((
            "test/scalar_overloads/1",
            "tests/fixtures/libraries/scalar_overloads_lib.pine",
        ))
    } else if text.contains("import user/lib/1") {
        Some(("user/lib/1", "tests/fixtures/libraries/import_lib.pine"))
    } else if text.contains("import user/udt_array_returns/1") {
        Some((
            "user/udt_array_returns/1",
            "tests/fixtures/libraries/import_udt_array_return_lib.pine",
        ))
    } else if text.contains("import user/udt/1") {
        Some(("user/udt/1", "tests/fixtures/libraries/import_udt_lib.pine"))
    } else if text.contains("import user/non_scalar_udt/1") {
        Some((
            "user/non_scalar_udt/1",
            "tests/fixtures/libraries/import_non_scalar_udt_lib.pine",
        ))
    } else {
        None
    };
    let Some((key, library_fixture)) = library else {
        return analyze_source(&source);
    };
    let library_path = workspace_fixture(library_fixture);
    let library_text = version_matched_fixture_library_text(
        &text,
        fs::read_to_string(&library_path).expect("import library fixture"),
    );
    let input = AnalysisInput::with_library_sources(
        source,
        vec![(
            key.to_owned(),
            SourceFile::new(library_path.display().to_string(), library_text),
        )],
    )
    .expect("import fixture input");
    analyze_input(&input)
}

fn version_matched_fixture_library_text(root: &str, mut library: String) -> String {
    let root_version = root
        .lines()
        .find(|line| line.trim_start().starts_with("//@version="));
    let library_version = library
        .lines()
        .position(|line| line.trim_start().starts_with("//@version="));
    if let (Some(root_version), Some(0)) = (root_version, library_version) {
        let first_newline = library.find('\n').unwrap_or(library.len());
        library.replace_range(..first_newline, root_version.trim_start());
    }
    library
}

#[test]
fn magnifier_batch_matches_incremental_append() {
    let source = SourceFile::new(
        "magnifier-incremental.pine",
        r#"//@version=6
strategy("magnifier incremental", initial_capital=100000, use_bar_magnifier=true)
if bar_index == 0
    strategy.entry("EN", strategy.long, qty=1, stop=10.5)
    strategy.exit("EX", "EN", limit=11.5)
plot(close)
"#
        .to_owned(),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    assert!(program.strategy_settings.use_bar_magnifier);
    let bars = vec![
        Bar {
            time: 1_000,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            close: 10.0,
            volume: 1.0,
        },
        Bar {
            time: 2_000,
            open: 10.0,
            high: 12.0,
            low: 8.0,
            close: 11.0,
            volume: 1.0,
        },
    ];
    let input = magnifier_input_from_groups(vec![
        MagnifierChartBarInput {
            chart_bar_index: 0,
            bars: vec![bars[0]],
        },
        MagnifierChartBarInput {
            chart_bar_index: 1,
            bars: vec![
                Bar {
                    time: 2_000,
                    open: 10.0,
                    high: 10.4,
                    low: 9.8,
                    close: 10.2,
                    volume: 1.0,
                },
                Bar {
                    time: 2_300,
                    open: 10.2,
                    high: 10.8,
                    low: 10.1,
                    close: 10.6,
                    volume: 1.0,
                },
                Bar {
                    time: 2_600,
                    open: 10.6,
                    high: 11.8,
                    low: 10.5,
                    close: 11.0,
                    volume: 1.0,
                },
            ],
        },
    ])
    .expect("valid");
    let mut batch_runtime = HistoricalRuntime::new(&program).with_magnifier_input(input.clone());
    batch_runtime.append_bars(&bars).expect("batch");
    let batch = batch_runtime.result();
    let mut incremental = HistoricalRuntime::new(&program).with_magnifier_input(input);
    incremental
        .prepare_magnifier_chart_bar_count(bars.len())
        .expect("magnifier preflight");
    for bar in &bars {
        incremental.append_bar(*bar).expect("append");
    }
    assert_eq!(batch, incremental.result());
}

fn load_bars(path: &PathBuf) -> Vec<Bar> {
    let text = fs::read_to_string(path).expect("bars fixture should be readable");
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            if line.trim().is_empty() || index == 0 {
                return None;
            }
            let columns: Vec<_> = line.split(',').map(str::trim).collect();
            assert_eq!(columns.len(), 6, "bars fixture should have 6 columns");
            Some(Bar {
                time: columns[0].parse().expect("time should parse"),
                open: columns[1].parse().expect("open should parse"),
                high: columns[2].parse().expect("high should parse"),
                low: columns[3].parse().expect("low should parse"),
                close: columns[4].parse().expect("close should parse"),
                volume: columns[5].parse().expect("volume should parse"),
            })
        })
        .collect()
}
