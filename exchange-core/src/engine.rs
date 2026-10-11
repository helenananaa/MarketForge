use im::{OrdMap, OrdSet};

use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, Money, notional},
    model::{
        AccountId, AmendOrder, AmendRejectReason, BookLevel, BookSnapshot, CancelOrder,
        CancelRejectReason, Command, Event, NewOrder, Order, OrderId, PriceTick, Qty, RejectReason,
        Side, Trade,
    },
};

#[derive(Clone, Debug, Default)]
pub struct OrderBook {
    reduce_orders: OrdSet<OrderId>,
    // Derived, incremental account index; rebuilt from queues on recovery.
    account_orders: OrdMap<AccountId, OrdSet<OrderId>>,
    order_expirations: OrdMap<OrderId, u64>,
    bids: OrdMap<PriceTick, PriceLevel>,
    asks: OrdMap<PriceTick, PriceLevel>,
    // Accepted IDs are never reused; internal slots can be reclaimed in each
    // isolated candidate. FIFO links resolve directly into shared node chunks.
    order_index: OrderNodes,
    seen_order_ids: OrdSet<OrderId>,
    next_seq: u64,
    next_trade_id: u64,
}

#[path = "orderbook_storage.rs"]
mod storage;

#[path = "orderbook_nodes.rs"]
mod nodes;
use nodes::{NodeSlot, OrderNodes};

#[derive(Clone, Debug)]
struct PriceLevel {
    head: NodeSlot,
    tail: NodeSlot,
    qty: u128,
    len: usize,
}

#[derive(Clone, Debug)]
struct OrderNode {
    order: Order,
    prev: Option<NodeSlot>,
    next: Option<NodeSlot>,
}

