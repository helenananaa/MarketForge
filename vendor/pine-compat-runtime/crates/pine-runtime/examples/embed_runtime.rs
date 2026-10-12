//! Host-neutral embedding walkthrough with deterministic synthetic inputs.
use std::error::Error;

use pine_runtime::{
    Bar, BarUpdate, ChartContext, HistoricalRuntime, RealtimeRuntime, RequestEnvironment,
    RequestTimeframe, RuntimeReplica, host_requirements_json, public_runtime_result_json,
};
use pine_sema::{AnalysisInput, analyze_input};
use pine_syntax::SourceFile;
use serde_json::{Value, json};

const SOURCE: &str = r#"//@version=6
import Example/Scale/1 as scale
indicator("Embedded runtime")
factor = input.float(2.0, "Scale")
plot(scale.apply(close, factor), "scaled close")
plot(timenow, "host clock")
plot(close[1], "previous close")
"#;

const LIBRARY: &str = r#"//@version=6
library("Scale")
export apply(float value, float factor) => value * factor
"#;

fn bar(time: i64, close: f64) -> Bar {
    Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // The host owns source acquisition; the compiler receives complete text.
    let input = AnalysisInput::with_library_sources(
        SourceFile::new("embedded.pine", SOURCE),
        vec![(
            "Example/Scale/1".into(),
            SourceFile::new("scale.pine", LIBRARY),
        )],
    )
    .map_err(|error| error.to_string())?;
    let analysis = analyze_input(&input);
    for diagnostic in &analysis.diagnostics {
        eprintln!("{}: {}", diagnostic.code, diagnostic.message);
    }
    let hir = analysis.hir.ok_or("source did not compile")?;
    let requirements: Value = serde_json::from_str(&host_requirements_json(&hir))?;

    // No symbol lookup or network service is involved. These are host choices.
    let chart = ChartContext::new(
        "EXAMPLE:UNIT",
        RequestTimeframe::parse("1").map_err(|error| error.to_string())?,
    )
    .with_price_grid(1, 100)
    .map_err(|error| error.to_string())?
    .with_quantity_precision(0)
    .map_err(|error| error.to_string())?;
    let environment = RequestEnvironment::default().for_chart(chart);
    let history = [bar(0, 10.0), bar(60_000, 11.0)];
    let clocks = [59_999, 119_999];

    let mut batch = HistoricalRuntime::with_request_environment(&hir, environment.clone());
    batch
        .append_bars_with_execution_times(&history, &clocks)
        .map_err(|error| error.message)?;
    let historical_json = public_runtime_result_json(&batch.result());
    let mut incremental = HistoricalRuntime::with_request_environment(&hir, environment.clone());
    for (bar, clock) in history.iter().zip(clocks) {
        incremental
            .append_bar_with_execution_time(*bar, clock)
            .map_err(|error| error.message)?;
    }
    assert_eq!(
        public_runtime_result_json(&incremental.result()),
        historical_json
    );

    let mut live = RealtimeRuntime::with_request_environment(&hir, environment.clone());
    live.seed_historical_with_execution_times(&history, &clocks)
        .map_err(|error| error.message)?;
    let forming = bar(120_000, 12.0);
    let before = public_runtime_result_json(&live.result());
    let rejected = live
        .update(BarUpdate::forming(forming))
        .expect_err("this source reaches timenow and needs an explicit clock");
    assert!(rejected.message.contains("timenow"));
    assert!(rejected.message.contains("execution timestamp"));
    assert_eq!(public_runtime_result_json(&live.result()), before);

    live.update_with_execution_time(BarUpdate::forming(forming), 120_100)
        .map_err(|error| error.message)?;
    let replacement = Bar {
        high: 14.0,
        close: 14.0,
        volume: 2.0,
        ..forming
    };
    let preview = live
        .update_with_execution_time(BarUpdate::forming(replacement), 120_200)
        .map_err(|error| error.message)?;
    assert_eq!(
        preview.plots[0]
            .values
            .last()
            .and_then(|value| value.as_f64()),
        Some(28.0)
    );
    assert_eq!(
        public_runtime_result_json(&live.confirmed_result()),
        historical_json
    );
    let closed = Bar {
        high: 14.0,
        close: 13.0,
        volume: 3.0,
        ..forming
    };
    let mut streaming = RealtimeRuntime::with_request_environment(&hir, environment.clone());
    let visible = streaming
        .seed_historical_with_execution_times(&history, &clocks)
        .map_err(|error| error.message)?;
    let forming_changes = streaming
        .apply_update_with_execution_time(BarUpdate::forming(forming), 120_100)
        .map_err(|error| error.message)?;
    let mut replica = RuntimeReplica::new(visible, forming_changes.base_revision);
    replica
        .apply(&forming_changes)
        .map_err(|error| error.message)?;
    let replacement_changes = streaming
        .apply_update_with_execution_time(BarUpdate::forming(replacement), 120_200)
        .map_err(|error| error.message)?;
    replica
        .apply(&replacement_changes)
        .map_err(|error| error.message)?;
    let visible = replica.result();
    assert_eq!(
        visible.plots[0]
            .values
            .last()
            .and_then(|value| value.as_f64()),
        Some(28.0)
    );
    let confirmed_changes = streaming
        .apply_update_with_execution_time(BarUpdate::confirmed(closed), 179_999)
        .map_err(|error| error.message)?;
    replica
        .apply(&confirmed_changes)
        .map_err(|error| error.message)?;
    let visible = replica.result();
    assert_eq!(visible.plots, streaming.result().plots);
    assert_eq!(
        visible.plots[0]
            .values
            .last()
            .and_then(|value| value.as_f64()),
        Some(26.0)
    );

    let result = live
        .update_with_execution_time(BarUpdate::confirmed(closed), 179_999)
        .map_err(|error| error.message)?;
    assert_eq!(result.plots[0].values.len(), 3);
    assert_eq!(
        result.plots[1]
            .values
            .last()
            .and_then(|value| value.as_i64()),
        Some(179_999)
    );
    assert_eq!(
        result.plots[0]
            .values
            .last()
            .and_then(|value| value.as_f64()),
        Some(26.0)
    );
    assert_eq!(
        result.plots[2]
            .values
            .last()
            .and_then(|value| value.as_f64()),
        Some(11.0)
    );

    let result: Value = serde_json::from_str(&public_runtime_result_json(&result))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "example":"host-neutral-rust-embedding", "syntheticInputs":true,
            "packageVersion": env!("CARGO_PKG_VERSION"),
            "historicalEqualsIncremental":true, "failedUpdatePreservedState":true,
            "streamingApplyMatchedSnapshot":true,
            "missingClockError":rejected.message, "requirements":requirements, "result":result,
        }))?
    );
    Ok(())
}
