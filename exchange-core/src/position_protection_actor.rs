impl ExchangeActor {
    pub fn position_risk(
        &self,
        instrument: &str,
        account: AccountId,
    ) -> Option<crate::position_protection::PositionRisk> {
        let Some(AccountSnapshot::Perp(a)) = self.account_snapshot_for(instrument, account).ok()?
        else {
            return None;
        };
        let crate::MarketConfig::Perp(config) = self.market(instrument).ok()?.config() else {
            return None;
        };
        Some(crate::position_protection::PositionRisk::from_account(
            &a,
            self.protection_price(instrument, crate::ProtectionTrigger::Mark)?,
            config.clearing.maintenance_margin_ppm,
        ))
    }
    fn protection_key(instrument: &str, account: AccountId, side: crate::PositionSide) -> String {
        format!("{instrument}:{account}:{side:?}")
    }

    pub fn has_active_position_protections(&self) -> bool {
        self.conditional_orders
            .values()
            .any(|p| p.status == "armed")
            || self
                .position_protections
                .values()
                .any(|p| matches!(p.status.as_str(), "armed" | "awaiting_fill" | "triggered"))
    }

    pub fn complete_flat_position_protections(&mut self) {
        for key in self
            .position_protections
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            let p = &self.position_protections[&key];
            if !matches!(p.status.as_str(), "armed" | "triggered") {
                continue;
            }
            let qty = self
                .account_snapshot_for(&p.instrument_id, p.account_id)
                .ok()
                .flatten()
                .and_then(|s| match s {
                    AccountSnapshot::Perp(a) => {
                        crate::position_protection::position_qty(&a, p.position_side)
                    }
                    _ => None,
                });
            if qty.is_some_and(|qty| qty == 0 || (qty > 0) != (p.exit_side == Side::Sell)) {
                self.position_protections.get_mut(&key).unwrap().status = "completed".into();
            }
        }
    }

    fn protection_event_seq(&self, instrument: &str) -> u64 {
        match self.markets.get(instrument).map(|m| &m.engine) {
            Some(MarketEngine::Perp(e)) => e.event_log().last().map_or(0, |e| e.seq),
            _ => 0,
        }
    }

    pub fn position_protections(
        &self,
        instrument: &str,
        account: AccountId,
    ) -> Vec<crate::PositionProtection> {
        self.position_protections
            .values()
            .filter(|p| p.instrument_id == instrument && p.account_id == account)
            .cloned()
            .collect()
    }

    pub fn protection_price(
        &self,
        instrument: &str,
        trigger: crate::ProtectionTrigger,
    ) -> Option<i64> {
        let MarketEngine::Perp(engine) = &self.markets.get(instrument)?.engine else {
            return None;
        };
        match trigger {
            crate::ProtectionTrigger::Mark => Some(engine.mark_price_tick()),
            crate::ProtectionTrigger::Last => {
                engine
                    .event_log()
                    .iter()
                    .rev()
                    .find_map(|e| match &e.event {
                        crate::Event::TradePrinted(t) => Some(t.price_tick),
                        _ => None,
                    })
            }
        }
    }

    fn apply_position_protection_command(
        &mut self,
        instrument: &str,
        command: Command,
        origin: CommandOrigin,
    ) -> Result<ActorExecution, ActorRejectReason> {
        let (account, side, spec, opening) = match &command {
            Command::SetPositionProtection {
                account_id,
                position_side,
                protection,
            } => (*account_id, *position_side, protection.clone(), None),
            Command::NewOrderWithProtection { order, protection } => (
                order.account_id,
                order.position_side,
                Some(protection.clone()),
                Some(order.clone()),
            ),
            _ => unreachable!(),
        };
        self.market(instrument)?;
        let validation = (|| {
            match self.status() {
                MarketStatus::Closed => return Err(ActorRejectReason::MarketClosed),
                MarketStatus::Paused if origin == CommandOrigin::External => {
                    return Err(ActorRejectReason::MarketPaused);
                }
                _ => {}
            }
            let Some(AccountSnapshot::Perp(snapshot)) =
                self.account_snapshot_for(instrument, account)?
            else {
                return Err(ActorRejectReason::WrongMarketKind);
            };
            let qty = crate::position_protection::position_qty(&snapshot, side)
                .ok_or(ActorRejectReason::InvalidPositionProtection)?;
            let direction = if let Some(order) = &opening {
                if order.reduce_only || (qty != 0 && (qty > 0) != (order.side == Side::Buy)) {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
                if side == crate::PositionSide::Long && order.side != Side::Buy
                    || side == crate::PositionSide::Short && order.side != Side::Sell
                {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
                order.side
            } else if qty > 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            if let Some(spec) = &spec {
                if !spec.valid() || opening.is_none() && qty == 0 {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
                let price = self
                    .protection_price(instrument, spec.trigger)
                    .ok_or(ActorRejectReason::InvalidPositionProtection)?;
                let tick_size = self.market(instrument)?.config().instrument().tick_size;
                if spec
                    .take_profit_tick
                    .into_iter()
                    .chain(spec.stop_loss_tick)
                    .chain(spec.trailing_distance_tick)
                    .chain(spec.exit_price_tick)
                    .chain(spec.take_profit_steps.iter().map(|s| s.price_tick))
                    .any(|p| p % tick_size != 0)
                {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
                if spec.take_profit_tick.is_some_and(|p| {
                    if direction == Side::Buy {
                        p <= price
                    } else {
                        p >= price
                    }
                }) || spec.stop_loss_tick.is_some_and(|p| {
                    if direction == Side::Buy {
                        p >= price
                    } else {
                        p <= price
                    }
                }) {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
                let lot = self.market(instrument)?.config().instrument().lot_size;
                if spec.exit_qty.is_some_and(|q| q % lot != 0)
                    || spec.take_profit_steps.iter().any(|s| {
                        s.qty % lot != 0
                            || if direction == Side::Buy {
                                s.price_tick <= price
                            } else {
                                s.price_tick >= price
                            }
                    })
                    || spec.take_profit_steps.windows(2).any(|s| {
                        if direction == Side::Buy {
                            s[1].price_tick <= s[0].price_tick
                        } else {
                            s[1].price_tick >= s[0].price_tick
                        }
                    })
                {
                    return Err(ActorRejectReason::InvalidPositionProtection);
                }
            }
            Ok(direction.opposite())
        })();
        let exit_side = match validation {
            Ok(side) => side,
            Err(reason) => {
                let seq = self.take_command_seq();
                return Ok(ActorExecution {
                    room_id: self.room_id.clone(),
                    instrument_id: instrument.into(),
                    command_seq: seq,
                    market_time_ms: self.clock.market_time_ms(),
                    status: self.status(),
                    result: ActorExecutionResult::Rejected(reason),
                    rejected_command: Some(command),
                    price_updates: Vec::new(),
                    funding_settlement: None,
                });
            }
        };
        let mut staged = self.clone();
        let mut execution = if let Some(order) = &opening {
            staged.apply_to_instrument_from(instrument, Command::NewOrder(order.clone()), origin)?
        } else {
            let seq = staged.take_command_seq();
            ActorExecution {
                room_id: staged.room_id.clone(),
                instrument_id: instrument.into(),
                command_seq: seq,
                market_time_ms: staged.clock.market_time_ms(),
                status: staged.status(),
                rejected_command: None,
                price_updates: Vec::new(),
                funding_settlement: None,
                result: ActorExecutionResult::Accepted(MarketExecution::Perp(
                    PerpTradingExecution {
                        command: crate::CommandRecord {
                            seq,
                            command: command.clone(),
                        },
                        events: Vec::new(),
                        clearing_events: Vec::new(),
                    },
                )),
            }
        };
        let admitted = match &execution.result {
            ActorExecutionResult::Accepted(MarketExecution::Perp(r)) => !r.events.iter().any(|e| {
                matches!(
                    e.event,
                    crate::Event::RiskRejected { .. } | crate::Event::OrderRejected { .. }
                )
            }),
            _ => false,
        };
        if admitted {
            let key = Self::protection_key(instrument, account, side);
            if let Some(id) = staged
                .position_protections
                .get(&key)
                .and_then(|p| p.last_exit_order_id)
                && staged.order_owner_for(instrument, id)?.is_some()
            {
                let canceled = staged.apply_to_instrument_from(
                    instrument,
                    Command::CancelOrder(crate::CancelOrder { order_id: id }),
                    origin,
                )?;
                execution.command_seq = canceled.command_seq;
                if let (
                    ActorExecutionResult::Accepted(MarketExecution::Perp(result)),
                    ActorExecutionResult::Accepted(MarketExecution::Perp(cancel)),
                ) = (&mut execution.result, canceled.result)
                {
                    result.events.extend(cancel.events);
                    result.clearing_events.extend(cancel.clearing_events);
                }
            }
            if let Some(spec) = spec {
                let Some(AccountSnapshot::Perp(snapshot)) =
                    staged.account_snapshot_for(instrument, account)?
                else {
                    unreachable!()
                };
                let qty = crate::position_protection::position_qty(&snapshot, side).unwrap_or(0);
                let awaiting = qty == 0;
                // An unfilled non-resting entry has no future position to protect.
                if !awaiting
                    || opening.as_ref().is_some_and(|o| {
                        staged
                            .order_owner_for(instrument, o.order_id)
                            .ok()
                            .flatten()
                            .is_some()
                    })
                {
                    staged.position_protections.insert(
                        key,
                        crate::PositionProtection {
                            instrument_id: instrument.into(),
                            account_id: account,
                            position_side: side,
                            exit_side,
                            spec: *spec,
                            entry_order_id: opening.as_ref().map(|o| o.order_id),
                            status: if awaiting { "awaiting_fill" } else { "armed" }.into(),
                            triggered_by: None,
                            trigger_price_tick: None,
                            triggered_at_market_time_ms: None,
                            last_exit_order_id: None,
                            last_checked_event_seq: self.protection_event_seq(instrument),
                            trailing_watermark_tick: None,
                            take_profit_step: 0,
                            remaining_exit_qty: None,
                            last_observed_qty: None,
                        },
                    );
                }
            } else {
                staged.position_protections.remove(&key);
            }
        }
        match &mut execution.result {
            ActorExecutionResult::Accepted(MarketExecution::Perp(r)) => {
                r.command.command = command.clone()
            }
            ActorExecutionResult::Rejected(_) => execution.rejected_command = Some(command),
            _ => {}
        }
        *self = staged;
        Ok(execution)
    }

    /// Called only inside the exchange mutation transaction. No model or timer.
    pub fn prepare_position_exits(
        &mut self,
    ) -> Vec<crate::position_protection::PositionExitRequest> {
        let mut exits = Vec::new();
        for key in self
            .position_protections
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            let mut p = self.position_protections[&key].clone();
            if !matches!(p.status.as_str(), "armed" | "awaiting_fill" | "triggered") {
                continue;
            }
            let Some(AccountSnapshot::Perp(account)) = self
                .account_snapshot_for(&p.instrument_id, p.account_id)
                .ok()
                .flatten()
            else {
                continue;
            };
            let qty =
                crate::position_protection::position_qty(&account, p.position_side).unwrap_or(0);
            if p.status == "triggered" {
                if let (Some(previous), Some(remaining)) =
                    (p.last_observed_qty, p.remaining_exit_qty)
                {
                    p.remaining_exit_qty =
                        Some(remaining.saturating_sub(previous.saturating_sub(qty.unsigned_abs())));
                }
                if p.spec.exit_qty.is_none()
                    && p.triggered_by.as_deref() != Some("take_profit_step")
                {
                    p.remaining_exit_qty = Some(qty.unsigned_abs());
                }
                if p.remaining_exit_qty == Some(0) {
                    if p.triggered_by.as_deref() == Some("take_profit_step") {
                        p.take_profit_step += 1;
                        p.status = if p.take_profit_step >= p.spec.take_profit_steps.len()
                            && p.spec.stop_loss_tick.is_none()
                            && p.spec.trailing_distance_tick.is_none()
                        {
                            "completed"
                        } else {
                            "armed"
                        }
                        .into();
                        p.triggered_by = None;
                        p.remaining_exit_qty = None;
                        p.last_exit_order_id = None;
                    } else {
                        p.status = "completed".into();
                    }
                }
            }
            p.last_observed_qty = Some(qty.unsigned_abs());
            self.position_protections.insert(key.clone(), p.clone());
            if p.status == "completed" {
                continue;
            }
            let parent_exists = p.entry_order_id.is_some_and(|id| {
                self.order_owner_for(&p.instrument_id, id).ok().flatten() == Some(p.account_id)
            });
            let latest_event_seq = self.protection_event_seq(&p.instrument_id);
            self.position_protections
                .get_mut(&key)
                .unwrap()
                .last_checked_event_seq = latest_event_seq;
            if qty == 0 || (qty > 0) != (p.exit_side == Side::Sell) {
                if !(p.status == "awaiting_fill" && parent_exists && qty == 0) {
                    self.position_protections.get_mut(&key).unwrap().status = "completed".into();
                }
                continue;
            }
            let mut price = self.protection_price(&p.instrument_id, p.spec.trigger);
            let mut watermark = p.trailing_watermark_tick;
            let mut crossed = |price: i64| {
                watermark = Some(watermark.map_or(price, |previous| {
                    if qty > 0 {
                        previous.max(price)
                    } else {
                        previous.min(price)
                    }
                }));
                let long = qty > 0;
                if p.spec
                    .stop_loss_tick
                    .is_some_and(|sl| if long { price <= sl } else { price >= sl })
                {
                    Some("stop_loss".to_string())
                } else if p.spec.trailing_distance_tick.zip(watermark).is_some_and(
                    |(distance, watermark)| {
                        if long {
                            price <= watermark.saturating_sub(distance)
                        } else {
                            price >= watermark.saturating_add(distance)
                        }
                    },
                ) {
                    Some("trailing_stop".into())
                } else if p
                    .spec
                    .take_profit_steps
                    .get(p.take_profit_step)
                    .is_some_and(|s| {
                        if long {
                            price >= s.price_tick
                        } else {
                            price <= s.price_tick
                        }
                    })
                {
                    Some("take_profit_step".into())
                } else if p
                    .spec
                    .take_profit_tick
                    .is_some_and(|tp| if long { price >= tp } else { price <= tp })
                {
                    Some("take_profit".to_string())
                } else {
                    None
                }
            };
            let crossing = if p.spec.trigger == crate::ProtectionTrigger::Last {
                let MarketEngine::Perp(engine) = &self.markets[&p.instrument_id].engine else {
                    continue;
                };
                engine
                    .event_log()
                    .iter()
                    .filter(|e| e.seq > p.last_checked_event_seq)
                    .find_map(|e| match &e.event {
                        crate::Event::TradePrinted(t) => crossed(t.price_tick)
                            .filter(|r| {
                                p.status != "triggered" || r == "stop_loss" || r == "trailing_stop"
                            })
                            .map(|r| (r, t.price_tick)),
                        _ => None,
                    })
            } else {
                None
            };
            let new_reason = if let Some((reason, trigger_price)) = crossing {
                price = Some(trigger_price);
                Some(reason)
            } else {
                price.and_then(&mut crossed)
            };
            let reason = if p.status == "triggered" {
                new_reason
                    .filter(|r| r == "stop_loss" || r == "trailing_stop")
                    .or_else(|| p.triggered_by.clone())
            } else {
                new_reason
            };
            self.position_protections
                .get_mut(&key)
                .unwrap()
                .trailing_watermark_tick = watermark;
            let preempt = matches!(
                p.triggered_by.as_deref(),
                Some("take_profit" | "take_profit_step")
            ) && matches!(reason.as_deref(), Some("stop_loss" | "trailing_stop"));
            let mut cancellations = p
                .entry_order_id
                .filter(|_| parent_exists)
                .into_iter()
                .collect::<Vec<_>>();
            if preempt {
                if let Some(id) = p.last_exit_order_id {
                    cancellations.push(id);
                }
                p.remaining_exit_qty = None;
                p.last_exit_order_id = None;
            }
            let config = self.market(&p.instrument_id).ok().map(|m| m.config());
            let cap = match config {
                Some(crate::MarketConfig::Perp(c)) => c.risk.max_order_qty.unwrap_or(u64::MAX),
                _ => u64::MAX,
            };
            let lot = config.map_or(1, |c| c.instrument().lot_size);
            let target = p
                .remaining_exit_qty
                .unwrap_or_else(|| {
                    if reason.as_deref() == Some("take_profit_step") {
                        u128::from(p.spec.take_profit_steps[p.take_profit_step].qty)
                    } else {
                        p.spec.exit_qty.map_or(qty.unsigned_abs(), u128::from)
                    }
                })
                .min(qty.unsigned_abs());
            let exit_qty = target.min(u128::from(cap)) as u64;
            let exit_qty = exit_qty - exit_qty % lot;
            let exit_qty = match (&self.markets[&p.instrument_id].engine, config) {
                (MarketEngine::Perp(engine), Some(crate::MarketConfig::Perp(c))) => {
                    if let Some(price) = p.spec.exit_price_tick {
                        let cap = c.risk.max_order_notional.map_or(u128::from(exit_qty), |n| {
                            (n / i128::from(price)).max(0) as u128
                        });
                        let q = u128::from(exit_qty).min(cap) as u64;
                        q - q % lot
                    } else {
                        engine.position_exit_qty(
                            p.exit_side,
                            exit_qty,
                            lot,
                            c.risk.max_order_notional,
                        )
                    }
                }
                _ => 0,
            };
            let exit_pending = p.last_exit_order_id.is_some_and(|id| {
                self.order_owner_for(&p.instrument_id, id).ok().flatten() == Some(p.account_id)
            });
            let current = self.position_protections.get_mut(&key).unwrap();
            if let Some(reason) = reason {
                if current.status != "triggered" || preempt {
                    current.triggered_by = Some(reason);
                    current.trigger_price_tick = price;
                    current.triggered_at_market_time_ms = Some(self.clock.market_time_ms());
                    current.remaining_exit_qty = Some(target);
                }
                current.status = "triggered".into();
                if exit_pending && !preempt {
                    continue;
                }
                if exit_qty > 0 || parent_exists {
                    exits.push((
                        p.instrument_id,
                        p.account_id,
                        p.position_side,
                        exit_qty,
                        cancellations,
                        p.spec.exit_price_tick,
                    ));
                }
            } else {
                current.status = "armed".into();
            }
        }
        exits
    }

    pub fn record_position_exit(
        &mut self,
        instrument: &str,
        account: AccountId,
        side: crate::PositionSide,
        order_id: u64,
    ) {
        let key = Self::protection_key(instrument, account, side);
        let qty = self
            .account_snapshot_for(instrument, account)
            .ok()
            .flatten()
            .and_then(|s| match s {
                AccountSnapshot::Perp(a) => crate::position_protection::position_qty(&a, side),
                _ => None,
            });
        if let Some(p) = self.position_protections.get_mut(&key) {
            p.last_exit_order_id = Some(order_id);
            if let (Some(previous), Some(remaining), Some(qty)) =
                (p.last_observed_qty, p.remaining_exit_qty, qty)
            {
                p.remaining_exit_qty =
                    Some(remaining.saturating_sub(previous.saturating_sub(qty.unsigned_abs())));
                p.last_observed_qty = Some(qty.unsigned_abs());
            }
            if qty == Some(0) {
                p.status = "completed".into();
            }
        }
    }
}
