//! Offline benchmark probe. No benchmark instrumentation enters the runtime API.
use std::{collections::BTreeMap, io, time::Instant};

#[path = "benchmark_support/memory.rs"]
mod memory;

use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, magnifier_input_from_json,
    public_runtime_profiled_result_json, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::{Severity, SourceFile};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    source: String,
    bars: Vec<InputBar>,
    warmup: usize,
    iters: usize,
    replacements: usize,
    magnifier: Option<Value>,
}

#[derive(Deserialize)]
struct InputBar {
    time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

fn timed<T>(times: &mut BTreeMap<&str, Vec<f64>>, key: &'static str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    times
        .entry(key)
        .or_default()
        .push(start.elapsed().as_secs_f64() * 1000.0);
    result
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input: Input = serde_json::from_reader(io::stdin())?;
    if input.iters == 0 || input.bars.len() < 2 || input.replacements < 2 {
        return Err("need iters > 0, at least two bars and two forming replacements".into());
    }
    let bars: Vec<Bar> = input
        .bars
        .iter()
        .map(|b| Bar {
            time: b.time,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume,
        })
        .collect();
    let source = SourceFile::new("benchmark.pine", input.source);
    let mut timings = BTreeMap::new();
    let mut expected = None;
    let mut expected_live = None;
    let mut profile = Value::Null;
    let mut live_profile = Value::Null;
    let mut final_result = String::new();
    let mut order_count = 0;
    for iteration in 0..input.warmup + input.iters {
        let mut current = BTreeMap::new();
        let analysis = timed(&mut current, "compile", || analyze_source(&source));
        if analysis
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
        {
            return Err(format!("analysis failed: {:?}", analysis.diagnostics).into());
        }
        let hir = analysis.hir.ok_or("no HIR")?;
        let magnifier = input
            .magnifier
            .as_ref()
            .map(|v| magnifier_input_from_json(&v.to_string()))
            .transpose()?;
        let mut batch = HistoricalRuntime::new(&hir);
        let mut incremental = HistoricalRuntime::new(&hir);
        if let Some(m) = &magnifier {
            batch = batch.with_magnifier_input(m.clone());
            incremental = incremental.with_magnifier_input(m.clone());
            batch
                .prepare_magnifier_chart_bar_count(bars.len())
                .map_err(|e| format!("{e:?}"))?;
            incremental
                .prepare_magnifier_chart_bar_count(bars.len())
                .map_err(|e| format!("{e:?}"))?;
        }
        timed(&mut current, "historicalRun", || batch.append_bars(&bars))
            .map_err(|e| format!("{e:?}"))?;
        timed(&mut current, "incrementalAppend", || {
            for bar in &bars {
                incremental.append_bar(*bar)?;
            }
            Ok::<_, pine_runtime::RuntimeError>(())
        })
        .map_err(|e| format!("{e:?}"))?;
        let result = timed(&mut current, "resultSnapshot", || batch.result());
        let incremental_result = incremental.result();
        let rendered = timed(&mut current, "outputSerialization", || {
            public_runtime_result_json(&result)
        });
        if rendered != public_runtime_result_json(&incremental_result) {
            return Err("batch/incremental result mismatch".into());
        }
        if expected.as_ref().is_some_and(|v| v != &rendered) {
            return Err("historical output changed between repetitions".into());
        }
        expected = Some(rendered.clone());
        profile = serde_json::from_str::<Value>(&public_runtime_profiled_result_json(
            &result,
            &batch.profile(),
        ))?["profile"]
            .clone();
        order_count = result.strategy.as_ref().map_or(0, |s| s.orders.len());
        // Lower-bar inputs are historical-only. Never silently benchmark fallback as magnification.
        if magnifier.is_none() {
            let mut live = RealtimeRuntime::new(&hir);
            timed(&mut current, "realtimeSeed", || {
                live.seed_historical(&bars[..bars.len() - 1])
            })
            .map_err(|e| format!("{e:?}"))?;
            let last = *bars.last().ok_or("missing last bar")?;
            timed(&mut current, "formingInitial", || {
                live.update(BarUpdate::forming(last))
            })
            .map_err(|e| format!("{e:?}"))?;
            for replacement in 0..input.replacements {
                let mut changed = last;
                changed.close = if replacement % 2 == 0 {
                    last.high
                } else {
                    last.low
                };
                let preview = timed(&mut current, "formingReplace", || {
                    live.update(BarUpdate::forming(changed))
                })
                .map_err(|e| format!("{e:?}"))?;
                std::hint::black_box(preview);
            }
            let confirmed = timed(&mut current, "formingConfirm", || {
                live.update(BarUpdate::confirmed(last))
            })
            .map_err(|e| format!("{e:?}"))?;
            // Same live sequence must be stable. It need not equal historical OHLC execution.
            let live_json = public_runtime_result_json(&confirmed);
            if expected_live.as_ref().is_some_and(|v| v != &live_json) {
                return Err("live output changed between repetitions".into());
            }
            expected_live = Some(live_json);
            live_profile = serde_json::from_str::<Value>(&public_runtime_profiled_result_json(
                &confirmed,
                &live.confirmed_profile(),
            ))?["profile"]
                .clone();
        }
        final_result = rendered;
        if iteration >= input.warmup {
            for (key, values) in current {
                timings.entry(key).or_insert_with(Vec::new).extend(values);
            }
        }
    }
    // Process high-water mark includes input, compilation, all phases and verification.
    // It is intentionally not presented as runtime-only retained memory.
    let memory = memory::read();
    println!(
        "{}",
        json!({
            "timingsMs": timings, "result": serde_json::from_str::<Value>(&final_result)?,
            "liveResult": expected_live.map(|s| serde_json::from_str::<Value>(&s)).transpose()?,
            "profile": profile, "confirmedRealtimeProfile": live_profile,
            "rssKiB": memory.resident_kib, "commitKiB": memory.commit_kib,
            "peakRssKiB": memory.peak_resident_kib, "peakCommitKiB": memory.peak_commit_kib, "memorySource": memory::SOURCE, "orderCount": order_count,
            "correctness": {"batchEqualsIncremental": true, "repeatedHistoricalStable": true,
                "repeatedLiveStable": if input.magnifier.is_none() { Some(true) } else { None }},
            "realtimeExclusion": if input.magnifier.is_some() { Some("historical-only magnifier input") } else { None },
        })
    );
    Ok(())
}
