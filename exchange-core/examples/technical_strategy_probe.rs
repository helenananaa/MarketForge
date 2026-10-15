//! Formula probe over actual serialized strategy state. No exchange connection.
use exchange_core::Candle;
use exchange_core::technical_trader::signals::{FAMILIES, Params, Signals};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, BufRead};
#[derive(Deserialize)]
struct Input {
    family: String,
    #[serde(default)]
    parameters: Value,
    bars: Vec<Candle>,
}
fn main() {
    for line in io::stdin().lock().lines() {
        let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
            let r: Input = serde_json::from_str(&line?)?;
            if !FAMILIES.contains(&r.family.as_str()) {
                return Err("unknown family".into());
            }
            let mut p = serde_json::to_value(Params::defaults(&r.family))?;
            if let Some(overrides) = r.parameters.as_object() {
                for (k, v) in overrides {
                    if !p.as_object().unwrap().contains_key(k) {
                        return Err("unknown parameter".into());
                    }
                    p.as_object_mut().unwrap().insert(k.clone(), v.clone());
                }
            }
            let p: Params = serde_json::from_value(p)?;
            if !p.valid() {
                return Err("invalid parameter".into());
            }
            let mut signals = Signals::default();
            let frames: Vec<_> = r
                .bars
                .iter()
                .map(|b| signals.update(&r.family, &p, b))
                .collect();
            Ok(json!({"frames":frames,"state":signals}))
        })();
        println!(
            "{}",
            match result {
                Ok(v) => v,
                Err(e) => json!({"error":e.to_string()}),
            }
        );
    }
}
