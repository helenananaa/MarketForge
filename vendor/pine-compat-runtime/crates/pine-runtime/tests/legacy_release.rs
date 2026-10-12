use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use pine_runtime::{
    Bar, BarUpdate, ChartContext, HistoricalRuntime, InMemoryRequestDataProvider, InputOverrides,
    RealtimeRuntime, RequestEnvironment, RequestKey, RequestTimeframe, RuntimeProfile,
    run_historical_profiled_with_request_environment,
    run_historical_profiled_with_request_environment_and_input_overrides_and_execution_times,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

const MANIFEST_HEADER: &str = "id\tversion\tmaturity\tcategory\tsource_path\tbars_profile\trequest_profile\texecution_profile\trealtime_policy\tlicense_class\tmax_retained_values";

#[derive(Debug)]
struct ReleaseFixture {
    id: String,
    version: u16,
    maturity: String,
    category: String,
    source_path: String,
    bars_profile: String,
    request_profile: String,
    execution_profile: String,
    realtime_policy: String,
    license_class: String,
    max_retained_values: usize,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn release_fixtures() -> Vec<ReleaseFixture> {
    let text =
        fs::read_to_string(workspace_root().join("tests/fixtures/legacy/release_profiles.tsv"))
            .expect("legacy release manifest should be readable");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(MANIFEST_HEADER));
    lines
        .enumerate()
        .map(|(index, line)| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 11, "manifest line {}: {line}", index + 2);
            ReleaseFixture {
                id: fields[0].to_owned(),
                version: fields[1].parse().expect("numeric legacy version"),
                maturity: fields[2].to_owned(),
                category: fields[3].to_owned(),
                source_path: fields[4].to_owned(),
                bars_profile: fields[5].to_owned(),
                request_profile: fields[6].to_owned(),
                execution_profile: fields[7].to_owned(),
                realtime_policy: fields[8].to_owned(),
                license_class: fields[9].to_owned(),
                max_retained_values: fields[10].parse().expect("numeric resource ceiling"),
            }
        })
        .collect()
}

fn bar(time: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar {
        time,
        open,
        high,
        low,
        close,
        volume,
    }
}

fn default_bars() -> Vec<Bar> {
    vec![
        bar(0, 10.0, 12.0, 9.0, 11.0, 100.0),
        bar(60_000, 11.0, 14.0, 10.0, 13.0, 110.0),
        bar(120_000, 13.0, 15.0, 11.0, 12.0, 120.0),
        bar(180_000, 12.0, 16.0, 8.0, 15.0, 130.0),
        bar(240_000, 15.0, 17.0, 13.0, 14.0, 140.0),
        bar(300_000, 14.0, 18.0, 12.0, 17.0, 150.0),
    ]
}

fn v2_core_bars() -> Vec<Bar> {
    vec![
        bar(0, 10.0, 12.0, 9.0, 11.0, 100.0),
        bar(60_000, 12.0, 13.0, 10.0, 11.0, 110.0),
        bar(120_000, 11.0, 14.0, 10.0, 13.0, 120.0),
        bar(180_000, 13.0, 14.0, 9.0, 10.0, 130.0),
        bar(240_000, 10.0, 12.0, 8.0, 10.0, 140.0),
        bar(300_000, 9.0, 12.0, 8.0, 11.0, 150.0),
    ]
}

fn v3_core_bars() -> Vec<Bar> {
    vec![
        bar(0, 10.0, 12.0, 9.0, 11.0, 100.0),
        bar(60_000, 12.0, 13.0, 9.0, 10.0, 110.0),
        bar(120_000, 10.0, 15.0, 10.0, 14.0, 120.0),
        bar(180_000, 14.0, 16.0, 12.0, 15.0, 130.0),
        bar(240_000, 16.0, 17.0, 13.0, 14.0, 140.0),
    ]
}

