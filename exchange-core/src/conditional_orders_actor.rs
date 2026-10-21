#[cfg(test)]
thread_local! {
    /// Tests: the pass after each order runs the reference scan instead.
    pub(crate) static CONDITIONAL_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl ExchangeActor {
    fn apply_conditional_command(
        &mut self,
        instrument: &str,
        command: Command,
        origin: CommandOrigin,
    ) -> Result<ActorExecution, ActorRejectReason> {
        let Command::SetConditionalOrder {
            account_id,
            key,
            spec,
        } = &command
        else {
            unreachable!()
        };
        let account = *account_id;
        let mapkey = format!("{instrument}:{account}:{key}");
        let validation = (|| {
            if self.status() == MarketStatus::Closed {
                return Err(ActorRejectReason::MarketClosed);
            }
            if key.is_empty()
                || key.len() > 64
                || !key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                return Err(ActorRejectReason::InvalidPositionProtection);
            }
            let Some(AccountSnapshot::Perp(snapshot)) =
                self.account_snapshot_for(instrument, account)?
            else {
                return Err(ActorRejectReason::WrongMarketKind);
            };
            if let Some(s) = spec {
                if self.status() == MarketStatus::Paused && origin == CommandOrigin::External {
                    return Err(ActorRejectReason::MarketPaused);
                }
                let config = self.market(instrument)?.config();
                let lot = config.instrument().lot_size;
                let tick = config.instrument().tick_size;
                if s.qty == 0
                    || s.qty % lot != 0
                    || s.trigger_price_tick <= 0
                    || s.trigger_price_tick % tick != 0
                    || s.limit_price_tick.is_some_and(|p| p <= 0 || p % tick != 0)
                    || s.protection.as_ref().is_some_and(|p| !p.valid())
                {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
                if snapshot.hedge_positions.is_some() {
                    if !matches!(
                        (s.position_side, s.side),
                        (crate::PositionSide::Long, Side::Buy)
                            | (crate::PositionSide::Short, Side::Sell)
                    ) {
                        return Err(ActorRejectReason::InvalidPositionProtection);
                    }
                } else if s.position_side != crate::PositionSide::Both {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
                if !self.conditional_orders.contains_key(&mapkey)
                    && self
                        .conditional_orders
                        .iter()
                        .filter(|(_, p)| p.account_id == account && p.status == "armed")
                        .count()
                        >= 32
                {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
            }
            Ok(())
        })();
        if let Err(reason) = validation {
            let seq = self.take_command_seq();
            return Ok(ActorExecution {
                room_id: self.room_id.clone(),
                instrument_id: instrument.into(),
                command_seq: seq,
                market_time_ms: self.clock.market_time_ms(),
                status: self.status(),
                rejected_command: Some(command),
                price_updates: Vec::new(),
                funding_settlement: None,
                book_delta: None,
                is_liquidation: false,
                wallet_updates: Vec::new(),
                margin_updates: Vec::new(),
                result: ActorExecutionResult::Rejected(reason),
            });
        }
        let mut cancel_events = Vec::new();
        let mut cancel_clearing = Vec::new();
        if let Some(id) = self
            .conditional_orders
            .get(&mapkey)
            .and_then(|p| p.submitted_order_id)
            && self.order_owner_for(instrument, id)? == Some(account)
        {
            let cancel = self.apply_to_instrument(
                instrument,
                Command::CancelOrder(crate::CancelOrder { order_id: id }),
            )?;
            if let ActorExecutionResult::Accepted(MarketExecution::Perp(result)) = cancel.result {
                cancel_events = result.events;
                cancel_clearing = result.clearing_events;
            }
        }
        let seq = self.take_command_seq();
        if let Some(spec) = spec {
            let last_checked_event_seq = self.protection_event_seq(instrument);
            self.conditional_orders.insert(
                mapkey,
                crate::conditional_orders::ConditionalOrder {
                    key: key.clone(),
                    instrument_id: instrument.into(),
                    account_id: account,
                    spec: (**spec).clone(),
                    status: "armed".into(),
                    last_checked_event_seq,
                    submitted_order_id: None,
                },
            );
        } else {
            self.conditional_orders.remove(&mapkey);
        }
        Ok(ActorExecution {
            room_id: self.room_id.clone(),
            instrument_id: instrument.into(),
            command_seq: seq,
            market_time_ms: self.clock.market_time_ms(),
            status: self.status(),
            rejected_command: None,
            price_updates: Vec::new(),
            funding_settlement: None,
            book_delta: None,
            is_liquidation: false,
            wallet_updates: Vec::new(),
                margin_updates: Vec::new(),
            result: ActorExecutionResult::Accepted(MarketExecution::Perp(PerpTradingExecution {
                command: crate::CommandRecord { seq, command },
                events: cancel_events,
                clearing_events: cancel_clearing,
            })),
        })
    }

    pub fn conditional_orders(
        &self,
        instrument: &str,
        account: AccountId,
    ) -> Vec<crate::conditional_orders::ConditionalOrder> {
        self.conditional_orders
            .iter()
            .filter(|(_, p)| p.instrument_id == instrument && p.account_id == account)
            .map(|(_, p)| self.conditional_orders.exact(p))
            .collect()
    }

    /// Every conditional order in key order with its event cursor, which the
    /// entry's own `last_checked_event_seq` can lag behind while it is armed
    /// (`conditional_orders` returns exact copies).
    pub fn all_conditional_orders(
        &self,
    ) -> impl Iterator<Item = (&crate::conditional_orders::ConditionalOrder, u64)> {
        self.conditional_orders
            .iter()
            .map(|(_, p)| (p, self.conditional_orders.cursor(p)))
    }

    /// Triggers every armed conditional order whose price was crossed, and
    /// moves each armed entry's event cursor to its market's latest event.
    /// Runs after every accepted order, so it visits only entries that can
    /// change. A market's armed entries share one cursor, its mark, unless
    /// fresh: such an entry is crossed exactly when its trigger price is
    /// within the range that the trades after the mark and the current price
    /// reach, which the index finds by range. Fresh entries are checked from
    /// their own cursor. The cursors move by writing the market's mark, when
    /// any armed entry's would move. Results come in key order.
    fn prepare_conditional_orders_body(
        &mut self,
    ) -> Vec<crate::conditional_orders::ConditionalOrder> {
        #[cfg(test)]
        if CONDITIONAL_REFERENCE.get() {
            return self.prepare_conditional_orders_reference();
        }
        let orders = &self.conditional_orders;
        // Crossed keys, with the latest event of their market.
        let mut crossed = BTreeMap::<&str, u64>::new();
        let mut marks = Vec::new();
        for instrument in orders.armed_markets() {
            let latest = self.protection_event_seq(instrument);
            // The lowest and highest trade after a cursor; a trade after it
            // crossed a level exactly when one of the two did.
            let trades = |cursor: u64| match &self.markets[instrument].engine {
                MarketEngine::Perp(e) if latest > cursor => {
                    e.event_history().events_after(cursor).fold(None, |range, r| match &r.event {
                        crate::Event::TradePrinted(t) => Some(range.map_or(
                            (t.price_tick, t.price_tick),
                            |(low, high): (i64, i64)| (low.min(t.price_tick), high.max(t.price_tick)),
                        )),
                        _ => range,
                    })
                }
                _ => None,
            };
            let prices = [
                self.protection_price(instrument, crate::ProtectionTrigger::Mark),
                self.protection_price(instrument, crate::ProtectionTrigger::Last),
            ];
            let mark = orders.mark(instrument);
            let fresh = orders.fresh(instrument);
            let mut after_mark = None;
            let mut armed = 0;
            for (group, entries) in orders.armed_groups(instrument) {
                armed += entries.len();
                let range = if group.last {
                    *after_mark.get_or_insert_with(|| trades(mark))
                } else {
                    None
                };
                let price = prices[usize::from(group.last)];
                // The highest (above) or lowest (below) price reached.
                let reached = if group.above {
                    range.map(|(_, high)| high).max(price)
                } else {
                    [range.map(|(low, _)| low), price].into_iter().flatten().min()
                };
                let Some(level) = reached else {
                    continue;
                };
                let hits: Box<dyn Iterator<Item = &(i64, String)>> = if group.above {
                    match level.checked_add(1) {
                        Some(next) => Box::new(entries.range(..(next, String::new()))),
                        None => Box::new(entries.iter()),
                    }
                } else {
                    Box::new(entries.range((level, String::new())..))
                };
                for (_, key) in hits {
                    if !fresh.is_some_and(|f| f.contains(key)) {
                        crossed.insert(key, latest);
                    }
                }
            }
            let mut moved = armed > fresh.map_or(0, |f| f.len()) && mark != latest;
            for key in fresh.into_iter().flatten() {
                let p = orders.get(key).expect("fresh entries are stored");
                // A fresh entry's stored cursor is its cursor: above the mark.
                let cursor = p.last_checked_event_seq;
                moved |= cursor != latest;
                let crosses = |price: i64| {
                    if p.spec.above {
                        price >= p.spec.trigger_price_tick
                    } else {
                        price <= p.spec.trigger_price_tick
                    }
                };
                let last = p.spec.trigger == crate::ProtectionTrigger::Last;
                if (last && trades(cursor).is_some_and(|(low, high)| crosses(low) || crosses(high)))
                    || prices[usize::from(last)].is_some_and(crosses)
                {
                    crossed.insert(key, latest);
                }
            }
            if moved {
                marks.push((instrument.to_string(), latest));
            }
        }
        let result = crossed
            .keys()
            .map(|key| orders.exact(orders.get(key).expect("crossed entries are stored")))
            .collect::<Vec<_>>();
        let crossed = crossed
            .into_iter()
            .map(|(key, latest)| (key.to_string(), latest))
            .collect::<Vec<_>>();
        for (instrument, latest) in marks {
            self.conditional_orders.set_mark(&instrument, latest);
        }
        for (key, latest) in crossed {
            self.conditional_orders.update(&key, |current| {
                current.last_checked_event_seq = latest;
                current.status = "triggered".into();
            });
        }
        result
    }

    /// Tests: no conditional order changed since `previous` (see `same`).
    #[cfg(test)]
    pub(crate) fn conditional_orders_unchanged(&self, previous: &Self) -> bool {
        self.conditional_orders.same(&previous.conditional_orders)
    }

    /// The scan `prepare_conditional_orders` replaced: every armed entry
    /// checked from its own cursor, and each one that changes written.
    #[cfg(test)]
    fn prepare_conditional_orders_reference_body(
        &mut self,
    ) -> Vec<crate::conditional_orders::ConditionalOrder> {
        let mut latest_seqs = Vec::<(&str, u64)>::new();
        let mut changes = Vec::new();
        for (key, p) in self.conditional_orders.iter() {
            if p.status != "armed" {
                continue;
            }
            let p = self.conditional_orders.exact(p);
            let crosses = |price: i64| {
                if p.spec.above {
                    price >= p.spec.trigger_price_tick
                } else {
                    price <= p.spec.trigger_price_tick
                }
            };
            let instrument = p.instrument_id.as_str();
            let latest = match latest_seqs.iter().find(|(i, _)| *i == instrument) {
                Some((_, seq)) => *seq,
                None => {
                    let seq = self.protection_event_seq(instrument);
                    let market = self.markets.get_key_value(instrument).unwrap().0;
                    latest_seqs.push((market.as_str(), seq));
                    seq
                }
            };
            let crossed = if p.spec.trigger == crate::ProtectionTrigger::Last {
                match &self.markets[instrument].engine {
                    MarketEngine::Perp(e) => e
                        .event_history()
                        .events_after(p.last_checked_event_seq)
                        .any(|r| matches!(&r.event, crate::Event::TradePrinted(t) if crosses(t.price_tick))),
                    _ => false,
                }
            } else {
                false
            } || self
                .protection_price(instrument, p.spec.trigger)
                .is_some_and(crosses);
            if crossed || latest != p.last_checked_event_seq {
                changes.push((key.clone(), latest, crossed.then(|| p.clone())));
            }
        }
        let mut result = Vec::new();
        for (key, latest, triggered) in changes {
            self.conditional_orders.update(&key, |current| {
                current.last_checked_event_seq = latest;
                if triggered.is_some() {
                    current.status = "triggered".into();
                }
            });
            result.extend(triggered);
        }
        result
    }

    fn record_conditional_order_body(
        &mut self,
        p: &crate::conditional_orders::ConditionalOrder,
        id: u64,
        accepted: bool,
    ) {
        self.conditional_orders.update(
            &format!("{}:{}:{}", p.instrument_id, p.account_id, p.key),
            |current| {
                current.submitted_order_id = Some(id);
                current.status = if accepted { "submitted" } else { "rejected" }.into();
            },
        );
    }
}