impl OrderNode {
    fn location(&self) -> OrderLocation {
        OrderLocation {
            side: self.order.side,
            price_tick: self.order.price_tick,
        }
    }
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
        self.apply_guarded(command, &mut UnlimitedMatching)
    }

    pub(crate) fn apply_guarded<G: MatchingGuard + Clone>(
        &mut self,
        command: Command,
        guard: &mut G,
    ) -> Vec<Event> {
        match command {
            Command::NewOrder(order) => self.place_order_guarded(order, guard),
            Command::CancelOrder(cancel) => self.cancel_order(cancel),
            Command::ExpireOrder {
                order_id,
                market_time_ms,
            } => self.expire_order(order_id, market_time_ms),
            Command::AmendOrder(amend) => self.amend_order(amend),
            Command::SetConditionalOrder { .. }
            | Command::SetPositionProtection { .. }
            | Command::NewOrderWithProtection { .. }
            | Command::SetMarkPrice(_)
            | Command::SettleFunding(_) => Vec::new(),
        }
    }

    pub fn place_order(&mut self, order: NewOrder) -> Vec<Event> {
        self.place_order_guarded(order, &mut UnlimitedMatching)
    }

    pub(crate) fn is_reduce_order(&self, id: OrderId) -> bool {
        self.reduce_orders.contains(&id)
    }

    fn place_order_guarded<G: MatchingGuard + Clone>(
        &mut self,
        order: NewOrder,
        guard: &mut G,
    ) -> Vec<Event> {
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

        let fok_unfilled = if order.kind.is_fill_or_kill() && self.reduce_orders.is_empty() {
            !self.can_fully_fill(order.side, limit_price, order.qty)
        } else if order.kind.is_fill_or_kill() {
            let mut trial = self.clone();
            let mut trial_guard = guard.clone();
            let mut probe = order.clone();
            probe.kind = crate::OrderKind::ImmediateOrCancel {
                price_tick: limit_price,
            };
            !trial
                .place_order_guarded(probe, &mut trial_guard)
                .iter()
                .any(|e| matches!(e,Event::OrderFilled{order_id} if *order_id==order.order_id))
        } else {
            false
        };
        if fok_unfilled {
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
        let reduce = order.reduce_only
            || matches!(
                (order.position_side, order.side),
                (crate::PositionSide::Long, Side::Sell) | (crate::PositionSide::Short, Side::Buy)
            );
        if reduce {
            self.reduce_orders.insert(order.order_id);
        }

        let mut incoming = IncomingOrder {
            order_id: order.order_id,
            account_id: order.account_id,
            side: order.side,
            position_side: order.position_side,
            limit_price,
            remaining_qty: order.qty,
        };

        self.match_incoming(&mut incoming, &mut events, guard);

        if incoming.remaining_qty == 0 {
            self.reduce_orders.remove(&incoming.order_id);
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
            && guard.cap(
                incoming.account_id,
                incoming.side,
                incoming.position_side,
                reduce,
                incoming.remaining_qty,
            ) > 0
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
            self.reduce_orders.remove(&incoming.order_id);
            events.push(Event::OrderExpired {
                order_id: incoming.order_id,
                unfilled_qty: incoming.remaining_qty,
            });
        }

        events
    }

    pub fn cancel_order(&mut self, cancel: CancelOrder) -> Vec<Event> {
        let Some(location) = self
            .order_index
            .get(&cancel.order_id)
            .map(OrderNode::location)
        else {
            return vec![Event::CancelRejected {
                order_id: cancel.order_id,
                reason: CancelRejectReason::UnknownOrder,
            }];
        };

        let removed = self
            .remove_resting_order(location, cancel.order_id)
            .expect("indexed orders have a live price level");
        self.order_expirations.remove(&cancel.order_id);
        self.reduce_orders.remove(&cancel.order_id);

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
        let Some(location) = self
            .order_index
            .get(&amend.order_id)
            .map(OrderNode::location)
        else {
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
                .map(|(price_tick, level)| BookLevel {
                    price_tick: *price_tick,
                    qty: level.qty as Qty,
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(price_tick, level)| BookLevel {
                    price_tick: *price_tick,
                    qty: level.qty as Qty,
                })
                .collect(),
        }
    }

    pub fn best_bid(&self) -> Option<PriceTick> {
        self.bids.get_max().map(|(price, _)| *price)
    }

    pub fn best_ask(&self) -> Option<PriceTick> {
        self.asks.get_min().map(|(price, _)| *price)
    }

    pub fn order_owner(&self, order_id: OrderId) -> Option<u64> {
        self.order_index
            .get(&order_id)
            .map(|node| node.order.account_id)
    }

    pub fn resting_orders_for_account(&self, account_id: AccountId) -> Vec<Order> {
        self.account_orders
            .get(&account_id)
            .into_iter()
            .flatten()
            .map(|id| self.order_index[id].order.clone())
            .collect()
    }

    pub(crate) fn cancel_orders_for_account(&mut self, account_id: AccountId) -> Vec<Event> {
        let order_ids = self.order_ids_for_account(account_id);

        order_ids
            .into_iter()
            .flat_map(|order_id| self.cancel_order(CancelOrder { order_id }))
            .collect()
    }

    pub(crate) fn order_ids_for_account(&self, account_id: AccountId) -> Vec<OrderId> {
        self.account_orders
            .get(&account_id)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(crate) fn order_ids_for_account_on_side(
        &self,
        account_id: AccountId,
        side: Side,
    ) -> Vec<OrderId> {
        self.account_orders
            .get(&account_id)
            .into_iter()
            .flatten()
            .filter(|id| self.order_index[*id].order.side == side)
            .copied()
            .collect()
    }

    pub fn fill_quote(
        &self,
        side: Side,
        limit_price: Option<PriceTick>,
        requested_qty: Qty,
    ) -> Result<FillQuote, ClearingError> {
        let mut quote = FillQuote::default();

        let mut add_level = |price_tick: PriceTick, level: &PriceLevel| {
            if quote.qty >= requested_qty || !crosses(side, limit_price, price_tick) {
                return Ok(true);
            }
            let fill_qty = level.qty.min(u128::from(requested_qty - quote.qty)) as Qty;
            quote.qty = quote
                .qty
                .checked_add(fill_qty)
                .ok_or(ClearingError::NotionalOverflow)?;
            quote.notional = quote
                .notional
                .checked_add(notional(price_tick, fill_qty)?)
                .ok_or(ClearingError::NotionalOverflow)?;
            Ok(quote.qty == requested_qty)
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

    fn match_incoming<G: MatchingGuard>(
        &mut self,
        incoming: &mut IncomingOrder,
        events: &mut Vec<Event>,
        guard: &mut G,
    ) {
        while incoming.remaining_qty > 0 {
            let taker_cap = guard.cap(
                incoming.account_id,
                incoming.side,
                incoming.position_side,
                self.is_reduce_order(incoming.order_id),
                incoming.remaining_qty,
            );
            if taker_cap == 0 {
                break;
            }
            let Some(best_price) = self.best_opposite_price(incoming.side) else {
                break;
            };

            if !incoming.crosses(best_price) {
                break;
            }

            let Some((maker_slot, mut resting)) = self
                .front_at(incoming.side.opposite(), best_price)
                .map(|(slot, order)| (slot, order.clone()))
            else {
                break;
            };

            let maker_cap = guard.cap(
                resting.account_id,
                resting.side,
                resting.position_side,
                self.is_reduce_order(resting.order_id),
                resting.remaining_qty,
            );
            if maker_cap == 0 {
                self.remove_node_at(maker_slot).expect("live maker");
                self.order_expirations.remove(&resting.order_id);
                self.reduce_orders.remove(&resting.order_id);
                events.push(Event::OrderExpired {
                    order_id: resting.order_id,
                    unfilled_qty: resting.remaining_qty,
                });
                continue;
            }
            let fill_qty = incoming
                .remaining_qty
                .min(resting.remaining_qty)
                .min(taker_cap)
                .min(maker_cap);
            incoming.remaining_qty -= fill_qty;
            resting.remaining_qty -= fill_qty;

            let trade = Trade {
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
            };
            guard.settle(&trade);
            events.push(Event::TradePrinted(trade));

            if resting.remaining_qty == 0 {
                self.remove_node_at(maker_slot).expect("live maker");
                self.order_expirations.remove(&resting.order_id);
                self.reduce_orders.remove(&resting.order_id);
                events.push(Event::OrderFilled {
                    order_id: resting.order_id,
                });
            } else {
                let remaining_qty = resting.remaining_qty;
                let resting_order_id = resting.order_id;
                if guard.cap(
                    resting.account_id,
                    resting.side,
                    resting.position_side,
                    self.is_reduce_order(resting.order_id),
                    remaining_qty,
                ) == 0
                {
                    self.remove_node_at(maker_slot).expect("live maker");
                    self.order_expirations.remove(&resting_order_id);
                    self.reduce_orders.remove(&resting_order_id);
                    events.push(Event::OrderExpired {
                        order_id: resting_order_id,
                        unfilled_qty: remaining_qty,
                    });
                } else {
                    self.update_qty_at(maker_slot, remaining_qty)
                        .expect("live partially filled maker");
                    events.push(Event::OrderPartiallyFilled {
                        order_id: resting_order_id,
                        remaining_qty,
                    });
                }
            }
            // Expire exhausted protection orders immediately, before another fill
            // could open a new position in the same matching command.
            // Ordinary matching has no reduce-only orders to scan.
            if !self.reduce_orders.is_empty() {
                let expired = self
                    .reduce_orders
                    .iter()
                    .filter_map(|id| {
                        let o = &self.order_index.get(id)?.order;
                        (guard.cap(o.account_id, o.side, o.position_side, true, o.remaining_qty)
                            == 0)
                            .then_some(*id)
                    })
                    .collect::<Vec<_>>();
                for id in expired {
                    events.extend(
                        self.cancel_order(CancelOrder { order_id: id })
                            .into_iter()
                            .map(|e| match e {
                                Event::OrderCanceled {
                                    order_id,
                                    remaining_qty,
                                } => Event::OrderExpired {
                                    order_id,
                                    unfilled_qty: remaining_qty,
                                },
                                other => other,
                            }),
                    );
                }
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
                    fillable_qty =
                        fillable_qty.saturating_add(queue.qty.min(u128::from(Qty::MAX)) as Qty);
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
                    fillable_qty =
                        fillable_qty.saturating_add(queue.qty.min(u128::from(Qty::MAX)) as Qty);
                    if fillable_qty >= qty {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn resting_order(&self, location: OrderLocation, order_id: OrderId) -> Option<&Order> {
        let node = self.order_index.get(&order_id)?;
        (node.location() == location).then_some(&node.order)
    }

    fn update_resting_qty(
        &mut self,
        location: OrderLocation,
        order_id: OrderId,
        new_qty: Qty,
    ) -> Option<()> {
        let slot = self.order_index.slot(&order_id)?;
        (self.order_index.at(slot)?.location() == location).then_some(())?;
        self.update_qty_at(slot, new_qty)
    }

    fn update_qty_at(&mut self, slot: NodeSlot, new_qty: Qty) -> Option<()> {
        let node = self.order_index.at_mut(slot)?;
        let location = node.location();
        let old_qty = node.order.remaining_qty;
        node.order.remaining_qty = new_qty;
        let level = self
            .book_side_mut(location.side)
            .get_mut(&location.price_tick)?;
        level.qty = level.qty - u128::from(old_qty) + u128::from(new_qty);
        Some(())
    }

    fn remove_resting_order(
        &mut self,
        location: OrderLocation,
        order_id: OrderId,
    ) -> Option<Order> {
        let slot = self.order_index.slot(&order_id)?;
        (self.order_index.at(slot)?.location() == location).then_some(())?;
        self.remove_node_at(slot)
    }

    fn remove_node_at(&mut self, slot: NodeSlot) -> Option<Order> {
        let node = self.order_index.remove(slot)?;
        let location = node.location();
        let order_id = node.order.order_id;
        if let Some(prev) = node.prev {
            self.order_index
                .at_mut(prev)
                .expect("live previous link")
                .next = node.next;
        }
        if let Some(next) = node.next {
            self.order_index.at_mut(next).expect("live next link").prev = node.prev;
        }
        let side = self.book_side_mut(location.side);
        let level = side.get_mut(&location.price_tick).expect("live level");
        if level.len == 1 {
            side.remove(&location.price_tick);
        } else {
            level.len -= 1;
            level.qty -= u128::from(node.order.remaining_qty);
            if level.head == slot {
                level.head = node.next.expect("next head");
            }
            if level.tail == slot {
                level.tail = node.prev.expect("previous tail");
            }
        }
        let ids = self
            .account_orders
            .get_mut(&node.order.account_id)
            .expect("live account index");
        ids.remove(&order_id);
        if ids.is_empty() {
            self.account_orders.remove(&node.order.account_id);
        }
        Some(node.order)
    }

    fn insert_resting_order(&mut self, order: Order) {
        self.attach_order(order);
    }

    fn front_at(&self, side: Side, price_tick: PriceTick) -> Option<(NodeSlot, &Order)> {
        let head = self.book_side(side).get(&price_tick)?.head;
        self.order_index.at(head).map(|node| (head, &node.order))
    }

    fn attach_order(&mut self, order: Order) {
        let id = order.order_id;
        let old = self.book_side(order.side).get(&order.price_tick).cloned();
        let prev = old.as_ref().map(|level| level.tail);
        let side = order.side;
        let price = order.price_tick;
        let account = order.account_id;
        let qty = order.remaining_qty;
        let slot = self.order_index.insert(
            id,
            OrderNode {
                order,
                prev,
                next: None,
            },
        );
        if let Some(prev) = prev {
            self.order_index.at_mut(prev).expect("live tail").next = Some(slot);
        }
        let level = match old {
            Some(level) => PriceLevel {
                head: level.head,
                tail: slot,
                qty: level.qty + u128::from(qty),
                len: level.len + 1,
            },
            None => PriceLevel {
                head: slot,
                tail: slot,
                qty: u128::from(qty),
                len: 1,
            },
        };
        self.book_side_mut(side).insert(price, level);
        self.account_orders.entry(account).or_default().insert(id);
    }

    fn level_orders<'a>(&'a self, level: &PriceLevel) -> impl Iterator<Item = &'a Order> {
        let mut next = Some(level.head);
        std::iter::from_fn(move || {
            let node = self.order_index.at(next?)?;
            next = node.next;
            Some(&node.order)
        })
    }

    fn book_side_mut(&mut self, side: Side) -> &mut OrdMap<PriceTick, PriceLevel> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    fn book_side(&self, side: Side) -> &OrdMap<PriceTick, PriceLevel> {
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

pub(crate) trait MatchingGuard {
    fn cap(
        &mut self,
        account: AccountId,
        side: Side,
        leg: crate::PositionSide,
        reduce: bool,
        qty: Qty,
    ) -> Qty;
    fn settle(&mut self, trade: &Trade);
}
#[derive(Clone)]
struct UnlimitedMatching;
impl MatchingGuard for UnlimitedMatching {
    fn cap(&mut self, _: AccountId, _: Side, _: crate::PositionSide, _: bool, qty: Qty) -> Qty {
        qty
    }
    fn settle(&mut self, _: &Trade) {}
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
    fn assert_links(book: &OrderBook) {
        let mut indexed = std::collections::BTreeSet::new();
        let mut accounts: OrdMap<AccountId, OrdSet<OrderId>> = OrdMap::new();
        for (side, levels) in [(Side::Buy, &book.bids), (Side::Sell, &book.asks)] {
            for (price, level) in levels {
                let mut prev = None;
                let mut next = Some(level.head);
                let mut qty = 0u128;
                let mut count = 0;
                while let Some(id) = next {
                    let node = book.order_index.at(id).expect("live price slot");
                    let order_id = node.order.order_id;
                    assert!(
                        indexed.insert(order_id),
                        "duplicate node or cyclic price queue"
                    );
                    assert_eq!(book.order_index.slot(&order_id), Some(id));
                    assert_eq!(node.prev, prev);
                    assert_eq!(node.order.side, side);
                    assert_eq!(node.order.price_tick, *price);
                    assert!(book.seen_order_ids.contains(&order_id));
                    accounts
                        .entry(node.order.account_id)
                        .or_default()
                        .insert(order_id);
                    qty += u128::from(node.order.remaining_qty);
                    count += 1;
                    prev = Some(id);
                    next = node.next;
                }
                assert_eq!(prev, Some(level.tail));
                assert_eq!(count, level.len);
                assert_eq!(qty, level.qty);
            }
        }
        assert_eq!(indexed.len(), book.order_index.len());
        assert_eq!(accounts, book.account_orders);
    }

    #[test]
    fn recycled_node_slots_keep_old_candidates_and_fifo_independent() {
        let mut book = OrderBook::new();
        for id in 1..=128 {
            book.place_order(limit(id, Side::Sell, 100, 3));
        }
        let original = book.clone();
        let original_wire = serde_json::to_value(&original).unwrap();
        let mut live: Vec<_> = (1..=128).collect();
        for generation in 0..8 {
            for id in &live {
                book.cancel_order(CancelOrder { order_id: *id });
            }
            let empty = book.clone();
            assert_links(&empty);
            live = (1000 + generation * 128..1128 + generation * 128).collect();
            for id in &live {
                book.place_order(limit(*id, Side::Sell, 100, 3));
            }
            assert_eq!(book.order_index.allocated_slots(), 128);
            let events = book.place_order(market(20_000 + generation, Side::Buy, 1));
            assert_eq!(trade_events(&events)[0].maker_order_id, live[0]);
            assert_links(&book);
            assert_links(&empty);
            assert!(empty.snapshot().asks.is_empty());
            assert_links(&original);
            assert_eq!(serde_json::to_value(&original).unwrap(), original_wire);
        }
        let mut restored: OrderBook =
            serde_json::from_value(serde_json::to_value(&book).unwrap()).unwrap();
        assert_links(&restored);
        assert_eq!(
            restored.place_order(market(30_000, Side::Buy, 4)),
            book.place_order(market(30_000, Side::Buy, 4))
        );
    }

    #[test]
    fn crowded_price_nodes_preserve_fifo_amends_and_independent_candidates() {
        let mut book = OrderBook::new();
        for id in 1..=4096 {
            book.place_order(limit(id, Side::Sell, 100, 3));
        }
        let frozen = book.clone();
        assert!(book.order_index.ptr_eq(&frozen.order_index));
        assert!(book.asks.ptr_eq(&frozen.asks));
        assert!(book.seen_order_ids.ptr_eq(&frozen.seen_order_ids));
        for id in (1..=4096).step_by(3) {
            book.cancel_order(CancelOrder { order_id: id });
        }
        book.amend_order(AmendOrder {
            order_id: 2,
            price_tick: None,
            qty: Some(2),
        });
        book.amend_order(AmendOrder {
            order_id: 3,
            price_tick: Some(101),
            qty: Some(1),
        });
        let fills = trade_events(&book.place_order(market(5000, Side::Buy, 4)));
        assert_eq!(
            fills
                .iter()
                .map(|t| (t.maker_order_id, t.qty))
                .collect::<Vec<_>>(),
            vec![(2, 2), (5, 2)]
        );
        assert_eq!(book.order_index[&5].order.remaining_qty, 1);
        assert_links(&book);
        assert_links(&frozen);
        assert_eq!(frozen.snapshot().asks[0].qty, 4096 * 3);
        let mut recovered: OrderBook =
            serde_json::from_value(serde_json::to_value(&book).unwrap()).unwrap();
        assert_links(&recovered);
        assert_eq!(recovered.snapshot(), book.snapshot());
        assert_eq!(
            recovered.place_order(market(5001, Side::Buy, 7)),
            book.place_order(market(5001, Side::Buy, 7))
        );
        assert_links(&book);
        assert_links(&recovered);
    }

    #[test]
    fn checkpoint_links_are_derived_and_duplicate_queue_nodes_are_rejected() {
        let mut book = OrderBook::new();
        book.place_order(limit(1, Side::Sell, 100, 2));
        let mut wire = serde_json::to_value(book).unwrap();
        wire["order_index"] = serde_json::json!({});
        wire["account_orders"] = serde_json::json!({"999":[1]});
        let restored: OrderBook = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(restored.order_owner(1), Some(1001));
        assert_links(&restored);
        let duplicate = wire["asks"]["100"][0].clone();
        wire["asks"]["100"].as_array_mut().unwrap().push(duplicate);
        assert!(serde_json::from_value::<OrderBook>(wire).is_err());
    }

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
