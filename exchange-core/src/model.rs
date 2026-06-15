use std::fmt;

use serde::{Deserialize, Serialize};

pub type OrderId = u64;
pub type AccountId = u64;
pub type PriceTick = i64;
pub type Qty = u64;

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
    Limit { price_tick: PriceTick },
    Market,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewOrder {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub side: Side,
    pub kind: OrderKind,
    pub qty: Qty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelOrder {
    pub order_id: OrderId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Command {
    NewOrder(NewOrder),
    CancelOrder(CancelOrder),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Order {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub side: Side,
    pub price_tick: PriceTick,
    pub remaining_qty: Qty,
    pub seq: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Trade {
    pub trade_id: u64,
    pub maker_order_id: OrderId,
    pub taker_order_id: OrderId,
    pub price_tick: PriceTick,
    pub qty: Qty,
    pub taker_side: Side,
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RejectReason {
    DuplicateOrderId,
    InvalidQuantity,
    InvalidPrice,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CancelRejectReason {
    UnknownOrder,
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
