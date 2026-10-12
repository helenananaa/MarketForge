//! Derived subscriptions for spot bar/account strategies. Any input outside
//! the exact append/diff proof retains the existing full event screening path.
use super::*;

#[derive(Default)]
struct MarketSubscriptions {
    all: BTreeSet<usize>,
    accounts: BTreeMap<AccountId, Vec<usize>>,
    periods: BTreeMap<u64, Vec<usize>>,
}

#[derive(Default)]
pub(super) struct WakeIndex {
    epoch: u64,
    roster: Option<Arc<()>>,
    previous: Option<Arc<RoomManager>>,
    markets: BTreeMap<String, MarketSubscriptions>,
    fallback: BTreeSet<usize>,
    dirty: BTreeSet<usize>,
    identities: BTreeMap<String, usize>,
    deadlines: BTreeSet<(u64, usize)>,
    deadline_by_index: BTreeMap<usize, u64>,
}

impl WakeIndex {
    pub(super) fn refresh(
        &mut self,
        rooms: &Arc<RoomManager>,
        room: &str,
        scheduler: &crate::room_runtime::DispatchState,
        epoch: u64,
    ) -> Result<(), String> {
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_EVENT_INDEX,
        );
        if self.epoch != epoch
            || self
                .roster
                .as_ref()
                .is_none_or(|r| !Arc::ptr_eq(r, &scheduler.roster))
        {
            *self = Self {
                epoch,
                roster: Some(scheduler.roster.clone()),
                ..Self::default()
            };
            let mut eligible_markets = BTreeMap::new();
            for (index, agent) in scheduler.agents.iter().enumerate() {
                self.identities
                    .insert(agent.template.participant_id().to_owned(), index);
                self.dirty.insert(index);
                let registry = &scheduler.registry;
                let qualified = (|| {
                    if registry.supports_native_batching(&agent.template)
                        || !registry.supports_instance_reuse(&agent.template)
                    {
                        return None;
                    }
                    let filter = registry.event_filter(&agent.template)?;
                    let history = registry.market_data_request(&agent.template).ok()??;
                    history.validate().ok()?;
                    if !registry
                        .related_instruments(&agent.template)
                        .ok()?
                        .is_empty()
                    {
                        return None;
                    }
                    let intervals = registry.additional_intervals(&agent.template).ok()?;
                    if intervals.contains(&0) || filter.interval_ms == Some(0) {
                        return None;
                    }
                    let config = agent.template.config();
                    let instrument = config.instrument_id.as_deref()?;
                    // Perp and unknown/restored structures can always fall back.
                    if !*eligible_markets
                        .entry(instrument.to_owned())
                        .or_insert_with(|| {
                            rooms
                                .spot_bot_changed_accounts(rooms, room, instrument)
                                .is_some()
                        })
                    {
                        return None;
                    }
                    Some((
                        instrument.to_owned(),
                        config.account_id,
                        filter,
                        history,
                        intervals,
                    ))
                })();
                let Some((instrument, account, filter, history, intervals)) = qualified else {
                    self.fallback.insert(index);
                    continue;
                };
                let group = self.markets.entry(instrument).or_default();
                group.all.insert(index);
                // Private changes also affect completeness/truncation, even if
                // a custom filter omits account signals.
                group.accounts.entry(account).or_default().push(index);
                if filter.closed_bars {
                    for period in std::iter::once(history.interval_ms).chain(intervals) {
                        group.periods.entry(period).or_default().push(index);
                    }
                } else {
                    // History-limit completeness changes at primary closes.
                    group
                        .periods
                        .entry(history.interval_ms)
                        .or_default()
                        .push(index);
                }
                if let Some(period) = filter.interval_ms {
                    group.periods.entry(period).or_default().push(index);
                }
            }
        }
        let time = rooms
            .clock(room)
            .map_err(|e| format!("{e:?}"))?
            .market_time_ms();
        if let Some(previous) = &self.previous {
            let old_time = previous
                .clock(room)
                .map_err(|e| format!("{e:?}"))?
                .market_time_ms();
            for (instrument, group) in &self.markets {
                match rooms.spot_bot_changed_accounts(previous, room, instrument) {
                    Some(accounts) => {
                        for account in accounts {
                            if let Some(indices) = group.accounts.get(&account) {
                                self.dirty.extend(indices);
                            }
                        }
                        for (period, indices) in &group.periods {
                            if old_time / period != time / period {
                                self.dirty.extend(indices);
                            }
                        }
                    }
                    None => self.dirty.extend(&group.all),
                }
            }
        }
        while self
            .deadlines
            .first()
            .is_some_and(|(deadline, _)| *deadline <= time)
        {
            let (_, index) = self.deadlines.pop_first().unwrap();
            self.deadline_by_index.remove(&index);
            self.dirty.insert(index);
        }
        self.previous = Some(rooms.clone());
        Ok(())
    }

    pub(super) fn candidates(&self) -> BTreeSet<usize> {
        self.dirty.union(&self.fallback).copied().collect()
    }

    pub(super) fn consumed(
        &mut self,
        index: usize,
        state: &exchange_core::PersistedAgentKindState,
    ) {
        self.dirty.remove(&index);
        if let Some(time) = self.deadline_by_index.remove(&index) {
            self.deadlines.remove(&(time, index));
        }
        if let exchange_core::PersistedAgentKindState::Plugin { data, .. } = state
            && let Some(time) = data
                .get("_next_wake_ms")
                .and_then(serde_json::Value::as_u64)
        {
            self.deadlines.insert((time, index));
            self.deadline_by_index.insert(index, time);
        }
    }

    pub(super) fn committed(&mut self, id: &str) {
        if let Some(index) = self.identities.get(id) {
            self.dirty.insert(*index);
        }
    }
}
