//! Aggregate elapsed times for benchmark builds only. These are cumulative
//! wall times, including scheduling and I/O waits, not CPU time. Percentiles
//! are upper bounds from power-of-two nanosecond buckets.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub(crate) struct Counter {
    count: AtomicU64,
    nanos: AtomicU64,
    max_nanos: AtomicU64,
    buckets: [AtomicU64; 65],
}

impl Counter {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            max_nanos: AtomicU64::new(0),
            buckets: [const { AtomicU64::new(0) }; 65],
        }
    }

    pub(crate) fn record(&self, started: Instant) {
        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.record_nanos(nanos);
    }

    fn record_nanos(&self, nanos: u64) {
        self.nanos.fetch_add(nanos, Ordering::Relaxed);
        self.max_nanos.fetch_max(nanos, Ordering::Relaxed);
        let bucket = 64 - nanos.saturating_sub(1).leading_zeros() as usize;
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn p95_upper_nanos(&self) -> u64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0;
        }
        let target = count.saturating_mul(95).div_ceil(100);
        let mut seen = 0u64;
        for (index, bucket) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(bucket.load(Ordering::Relaxed));
            if seen >= target {
                return 1u64.checked_shl(index as u32).unwrap_or(u64::MAX);
            }
        }
        self.max_nanos.load(Ordering::Relaxed)
    }
}

