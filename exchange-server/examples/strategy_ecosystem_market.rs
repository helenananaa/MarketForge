//! Actual installed process Pine + native strategies, real matching research.
#[allow(dead_code)]
#[path = "../../exchange-core/examples/strategy_market.rs"]
mod research;
// Each order makes a few hundred small allocations, and the system heap slows
// as account count grows; mimalloc keeps them cheap without changing results.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
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
