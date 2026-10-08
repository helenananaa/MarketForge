//! Offline/read-only audit: database [room-id], or stdin with JournalRecovery JSON.
use std::{env, io, time::Instant};

use exchange_server::{
    journal::{JournalRecovery, JournalStore, PostgresJournalStore},
    storage_audit::{audit_recovery, audit_runtime_recovery},
};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let started = Instant::now();
    let recovery: JournalRecovery = match args.first().map(String::as_str) {
        Some("stdin") if args.len() == 1 => serde_json::from_reader(io::stdin().lock())?,
        Some("database" | "runtime") if args.len() <= 2 => {
            let dsn = env::var("MARKETFORGE_DATABASE_URL")?;
            // Schema migration belongs to server startup, not this read-only tool.
            let mut store = PostgresJournalStore::connect(&dsn)?;
            match (args[0].as_str(), args.get(1)) {
                ("runtime", Some(room_id)) => store.load_room_recovery(room_id)?,
                ("runtime", None) => store.load_recovery()?,
                (_, Some(room_id)) => store.load_room_replay(room_id)?,
                (_, None) => store.load_full_recovery()?,
            }
        }
        _ => {
            return Err(
                "usage: journal-audit database [room-id] | runtime [room-id] | stdin".into(),
            );
        }
    };
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut report = if args[0] == "runtime" {
        audit_runtime_recovery(&recovery)?
    } else {
        audit_recovery(&recovery)?
    };
    report["load_ms"] = serde_json::json!(load_ms);
    serde_json::to_writer(io::stdout().lock(), &report)?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("journal audit failed: {error}");
        std::process::exit(1);
    }
}
