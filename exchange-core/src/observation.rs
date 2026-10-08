use serde::{Deserialize, Serialize};

use crate::{
    actor::{AccountSnapshot, MarketStatus},
    clock::SimulationClock,
    market::{InstrumentId, VenueId},
    model::{BookSnapshot, Order, Trade},
};

pub const PARTICIPANT_OBSERVATION_VERSION: u16 = 1;
pub const MAX_PUBLIC_TRADES_IN_OBSERVATION: usize = 32;
pub const STRATEGY_PROTOCOL_VERSION: &str = "strategy.v1";
pub const EXTERNAL_ACTIONS_PER_STEP: u32 = 8;

/// Public trade plus authoritative accounting metadata for history recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BotTradeReceipt {
    pub instrument_id: String,
    pub market_time_ms: u64,
    pub trade: Trade,
    pub buyer_fee: Option<i128>,
    pub seller_fee: Option<i128>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BotFillDetail {
    pub trade_id: u64,
    pub market_time_ms: u64,
    pub fee_paid: Option<i128>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BotMarketData {
    pub interval_ms: u64,
    /// Closed, nonempty bars only; no synthetic volume or prices.
    pub candles: Vec<crate::Candle>,
    /// Complete public fills involving this account, in execution order.
    pub own_fills: Vec<Trade>,
    pub fill_details: Vec<BotFillDetail>,
    /// A consumer requiring complete history must refuse truncated input.
    pub truncated: bool,
}

/// Restricted view a participant may use to decide. Version 1 exposes public
/// book, recent public trades, simulation time, and the caller's own orders
/// and account. It never includes other accounts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParticipantObservation {
    pub version: u16,
    pub room_id: String,
    pub venue_id: VenueId,
    pub instrument_id: InstrumentId,
    pub status: MarketStatus,
    pub step: u64,
    pub market_time_ms: u64,
    pub book: BookSnapshot,
    pub public_trades: Vec<Trade>,
    pub own_orders: Vec<Order>,
    pub own_account: Option<AccountSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_market_data: Option<BotMarketData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perp_price: Option<crate::PerpPriceSnapshot>,
}

impl ParticipantObservation {
    pub fn clock(&self) -> SimulationClock {
        let mut clock = SimulationClock::new(1_000);
        clock.set_position(self.step, self.market_time_ms);
        clock
    }
}
