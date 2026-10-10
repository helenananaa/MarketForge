use serde::{Deserialize, Serialize};

use crate::model::PriceTick;

/// An explicit index source on the same venue, with matching base/quote assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PerpPriceLinkConfig {
    pub spot_instrument_id: String,
    #[serde(default = "default_max_age_ms")]
    pub max_age_ms: u64,
}

fn default_max_age_ms() -> u64 {
    30_000
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexPriceSource {
    SpotMid,
    SpotTrade,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceLinkStatus {
    AwaitingPrice,
    Live,
    Stale,
    Unavailable,
}

/// Public price information only. No account data is included in a receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpPriceSnapshot {
    pub instrument_id: String,
    pub spot_instrument_id: String,
    pub index_price_tick: Option<PriceTick>,
    pub mark_price_tick: PriceTick,
    pub source: Option<IndexPriceSource>,
    pub source_time_ms: Option<u64>,
    pub max_age_ms: u64,
    pub status: PriceLinkStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding: Option<crate::FundingSnapshot>,
}

impl PerpPriceSnapshot {
    pub(crate) fn at_time(mut self, now_ms: u64) -> Self {
        if self.status == PriceLinkStatus::Live
            && self
                .source_time_ms
                .is_some_and(|time| now_ms.saturating_sub(time) > self.max_age_ms)
        {
            self.status = PriceLinkStatus::Stale;
        }
        self
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SpotTradePrice {
    pub price_tick: PriceTick,
    pub market_time_ms: u64,
}

/// Prices are quote units; tick_size is an increment, not a multiplier.
/// Round down to the perpetual's valid grid without overflowing i64.
pub(crate) fn mark_on_grid(index: PriceTick, tick_size: PriceTick) -> PriceTick {
    (index / tick_size * tick_size).max(tick_size)
}
