use std::io;

use marketforge_candlescope_plugin::{
    JsonLineServer, PlatformRuntime, PluginService, RemoteBackendConfig,
};

fn run() -> Result<(), String> {
    for argument in std::env::args().skip(1) {
        if argument != "--jsonl" {
            return Err(format!("unsupported argument {argument}"));
        }
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let service = match RemoteBackendConfig::from_env().map_err(|error| error.to_string())? {
        Some(config) => PluginService::with_remote_backend(config),
        None => PluginService::new(),
    };
    JsonLineServer::with_runtime(PlatformRuntime::with_service(service))
        .serve(stdin.lock(), &mut stdout.lock())
        .map_err(|error| format!("JSONL transport failed: {error}"))
}

fn main() {
    if let Err(message) = run() {
        eprintln!("MarketForge CandleScope plugin failed: {message}");
        std::process::exit(2);
    }
}