fn load_csv_bars(path: &Path) -> Vec<Bar> {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .lines()
        .skip(1)
        .map(|line| {
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(fields.len(), 6, "{}: {line}", path.display());
            bar(
                fields[0].parse().expect("bar time"),
                fields[1].parse().expect("bar open"),
                fields[2].parse().expect("bar high"),
                fields[3].parse().expect("bar low"),
                fields[4].parse().expect("bar close"),
                fields[5].parse().expect("bar volume"),
            )
        })
        .collect()
}

fn fixture_bars(profile: &str) -> Vec<Bar> {
    match profile {
        "default" => default_bars(),
        "v2_core" => v2_core_bars(),
        "v3_core" => v3_core_bars(),
        "logical_strict" => vec![
            bar(0, 10.0, 10.0, -20.0, 9.0, 100.0),
            bar(60_000, 11.0, 15.0, -15.0, 12.0, 110.0),
            bar(120_000, 12.0, 30.0, -5.0, 13.0, 120.0),
            bar(180_000, 13.0, 25.0, -30.0, 14.0, 130.0),
        ],
        "session_weekend" => [
            1_609_459_200_000,
            1_609_545_600_000,
            1_609_632_000_000,
            1_609_718_400_000,
        ]
        .into_iter()
        .map(|time| bar(time, 1.0, 1.0, 1.0, 1.0, 1.0))
        .collect(),
        "security_chart" => vec![
            bar(0, 1.0, 1.0, 1.0, 1.0, 1.0),
            bar(60_000, 2.0, 2.0, 2.0, 2.0, 1.0),
            bar(240_000, 3.0, 3.0, 3.0, 3.0, 1.0),
            bar(300_000, 4.0, 4.0, 4.0, 4.0, 1.0),
            bar(540_000, 5.0, 5.0, 5.0, 5.0, 1.0),
            bar(600_000, 6.0, 6.0, 6.0, 6.0, 1.0),
        ],
        "legacy_chart" => {
            load_csv_bars(&workspace_root().join("tests/fixtures/legacy/chart_1m.csv"))
        }
        other => panic!("unknown legacy bars profile `{other}`"),
    }
}

fn request_environment(profile: &str) -> RequestEnvironment {
    match profile {
        "none" => RequestEnvironment::default(),
        "test_chart" => RequestEnvironment::new(
            ChartContext::new(
                "TEST",
                RequestTimeframe::parse("1").expect("one minute timeframe"),
            ),
            Arc::new(InMemoryRequestDataProvider::new()),
        ),
        "ibm_5" => {
            let key = RequestKey::new(
                "NYSE:IBM",
                RequestTimeframe::parse("5").expect("five minute timeframe"),
            );
            let bars = vec![
                bar(0, 100.0, 100.0, 100.0, 100.0, 1.0),
                bar(300_000, 200.0, 200.0, 200.0, 200.0, 1.0),
                bar(600_000, 300.0, 300.0, 300.0, 300.0, 1.0),
            ];
            let provider = InMemoryRequestDataProvider::from_streams([(key, bars)])
                .expect("valid IBM request stream");
            RequestEnvironment::new(ChartContext::default(), Arc::new(provider))
        }
        "ibm_1" => {
            let key = RequestKey::new(
                "NYSE:IBM",
                RequestTimeframe::parse("1").expect("one minute timeframe"),
            );
            let bars = load_csv_bars(
                &workspace_root().join("tests/fixtures/legacy/v4/runtime/security_chart_bars.csv"),
            );
            let provider = InMemoryRequestDataProvider::from_streams([(key, bars)])
                .expect("valid IBM request stream");
            RequestEnvironment::new(ChartContext::default(), Arc::new(provider))
        }
        "test_daily" => {
            let key = RequestKey::new(
                "TEST",
                RequestTimeframe::parse("D").expect("daily timeframe"),
            );
            let bars =
                load_csv_bars(&workspace_root().join("tests/fixtures/legacy/request_daily.csv"));
            let provider = InMemoryRequestDataProvider::from_streams([(key, bars)])
                .expect("valid daily request stream");
            RequestEnvironment::new(
                ChartContext::new(
                    "TEST",
                    RequestTimeframe::parse("1").expect("one minute timeframe"),
                ),
                Arc::new(provider),
            )
        }
        other => panic!("unknown legacy request profile `{other}`"),
    }
}

