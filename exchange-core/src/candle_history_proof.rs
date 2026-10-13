//! A window carries the hash of every authoritative closed candle before it.
//! Consumers continue this chain through their saved cursor and compare their
//! saved count/digest before accepting any newly visible candles.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Candle;

pub const CANDLE_HISTORY_PROOF_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandleHistoryProof {
    pub version: u16,
    /// Number of complete, nonempty closed candles before the supplied window.
    pub prefix_bars: u64,
    /// Lowercase SHA-256 of the interval seed and the complete omitted prefix.
    pub prefix_sha256: String,
}

/// Domain-separated initial value. Empty histories of different intervals
/// cannot be substituted for one another.
pub fn candle_history_seed(interval_ms: u64) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"marketforge.closed-candle-history.v1\0");
    hash.update(interval_ms.to_le_bytes());
    hash.finalize().into()
}

/// All integer fields retain their complete width, including i128 quantities.
/// Option tags distinguish missing data from an authoritative zero. No JSON
/// encoder, floating-point conversion, locale or platform width participates.
pub fn candle_history_append(previous: &[u8; 32], candle: &Candle) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"marketforge.closed-candle-link.v1\0");
    hash.update(previous);
    hash.update(candle.schema_version.to_le_bytes());
    hash.update(candle.open_time_ms.to_le_bytes());
    hash.update(candle.close_time_ms.to_le_bytes());
    hash.update(candle.open_tick.to_le_bytes());
    hash.update(candle.high_tick.to_le_bytes());
    hash.update(candle.low_tick.to_le_bytes());
    hash.update(candle.close_tick.to_le_bytes());
    hash.update(candle.volume.to_le_bytes());
    hash.update(candle.quote_volume.to_le_bytes());
    hash.update(candle.trades.to_le_bytes());
    hash.update([u8::from(candle.taker_buy_base.is_some())]);
    if let Some(value) = candle.taker_buy_base {
        hash.update(value.to_le_bytes());
    }
    hash.update([u8::from(candle.taker_buy_quote.is_some())]);
    if let Some(value) = candle.taker_buy_quote {
        hash.update(value.to_le_bytes());
    }
    hash.update([u8::from(candle.is_final)]);
    hash.finalize().into()
}

pub fn candle_history_hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut value = String::with_capacity(64);
    for byte in digest {
        write!(&mut value, "{byte:02x}").expect("String writes cannot fail");
    }
    value
}

pub fn candle_history_parse(value: &str) -> Option<[u8; 32]> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0; 32];
    for (target, pair) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *target = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Some(digest)
}

impl CandleHistoryProof {
    pub fn prefix_digest(&self) -> Option<[u8; 32]> {
        (self.version == CANDLE_HISTORY_PROOF_VERSION)
            .then(|| candle_history_parse(&self.prefix_sha256))
            .flatten()
    }
}

/// The shared observation wave prepares this once per interval. Index `n`
/// authenticates exactly the first `n` candles; a bot takes its window proof in
/// constant time even when other bots request different window lengths.
pub(crate) fn candle_history_prefixes(interval_ms: u64, candles: &[Candle]) -> Vec<[u8; 32]> {
    let mut prefixes = Vec::with_capacity(candles.len() + 1);
    let mut digest = candle_history_seed(interval_ms);
    prefixes.push(digest);
    for candle in candles {
        digest = candle_history_append(&digest, candle);
        prefixes.push(digest);
    }
    prefixes
}

pub(crate) fn window_proof(prefixes: &[[u8; 32]], start: usize) -> CandleHistoryProof {
    CandleHistoryProof {
        version: CANDLE_HISTORY_PROOF_VERSION,
        prefix_bars: u64::try_from(start).expect("a candle vector fits in u64"),
        prefix_sha256: candle_history_hex(&prefixes[start]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(index: u64) -> Candle {
        Candle {
            schema_version: 1,
            open_time_ms: index * 1000,
            close_time_ms: (index + 1) * 1000,
            open_tick: 100,
            high_tick: 103,
            low_tick: 99,
            close_tick: 102,
            volume: 7,
            quote_volume: 714,
            trades: 2,
            taker_buy_base: Some(3),
            taker_buy_quote: Some(306),
            is_final: true,
        }
    }

    #[test]
    fn rolling_window_authenticates_a_change_far_before_retained_overlap() {
        let mut candles: Vec<_> = (0..3000).map(candle).collect();
        let prefixes = candle_history_prefixes(1000, &candles);
        let saved = prefixes[2980];
        let proof = window_proof(&prefixes, 2950);
        let continued = candles[2950..2980]
            .iter()
            .fold(proof.prefix_digest().unwrap(), |digest, bar| {
                candle_history_append(&digest, bar)
            });
        assert_eq!(continued, saved);
        // The retained 50 bars are unchanged; checking only overlap would pass.
        candles[0].quote_volume += 1;
        let corrected = candle_history_prefixes(1000, &candles);
        let altered = window_proof(&corrected, 2950);
        let continued = candles[2950..2980]
            .iter()
            .fold(altered.prefix_digest().unwrap(), |digest, bar| {
                candle_history_append(&digest, bar)
            });
        assert_ne!(continued, saved);
    }

    #[test]
    fn domains_and_exact_optional_integer_fields_are_not_interchangeable() {
        let seed = candle_history_seed(1000);
        assert_ne!(seed, candle_history_seed(2000));
        let base = candle(0);
        let digest = candle_history_append(&seed, &base);
        let mut modified = base.clone();
        modified.quote_volume += 1_i128 << 100;
        assert_ne!(digest, candle_history_append(&seed, &modified));
        modified = base.clone();
        modified.taker_buy_base = None;
        let missing = candle_history_append(&seed, &modified);
        modified.taker_buy_base = Some(0);
        assert_ne!(missing, candle_history_append(&seed, &modified));
        assert_eq!(
            candle_history_parse(&candle_history_hex(&digest)),
            Some(digest)
        );
        assert!(candle_history_parse(&"Z".repeat(64)).is_none());
        assert!(candle_history_parse(&"0".repeat(63)).is_none());
    }

    #[test]
    fn legacy_observations_default_to_no_history_attestation() {
        let data: crate::BotMarketData = serde_json::from_value(serde_json::json!({
            "interval_ms": 1000, "candles": [], "own_fills": [], "fill_details": [], "truncated": false
        })).unwrap();
        assert!(data.history_proof.is_none());
        assert!(!data.private_history_complete);
        let stream: crate::observation::BotDataStream = serde_json::from_value(serde_json::json!({
            "instrument_id": "spot", "interval_ms": 1000, "candles": [], "truncated": false
        }))
        .unwrap();
        assert!(stream.history_proof.is_none());
    }
}
