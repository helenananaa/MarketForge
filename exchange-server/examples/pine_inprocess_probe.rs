//! Fixed requests for Python/Rust differential and restart diagnostics.
use exchange_server::pine_inprocess::Host;
use std::io::{self, BufRead};
fn main() {
    let directory = std::env::args()
        .nth(1)
        .expect("installed Pine package path");
    let host = Host::load(std::path::Path::new(&directory)).unwrap();
    for line in io::stdin().lock().lines() {
        let input: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        if input.get("reset") == Some(&serde_json::json!(true)) {
            host.clear_sessions();
        }
        let result = match host.decide_value(
            &input["request"],
            input["timeout_ms"].as_u64().unwrap_or(5000),
        ) {
            Ok(response) => serde_json::json!({"response":response}),
            Err(error) => serde_json::json!({"error":error.to_string()}),
        };
        println!("{}", serde_json::to_string(&result).unwrap());
    }
}