fn execution_times(profile: &str, bars: &[Bar]) -> Option<Vec<i64>> {
    const DETERMINISTIC_CLOCK: &[i64] = &[
        1_700_000_000_101,
        1_700_000_060_205,
        1_700_000_120_333,
        1_700_000_180_499,
        1_700_000_240_711,
        1_700_000_300_977,
        1_700_000_361_261,
        1_700_000_421_577,
        1_700_000_481_921,
        1_700_000_542_299,
    ];
    match profile {
        "none" => None,
        "deterministic_clock" => {
            assert!(
                bars.len() <= DETERMINISTIC_CLOCK.len(),
                "execution clock profile has too few timestamps"
            );
            Some(DETERMINISTIC_CLOCK[..bars.len()].to_vec())
        }
        other => panic!("unknown legacy execution profile `{other}`"),
    }
}

fn append_historical(
    runtime: &mut HistoricalRuntime<'_>,
    bar: Bar,
    execution_time: Option<i64>,
) -> Result<(), pine_runtime::RuntimeError> {
    match execution_time {
        Some(execution_time) => runtime.append_bar_with_execution_time(bar, execution_time),
        None => runtime.append_bar(bar),
    }
}

fn update_realtime(
    runtime: &mut RealtimeRuntime<'_>,
    update: BarUpdate,
    execution_time: Option<i64>,
) -> Result<pine_runtime::RuntimeResult, pine_runtime::RuntimeError> {
    match execution_time {
        Some(execution_time) => runtime.update_with_execution_time(update, execution_time),
        None => runtime.update(update),
    }
}

fn retained_values(profile: &RuntimeProfile) -> usize {
    profile.request_cache_values
        + profile.series_values
        + profile.rolling_window_values
        + profile.valuewhen_state_values
        + profile.array_values
        + profile.matrix_cells
        + profile.plot_values
        + profile.plot_char_values
        + profile.plot_shape_values
        + profile.plot_arrow_values
        + profile.plot_bar_values
        + profile.plot_candle_values
        + profile.bg_color_values
        + profile.bar_color_values
        + profile.label_snapshots
        + profile.line_snapshots
        + profile.line_fill_snapshots
        + profile.polyline_snapshots
        + profile.polyline_points
        + profile.box_snapshots
        + profile.table_cells
}

fn assert_release_profile(
    row: &ReleaseFixture,
    mode: &str,
    profile: &RuntimeProfile,
    batch: Option<&RuntimeProfile>,
) {
    let retained = retained_values(profile);
    assert!(
        retained <= row.max_retained_values,
        "{} {mode} retained {retained} values above ceiling {}: {:?}",
        row.id,
        row.max_retained_values,
        profile
    );
    assert!(
        profile.request_cache_entries >= profile.request_cache_contexts,
        "{} {mode} has fewer request-cache entries than contexts: {:?}",
        row.id,
        profile
    );
    if matches!(
        row.request_profile.as_str(),
        "ibm_5" | "ibm_1" | "test_daily"
    ) {
        assert!(
            profile.request_cache_entries > 0
                && profile.request_cache_contexts > 0
                && profile.request_cache_values > 0,
            "{} {mode} omitted provider-backed request-cache resources: {:?}",
            row.id,
            profile
        );
    }
    if let Some(batch) = batch {
        assert_eq!(profile.bars, batch.bars, "{} {mode} bars", row.id);
        assert_eq!(
            profile.max_series_depth, batch.max_series_depth,
            "{} {mode} max series depth",
            row.id
        );
        assert_eq!(
            retained,
            retained_values(batch),
            "{} {mode} retained values",
            row.id
        );
        assert_eq!(
            profile.request_cache_entries, batch.request_cache_entries,
            "{} {mode} request-cache entries",
            row.id
        );
        assert_eq!(
            profile.request_cache_contexts, batch.request_cache_contexts,
            "{} {mode} request-cache contexts",
            row.id
        );
        assert_eq!(
            profile.request_cache_values, batch.request_cache_values,
            "{} {mode} request-cache values",
            row.id
        );
    }
}

