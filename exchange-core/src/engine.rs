use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, Money, notional},
    model::{
        AccountId, AmendOrder, AmendRejectReason, BookLevel, BookSnapshot, CancelOrder,
        CancelRejectReason, Command, Event, NewOrder, Order, OrderId, PriceTick, Qty, RejectReason,
        Side, Trade,
    },
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OrderBook {
    // Derived from the live book, never trusted from a checkpoint. Cloned
    // candidates share the immutable index until their first book mutation.
    #[serde(skip)]
    account_orders: OnceLock<Arc<BTreeMap<AccountId, Vec<Order>>>>,
    #[serde(default)]
    order_expirations: BTreeMap<OrderId, u64>,
    bids: BTreeMap<PriceTick, VecDeque<Order>>,
    asks: BTreeMap<PriceTick, VecDeque<Order>>,
    order_index: HashMap<OrderId, OrderLocation>,
    seen_order_ids: HashSet<OrderId>,
    next_seq: u64,
    next_trade_id: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OrderLocation {
    side: Side,
    price_tick: PriceTick,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, command: Command) -> Vec<Event> {
        match command {
            Command::NewOrder(order) => self.place_order(order),
            Command::CancelOrder(cancel) => self.cancel_order(cancel),
            Command::ExpireOrder {
                order_id,
                market_time_ms,
            } => self.expire_order(order_id, market_time_ms),
            Command::AmendOrder(amend) => self.amend_order(amend),
            Command::SetMarkPrice(_) | Command::SettleFunding(_) => Vec::new(),
        }
    }

    pub fn place_order(&mut self, order: NewOrder) -> Vec<Event> {
        self.account_orders.take();
        let mut events = Vec::new();

        if order.qty == 0 {
            events.push(Event::OrderRejected {
                order_id: order.order_id,
                reason: RejectReason::InvalidQuantity,
            });
            return events;
        }

        if self.seen_order_ids.contains(&order.order_id) {
            events.push(Event::OrderRejected {
                order_id: order.order_id,
                reason: RejectReason::DuplicateOrderId,
            });
            return events;
        }

        let limit_price = order.kind.limit_price_tick();
        if limit_price.is_some_and(|price_tick| price_tick <= 0) {
            events.push(Event::OrderRejected {
                order_id: order.order_id,
                reason: RejectReason::InvalidPrice,
            });
            return events;
        }

        if order.kind.is_post_only()
            && limit_price.is_some_and(|price_tick| self.would_cross(order.side, price_tick))
        {
            events.push(Event::OrderRejected {
                order_id: order.order_id,
                reason: RejectReason::PostOnlyWouldTakeLiquidity,
            });
            return events;
        }

        if order.kind.is_fill_or_kill() && !self.can_fully_fill(order.side, limit_price, order.qty)
        {
            events.push(Event::OrderRejected {
                order_id: order.order_id,
                reason: RejectReason::FillOrKillWouldNotFill,
            });
            return events;
        }

        events.push(Event::OrderAccepted {
            order_id: order.order_id,
        });
        self.seen_order_ids.insert(order.order_id);

        let mut incoming = IncomingOrder {
            order_id: order.order_id,
            account_id: order.account_id,
            side: order.side,
            position_side: order.position_side,
            limit_price,
            remaining_qty: order.qty,
        };

        self.match_incoming(&mut incoming, &mut events);

        if incoming.remaining_qty == 0 {
            if !events
                .iter()
                .any(|event| matches!(event, Event::OrderFilled { order_id } if *order_id == incoming.order_id))
            {
                events.push(Event::OrderFilled {
                    order_id: incoming.order_id,
                });
            }
            return events;
        }

        if order.kind.rests_remainder()
            && let Some(price_tick) = incoming.limit_price
        {
            if let Some(deadline) = order.kind.expires_at_market_time_ms() {
                self.order_expirations.insert(order.order_id, deadline);
            }
            let seq = self.take_seq();
            self.rest_order(
                Order {
                    order_id: incoming.order_id,
                    account_id: incoming.account_id,
                    side: incoming.side,
                    position_side: incoming.position_side,
                    price_tick,
                    remaining_qty: incoming.remaining_qty,
                    seq,
                },
                &mut events,
            );
        } else {
            events.push(Event::OrderExpired {
                order_id: incoming.order_id,
                unfilled_qty: incoming.remaining_qty,
            });
        }

        events
    }

    pub fn cancel_order(&mut self, cancel: CancelOrder) -> Vec<Event> {
        self.account_orders.take();
        let Some(location) = self.order_index.remove(&cancel.order_id) else {
            return vec![Event::CancelRejected {
                order_id: cancel.order_id,
                reason: CancelRejectReason::UnknownOrder,
            }];
        };

        let book_side = self.book_side_mut(location.side);
        let Some(queue) = book_side.get_mut(&location.price_tick) else {
            return vec![Event::CancelRejected {
                order_id: cancel.order_id,
                reason: CancelRejectReason::UnknownOrder,
            }];
        };

        let Some(index) = queue
            .iter()
            .position(|order| order.order_id == cancel.order_id)
        else {
            return vec![Event::CancelRejected {
                order_id: cancel.order_id,
                reason: CancelRejectReason::UnknownOrder,
            }];
        };

        let removed = queue
            .remove(index)
            .expect("position returned an in-bounds order index");
        if queue.is_empty() {
            book_side.remove(&location.price_tick);
        }
        self.order_expirations.remove(&cancel.order_id);

        vec![Event::OrderCanceled {
            order_id: removed.order_id,
            remaining_qty: removed.remaining_qty,
        }]
    }

    pub fn expiring_order_ids(&self, market_time_ms: u64) -> Vec<OrderId> {
        self.order_expirations
            .iter()
            .filter_map(|(&id, &deadline)| {
                (deadline <= market_time_ms && self.order_index.contains_key(&id)).then_some(id)
            })
            .collect()
    }

    fn expire_order(&mut self, order_id: OrderId, market_time_ms: u64) -> Vec<Event> {
        if !self
            .order_expirations
            .get(&order_id)
            .is_some_and(|&deadline| deadline <= market_time_ms)
        {
            return vec![Event::CancelRejected {
                order_id,
                reason: CancelRejectReason::UnknownOrder,
            }];
        }
        self.cancel_order(CancelOrder { order_id })
            .into_iter()
            .map(|event| match event {
                Event::OrderCanceled {
                    order_id,
                    remaining_qty,
                } => Event::OrderExpired {
                    order_id,
                    unfilled_qty: remaining_qty,
                },
                other => other,
            })
            .collect()
    }

    pub fn amend_order(&mut self, amend: AmendOrder) -> Vec<Event> {
        self.account_orders.take();
        let Some(location) = self.order_index.get(&amend.order_id).copied() else {
            return vec![Event::AmendRejected {
                order_id: amend.order_id,
                reason: AmendRejectReason::UnknownOrder,
            }];
        };

        let Some(current) = self.resting_order(location, amend.order_id).cloned() else {
            return vec![Event::AmendRejected {
                order_id: amend.order_id,
                reason: AmendRejectReason::UnknownOrder,
            }];
        };

        let new_price_tick = amend.price_tick.unwrap_or(current.price_tick);
        let new_qty = amend.qty.unwrap_or(current.remaining_qty);

        if new_price_tick <= 0 {
            return vec![Event::AmendRejected {
                order_id: amend.order_id,
                reason: AmendRejectReason::InvalidPrice,
            }];
        }
        if new_qty == 0 {
            return vec![Event::AmendRejected {
                order_id: amend.order_id,
                reason: AmendRejectReason::InvalidQuantity,
            }];
        }
        if new_qty > current.remaining_qty {
            return vec![Event::AmendRejected {
                order_id: amend.order_id,
                reason: AmendRejectReason::QuantityIncreaseUnsupported,
            }];
        }
        if price_increases_aggression(current.side, current.price_tick, new_price_tick) {
            return vec![Event::AmendRejected {
                order_id: amend.order_id,
                reason: AmendRejectReason::PriceWouldIncreaseAggression,
            }];
        }
        if new_price_tick == current.price_tick && new_qty == current.remaining_qty {
            return vec![Event::AmendRejected {
                order_id: amend.order_id,
                reason: AmendRejectReason::NoChange,
            }];
        }

        if new_price_tick == current.price_tick {
            self.update_resting_qty(location, amend.order_id, new_qty)
                .expect("resting order was found before in-place amend");
        } else {
            let mut amended = self
                .remove_resting_order(location, amend.order_id)
                .expect("resting order was found before price amend");
            amended.price_tick = new_price_tick;
            amended.remaining_qty = new_qty;
            amended.seq = self.take_seq();
            self.insert_resting_order(amended);
        }

        vec![Event::OrderAmended {
            order_id: amend.order_id,
            old_price_tick: current.price_tick,
            new_price_tick,
            old_qty: current.remaining_qty,
            new_qty,
        }]
    }

    pub fn snapshot(&self) -> BookSnapshot {
        BookSnapshot {
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(price_tick, queue)| BookLevel {
                    price_tick: *price_tick,
                    qty: queue.iter().map(|order| order.remaining_qty).sum(),
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(price_tick, queue)| BookLevel {
                    price_tick: *price_tick,
                    qty: queue.iter().map(|order| order.remaining_qty).sum(),
                })
                .collect(),
        }
    }

    pub fn best_bid(&self) -> Option<PriceTick> {
        self.bids.keys().next_back().copied()
    }

    pub fn best_ask(&self) -> Option<PriceTick> {
        self.asks.keys().next().copied()
    }

    pub fn order_owner(&self, order_id: OrderId) -> Option<u64> {
        let location = self.order_index.get(&order_id).copied()?;
        self.resting_order(location, order_id)
            .map(|order| order.account_id)
    }

    pub fn resting_orders_for_account(&self, account_id: AccountId) -> Vec<Order> {
        self.orders_by_account()
            .get(&account_id)
            .cloned()
            .unwrap_or_default()
    }

    fn orders_by_account(&self) -> &BTreeMap<AccountId, Vec<Order>> {
        self.account_orders.get_or_init(|| {
            let mut accounts: BTreeMap<AccountId, Vec<Order>> = BTreeMap::new();
            for order in self.bids.values().chain(self.asks.values()).flatten() {
                accounts
                    .entry(order.account_id)
                    .or_default()
                    .push(order.clone());
            }
            for orders in accounts.values_mut() {
                orders.sort_unstable_by_key(|order| order.order_id);
            }
            Arc::new(accounts)
        })
    }

    pub(crate) fn cancel_orders_for_account(&mut self, account_id: AccountId) -> Vec<Event> {
        let order_ids = self.order_ids_for_account(account_id);

        order_ids
            .into_iter()
            .flat_map(|order_id| self.cancel_order(CancelOrder { order_id }))
            .collect()
    }

    pub(crate) fn order_ids_for_account(&self, account_id: AccountId) -> Vec<OrderId> {
        self.orders_by_account()
            .get(&account_id)
            .into_iter()
            .flatten()
            .map(|order| order.order_id)
            .collect()
    }

    pub(crate) fn order_ids_for_account_on_side(
        &self,
        account_id: AccountId,
        side: Side,
    ) -> Vec<OrderId> {
        self.orders_by_account()
            .get(&account_id)
            .into_iter()
            .flatten()
            .filter(|order| order.side == side)
            .map(|order| order.order_id)
            .collect()
    }

    pub fn fill_quote(
        &self,
        side: Side,
        limit_price: Option<PriceTick>,
        requested_qty: Qty,
    ) -> Result<FillQuote, ClearingError> {
        let mut quote = FillQuote::default();

        let mut add_level = |price_tick: PriceTick, queue: &VecDeque<Order>| {
            if quote.qty >= requested_qty || !crosses(side, limit_price, price_tick) {
                return Ok(false);
            }
            for order in queue {
                let remaining = requested_qty - quote.qty;
                let fill_qty = remaining.min(order.remaining_qty);
                quote.qty = quote
                    .qty
                    .checked_add(fill_qty)
                    .ok_or(ClearingError::NotionalOverflow)?;
                quote.notional = quote
                    .notional
                    .checked_add(notional(price_tick, fill_qty)?)
                    .ok_or(ClearingError::NotionalOverflow)?;
                if quote.qty == requested_qty {
                    return Ok(true);
                }
            }
            Ok(false)
        };

        match side {
            Side::Buy => {
                for (price_tick, queue) in &self.asks {
                    if add_level(*price_tick, queue)? {
                        break;
                    }
                }
            }
            Side::Sell => {
                for (price_tick, queue) in self.bids.iter().rev() {
                    if add_level(*price_tick, queue)? {
                        break;
                    }
                }
            }
        }

        Ok(quote)
    }

    fn match_incoming(&mut self, incoming: &mut IncomingOrder, events: &mut Vec<Event>) {
        while incoming.remaining_qty > 0 {
            let Some(best_price) = self.best_opposite_price(incoming.side) else {
                break;
            };

            if !incoming.crosses(best_price) {
                break;
            }

            let Some(mut resting) = self.pop_front_at(incoming.side.opposite(), best_price) else {
                break;
            };

            let fill_qty = incoming.remaining_qty.min(resting.remaining_qty);
            incoming.remaining_qty -= fill_qty;
            resting.remaining_qty -= fill_qty;

            events.push(Event::TradePrinted(Trade {
                trade_id: self.take_trade_id(),
                maker_order_id: resting.order_id,
                maker_account_id: resting.account_id,
                taker_order_id: incoming.order_id,
                taker_account_id: incoming.account_id,
                price_tick: resting.price_tick,
                qty: fill_qty,
                taker_side: incoming.side,
                maker_position_side: resting.position_side,
                taker_position_side: incoming.position_side,
            }));

            if resting.remaining_qty == 0 {
                self.order_index.remove(&resting.order_id);
                self.order_expirations.remove(&resting.order_id);
                events.push(Event::OrderFilled {
                    order_id: resting.order_id,
                });
            } else {
                let remaining_qty = resting.remaining_qty;
                let resting_order_id = resting.order_id;
                self.push_front_at(resting);
                events.push(Event::OrderPartiallyFilled {
                    order_id: resting_order_id,
                    remaining_qty,
                });
            }

            if incoming.remaining_qty == 0 {
                events.push(Event::OrderFilled {
                    order_id: incoming.order_id,
                });
            } else {
                events.push(Event::OrderPartiallyFilled {
                    order_id: incoming.order_id,
                    remaining_qty: incoming.remaining_qty,
                });
            }
        }
    }

    fn rest_order(&mut self, order: Order, events: &mut Vec<Event>) {
        let order_id = order.order_id;
        let price_tick = order.price_tick;
        let remaining_qty = order.remaining_qty;

        self.insert_resting_order(order);

        events.push(Event::OrderRested {
            order_id,
            price_tick,
            remaining_qty,
        });
    }

    fn best_opposite_price(&self, side: Side) -> Option<PriceTick> {
        match side {
            Side::Buy => self.best_ask(),
            Side::Sell => self.best_bid(),
        }
    }

    fn would_cross(&self, side: Side, limit_price: PriceTick) -> bool {
        self.best_opposite_price(side)
            .is_some_and(|best_price| crosses(side, Some(limit_price), best_price))
    }

    fn can_fully_fill(&self, side: Side, limit_price: Option<PriceTick>, qty: Qty) -> bool {
        let mut fillable_qty = 0u64;
        match side {
            Side::Buy => {
                for (price_tick, queue) in &self.asks {
                    if !crosses(side, limit_price, *price_tick) {
                        break;
                    }
                    fillable_qty = fillable_qty
                        .saturating_add(queue.iter().map(|order| order.remaining_qty).sum::<Qty>());
                    if fillable_qty >= qty {
                        return true;
                    }
                }
            }
            Side::Sell => {
                for (price_tick, queue) in self.bids.iter().rev() {
                    if !crosses(side, limit_price, *price_tick) {
                        break;
                    }
                    fillable_qty = fillable_qty
                        .saturating_add(queue.iter().map(|order| order.remaining_qty).sum::<Qty>());
                    if fillable_qty >= qty {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn resting_order(&self, location: OrderLocation, order_id: OrderId) -> Option<&Order> {
        self.book_side(location.side)
            .get(&location.price_tick)?
            .iter()
            .find(|order| order.order_id == order_id)
    }

    fn update_resting_qty(
        &mut self,
        location: OrderLocation,
        order_id: OrderId,
        new_qty: Qty,
    ) -> Option<()> {
        let order = self
            .book_side_mut(location.side)
            .get_mut(&location.price_tick)?
            .iter_mut()
            .find(|order| order.order_id == order_id)?;
        order.remaining_qty = new_qty;
        Some(())
    }

    fn remove_resting_order(
        &mut self,
        location: OrderLocation,
        order_id: OrderId,
    ) -> Option<Order> {
        let book_side = self.book_side_mut(location.side);
        let queue = book_side.get_mut(&location.price_tick)?;
        let index = queue.iter().position(|order| order.order_id == order_id)?;
        let removed = queue.remove(index);
        if queue.is_empty() {
            book_side.remove(&location.price_tick);
        }
        self.order_index.remove(&order_id);
        removed
    }

    fn insert_resting_order(&mut self, order: Order) {
        let location = OrderLocation {
            side: order.side,
            price_tick: order.price_tick,
        };
        let order_id = order.order_id;
        self.book_side_mut(order.side)
            .entry(order.price_tick)
            .or_default()
            .push_back(order);
        self.order_index.insert(order_id, location);
    }

    fn pop_front_at(&mut self, side: Side, price_tick: PriceTick) -> Option<Order> {
        let book_side = self.book_side_mut(side);
        let order = book_side.get_mut(&price_tick)?.pop_front();
        if book_side
            .get(&price_tick)
            .is_some_and(|queue| queue.is_empty())
        {
            book_side.remove(&price_tick);
        }
        order
    }

    fn push_front_at(&mut self, order: Order) {
        self.book_side_mut(order.side)
            .entry(order.price_tick)
            .or_default()
            .push_front(order);
    }

    fn book_side_mut(&mut self, side: Side) -> &mut BTreeMap<PriceTick, VecDeque<Order>> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    fn book_side(&self, side: Side) -> &BTreeMap<PriceTick, VecDeque<Order>> {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    fn take_trade_id(&mut self) -> u64 {
        let trade_id = self.next_trade_id;
        self.next_trade_id += 1;
        trade_id
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FillQuote {
    pub qty: Qty,
    pub notional: Money,
}

#[derive(Clone, Debug)]
struct IncomingOrder {
    order_id: OrderId,
    account_id: u64,
    side: Side,
    position_side: crate::model::PositionSide,
    limit_price: Option<PriceTick>,
    remaining_qty: Qty,
}

impl IncomingOrder {
    fn crosses(&self, resting_price: PriceTick) -> bool {
        crosses(self.side, self.limit_price, resting_price)
    }
}

fn crosses(side: Side, limit_price: Option<PriceTick>, resting_price: PriceTick) -> bool {
    match (side, limit_price) {
        (_, None) => true,
        (Side::Buy, Some(limit_price)) => limit_price >= resting_price,
        (Side::Sell, Some(limit_price)) => limit_price <= resting_price,
    }
}

fn price_increases_aggression(
    side: Side,
    old_price_tick: PriceTick,
    new_price_tick: PriceTick,
) -> bool {
    match side {
        Side::Buy => new_price_tick > old_price_tick,
        Side::Sell => new_price_tick < old_price_tick,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn account_lookup_survives_fills_amends_cancel_and_restore() {
        let mut book = OrderBook::new();
        let mut first = limit(1, Side::Sell, 100, 8);
        first.account_id = 7;
        let mut second = limit(2, Side::Sell, 101, 6);
        second.account_id = 7;
        book.place_order(first);
        book.place_order(second);
        assert_eq!(book.order_ids_for_account(7), vec![1, 2]);
        let frozen = book.clone();
        book.place_order(market(3, Side::Buy, 3));
        assert_eq!(book.resting_orders_for_account(7)[0].remaining_qty, 5);
        assert_eq!(frozen.resting_orders_for_account(7)[0].remaining_qty, 8);
        book.amend_order(AmendOrder {
            order_id: 2,
            price_tick: Some(102),
            qty: Some(4),
        });
        assert_eq!(book.resting_orders_for_account(7)[1].price_tick, 102);
        book.place_order(market(4, Side::Buy, 5));
        assert_eq!(book.order_ids_for_account(7), vec![2]);
        let json = serde_json::to_value(&book).unwrap();
        assert!(json.get("account_orders").is_none());
        let mut restored: OrderBook = serde_json::from_value(json).unwrap();
        assert_eq!(
            restored.resting_orders_for_account(7),
            book.resting_orders_for_account(7)
        );
        restored.cancel_order(CancelOrder { order_id: 2 });
        assert!(restored.resting_orders_for_account(7).is_empty());
        assert_eq!(book.order_ids_for_account(7), vec![2]);
    }

    use super::*;
    use crate::model::{AmendOrder, CancelRejectReason, OrderKind, RejectReason};

    fn limit(order_id: OrderId, side: Side, price_tick: PriceTick, qty: Qty) -> NewOrder {
        NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
            reduce_only: false,
        }
    }

    fn market(order_id: OrderId, side: Side, qty: Qty) -> NewOrder {
        NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Market,
            qty,
            reduce_only: false,
        }
    }

    fn post_only(order_id: OrderId, side: Side, price_tick: PriceTick, qty: Qty) -> NewOrder {
        NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::PostOnly { price_tick },
            qty,
            reduce_only: false,
        }
    }

    fn ioc(order_id: OrderId, side: Side, price_tick: Option<PriceTick>, qty: Qty) -> NewOrder {
        NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::ImmediateOrCancel { price_tick },
            qty,
            reduce_only: false,
        }
    }

    fn fok(order_id: OrderId, side: Side, price_tick: Option<PriceTick>, qty: Qty) -> NewOrder {
        NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::FillOrKill { price_tick },
            qty,
            reduce_only: false,
        }
    }

    fn trade_events(events: &[Event]) -> Vec<Trade> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::TradePrinted(trade) => Some(trade.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rests_non_crossing_limit_orders_by_price_side() {
        let mut book = OrderBook::new();

        book.place_order(limit(1, Side::Buy, 100, 10));
        book.place_order(limit(2, Side::Buy, 101, 5));
        book.place_order(limit(3, Side::Sell, 103, 7));
        book.place_order(limit(4, Side::Sell, 102, 9));

        assert_eq!(book.best_bid(), Some(101));
        assert_eq!(book.best_ask(), Some(102));
        assert_eq!(
            book.snapshot(),
            BookSnapshot {
                bids: vec![
                    BookLevel {
                        price_tick: 101,
                        qty: 5,
                    },
                    BookLevel {
                        price_tick: 100,
                        qty: 10,
                    },
                ],
                asks: vec![
                    BookLevel {
                        price_tick: 102,
                        qty: 9,
                    },
                    BookLevel {
                        price_tick: 103,
                        qty: 7,
                    },
                ],
            }
        );
    }

    #[test]
    fn crossing_limit_order_trades_at_resting_price() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 10));

        let events = book.place_order(limit(2, Side::Buy, 105, 4));
        let trades = trade_events(&events);

        assert_eq!(
            trades,
            vec![Trade {
                maker_position_side: Default::default(),
                taker_position_side: Default::default(),
                trade_id: 0,
                maker_order_id: 1,
                maker_account_id: 1001,
                taker_order_id: 2,
                taker_account_id: 1002,
                price_tick: 100,
                qty: 4,
                taker_side: Side::Buy,
            }]
        );
        assert!(events.contains(&Event::OrderPartiallyFilled {
            order_id: 1,
            remaining_qty: 6,
        }));
        assert!(events.contains(&Event::OrderFilled { order_id: 2 }));
        assert_eq!(
            book.snapshot().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 6,
            }]
        );
    }

    #[test]
    fn same_price_orders_match_fifo() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 5));
        book.place_order(limit(2, Side::Sell, 100, 5));

        let events = book.place_order(limit(3, Side::Buy, 100, 7));
        let trades = trade_events(&events);

        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].maker_order_id, 1);
        assert_eq!(trades[0].qty, 5);
        assert_eq!(trades[1].maker_order_id, 2);
        assert_eq!(trades[1].qty, 2);
        assert_eq!(
            book.snapshot().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 3,
            }]
        );
    }

    #[test]
    fn partially_filled_limit_order_rests_remaining_quantity() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 4));

        let events = book.place_order(limit(2, Side::Buy, 101, 10));

        assert!(events.contains(&Event::OrderRested {
            order_id: 2,
            price_tick: 101,
            remaining_qty: 6,
        }));
        assert_eq!(
            book.snapshot().bids,
            vec![BookLevel {
                price_tick: 101,
                qty: 6,
            }]
        );
        assert!(book.snapshot().asks.is_empty());
    }

    #[test]
    fn market_order_consumes_multiple_price_levels_and_expires_remainder() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 3));
        book.place_order(limit(2, Side::Sell, 101, 4));

        let events = book.place_order(market(3, Side::Buy, 10));
        let trades = trade_events(&events);

        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].price_tick, 100);
        assert_eq!(trades[0].qty, 3);
        assert_eq!(trades[1].price_tick, 101);
        assert_eq!(trades[1].qty, 4);
        assert!(events.contains(&Event::OrderExpired {
            order_id: 3,
            unfilled_qty: 3,
        }));
        assert!(book.snapshot().asks.is_empty());
    }

    #[test]
    fn post_only_rests_without_taking_liquidity() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 3));

        let events = book.place_order(post_only(2, Side::Buy, 99, 2));

        assert_eq!(
            events,
            vec![
                Event::OrderAccepted { order_id: 2 },
                Event::OrderRested {
                    order_id: 2,
                    price_tick: 99,
                    remaining_qty: 2,
                },
            ]
        );
        assert_eq!(
            book.snapshot().bids,
            vec![BookLevel {
                price_tick: 99,
                qty: 2,
            }]
        );
    }

    #[test]
    fn post_only_rejects_when_it_would_take_liquidity() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 3));

        assert_eq!(
            book.place_order(post_only(2, Side::Buy, 100, 2)),
            vec![Event::OrderRejected {
                order_id: 2,
                reason: RejectReason::PostOnlyWouldTakeLiquidity,
            }]
        );
        assert_eq!(
            book.snapshot().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 3,
            }]
        );
    }

    #[test]
    fn immediate_or_cancel_fills_available_quantity_and_expires_remainder() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 3));
        book.place_order(limit(2, Side::Sell, 102, 4));

        let events = book.place_order(ioc(3, Side::Buy, Some(101), 5));
        let trades = trade_events(&events);

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price_tick, 100);
        assert_eq!(trades[0].qty, 3);
        assert!(events.contains(&Event::OrderExpired {
            order_id: 3,
            unfilled_qty: 2,
        }));
        assert_eq!(
            book.snapshot().asks,
            vec![BookLevel {
                price_tick: 102,
                qty: 4,
            }]
        );
        assert!(book.snapshot().bids.is_empty());
    }

    #[test]
    fn fill_or_kill_rejects_without_mutating_when_full_quantity_is_unavailable() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 3));
        book.place_order(limit(2, Side::Sell, 102, 4));

        assert_eq!(
            book.place_order(fok(3, Side::Buy, Some(101), 5)),
            vec![Event::OrderRejected {
                order_id: 3,
                reason: RejectReason::FillOrKillWouldNotFill,
            }]
        );
        assert_eq!(
            book.snapshot().asks,
            vec![
                BookLevel {
                    price_tick: 100,
                    qty: 3,
                },
                BookLevel {
                    price_tick: 102,
                    qty: 4,
                },
            ]
        );
    }

    #[test]
    fn fill_or_kill_executes_when_full_quantity_is_available() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 3));
        book.place_order(limit(2, Side::Sell, 101, 4));

        let events = book.place_order(fok(3, Side::Buy, Some(101), 5));
        let trades = trade_events(&events);

        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].qty, 3);
        assert_eq!(trades[1].qty, 2);
        assert!(events.contains(&Event::OrderFilled { order_id: 3 }));
        assert_eq!(
            book.snapshot().asks,
            vec![BookLevel {
                price_tick: 101,
                qty: 2,
            }]
        );
    }

    #[test]
    fn amend_reduces_quantity_without_losing_priority() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Buy, 100, 5));
        book.place_order(limit(2, Side::Buy, 100, 5));

        assert_eq!(
            book.amend_order(AmendOrder {
                order_id: 1,
                price_tick: None,
                qty: Some(3),
            }),
            vec![Event::OrderAmended {
                order_id: 1,
                old_price_tick: 100,
                new_price_tick: 100,
                old_qty: 5,
                new_qty: 3,
            }]
        );

        let events = book.place_order(market(3, Side::Sell, 4));
        let trades = trade_events(&events);
        assert_eq!(trades[0].maker_order_id, 1);
        assert_eq!(trades[0].qty, 3);
        assert_eq!(trades[1].maker_order_id, 2);
        assert_eq!(trades[1].qty, 1);
    }

    #[test]
    fn amend_to_less_aggressive_price_loses_priority() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Buy, 100, 5));
        book.place_order(limit(2, Side::Buy, 99, 5));

        let events = book.amend_order(AmendOrder {
            order_id: 1,
            price_tick: Some(99),
            qty: Some(4),
        });

        assert_eq!(
            events,
            vec![Event::OrderAmended {
                order_id: 1,
                old_price_tick: 100,
                new_price_tick: 99,
                old_qty: 5,
                new_qty: 4,
            }]
        );
        let trades = trade_events(&book.place_order(market(3, Side::Sell, 6)));
        assert_eq!(trades[0].maker_order_id, 2);
        assert_eq!(trades[1].maker_order_id, 1);
    }

    #[test]
    fn amend_rejects_unknown_order_quantity_increase_and_aggressive_price() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Buy, 100, 5));

        assert_eq!(
            book.amend_order(AmendOrder {
                order_id: 99,
                price_tick: Some(100),
                qty: Some(1),
            }),
            vec![Event::AmendRejected {
                order_id: 99,
                reason: AmendRejectReason::UnknownOrder,
            }]
        );
        assert_eq!(
            book.amend_order(AmendOrder {
                order_id: 1,
                price_tick: None,
                qty: Some(6),
            }),
            vec![Event::AmendRejected {
                order_id: 1,
                reason: AmendRejectReason::QuantityIncreaseUnsupported,
            }]
        );
        assert_eq!(
            book.amend_order(AmendOrder {
                order_id: 1,
                price_tick: Some(101),
                qty: None,
            }),
            vec![Event::AmendRejected {
                order_id: 1,
                reason: AmendRejectReason::PriceWouldIncreaseAggression,
            }]
        );
    }

    #[test]
    fn cancel_removes_resting_order() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Buy, 100, 10));

        let events = book.cancel_order(CancelOrder { order_id: 1 });

        assert_eq!(
            events,
            vec![Event::OrderCanceled {
                order_id: 1,
                remaining_qty: 10,
            }]
        );
        assert!(book.snapshot().bids.is_empty());
    }

    #[test]
    fn cancel_unknown_order_is_rejected() {
        let mut book = OrderBook::new();

        assert_eq!(
            book.cancel_order(CancelOrder { order_id: 99 }),
            vec![Event::CancelRejected {
                order_id: 99,
                reason: CancelRejectReason::UnknownOrder,
            }]
        );
    }

    #[test]
    fn duplicate_order_ids_are_rejected_even_after_fill() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 1));
        book.place_order(limit(2, Side::Buy, 100, 1));

        assert_eq!(
            book.place_order(limit(1, Side::Sell, 100, 1)),
            vec![Event::OrderRejected {
                order_id: 1,
                reason: RejectReason::DuplicateOrderId,
            }]
        );
    }

    #[test]
    fn invalid_order_inputs_are_rejected() {
        let mut book = OrderBook::new();

        assert_eq!(
            book.place_order(limit(1, Side::Buy, 100, 0)),
            vec![Event::OrderRejected {
                order_id: 1,
                reason: RejectReason::InvalidQuantity,
            }]
        );
        assert_eq!(
            book.place_order(limit(2, Side::Buy, 0, 1)),
            vec![Event::OrderRejected {
                order_id: 2,
                reason: RejectReason::InvalidPrice,
            }]
        );
    }
}
