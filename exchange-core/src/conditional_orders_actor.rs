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
                        .values()
                        .filter(|p| p.account_id == account && p.status == "armed")
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
            .values()
            .filter(|p| p.instrument_id == instrument && p.account_id == account)
            .cloned()
            .collect()
    }

    /// Every conditional order in key order; `conditional_orders` filters it.
    pub fn all_conditional_orders(
        &self,
    ) -> impl Iterator<Item = &crate::conditional_orders::ConditionalOrder> {
        self.conditional_orders.values()
    }

    /// Triggers every armed conditional order whose price was crossed, and
    /// moves each armed entry's event cursor to its market's latest event.
    /// Runs after every accepted order, so entries are read by reference and
    /// only entries that change are written: an entry reads market state and
    /// never another entry, so checking all before writing any is the same.
    pub fn prepare_conditional_orders(
        &mut self,
    ) -> Vec<crate::conditional_orders::ConditionalOrder> {
        let mut latest_seqs = Vec::<(&str, u64)>::new();
        let mut prices = Vec::<(&str, crate::ProtectionTrigger, Option<i64>)>::new();
        // The lowest and highest trade after a cursor, per market and cursor
        // (armed entries usually share one): a trade after it crossed a
        // level exactly when one of the two did.
        let mut extremes = Vec::<(&str, u64, Option<(i64, i64)>)>::new();
        let mut changes = Vec::new();
        for (key, p) in self.conditional_orders.iter() {
            if p.status != "armed" {
                continue;
            }
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
                    latest_seqs.push((instrument, seq));
                    seq
                }
            };
            // Event sequence numbers grow in append order, so there is a
            // record after the cursor exactly when the latest one is.
            let crossed = if p.spec.trigger == crate::ProtectionTrigger::Last {
                match &self.markets[instrument].engine {
                    MarketEngine::Perp(e) if latest > p.last_checked_event_seq => {
                        let cursor = p.last_checked_event_seq;
                        let range = match extremes
                            .iter()
                            .find(|(i, c, _)| *i == instrument && *c == cursor)
                        {
                            Some((_, _, range)) => *range,
                            None => {
                                let range = e.event_history().events_after(cursor).fold(
                                    None,
                                    |range: Option<(i64, i64)>, r| match &r.event {
                                        crate::Event::TradePrinted(t) => Some(range.map_or(
                                            (t.price_tick, t.price_tick),
                                            |(low, high)| (low.min(t.price_tick), high.max(t.price_tick)),
                                        )),
                                        _ => range,
                                    },
                                );
                                extremes.push((instrument, cursor, range));
                                range
                            }
                        };
                        range.is_some_and(|(low, high)| crosses(low) || crosses(high))
                    }
                    _ => false,
                }
            } else {
                false
            } || {
                let trigger = p.spec.trigger;
                let price = match prices.iter().find(|(i, t, _)| *i == instrument && *t == trigger) {
                    Some((_, _, price)) => *price,
                    None => {
                        let price = self.protection_price(instrument, trigger);
                        prices.push((instrument, trigger, price));
                        price
                    }
                };
                price.is_some_and(crosses)
            };
            if crossed || latest != p.last_checked_event_seq {
                changes.push((key.clone(), latest, crossed.then(|| p.clone())));
            }
        }
        let mut result = Vec::new();
        for (key, latest, triggered) in changes {
            let current = self.conditional_orders.get_mut(&key).unwrap();
            current.last_checked_event_seq = latest;
            if let Some(p) = triggered {
                current.status = "triggered".into();
                result.push(p);
            }
        }
        result
    }

    /// The scan `prepare_conditional_orders` replaced: every entry cloned,
    /// every armed entry rewritten. Tests compare the two.
    #[cfg(test)]
    pub(crate) fn prepare_conditional_orders_reference(
        &mut self,
    ) -> Vec<crate::conditional_orders::ConditionalOrder> {
        let mut result = Vec::new();
        for key in self.conditional_orders.keys().cloned().collect::<Vec<_>>() {
            let p = self.conditional_orders[&key].clone();
            if p.status != "armed" {
                continue;
            }
            let crosses = |price: i64| {
                if p.spec.above {
                    price >= p.spec.trigger_price_tick
                } else {
                    price <= p.spec.trigger_price_tick
                }
            };
            let crossed = if p.spec.trigger == crate::ProtectionTrigger::Last {
                match &self.markets[&p.instrument_id].engine {
                    MarketEngine::Perp(e)=>e.event_history().events_after(p.last_checked_event_seq).any(|r|matches!(&r.event,crate::Event::TradePrinted(t) if crosses(t.price_tick))),
                    _=>false
                }
            } else {
                false
            } || self
                .protection_price(&p.instrument_id, p.spec.trigger)
                .is_some_and(crosses);
            let latest = self.protection_event_seq(&p.instrument_id);
            let current = self.conditional_orders.get_mut(&key).unwrap();
            current.last_checked_event_seq = latest;
            if crossed {
                current.status = "triggered".into();
                result.push(p);
            }
        }
        result
    }

    pub fn record_conditional_order(
        &mut self,
        p: &crate::conditional_orders::ConditionalOrder,
        id: u64,
        accepted: bool,
    ) {
        if let Some(current) = self
            .conditional_orders
            .get_mut(&format!("{}:{}:{}", p.instrument_id, p.account_id, p.key))
        {
            current.submitted_order_id = Some(id);
            current.status = if accepted { "submitted" } else { "rejected" }.into();
        }
    }
}
