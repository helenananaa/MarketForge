//! Copy-cost probe: complete snapshot construction versus streaming apply.
use std::{collections::BTreeMap, time::Instant};

use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, public_runtime_result_json};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;
use serde_json::json;

const SOURCE: &str = "//@version=6\nindicator(\"copy cost\")\nplot(close)\nplot(close[1])\n";

fn bar(index: usize) -> Bar {
    let close = 100.0 + index as f64 * 0.01;
    Bar {
        time: index as i64 * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn timed_ms<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed().as_secs_f64() * 1000.0)
}

fn measure(history_len: usize, tail: usize) -> serde_json::Value {
    let hir = analyze_source(&SourceFile::new("copy_cost.pine", SOURCE))
        .hir
        .expect("source should compile");
    let history: Vec<Bar> = (0..history_len).map(bar).collect();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.seed_historical(&history).expect("seed should run");

    let mut apply_ms = Vec::new();
    let mut snapshot_ms = Vec::new();
    let mut serialize_ms = Vec::new();
    let mut drop_ms = Vec::new();

    for offset in 0..tail {
        let time = (history_len + offset) as i64 * 60_000;
        let forming = bar(history_len + offset);
        let (_, apply) = timed_ms(|| {
            runtime
                .apply_update(BarUpdate::forming(forming))
                .expect("streaming apply");
        });
        apply_ms.push(apply);

        let (snapshot, snapshot_time) = timed_ms(|| runtime.result());
        snapshot_ms.push(snapshot_time);
        let (rendered, serialize_time) = timed_ms(|| public_runtime_result_json(&snapshot));
        serialize_ms.push(serialize_time);
        std::hint::black_box(rendered);
        let (_, drop_time) = timed_ms(|| drop(snapshot));
        drop_ms.push(drop_time);

        runtime
            .apply_update(BarUpdate::confirmed(Bar { time, ..forming }))
            .expect("streaming confirm");
    }

    json!({
        "historyLen": history_len,
        "tail": tail,
        "applyUpdateMs": summary(&apply_ms),
        "resultSnapshotMs": summary(&snapshot_ms),
        "outputSerializationMs": summary(&serialize_ms),
        "snapshotDropMs": summary(&drop_ms),
    })
}

fn summary(samples: &[f64]) -> serde_json::Value {
    let mut ordered = samples.to_vec();
    ordered.sort_by(|left, right| left.partial_cmp(right).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    json!({
        "count": samples.len(),
        "mean": mean,
        "p50": percentile(&ordered, 0.50),
        "p95": percentile(&ordered, 0.95),
    })
}

fn percentile(ordered: &[f64], quantile: f64) -> f64 {
    if ordered.is_empty() {
        return 0.0;
    }
    let index = ((ordered.len() as f64 - 1.0) * quantile).round() as usize;
    ordered[index.min(ordered.len() - 1)]
}

fn main() {
    let mut report: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    report.insert("short", measure(256, 8));
    report.insert("longer", measure(1024, 8));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "probe": "streaming-copy-cost",
            "note": "diagnostic only; not a D4 100k/10k acceptance run",
            "workloads": report,
        }))
        .expect("json")
    );
}
