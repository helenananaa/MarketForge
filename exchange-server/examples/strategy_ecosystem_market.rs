//! Actual installed process Pine + native strategies, real matching research.
#[allow(dead_code)]
#[path = "../../exchange-core/examples/strategy_market.rs"]
mod research;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
// allocs, alloc ns, frees, free ns
pub static ALLOC: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
struct Timed;
unsafe impl GlobalAlloc for Timed {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let t = std::time::Instant::now();
        let p = unsafe { System.alloc(l) };
        ALLOC[0].fetch_add(1, Relaxed);
        ALLOC[1].fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let t = std::time::Instant::now();
        let p = unsafe { System.alloc_zeroed(l) };
        ALLOC[0].fetch_add(1, Relaxed);
        ALLOC[1].fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        p
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let t = std::time::Instant::now();
        let p = unsafe { System.realloc(p, l, n) };
        ALLOC[0].fetch_add(1, Relaxed);
        ALLOC[1].fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let t = std::time::Instant::now();
        unsafe { System.dealloc(p, l) };
        ALLOC[2].fetch_add(1, Relaxed);
        ALLOC[3].fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
    }
}
#[global_allocator]
static GLOBAL: Timed = Timed;
pub fn alloc_snapshot() -> [u64; 4] {
    [0, 1, 2, 3].map(|i| ALLOC[i].load(Relaxed))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry =
        exchange_server::bot_plugins::load_bot_plugins(std::path::Path::new("bot-plugins"))?;
    let result = research::run(registry);
    // Diagnostic builds also report the plugin host's cumulative phase timers.
    #[cfg(feature = "bench-execution-timing")]
    for line in exchange_server::bench_execution_metrics().lines() {
        if line.starts_with("marketforge_bench_bot") && !line.ends_with(" 0") {
            eprintln!("{line}");
        }
    }
    result
}
