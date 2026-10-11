//! Aggregate elapsed times for benchmark builds only. These are cumulative
//! wall times, including scheduling and I/O waits, not CPU time or percentiles.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub(crate) struct Counter {
    count: AtomicU64,
    nanos: AtomicU64,
    max_nanos: AtomicU64,
}

impl Counter {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            max_nanos: AtomicU64::new(0),
        }
    }

    pub(crate) fn record(&self, started: Instant) {
        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.nanos.fetch_add(nanos, Ordering::Relaxed);
        self.max_nanos.fetch_max(nanos, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
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

pub(crate) fn append_metrics(body: &mut String) {
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
