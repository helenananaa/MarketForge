use std::fmt;

use serde::{Deserialize, Serialize};

pub type OrderId = u64;
pub type AccountId = u64;
pub type PriceTick = i64;
pub type Qty = u64;

/// Both is the legacy net position. Hedge orders must select a leg explicitly.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum PositionSide {
    #[default]
    Both,
    Long,
    Short,
}

impl PositionSide {
    pub fn is_both(&self) -> bool {
        *self == Self::Both
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OrderKind {
    Limit {
        price_tick: PriceTick,
    },
    Market,
    /// Unbounded market execution with an independent decision deadline.
    TimedMarket {
        valid_until_market_time_ms: u64,
    },
    PostOnly {
        price_tick: PriceTick,
    },
    ImmediateOrCancel {
        price_tick: Option<PriceTick>,
    },
    FillOrKill {
        price_tick: Option<PriceTick>,
    },
    /// Price-bounded intent with optional deadlines in simulation market time.
    Protected {
        order_type: ProtectedOrderType,
        price_tick: PriceTick,
        valid_until_market_time_ms: Option<u64>,
        expires_at_market_time_ms: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProtectedOrderType {
    Limit,
    ImmediateOrCancel,
    PostOnly,
}

impl OrderKind {
    pub fn execution_kind(self) -> Self {
        match self {
            Self::TimedMarket { .. } => Self::Market,
            Self::Protected {
                order_type,
                price_tick,
                ..
            } => match order_type {
                ProtectedOrderType::Limit => Self::Limit { price_tick },
                ProtectedOrderType::PostOnly => Self::PostOnly { price_tick },
                ProtectedOrderType::ImmediateOrCancel => Self::ImmediateOrCancel {
                    price_tick: Some(price_tick),
                },
            },
            kind => kind,
        }
    }

    pub fn expires_at_market_time_ms(self) -> Option<u64> {
        match self {
            Self::Protected {
                expires_at_market_time_ms,
                ..
            } => expires_at_market_time_ms,
            _ => None,
        }
    }

    pub fn valid_until_market_time_ms(self) -> Option<u64> {
        match self {
            Self::Protected {
                valid_until_market_time_ms,
                ..
            } => valid_until_market_time_ms,
            Self::TimedMarket {
                valid_until_market_time_ms,
            } => Some(valid_until_market_time_ms),
            _ => None,
        }
    }

    pub fn limit_price_tick(self) -> Option<PriceTick> {
        match self {
            Self::Limit { price_tick }
            | Self::PostOnly { price_tick }
            | Self::ImmediateOrCancel {
                price_tick: Some(price_tick),
            }
            | Self::FillOrKill {
                price_tick: Some(price_tick),
            } => Some(price_tick),
            Self::Protected { price_tick, .. } => Some(price_tick),
            Self::Market
            | Self::TimedMarket { .. }
            | Self::ImmediateOrCancel { price_tick: None }
            | Self::FillOrKill { price_tick: None } => None,
        }
    }

    pub fn rests_remainder(self) -> bool {
        matches!(
            self.execution_kind(),
            Self::Limit { .. } | Self::PostOnly { .. }
        )
    }

    pub fn is_post_only(self) -> bool {
        matches!(self.execution_kind(), Self::PostOnly { .. })
    }

    pub fn is_fill_or_kill(self) -> bool {
        matches!(self, Self::FillOrKill { .. })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewOrder {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub side: Side,
    #[serde(default, skip_serializing_if = "PositionSide::is_both")]
    pub position_side: PositionSide,
    pub kind: OrderKind,
    pub qty: Qty,
    #[serde(default)]
    pub reduce_only: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelOrder {
    pub order_id: OrderId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AmendOrder {
    pub order_id: OrderId,
    pub price_tick: Option<PriceTick>,
    pub qty: Option<Qty>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetMarkPrice {
    pub price_tick: PriceTick,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Command {
    SetConditionalOrder {
        account_id: AccountId,
        key: String,
        spec: Option<Box<crate::conditional_orders::ConditionalOrderSpec>>,
    },
    SetPositionProtection {
        account_id: AccountId,
        position_side: PositionSide,
        protection: Option<Box<crate::PositionProtectionSpec>>,
    },
    NewOrderWithProtection {
        order: NewOrder,
        protection: Box<crate::PositionProtectionSpec>,
    },
    NewOrder(NewOrder),
    CancelOrder(CancelOrder),
    /// Generated at the expiry boundary; carries time for deterministic replay.
    ExpireOrder {
        order_id: OrderId,
        market_time_ms: u64,
    },
    AmendOrder(AmendOrder),
    SetMarkPrice(SetMarkPrice),
    /// Only the simulation clock may generate this clearing command.
    SettleFunding(crate::FundingSettlement),
}

impl Command {
    pub fn new_order(&self) -> Option<&NewOrder> {
        match self {
            Self::NewOrder(order) | Self::NewOrderWithProtection { order, .. } => Some(order),
            _ => None,
        }
    }
    pub fn order_id(&self) -> OrderId {
        match self {
            Self::NewOrder(order) | Self::NewOrderWithProtection { order, .. } => order.order_id,
            Self::SetPositionProtection { .. } | Self::SetConditionalOrder { .. } => 0,
            Self::CancelOrder(cancel) => cancel.order_id,
            Self::ExpireOrder { order_id, .. } => *order_id,
            Self::AmendOrder(amend) => amend.order_id,
            Self::SetMarkPrice(_) | Self::SettleFunding(_) => 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Order {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub side: Side,
    #[serde(default, skip_serializing_if = "PositionSide::is_both")]
    pub position_side: PositionSide,
    pub price_tick: PriceTick,
    pub remaining_qty: Qty,
    pub seq: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Trade {
    pub trade_id: u64,
    pub maker_order_id: OrderId,
    pub maker_account_id: AccountId,
    pub taker_order_id: OrderId,
    pub taker_account_id: AccountId,
    pub price_tick: PriceTick,
    pub qty: Qty,
    pub taker_side: Side,
    #[serde(default, skip_serializing_if = "PositionSide::is_both")]
    pub maker_position_side: PositionSide,
    #[serde(default, skip_serializing_if = "PositionSide::is_both")]
    pub taker_position_side: PositionSide,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Event {
    OrderAccepted {
        order_id: OrderId,
    },
    OrderRejected {
        order_id: OrderId,
        reason: RejectReason,
    },
    RiskRejected {
        order_id: OrderId,
        reason: RiskRejectReason,
    },
    TradePrinted(Trade),
    OrderPartiallyFilled {
        order_id: OrderId,
        remaining_qty: Qty,
    },
    OrderFilled {
        order_id: OrderId,
    },
    OrderRested {
        order_id: OrderId,
        price_tick: PriceTick,
        remaining_qty: Qty,
    },
    OrderExpired {
        order_id: OrderId,
        unfilled_qty: Qty,
    },
    OrderCanceled {
        order_id: OrderId,
        remaining_qty: Qty,
    },
    CancelRejected {
        order_id: OrderId,
        reason: CancelRejectReason,
    },
    OrderAmended {
        order_id: OrderId,
        old_price_tick: PriceTick,
        new_price_tick: PriceTick,
        old_qty: Qty,
        new_qty: Qty,
    },
    AmendRejected {
        order_id: OrderId,
        reason: AmendRejectReason,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RejectReason {
    DuplicateOrderId,
    InvalidQuantity,
    InvalidPrice,
    PostOnlyWouldTakeLiquidity,
    FillOrKillWouldNotFill,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RiskRejectReason {
    InvalidPositionSide,
    AccountNotFound,
    InsufficientCash,
    InsufficientPosition,
    MaxOrderQtyExceeded,
    MaxOrderNotionalExceeded,
    MaxPositionExceeded,
    InsufficientMargin,
    UnsupportedMarketOrder,
    InvalidPriceTick,
    InvalidLotSize,
    ReduceOnlyUnsupported,
    ReduceOnlyWouldIncreasePosition,
    ReduceOnlyExceedsPosition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CancelRejectReason {
    UnknownOrder,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AmendRejectReason {
    UnknownOrder,
    NoChange,
    InvalidQuantity,
    InvalidPrice,
    QuantityIncreaseUnsupported,
    PriceWouldIncreaseAggression,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BookLevel {
    pub price_tick: PriceTick,
    pub qty: Qty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BookSnapshot {
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buy => f.write_str("buy"),
            Self::Sell => f.write_str("sell"),
        }
    }
}