fn mutated_forming_bar(bar: &Bar) -> Bar {
    Bar {
        time: bar.time,
        open: bar.open + 3.0,
        high: bar.high + 9.0,
        low: bar.low - 7.0,
        close: bar.close + 5.0,
        volume: bar.volume + 11.0,
    }
}

fn collect_runtime_legacy_sources(dir: &Path, output: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).expect("legacy runtime directory should be readable") {
        let path = entry.expect("legacy runtime entry").path();
        if path.is_dir() {
            collect_runtime_legacy_sources(&path, output);
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("pine")
            || !path
                .components()
                .any(|component| component.as_os_str() == "runtime")
        {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if file_name.ends_with("_legacy.pine")
            || matches!(file_name, "shared_v1.pine" | "shared_v2.pine")
        {
            output.insert(
                path.strip_prefix(workspace_root())
                    .expect("workspace-relative fixture")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

#[test]
fn release_manifest_is_complete_versioned_and_source_licensed() {
    let fixtures = release_fixtures();
    assert_eq!(fixtures.len(), 45);
    let ids = fixtures.iter().map(|row| &row.id).collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), fixtures.len(), "duplicate release fixture id");
    let paths = fixtures
        .iter()
        .map(|row| &row.source_path)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        paths.len(),
        fixtures.len(),
        "duplicate release fixture path"
    );

    for row in &fixtures {
        assert!(matches!(row.version, 1..=4), "{}", row.id);
        assert_eq!(
            row.maturity,
            if row.version >= 3 {
                "preview"
            } else {
                "experimental"
            },
            "{}",
            row.id
        );
        assert!(
            matches!(row.category.as_str(), "runtime" | "mtf"),
            "{}",
            row.id
        );
        assert!(
            matches!(
                row.realtime_policy.as_str(),
                "parity" | "legacy_lookahead_safe"
            ),
            "{}",
            row.id
        );
        assert_eq!(
            row.realtime_policy == "legacy_lookahead_safe",
            matches!(
                row.id.as_str(),
                "v1_security_aliases" | "v2_security_lookahead"
            ),
            "only v1/v2 historical lookahead profiles may diverge in realtime"
        );
        assert_eq!(row.license_class, "original", "{}", row.id);
        assert_eq!(
            row.execution_profile == "deterministic_clock",
            row.id == "v4_timenow_execution_clock",
            "only the timenow release fixture may require the execution clock profile"
        );
        assert!(
            workspace_root().join(&row.source_path).is_file(),
            "{}",
            row.id
        );
    }

    let manifest_runtime = fixtures
        .iter()
        .filter(|row| row.category == "runtime")
        .map(|row| row.source_path.clone())
        .collect::<BTreeSet<_>>();
    let mut discovered_runtime = BTreeSet::new();
    collect_runtime_legacy_sources(
        &workspace_root().join("tests/fixtures/legacy"),
        &mut discovered_runtime,
    );
    assert_eq!(manifest_runtime, discovered_runtime);
}

#[test]
fn every_release_fixture_matches_batch_incremental_realtime_and_resource_gates() {
    for row in release_fixtures() {
        let source_text = fs::read_to_string(workspace_root().join(&row.source_path))
            .unwrap_or_else(|error| panic!("{}: {error}", row.source_path));
        let analysis = analyze_source(&SourceFile::new(&row.source_path, source_text));
        assert!(
            analysis.diagnostics.is_empty(),
            "{} diagnostics: {:?}",
            row.id,
            analysis.diagnostics
        );
        assert_eq!(
            analysis.compatibility.language_version,
            Some(row.version),
            "{}",
            row.id
        );
        let program = analysis.hir.expect("release fixture should lower to HIR");
        let bars = fixture_bars(&row.bars_profile);
        assert!(!bars.is_empty(), "{}", row.id);
        let environment = request_environment(&row.request_profile);
        let execution_times = execution_times(&row.execution_profile, &bars);

        let profiled = match execution_times.as_deref() {
            Some(execution_times) => {
                run_historical_profiled_with_request_environment_and_input_overrides_and_execution_times(
                    &program,
                    &bars,
                    environment.clone(),
                    InputOverrides::new(),
                    execution_times,
                )
            }
            None => run_historical_profiled_with_request_environment(
                &program,
                &bars,
                environment.clone(),
            ),
        }
        .unwrap_or_else(|error| panic!("{} batch: {error:?}", row.id));
        let batch = profiled.result;
        assert_eq!(profiled.profile.bars, bars.len(), "{}", row.id);
        assert_release_profile(&row, "batch", &profiled.profile, None);

        let mut incremental =
            HistoricalRuntime::with_request_environment(&program, environment.clone());
        for (index, bar) in bars.iter().enumerate() {
            append_historical(
                &mut incremental,
                *bar,
                execution_times.as_ref().map(|values| values[index]),
            )
            .unwrap_or_else(|error| panic!("{} incremental: {error:?}", row.id));
        }
        assert_eq!(incremental.result(), batch, "{} incremental", row.id);
        assert_release_profile(
            &row,
            "incremental",
            &incremental.profile(),
            Some(&profiled.profile),
        );

        let mut historical_realtime =
            RealtimeRuntime::with_request_environment(&program, environment.clone());
        for (index, bar) in bars.iter().enumerate() {
            update_realtime(
                &mut historical_realtime,
                BarUpdate::historical(*bar),
                execution_times.as_ref().map(|values| values[index]),
            )
            .unwrap_or_else(|error| panic!("{} realtime history: {error:?}", row.id));
        }
        assert_eq!(
            historical_realtime.confirmed_result(),
            batch,
            "{} realtime historical handoff",
            row.id
        );
        assert_release_profile(
            &row,
            "realtime history",
            &historical_realtime.confirmed_profile(),
            Some(&profiled.profile),
        );

        let (last, history) = bars.split_last().expect("nonempty fixture bars");
        let mut realtime = RealtimeRuntime::with_request_environment(&program, environment);
        for (index, bar) in history.iter().enumerate() {
            update_realtime(
                &mut realtime,
                BarUpdate::historical(*bar),
                execution_times.as_ref().map(|values| values[index]),
            )
            .unwrap_or_else(|error| panic!("{} realtime prefix: {error:?}", row.id));
        }
        let confirmed_execution_time = execution_times
            .as_ref()
            .and_then(|values| values.last().copied());
        update_realtime(
            &mut realtime,
            BarUpdate::forming(mutated_forming_bar(last)),
            confirmed_execution_time.map(|value| value.saturating_sub(2)),
        )
        .unwrap_or_else(|error| panic!("{} first forming update: {error:?}", row.id));
        update_realtime(
            &mut realtime,
            BarUpdate::forming(*last),
            confirmed_execution_time.map(|value| value.saturating_sub(1)),
        )
        .unwrap_or_else(|error| panic!("{} replacement forming update: {error:?}", row.id));
        let confirmed = update_realtime(
            &mut realtime,
            BarUpdate::confirmed(*last),
            confirmed_execution_time,
        )
        .unwrap_or_else(|error| panic!("{} confirmed update: {error:?}", row.id));
        if row.realtime_policy == "parity" {
            assert_eq!(confirmed, batch, "{} forming rollback and confirm", row.id);
            assert_eq!(
                realtime.confirmed_result(),
                batch,
                "{} confirmed state",
                row.id
            );
        } else {
            let expected_confirmed = if row.id == "v1_security_aliases" {
                pine_runtime::PineValue::Float(300.0)
            } else {
                pine_runtime::PineValue::Na
            };
            assert_eq!(
                confirmed.plots[0].values.last(),
                Some(&expected_confirmed),
                "{} realtime must not expose historical lookahead",
                row.id
            );
            assert_ne!(
                confirmed, batch,
                "{} historical repaint must stay distinct from realtime",
                row.id
            );
        }
        assert_release_profile(
            &row,
            "realtime rollback/confirm",
            &realtime.confirmed_profile(),
            Some(&profiled.profile),
        );
    }
}
