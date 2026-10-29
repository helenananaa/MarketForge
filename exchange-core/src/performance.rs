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

pub const NANO_PHASES: [&str; 38] = ["actor_before", "actor_prechecks", "actor_clone", "actor_pre_reconcile", "actor_match", "actor_post_reconcile", "actor_sync_venue", "actor_price_links", "actor_margin_group", "actor_margin_group_full", "actor_commit", "actor_attach", "sim_make_mut", "room_exec_clone", "room_record", "room_protections", "room_liquidations", "gateway_owner", "pp_flat", "pp_cond", "pp_exits", "mkt_depth", "mkt_inner", "mkt_delta", "perp_clone", "perp_apply", "perp_commit", "spot_clone", "spot_apply", "spot_commit", "spot_risk", "spot_book", "spot_log", "spot_settle", "perp_risk", "perp_book", "perp_log", "perp_settle"];
static NANOS: [AtomicU64; 38] = [const { AtomicU64::new(0) }; 38];
static NANO_CALLS: [AtomicU64; 38] = [const { AtomicU64::new(0) }; 38];
static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub const COUNT_NAMES: [&str; 8] = ["pp_entries", "pp_active", "co_entries", "co_armed", "flat_entries", "flat_armed", "pp_passes", "co_passes"];
pub fn count(i: usize, n: u64) {
    COUNTS[i].fetch_add(n, Ordering::Relaxed);
}
pub fn count_snapshot() -> Vec<(&'static str, u64)> {
    (0..8).map(|i| (COUNT_NAMES[i], COUNTS[i].load(Ordering::Relaxed))).collect()
}
pub fn nano_snapshot() -> Vec<(&'static str, u64, u64)> {
    (0..NANO_PHASES.len()).map(|i| (NANO_PHASES[i], NANO_CALLS[i].load(Ordering::Relaxed), NANOS[i].load(Ordering::Relaxed))).collect()
}
pub struct NanoTimer(usize, Instant);
pub fn nano(phase: usize) -> NanoTimer {
    NanoTimer(phase, Instant::now())
}
impl Drop for NanoTimer {
    fn drop(&mut self) {
        NANO_CALLS[self.0].fetch_add(1, Ordering::Relaxed);
        NANOS[self.0].fetch_add(self.1.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
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
