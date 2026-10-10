//! Process-wide diagnostic counters. Timings may nest; never sum them as
//! disjoint CPU time or interpret them as latency percentiles.
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

pub const PHASES: [&str; 4] = [
    "actor_clone",
    "margin_sync",
    "reservation_reconcile",
    "liquidation_scan",
];
static MICROS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static CALLS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

pub fn snapshot() -> [(u64, u64); 4] {
    std::array::from_fn(|i| {
        (
            CALLS[i].load(Ordering::Relaxed),
            MICROS[i].load(Ordering::Relaxed),
        )
    })
}

pub(crate) struct Timer(usize, Instant);
impl Timer {
    pub(crate) fn start(phase: usize) -> Self {
        Self(phase, Instant::now())
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        CALLS[self.0].fetch_add(1, Ordering::Relaxed);
        MICROS[self.0].fetch_add(
            u64::try_from(self.1.elapsed().as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}
