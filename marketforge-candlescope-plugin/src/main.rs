use std::io;

use marketforge_candlescope_plugin::JsonLineServer;

fn run() -> Result<(), String> {
    for argument in std::env::args().skip(1) {
        if argument != "--jsonl" {
            return Err(format!("unsupported argument {argument}"));
        }
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    JsonLineServer::new()
        .serve(stdin.lock(), &mut stdout.lock())
        .map_err(|error| format!("JSONL transport failed: {error}"))
}

fn main() {
    if let Err(message) = run() {
        eprintln!("MarketForge CandleScope plugin failed: {message}");
        std::process::exit(2);
    }
}
