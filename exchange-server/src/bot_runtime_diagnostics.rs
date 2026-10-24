//! Low-cardinality runtime routing counters and opt-in decision receipts.
//!
//! A route is a host choice; an evaluation mode is reported where evaluation
//! actually occurs. Neither is evidence that an order was durably admitted.
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

#[path = "bot_runtime_diagnostics_shutdown.rs"]
mod shutdown;
pub(crate) use shutdown::{finish_trace, register_worker, shutdown_failed, shutdown_requested};

const ROUTES: [&str; 5] = ["native", "direct", "process", "blocked", "other"];
const MODES: [&str; 6] = [
    "append",
    "rebuild",
    "replay",
    "checkpoint_restore",
    "no_evaluation",
    "unknown",
];
const REASONS: [&str; 23] = [
    "none",
    "feature_disabled",
    "disabled",
    "plugin_id",
    "plugin_version",
    "state_version",
    "execution_mode",
    "source_unqualified",
    "ecosystem_config",
    "ecosystem_history",
    "hedging",
    "noninteger_inputs",
    "hedge_state",
    "exit_protection",
    "runtime_no_session",
    "session_missing",
    "state_mismatch",
    "one_shot",
    "worker_unreported",
    "legacy",
    "worker_failure",
    "checkpoint_requires_direct",
    "unknown",
];
static ROUTE_COUNTS: [AtomicU64; ROUTES.len() * REASONS.len()] =
    [const { AtomicU64::new(0) }; ROUTES.len() * REASONS.len()];
static EVALUATION_COUNTS: [AtomicU64; ROUTES.len() * MODES.len() * REASONS.len()] =
    [const { AtomicU64::new(0) }; ROUTES.len() * MODES.len() * REASONS.len()];
static TRACE_ERRORS: AtomicU64 = AtomicU64::new(0);
static DECISION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn label_index(labels: &[&str], value: &str) -> usize {
    labels
        .iter()
        .position(|label| *label == value)
        .unwrap_or(labels.len() - 1)
}

pub(crate) fn record_route(route: &str, reason: Option<&str>) {
    let index = label_index(&ROUTES, route) * REASONS.len()
        + label_index(&REASONS, reason.unwrap_or("none"));
    ROUTE_COUNTS[index].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_evaluation(route: &str, mode: &str, reason: Option<&str>) {
    let index = (label_index(&ROUTES, route) * MODES.len() + label_index(&MODES, mode))
        * REASONS.len()
        + label_index(&REASONS, reason.unwrap_or("none"));
    EVALUATION_COUNTS[index].fetch_add(1, Ordering::Relaxed);
}

struct Trace {
    origin: Instant,
    writer: Mutex<std::io::BufWriter<std::fs::File>>,
}
static TRACE: OnceLock<Option<Trace>> = OnceLock::new();

fn trace_sink() -> Option<&'static Trace> {
    TRACE
        .get_or_init(|| {
            let path = std::env::var_os("MARKETFORGE_BOT_TRACE")?;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(file) => Some(Trace {
                    origin: Instant::now(),
                    writer: Mutex::new(std::io::BufWriter::new(file)),
                }),
                Err(_) => {
                    TRACE_ERRORS.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        })
        .as_ref()
}

/// Call before constructing a receipt so ordinary runs do not allocate JSON.
pub(crate) fn next_decision_id() -> Option<u64> {
    trace_sink()?;
    Some(DECISION_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1)
}

pub(crate) fn trace_enabled() -> bool {
    trace_sink().is_some()
}

pub(crate) fn trace_wall_us() -> Option<u64> {
    Some(u64::try_from(trace_sink()?.origin.elapsed().as_micros()).unwrap_or(u64::MAX))
}

/// Receipts contain identities, timing and outcomes only, never strategy inputs
/// or account payloads. Transaction callers emit durable outcomes after install.
pub(crate) fn trace_event(event: serde_json::Value) {
    let Some(wall_us) = trace_wall_us() else {
        return;
    };
    trace_event_at(event, wall_us);
}

/// A transaction uses one captured publication timestamp for all its actions;
/// formatting/flushing the first receipt must not delay the timestamp of later ones.
pub(crate) fn trace_event_at(mut event: serde_json::Value, wall_us: u64) {
    use std::io::Write;
    let Some(trace) = trace_sink() else { return };
    let Some(object) = event.as_object_mut() else {
        TRACE_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    object.insert("schema_version".into(), 1.into());
    object.insert("wall_us".into(), wall_us.into());
    let result = trace.writer.lock().map_err(|_| ()).and_then(|mut writer| {
        serde_json::to_writer(&mut *writer, &event).map_err(|_| ())?;
        writer.write_all(b"\n").map_err(|_| ())
    });
    if result.is_err() {
        TRACE_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn append_metrics(body: &mut String) {
    use std::fmt::Write as _;
    use std::io::Write as _;
    let trace = trace_sink();
    if let Some(trace) = trace {
        let flushed = trace
            .writer
            .lock()
            .map_err(|_| ())
            .and_then(|mut writer| writer.flush().map_err(|_| ()));
        if flushed.is_err() {
            TRACE_ERRORS.fetch_add(1, Ordering::Relaxed);
        }
    }
    let _ = writeln!(body, "marketforge_bot_runtime_diagnostics 1");
    let _ = writeln!(
        body,
        "marketforge_bot_trace_enabled {}",
        u8::from(trace.is_some())
    );
    if let Some(trace) = trace {
        let _ = writeln!(
            body,
            "marketforge_bot_trace_wall_us {}",
            trace.origin.elapsed().as_micros()
        );
    }
    let _ = writeln!(
        body,
        "marketforge_bot_trace_errors_total {}",
        TRACE_ERRORS.load(Ordering::Relaxed)
    );
    for (route_index, route) in ROUTES.iter().enumerate() {
        for (reason_index, reason) in REASONS.iter().enumerate() {
            let count =
                ROUTE_COUNTS[route_index * REASONS.len() + reason_index].load(Ordering::Relaxed);
            if count != 0 {
                let _ = writeln!(
                    body,
                    "marketforge_bot_runtime_route_total{{route=\"{route}\",reason=\"{reason}\"}} {count}"
                );
            }
            for (mode_index, mode) in MODES.iter().enumerate() {
                let index = (route_index * MODES.len() + mode_index) * REASONS.len() + reason_index;
                let count = EVALUATION_COUNTS[index].load(Ordering::Relaxed);
                if count != 0 {
                    let _ = writeln!(
                        body,
                        "marketforge_bot_runtime_evaluation_total{{route=\"{route}\",mode=\"{mode}\",reason=\"{reason}\"}} {count}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_worker_labels_cannot_increase_metric_cardinality() {
        assert_eq!(
            label_index(&ROUTES, "private-script-name"),
            ROUTES.len() - 1
        );
        assert_eq!(
            label_index(&REASONS, "an arbitrary worker error"),
            REASONS.len() - 1
        );
        assert_eq!(label_index(&MODES, "replay"), 2);
    }
}
