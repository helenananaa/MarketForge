//! Identical research decisions; standard buffered durable checkpoint output.
#[allow(dead_code)]
#[path = "../../exchange-core/examples/strategy_market.rs"]
mod research;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry =
        exchange_server::bot_plugins::load_bot_plugins(std::path::Path::new("bot-plugins"))?;
    research::run(registry)
}
