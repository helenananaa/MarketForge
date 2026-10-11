use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    /// Cumulative quantity excluding any trade involving this account.
    #[serde(default)]
    pub external_volume_qty: String,
    /// Closed, nonempty bars only; no synthetic volume or prices.
    pub candles: Vec<crate::Candle>,
    /// Complete public fills involving this account, in execution order.
    pub own_fills: Vec<Trade>,
    pub fill_details: Vec<BotFillDetail>,
    /// A consumer requiring complete history must refuse truncated input.
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<BotDataStream>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_events: Vec<BotAccountEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditional_orders: Vec<crate::conditional_orders::ConditionalOrder>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BotAccountEvent {
    #[serde(default)]
    pub instrument_id: String,
    pub command_seq: u64,
    pub market_time_ms: u64,
    pub account: BotAccountFrame,
}

/// Historical spot receipts do not retain resting-order reservations. Expose
/// only ledger fields that can be restored exactly, never invented budgets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BotAccountFrame {
    Spot {
        account_id: u64,
        cash_balance: i128,
        position_qty: i128,
        fees_paid: i128,
    },
    Perp(Box<crate::PerpAccountSnapshot>),
}

impl BotAccountFrame {
    pub fn account_id(&self) -> u64 {
        match self {
            Self::Spot { account_id, .. } => *account_id,
            Self::Perp(a) => a.account_id,
        }
    }
}

impl From<AccountSnapshot> for BotAccountFrame {
    fn from(account: AccountSnapshot) -> Self {
        match account {
            AccountSnapshot::Spot(a) => Self::Spot {
                account_id: a.account_id,
                cash_balance: a.cash_balance,
                position_qty: a.position_qty,
                fees_paid: a.fees_paid,
            },
            AccountSnapshot::Perp(a) => Self::Perp(Box::new(a)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BotDataStream {
    pub instrument_id: String,
    pub interval_ms: u64,
    pub candles: Vec<crate::Candle>,
    pub truncated: bool,
}

/// Restricted view a participant may use to decide. Version 1 exposes public
/// book, recent public trades, simulation time, and the caller's own orders
/// and account. It never includes other accounts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParticipantObservation {
    #[serde(default)]
    pub position_protections: Vec<crate::PositionProtection>,
    #[serde(default)]
    pub risk: Option<crate::position_protection::PositionRisk>,
    pub version: u16,
    pub room_id: String,
    pub venue_id: VenueId,
    pub instrument_id: InstrumentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<crate::InstrumentConfig>,
    pub status: MarketStatus,
    pub step: u64,
    pub market_time_ms: u64,
    pub book: BookSnapshot,
    pub public_trades: Vec<Trade>,
    pub own_orders: Vec<Order>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_markets: Vec<ParticipantObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub market_events: Vec<crate::market_events::MarketEvent>,
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

/// One frozen public market view shared by native traders, plus strictly private
/// account inputs. This is an in-process delivery type; the wire protocol stays
/// ParticipantObservation. No writable strategy state is shared.
#[derive(Clone, Debug)]
pub struct SharedParticipantObservation {
    pub public: Arc<ParticipantObservation>,
    pub own_account: Option<AccountSnapshot>,
    pub own_orders: Vec<Order>,
    pub position_protections: Vec<crate::PositionProtection>,
    pub risk: Option<crate::position_protection::PositionRisk>,
    pub bot_market_data: Option<BotMarketData>,
    pub related_markets: Vec<SharedParticipantObservation>,
}

impl SharedParticipantObservation {
    pub fn to_owned_observation(&self) -> ParticipantObservation {
        let mut view = (*self.public).clone();
        view.own_account = self.own_account.clone();
        view.own_orders = self.own_orders.clone();
        view.position_protections = self.position_protections.clone();
        view.risk = self.risk.clone();
        view.bot_market_data = self.bot_market_data.clone();
        view.related_markets = self
            .related_markets
            .iter()
            .map(Self::to_owned_observation)
            .collect();
        view
    }

    pub fn view(&self) -> ParticipantObservationView<'_> {
        let mut view = ParticipantObservationView::from(&*self.public);
        view.own_account = &self.own_account;
        view.own_orders = &self.own_orders;
        view.bot_market_data = &self.bot_market_data;
        view.related_markets = self.related_markets.iter().map(Self::view).collect();
        view
    }
}

/// Borrowed decision inputs. Native strategies read the same values in the same
/// order as the owned protocol; public book/trade/event vectors are never copied.
pub struct ParticipantObservationView<'a> {
    pub instrument_id: &'a str,
    pub venue_id: &'a str,
    pub status: MarketStatus,
    pub step: u64,
    pub market_time_ms: u64,
    pub book: &'a BookSnapshot,
    pub public_trades: &'a [Trade],
    pub market_events: &'a [crate::market_events::MarketEvent],
    pub perp_price: &'a Option<crate::PerpPriceSnapshot>,
    pub own_account: &'a Option<AccountSnapshot>,
    pub own_orders: &'a [Order],
    pub bot_market_data: &'a Option<BotMarketData>,
    pub related_markets: Vec<ParticipantObservationView<'a>>,
}

impl<'a> From<&'a ParticipantObservation> for ParticipantObservationView<'a> {
    fn from(view: &'a ParticipantObservation) -> Self {
        Self {
            instrument_id: &view.instrument_id,
            venue_id: &view.venue_id,
            status: view.status,
            step: view.step,
            market_time_ms: view.market_time_ms,
            book: &view.book,
            public_trades: &view.public_trades,
            market_events: &view.market_events,
            perp_price: &view.perp_price,
            own_account: &view.own_account,
            own_orders: &view.own_orders,
            bot_market_data: &view.bot_market_data,
            related_markets: view.related_markets.iter().map(Self::from).collect(),
        }
    }
}