// Scope timers also record failures and early returns in diagnostic builds.
pub(crate) struct Timer(&'static Counter, pub(crate) Instant);
impl Timer {
    pub(crate) fn start(counter: &'static Counter) -> Self {
        Self(counter, Instant::now())
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        self.0.record(self.1);
    }
}

pub(crate) static DIRECTORY_WAIT: Counter = Counter::new();
pub(crate) static ROOM_WAIT: Counter = Counter::new();
pub(crate) static ROOM_HELD: Counter = Counter::new();
pub(crate) static VENUE_WAIT: Counter = Counter::new();
pub(crate) static VENUE_COMPUTE: Counter = Counter::new();
pub(crate) static VENUE_COMMIT: Counter = Counter::new();
pub(crate) static VENUE_FALLBACK: Counter = Counter::new();
pub(crate) static ORDER_PREPARE: Counter = Counter::new();
pub(crate) static ORDER_APPLY: Counter = Counter::new();
pub(crate) static ORDER_RECEIPT: Counter = Counter::new();
pub(crate) static ORDER_JOURNAL: Counter = Counter::new();
pub(crate) static ORDER_PUBLISH: Counter = Counter::new();
pub(crate) static BOT_STATE_COPY: Counter = Counter::new();
pub(crate) static BOT_BLOCKING_WAIT: Counter = Counter::new();
pub(crate) static BOT_TURN: Counter = Counter::new();
pub(crate) static BOT_OBSERVATION: Counter = Counter::new();
pub(crate) static BOT_EVENT_PREFILTER: Counter = Counter::new();
pub(crate) static BOT_EVENT_INDEX: Counter = Counter::new();
pub(crate) static BOT_EVENT_SKIP: Counter = Counter::new();
pub(crate) static BOT_TRIGGER: Counter = Counter::new();
pub(crate) static BOT_EVALUATE: Counter = Counter::new();
pub(crate) static BOT_WORKER_ACQUIRE: Counter = Counter::new();
pub(crate) static BOT_WIRE_ENCODE: Counter = Counter::new();
pub(crate) static BOT_WORKER_ROUNDTRIP: Counter = Counter::new();
pub(crate) static BOT_WIRE_DECODE: Counter = Counter::new();

pub(crate) static SCHEDULER_ADMISSION: Counter = Counter::new();
pub(crate) static SCHEDULER_APPLY: Counter = Counter::new();
pub(crate) static SCHEDULER_MUTATION_BUILD: Counter = Counter::new();
pub(crate) static SCHEDULER_CHECKPOINT: Counter = Counter::new();
pub(crate) static SCHEDULER_INSTALL: Counter = Counter::new();
pub(crate) static SCHEDULER_MATERIALIZE: Counter = Counter::new();
pub(crate) static SCHEDULER_DELTA_CLONE: Counter = Counter::new();
pub(crate) static SCHEDULER_DELTA_BETWEEN: Counter = Counter::new();
pub(crate) static SCHEDULER_PUBLISH: Counter = Counter::new();
pub(crate) static JOURNAL_REQUEST_CLONE: Counter = Counter::new();
pub(crate) static JOURNAL_QUEUE: Counter = Counter::new();
pub(crate) static JOURNAL_OPERATION: Counter = Counter::new();
pub(crate) static JOURNAL_VALIDATE: Counter = Counter::new();
pub(crate) static JOURNAL_SERIALIZE: Counter = Counter::new();
pub(crate) static JOURNAL_COMPRESS: Counter = Counter::new();
pub(crate) static BOT_WORKER_START: Counter = Counter::new();
pub(crate) static BOT_REQUEST_CLONE: Counter = Counter::new();
pub(crate) static BOT_STATE_RESTORE: Counter = Counter::new();
pub(crate) static BOT_INPROCESS_CALL: Counter = Counter::new();
pub(crate) static BOT_INPROCESS_REBUILD: Counter = Counter::new();
pub(crate) static BOT_INPROCESS_APPEND: Counter = Counter::new();
pub(crate) static BOT_DISPATCH_QUEUE: Counter = Counter::new();
pub(crate) static BOT_QUEUE_DISCARD: Counter = Counter::new();
pub(crate) static JOURNAL_FULL_VALIDATE: Counter = Counter::new();
pub(crate) static JOURNAL_FULL_SERIALIZE: Counter = Counter::new();
pub(crate) static BOT_WAVE_BUILD: Counter = Counter::new();
pub(crate) static BOT_WAVE_REUSE: Counter = Counter::new();

// Optional benchmark trace contains timing/identity only, never account payloads.
static TRACE: std::sync::OnceLock<Option<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>> =
    std::sync::OnceLock::new();
pub(crate) fn trace(stage: &str, id: &str, market_time_ms: u64, elapsed_us: u64) {
    use std::io::Write;
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    let Some(writer) = TRACE.get_or_init(|| {
        std::env::var("PINE_BENCH_HOST_TRACE")
            .ok()
            .and_then(|path| std::fs::File::create(path).ok())
            .map(|file| std::sync::Mutex::new(std::io::BufWriter::new(file)))
    }) else {
        return;
    };
    let record = serde_json::json!({"stage":stage,"id":id,"market_time_ms":market_time_ms,
        "wall_us":origin.elapsed().as_micros() as u64,"elapsed_us":elapsed_us});
    if let Ok(mut writer) = writer.lock() {
        let _ = writeln!(writer, "{record}");
    }
}

pub(crate) fn append_metrics(body: &mut String) {
    use std::io::Write;
    if let Some(Some(writer)) = TRACE.get()
        && let Ok(mut writer) = writer.lock()
    {
        let _ = writer.flush();
    }
    for (name, counter) in [
        ("directory_wait", &DIRECTORY_WAIT),
        ("room_wait", &ROOM_WAIT),
        ("room_held", &ROOM_HELD),
        ("venue_wait", &VENUE_WAIT),
        ("venue_compute", &VENUE_COMPUTE),
        ("venue_commit", &VENUE_COMMIT),
        ("venue_fallback", &VENUE_FALLBACK),
        ("order_prepare", &ORDER_PREPARE),
        ("order_apply", &ORDER_APPLY),
        ("order_receipt", &ORDER_RECEIPT),
        ("order_journal", &ORDER_JOURNAL),
        ("order_publish", &ORDER_PUBLISH),
        ("bot_state_copy", &BOT_STATE_COPY),
        ("bot_blocking_wait", &BOT_BLOCKING_WAIT),
        ("bot_turn", &BOT_TURN),
        ("bot_observation", &BOT_OBSERVATION),
        ("bot_wave_build", &BOT_WAVE_BUILD),
        ("bot_wave_reuse", &BOT_WAVE_REUSE),
        ("bot_event_prefilter", &BOT_EVENT_PREFILTER),
        ("bot_event_index", &BOT_EVENT_INDEX),
        ("bot_event_skip", &BOT_EVENT_SKIP),
        ("bot_trigger", &BOT_TRIGGER),
        ("bot_evaluate", &BOT_EVALUATE),
        ("bot_worker_acquire", &BOT_WORKER_ACQUIRE),
        ("bot_wire_encode", &BOT_WIRE_ENCODE),
        ("bot_worker_roundtrip", &BOT_WORKER_ROUNDTRIP),
        ("bot_wire_decode", &BOT_WIRE_DECODE),
        ("scheduler_admission", &SCHEDULER_ADMISSION),
        ("scheduler_apply", &SCHEDULER_APPLY),
        ("scheduler_mutation_build", &SCHEDULER_MUTATION_BUILD),
        ("scheduler_checkpoint", &SCHEDULER_CHECKPOINT),
        ("scheduler_install", &SCHEDULER_INSTALL),
        ("scheduler_materialize", &SCHEDULER_MATERIALIZE),
        ("scheduler_delta_clone", &SCHEDULER_DELTA_CLONE),
        ("scheduler_delta_between", &SCHEDULER_DELTA_BETWEEN),
        ("scheduler_publish", &SCHEDULER_PUBLISH),
        ("journal_request_clone", &JOURNAL_REQUEST_CLONE),
        ("journal_queue", &JOURNAL_QUEUE),
        ("journal_operation", &JOURNAL_OPERATION),
        ("journal_validate", &JOURNAL_VALIDATE),
        ("journal_serialize", &JOURNAL_SERIALIZE),
        ("journal_compress", &JOURNAL_COMPRESS),
        ("bot_worker_start", &BOT_WORKER_START),
        ("bot_request_clone", &BOT_REQUEST_CLONE),
        ("bot_state_restore", &BOT_STATE_RESTORE),
        ("bot_inprocess_call", &BOT_INPROCESS_CALL),
        ("bot_inprocess_rebuild", &BOT_INPROCESS_REBUILD),
        ("bot_inprocess_append", &BOT_INPROCESS_APPEND),
        ("bot_dispatch_queue", &BOT_DISPATCH_QUEUE),
        ("bot_queue_discard", &BOT_QUEUE_DISCARD),
        ("journal_full_validate", &JOURNAL_FULL_VALIDATE),
        ("journal_full_serialize", &JOURNAL_FULL_SERIALIZE),
    ] {
        for (suffix, kind, value) in [
            (
                "count_total",
                "counter",
                counter.count.load(Ordering::Relaxed),
            ),
            (
                "nanos_total",
                "counter",
                counter.nanos.load(Ordering::Relaxed),
            ),
            (
                "max_nanos",
                "gauge",
                counter.max_nanos.load(Ordering::Relaxed),
            ),
            ("p95_upper_nanos", "gauge", counter.p95_upper_nanos()),
        ] {
            super::append_prometheus_metric(
                body,
                &format!("marketforge_bench_{name}_{suffix}"),
                "Benchmark-only cumulative elapsed wall time; excludes no scheduling waits.",
                kind,
                value,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Counter;

    #[test]
    fn elapsed_histogram_bounds_a_known_tail_and_handles_empty_and_extreme_samples() {
        let counter = Counter::new();
        assert_eq!(counter.p95_upper_nanos(), 0);
        for _ in 0..94 {
            counter.record_nanos(1_000_000);
        }
        for _ in 0..6 {
            counter.record_nanos(5_000_000);
        }
        assert!((5_000_000..10_000_000).contains(&counter.p95_upper_nanos()));
        let extreme = Counter::new();
        extreme.record_nanos(u64::MAX);
        assert_eq!(extreme.p95_upper_nanos(), u64::MAX);
    }
}
