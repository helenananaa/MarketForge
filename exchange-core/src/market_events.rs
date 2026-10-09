//! Immutable public information on the simulation clock, never price writes.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEvent {
    pub id: String,
    pub instrument_id: String,
    pub published_at_ms: u64,
    pub expires_at_ms: u64,
    pub impact_ticks: i64,
    pub headline: String,
}

pub fn validate_events(events: &[MarketEvent], instruments: &[String]) -> Result<(), String> {
    if events.len() > 256 {
        return Err("at most 256 market events".into());
    }
    let mut ids = BTreeSet::new();
    let mut previous = 0;
    for e in events {
        if e.id.is_empty()
            || e.id.len() > 128
            || !ids.insert(&e.id)
            || !instruments.contains(&e.instrument_id)
            || e.published_at_ms < previous
            || e.expires_at_ms <= e.published_at_ms
            || e.impact_ticks.unsigned_abs() > 1_000_000
            || e.headline.len() > 1024
        {
            return Err("invalid, duplicate or unordered market event".into());
        }
        previous = e.published_at_ms;
    }
    Ok(())
}
