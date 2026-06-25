use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::model::{
    BookLevel, BookSnapshot, CancelOrder, CancelRejectReason, Command, Event, NewOrder, Order,
    OrderId, OrderKind, PriceTick, Qty, RejectReason, Side, Trade,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OrderBook {
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
        }
    }

    pub fn place_order(&mut self, order: NewOrder) -> Vec<Event> {
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

        let limit_price = match order.kind {
            OrderKind::Limit { price_tick } => {
                if price_tick <= 0 {
                    events.push(Event::OrderRejected {
                        order_id: order.order_id,
                        reason: RejectReason::InvalidPrice,
                    });
                    return events;
                }
                Some(price_tick)
            }
            OrderKind::Market => None,
        };

        events.push(Event::OrderAccepted {
            order_id: order.order_id,
        });
        self.seen_order_ids.insert(order.order_id);

        let mut incoming = IncomingOrder {
            order_id: order.order_id,
            account_id: order.account_id,
            side: order.side,
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

        if let Some(price_tick) = incoming.limit_price {
            let seq = self.take_seq();
            self.rest_order(
                Order {
                    order_id: incoming.order_id,
                    account_id: incoming.account_id,
                    side: incoming.side,
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

        vec![Event::OrderCanceled {
            order_id: removed.order_id,
            remaining_qty: removed.remaining_qty,
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
            }));

            if resting.remaining_qty == 0 {
                self.order_index.remove(&resting.order_id);
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
        let location = OrderLocation {
            side: order.side,
            price_tick: order.price_tick,
        };
        let order_id = order.order_id;
        let price_tick = order.price_tick;
        let remaining_qty = order.remaining_qty;

        self.book_side_mut(order.side)
            .entry(order.price_tick)
            .or_default()
            .push_back(order);
        self.order_index.insert(order_id, location);

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

#[derive(Clone, Debug)]
struct IncomingOrder {
    order_id: OrderId,
    account_id: u64,
    side: Side,
    limit_price: Option<PriceTick>,
    remaining_qty: Qty,
}

impl IncomingOrder {
    fn crosses(&self, resting_price: PriceTick) -> bool {
        match (self.side, self.limit_price) {
            (_, None) => true,
            (Side::Buy, Some(limit_price)) => limit_price >= resting_price,
            (Side::Sell, Some(limit_price)) => limit_price <= resting_price,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CancelRejectReason, RejectReason};

    fn limit(order_id: OrderId, side: Side, price_tick: PriceTick, qty: Qty) -> NewOrder {
        NewOrder {
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
        }
    }

    fn market(order_id: OrderId, side: Side, qty: Qty) -> NewOrder {
        NewOrder {
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Market,
            qty,
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
