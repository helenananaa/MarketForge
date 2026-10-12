use pine_ir::ScriptMode;

use super::historical::HistoricalRuntime;
use super::strategy_path::{
    HistoricalPath, MagnifierHostBar, MagnifierHostGap, magnifier_host_sequence,
};
use crate::strategy::{EntryPathTick, PathEventOutcome};
use crate::{Bar, RuntimeError};

/// Internal extra-pass cap per bar. Extra `calc_on_order_fills` passes stop
/// with a runtime error when this bound is exceeded.
pub(crate) const DEFAULT_MAX_RECALCULATION_PASSES: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StrategyExecutionIdentity {
    pub bar_index: usize,
    pub phase: StrategyBarPhase,
    pub fill_step: Option<HistoricalFillStep>,
    pub pass: u32,
}

impl Default for StrategyExecutionIdentity {
    fn default() -> Self {
        Self {
            bar_index: 0,
            phase: StrategyBarPhase::EligibleEntryFills,
            fill_step: None,
            pass: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrategyPathPhase {
    HostOpen,
    PathLeg,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StrategyPathCursor {
    pub host_bar_index: usize,
    pub path_phase: StrategyPathPhase,
    pub leg_index: u8,
    pub mark: f64,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StrategyPathTraceEntry {
    pub chart_bar_index: usize,
    pub host_bar_index: usize,
    pub path_phase: StrategyPathPhase,
    pub leg_index: u8,
    pub mark: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StrategySchedulerState {
    pub(crate) identity: StrategyExecutionIdentity,
    pub(crate) path_cursor: Option<StrategyPathCursor>,
    last_host_bar: Option<Bar>,
    last_realtime_observation: Option<Bar>,
    max_recalculation_passes: u32,
    script_passes: usize,
    recalculation_passes: usize,
    max_passes_on_bar: u32,
    current_bar_script_passes: u32,
}

impl Default for StrategySchedulerState {
    fn default() -> Self {
        Self::new()
    }
}

impl StrategySchedulerState {
    pub(crate) fn new() -> Self {
        Self::with_max_recalculation_passes(DEFAULT_MAX_RECALCULATION_PASSES)
    }

    pub(crate) fn with_max_recalculation_passes(max_recalculation_passes: u32) -> Self {
        Self {
            identity: StrategyExecutionIdentity::default(),
            path_cursor: None,
            last_host_bar: None,
            last_realtime_observation: None,
            max_recalculation_passes,
            script_passes: 0,
            recalculation_passes: 0,
            max_passes_on_bar: 0,
            current_bar_script_passes: 0,
        }
    }

    pub(crate) fn script_passes(&self) -> usize {
        self.script_passes
    }

    pub(crate) fn recalculation_passes(&self) -> usize {
        self.recalculation_passes
    }

    pub(crate) fn max_passes_on_bar(&self) -> u32 {
        self.max_passes_on_bar
    }

    pub(crate) fn max_recalculation_passes(&self) -> u32 {
        self.max_recalculation_passes
    }

    #[cfg(test)]
    pub(crate) fn set_max_recalculation_passes(&mut self, max_recalculation_passes: u32) {
        self.max_recalculation_passes = max_recalculation_passes;
    }

    pub(crate) fn begin_bar(&mut self, bar_index: usize) {
        self.identity.bar_index = bar_index;
        self.identity.phase = StrategyBarPhase::EligibleEntryFills;
        self.identity.fill_step = None;
        self.identity.pass = 0;
        self.path_cursor = None;
        self.current_bar_script_passes = 0;
    }

    #[cfg(test)]
    pub(crate) fn set_path_cursor(&mut self, leg_index: u8, mark: f64) {
        self.set_host_path_cursor(0, StrategyPathPhase::PathLeg, leg_index, mark);
    }

    pub(crate) fn set_host_path_cursor(
        &mut self,
        host_bar_index: usize,
        path_phase: StrategyPathPhase,
        leg_index: u8,
        mark: f64,
    ) {
        self.path_cursor = Some(StrategyPathCursor {
            host_bar_index,
            path_phase,
            leg_index,
            mark,
        });
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn host_bar_index(&self) -> usize {
        self.path_cursor.map_or(0, |cursor| cursor.host_bar_index)
    }

    pub(crate) fn clear_path_cursor(&mut self) {
        self.path_cursor = None;
    }

    pub(crate) fn set_phase(&mut self, phase: StrategyBarPhase) {
        self.identity.phase = phase;
        if !matches!(
            phase,
            StrategyBarPhase::EligibleEntryFills
                | StrategyBarPhase::BarCloseMarketFills
                | StrategyBarPhase::CurrentTickMarketFills
        ) {
            self.identity.fill_step = None;
        }
    }

    pub(crate) fn set_fill_step(&mut self, step: HistoricalFillStep) {
        self.identity.fill_step = Some(step);
        self.identity.phase = if matches!(
            step,
            HistoricalFillStep::SameBarMarketClosesAtClose
                | HistoricalFillStep::SameBarMarketEntriesAtClose
                | HistoricalFillStep::SameBarLimitEntriesAtClose
                | HistoricalFillStep::SameBarPriceExitsAtClose
        ) {
            StrategyBarPhase::BarCloseMarketFills
        } else {
            StrategyBarPhase::EligibleEntryFills
        };
    }

    pub(crate) fn begin_script_pass(&mut self) -> Result<(), RuntimeError> {
        if self.current_bar_script_passes == 0 {
            self.identity.pass = 0;
            self.identity.phase = StrategyBarPhase::ScriptStatements;
            self.identity.fill_step = None;
            self.current_bar_script_passes = 1;
            self.script_passes += 1;
            self.max_passes_on_bar = self.max_passes_on_bar.max(1);
            return Ok(());
        }

        let extra_pass = self.current_bar_script_passes;
        if extra_pass > self.max_recalculation_passes {
            return Err(RuntimeError {
                message: format!(
                    "strategy recalculation pass limit exceeded: bar {} {:?} {:?} pass {} (limit {} extra passes)",
                    self.identity.bar_index,
                    self.identity.phase,
                    self.identity.fill_step,
                    extra_pass,
                    self.max_recalculation_passes
                ),
            });
        }

        self.identity.pass = extra_pass;
        self.identity.phase = StrategyBarPhase::ScriptStatements;
        self.identity.fill_step = None;
        self.current_bar_script_passes += 1;
        self.script_passes += 1;
        self.recalculation_passes += 1;
        self.max_passes_on_bar = self.max_passes_on_bar.max(self.current_bar_script_passes);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrategyBarPhase {
    EligibleEntryFills,
    TradeExtremes,
    MarginCall,
    BuiltinRefresh,
    ScriptStatements,
    CurrentTickMarketFills,
    BarCloseMarketFills,
    ExitFills,
    Equity,
    OutputCommit,
}

/// Point-phase identity for market-open, bar-close, and the shared OHLC path
/// walk. Price-family variants remain for magnifier host-tick tests; they are
/// no longer the production price-path model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HistoricalFillStep {
    MarketClosesAtOpen,
    MarketEntriesAtOpen,
    IntrabarPath,
    #[allow(dead_code)]
    LimitLong,
    #[allow(dead_code)]
    StopLong,
    #[allow(dead_code)]
    StopLimitLong,
    #[allow(dead_code)]
    LimitShort,
    #[allow(dead_code)]
    StopShort,
    #[allow(dead_code)]
    StopLimitShort,
    SameBarMarketClosesAtClose,
    SameBarMarketEntriesAtClose,
    SameBarLimitEntriesAtClose,
    SameBarPriceExitsAtClose,
}

impl HistoricalFillStep {
    fn pre_script_path() -> &'static [Self] {
        &[Self::MarketClosesAtOpen, Self::MarketEntriesAtOpen]
    }

    fn bar_close_path() -> &'static [Self] {
        &[
            Self::SameBarMarketClosesAtClose,
            Self::SameBarMarketEntriesAtClose,
            Self::SameBarLimitEntriesAtClose,
            Self::SameBarPriceExitsAtClose,
        ]
    }

    pub(crate) fn ordering_key(self) -> (u8, u8) {
        (self.phase_rank(), self as u8)
    }

    fn phase_rank(self) -> u8 {
        match self {
            Self::MarketClosesAtOpen
            | Self::MarketEntriesAtOpen
            | Self::IntrabarPath
            | Self::LimitLong
            | Self::StopLong
            | Self::StopLimitLong
            | Self::LimitShort
            | Self::StopShort
            | Self::StopLimitShort => 0,
            Self::SameBarMarketClosesAtClose
            | Self::SameBarMarketEntriesAtClose
            | Self::SameBarLimitEntriesAtClose
            | Self::SameBarPriceExitsAtClose => 1,
        }
    }
}

impl HistoricalRuntime<'_> {
    pub(crate) fn advance_broker_only_forming(&mut self, bar: Bar) -> Result<(), RuntimeError> {
        debug_assert_eq!(self.program.script_mode, ScriptMode::Strategy);
        debug_assert!(!self.program.strategy_settings.calc_on_every_tick);
        debug_assert!(!self.program.strategy_settings.calc_on_order_fills);
        self.session_windows
            .validate_range(self.bars, self.bars + 1)
            .map_err(crate::SessionWindowInputError::runtime_error)?;
        self.strategy_scheduler.begin_bar(self.bars);
        self.run_realtime_broker_tick(self.bars, bar)?;
        self.strategy_broker.record_equity(self.bars, bar.close);
        Ok(())
    }

    fn run_realtime_broker_tick(&mut self, bar_index: usize, bar: Bar) -> Result<(), RuntimeError> {
        let timeframe_seconds =
            crate::builtins::time::timeframe_seconds(crate::DEFAULT_CHART_TIMEFRAME).unwrap_or(0);
        self.strategy_broker.reset_risk_windows(
            bar_index,
            bar.time,
            timeframe_seconds,
            self.strategy_broker.equity_value(bar.close),
            bar.close,
            self.session_windows.ids_for(bar_index),
        );
        let market_price = self
            .strategy_scheduler
            .last_realtime_observation
            .filter(|previous| previous.time == bar.time)
            .map_or(bar.close, |previous| {
                match (bar.high > previous.high, bar.low < previous.low) {
                    (true, false) => bar.high,
                    (false, true) => bar.low,
                    // No new extreme, or an unqualified two-sided expansion.
                    _ => bar.close,
                }
            });
        let filled = self.strategy_broker.process_realtime_tick(
            bar_index,
            bar.time,
            bar.close,
            market_price,
            self.program.strategy_settings.calc_on_order_fills,
        )?;
        self.strategy_scheduler.last_realtime_observation = Some(bar);
        self.strategy_scheduler.last_host_bar = Some(Bar {
            open: bar.close,
            high: bar.close,
            low: bar.close,
            ..bar
        });
        self.recalculate_after_fill(filled, bar.close)
    }

    pub(crate) fn run_pre_script_strategy_phases(
        &mut self,
        bar_index: usize,
        bar: Bar,
    ) -> Result<(), RuntimeError> {
        if self.program.script_mode != ScriptMode::Strategy {
            return Ok(());
        }
        if self.current_bar_update_kind != crate::BarUpdateKind::Historical {
            return self.run_realtime_broker_tick(bar_index, bar);
        }
        let sequence = magnifier_host_sequence(
            bar_index,
            bar,
            &self.magnifier_input,
            self.program.strategy_settings.use_bar_magnifier,
        );
        if let Some(warning) = sequence.warning {
            self.push_magnifier_diagnostic(warning);
        }
        let open_price = sequence
            .bars
            .first()
            .map(|host| host.bar.open)
            .unwrap_or(bar.open);
        let timeframe_seconds =
            crate::builtins::time::timeframe_seconds(crate::DEFAULT_CHART_TIMEFRAME).unwrap_or(0);
        let equity = self.strategy_broker.equity_value(open_price);
        self.strategy_broker.reset_risk_windows(
            bar_index,
            bar.time,
            timeframe_seconds,
            equity,
            open_price,
            self.session_windows.ids_for(bar_index),
        );
        self.trace_strategy_phase(StrategyBarPhase::EligibleEntryFills);
        let mut steps: Vec<_> = HistoricalFillStep::pre_script_path().to_vec();
        steps.sort_by_key(|step| step.ordering_key());
        let open_bar = Bar {
            open: open_price,
            ..bar
        };
        for step in steps {
            self.strategy_scheduler.set_fill_step(step);
            let filled = self.apply_historical_fill_step(step, bar_index, open_bar);
            if filled {
                self.strategy_broker
                    .flatten_if_risk_blocked(bar_index, bar.time, open_price);
            }
            self.recalculate_after_fill(filled, open_price)?;
            if step == HistoricalFillStep::MarketEntriesAtOpen
                && filled
                && self.program.strategy_settings.calc_on_order_fills
            {
                // An entry fill at the open can make a close condition true
                // during its recalculation. The broker processes that market
                // close at the current open before the OHLC path advances.
                let before = self.strategy_broker.public_fill_event_count();
                self.strategy_broker
                    .fill_same_bar_market_closes(bar_index, bar.time, open_price);
                self.recalculate_after_fill(
                    self.strategy_broker.public_fill_event_count() > before,
                    open_price,
                )?;
            }
        }
        self.walk_host_sequence(bar_index, bar.time, &sequence.bars)?;
        self.trace_strategy_phase(StrategyBarPhase::TradeExtremes);
        self.trace_strategy_phase(StrategyBarPhase::MarginCall);
        Ok(())
    }

    fn apply_historical_fill_step(
        &mut self,
        step: HistoricalFillStep,
        bar_index: usize,
        bar: Bar,
    ) -> bool {
        let before = self.strategy_broker.public_fill_event_count();
        match step {
            HistoricalFillStep::MarketClosesAtOpen => {
                self.strategy_broker
                    .fill_pending_market_closes(bar_index, bar.time, bar.open);
            }
            HistoricalFillStep::MarketEntriesAtOpen => {
                self.strategy_broker
                    .fill_pending_market_entries(bar_index, bar.time, bar.open);
                if self.program.strategy_settings.calc_on_order_fills {
                    // A market close filled at this open can trigger a script
                    // pass that places a replacement entry before the entry
                    // fill phase reaches the same open.
                    self.strategy_broker
                        .fill_same_bar_market_entries(bar_index, bar.time, bar.open);
                }
            }
            HistoricalFillStep::LimitLong => {
                self.strategy_broker
                    .fill_pending_limit_long_entries(bar_index, bar.time, bar.low);
            }
            HistoricalFillStep::StopLong => {
                self.strategy_broker
                    .fill_pending_stop_long_entries(bar_index, bar.time, bar.high);
            }
            HistoricalFillStep::StopLimitLong => {
                self.strategy_broker
                    .fill_pending_stop_limit_long_entries(bar_index, bar.time, bar.high, bar.low);
            }
            HistoricalFillStep::LimitShort => {
                self.strategy_broker
                    .fill_pending_limit_short_entries(bar_index, bar.time, bar.high);
            }
            HistoricalFillStep::StopShort => {
                self.strategy_broker
                    .fill_pending_stop_short_entries(bar_index, bar.time, bar.low);
            }
            HistoricalFillStep::StopLimitShort => {
                self.strategy_broker
                    .fill_pending_stop_limit_short_entries(bar_index, bar.time, bar.high, bar.low);
            }
            HistoricalFillStep::IntrabarPath => {}
            HistoricalFillStep::SameBarMarketClosesAtClose => {
                self.strategy_broker
                    .fill_same_bar_market_closes(bar_index, bar.time, bar.close);
            }
            HistoricalFillStep::SameBarMarketEntriesAtClose => {
                self.strategy_broker
                    .fill_same_bar_market_entries(bar_index, bar.time, bar.close);
            }
            HistoricalFillStep::SameBarLimitEntriesAtClose => {
                self.strategy_broker
                    .fill_same_bar_limit_entries(bar_index, bar.time, bar.close);
            }
            HistoricalFillStep::SameBarPriceExitsAtClose => {
                self.strategy_broker
                    .fill_same_bar_close_exits(bar_index, bar.time, bar.close);
            }
        }
        self.strategy_broker.public_fill_event_count() > before
    }

    fn walk_host_sequence(
        &mut self,
        chart_bar_index: usize,
        chart_time: i64,
        hosts: &[MagnifierHostBar],
    ) -> Result<(), RuntimeError> {
        // Every host open is a new order observation, including an open equal to
        // the previous close. Orders placed at that close can fill at the open.
        if let (Some(previous), Some(first)) =
            (self.strategy_scheduler.last_host_bar, hosts.first())
        {
            let gap = MagnifierHostGap {
                previous_close: previous.close,
                next_open: first.bar.open,
            };
            self.observe_host_open_gap(chart_bar_index, chart_time, first, gap)?;
        }
        for (index, host) in hosts.iter().enumerate() {
            if index > 0 {
                let gap = MagnifierHostGap {
                    previous_close: hosts[index - 1].bar.close,
                    next_open: host.bar.open,
                };
                self.observe_host_open_gap(chart_bar_index, chart_time, host, gap)?;
            }
            self.walk_one_host_bar(chart_bar_index, chart_time, host)?;
        }
        if let Some(last) = hosts.last() {
            self.strategy_scheduler.last_host_bar = Some(last.bar);
        }
        self.strategy_scheduler.clear_path_cursor();
        Ok(())
    }

    fn observe_host_open_gap(
        &mut self,
        chart_bar_index: usize,
        chart_time: i64,
        host: &MagnifierHostBar,
        gap: MagnifierHostGap,
    ) -> Result<(), RuntimeError> {
        self.strategy_scheduler.set_host_path_cursor(
            host.host_bar_index,
            StrategyPathPhase::HostOpen,
            0,
            gap.next_open,
        );
        self.record_path_trace(
            chart_bar_index,
            host.host_bar_index,
            StrategyPathPhase::HostOpen,
            0,
            gap.next_open,
        );
        self.observe_path_mark(chart_bar_index, chart_time, gap.next_open);
        let long_blocked = self.strategy_broker.same_side_long_entry_blocked();
        let short_blocked = self.strategy_broker.same_side_short_entry_blocked();
        let tick = EntryPathTick {
            bar_index: chart_bar_index,
            time: chart_time,
            leg: crate::runtime::strategy_path::PathLeg::point(gap.next_open),
            path_kind: crate::runtime::strategy_path::HistoricalPathKind::OpenHighLowClose,
            mark: gap.next_open,
            long_blocked_at_path_start: long_blocked,
            short_blocked_at_path_start: short_blocked,
        };
        let mut steps = 0_u32;
        loop {
            steps += 1;
            if steps > 10_000 {
                return Err(RuntimeError {
                    message: format!(
                        "strategy path event loop made no progress: bar {} host {} gap",
                        chart_bar_index, host.host_bar_index
                    ),
                });
            }
            let Some(outcome) = self.strategy_broker.take_next_gap_event(tick, gap) else {
                break;
            };
            if let PathEventOutcome::Filled { fill_price, .. } = outcome {
                self.strategy_broker.flatten_if_risk_blocked(
                    chart_bar_index,
                    chart_time,
                    fill_price,
                );
                self.recalculate_after_fill(true, gap.next_open)?;
            }
        }
        Ok(())
    }

    fn walk_one_host_bar(
        &mut self,
        chart_bar_index: usize,
        chart_time: i64,
        host: &MagnifierHostBar,
    ) -> Result<(), RuntimeError> {
        let bar = host.bar;
        let Some(path) = HistoricalPath::from_validated_bar(&bar) else {
            self.strategy_broker
                .update_open_trade_extremes(bar.high, bar.low);
            let adverse = if self.strategy_broker.position_size() > 0.0 {
                bar.low
            } else if self.strategy_broker.position_size() < 0.0 {
                bar.high
            } else {
                bar.close
            };
            self.strategy_broker
                .evaluate_risk_equity_stops(chart_bar_index, chart_time, adverse);
            self.strategy_broker
                .evaluate_margin_call_long(chart_bar_index, chart_time, bar.low);
            self.strategy_broker
                .evaluate_margin_call_short(chart_bar_index, chart_time, bar.high);
            self.strategy_broker
                .flatten_if_risk_blocked(chart_bar_index, chart_time, adverse);
            return Ok(());
        };
        self.strategy_scheduler
            .set_fill_step(HistoricalFillStep::IntrabarPath);
        let long_blocked = self.strategy_broker.same_side_long_entry_blocked();
        let short_blocked = self.strategy_broker.same_side_short_entry_blocked();
        for leg in path.legs() {
            let mut mark = leg.from.price;
            if self.program.strategy_settings.calc_on_order_fills && leg.index > 0 {
                // The v4 historical broker has a fill opportunity at the
                // high and low after the open. Orders created by a fill at
                // the low wait for the next bar's open; the close is the
                // regular script execution point, not another opportunity
                // for those market orders.
                let before = self.strategy_broker.public_fill_event_count();
                self.strategy_broker
                    .fill_same_bar_market_closes(chart_bar_index, chart_time, mark);
                self.strategy_broker.fill_same_bar_market_entries(
                    chart_bar_index,
                    chart_time,
                    mark,
                );
                let filled = self.strategy_broker.public_fill_event_count() > before;
                if filled {
                    self.strategy_broker
                        .flatten_if_risk_blocked(chart_bar_index, chart_time, mark);
                }
                self.recalculate_after_fill(filled, mark)?;
            }
            self.strategy_scheduler.set_host_path_cursor(
                host.host_bar_index,
                StrategyPathPhase::PathLeg,
                leg.index,
                mark,
            );
            self.record_path_trace(
                chart_bar_index,
                host.host_bar_index,
                StrategyPathPhase::PathLeg,
                leg.index,
                mark,
            );
            self.observe_path_mark(chart_bar_index, chart_time, mark);
            let mut steps = 0_u32;
            loop {
                steps += 1;
                if steps > 10_000 {
                    return Err(RuntimeError {
                        message: format!(
                            "strategy path event loop made no progress: bar {} host {} leg {}",
                            chart_bar_index, host.host_bar_index, leg.index
                        ),
                    });
                }
                let Some(outcome) =
                    self.strategy_broker
                        .take_next_entry_path_event(EntryPathTick {
                            bar_index: chart_bar_index,
                            time: chart_time,
                            leg,
                            path_kind: path.kind,
                            mark,
                            long_blocked_at_path_start: long_blocked,
                            short_blocked_at_path_start: short_blocked,
                        })
                else {
                    mark = leg.to.price;
                    self.strategy_scheduler.set_host_path_cursor(
                        host.host_bar_index,
                        StrategyPathPhase::PathLeg,
                        leg.index,
                        mark,
                    );
                    self.record_path_trace(
                        chart_bar_index,
                        host.host_bar_index,
                        StrategyPathPhase::PathLeg,
                        leg.index,
                        mark,
                    );
                    self.observe_path_mark(chart_bar_index, chart_time, mark);
                    break;
                };
                mark = outcome.mark();
                self.strategy_scheduler.set_host_path_cursor(
                    host.host_bar_index,
                    StrategyPathPhase::PathLeg,
                    leg.index,
                    mark,
                );
                self.record_path_trace(
                    chart_bar_index,
                    host.host_bar_index,
                    StrategyPathPhase::PathLeg,
                    leg.index,
                    mark,
                );
                self.observe_path_mark(chart_bar_index, chart_time, mark);
                if let PathEventOutcome::Filled { fill_price, .. } = outcome {
                    self.strategy_broker.flatten_if_risk_blocked(
                        chart_bar_index,
                        chart_time,
                        fill_price,
                    );
                    self.recalculate_after_fill(true, mark)?;
                }
            }
        }
        Ok(())
    }

    #[allow(unused_variables)]
    fn record_path_trace(
        &mut self,
        chart_bar_index: usize,
        host_bar_index: usize,
        path_phase: StrategyPathPhase,
        leg_index: u8,
        mark: f64,
    ) {
        #[cfg(test)]
        self.strategy_path_trace.push(StrategyPathTraceEntry {
            chart_bar_index,
            host_bar_index,
            path_phase,
            leg_index,
            mark,
        });
    }

    fn observe_path_mark(&mut self, bar_index: usize, time: i64, mark: f64) {
        self.strategy_broker.update_open_trade_extremes(mark, mark);
        self.strategy_broker
            .evaluate_risk_equity_stops(bar_index, time, mark);
    }

    pub(crate) fn fill_current_tick_market_closes(&mut self) {
        let Some(bar) = self.current_bar else {
            return;
        };
        if self.program.script_mode != ScriptMode::Strategy {
            return;
        }
        let before = self.strategy_broker.public_fill_event_count();
        let mark = self.strategy_mark_price().unwrap_or(bar.close);
        self.strategy_broker
            .fill_immediate_market_closes(self.bars, bar.time, mark);
        if self.strategy_broker.public_fill_event_count() > before {
            self.trace_strategy_phase(StrategyBarPhase::CurrentTickMarketFills);
        }
    }

    pub(crate) fn run_post_script_strategy_phases(
        &mut self,
        bar_index: usize,
        bar: Bar,
    ) -> Result<(), RuntimeError> {
        if self.program.script_mode != ScriptMode::Strategy {
            return Ok(());
        }
        if self.program.strategy_settings.process_orders_on_close
            && self.current_bar_update_kind != crate::BarUpdateKind::Forming
        {
            self.trace_strategy_phase(StrategyBarPhase::BarCloseMarketFills);
            let mut steps: Vec<_> = HistoricalFillStep::bar_close_path().to_vec();
            steps.sort_by_key(|step| step.ordering_key());
            for step in steps {
                self.strategy_scheduler.set_fill_step(step);
                let filled = self.apply_historical_fill_step(step, bar_index, bar);
                if filled {
                    self.strategy_broker
                        .flatten_if_risk_blocked(bar_index, bar.time, bar.close);
                }
                self.recalculate_after_fill(filled, bar.close)?;
            }
        }
        self.trace_strategy_phase(StrategyBarPhase::ExitFills);
        self.strategy_broker
            .flatten_if_risk_blocked(bar_index, bar.time, bar.close);
        self.strategy_broker
            .evaluate_risk_equity_stops(bar_index, bar.time, bar.close);
        self.trace_strategy_phase(StrategyBarPhase::Equity);
        self.strategy_broker.record_equity(bar_index, bar.close);
        Ok(())
    }

    pub(crate) fn trace_strategy_phase(&mut self, phase: StrategyBarPhase) {
        self.strategy_scheduler.set_phase(phase);
        #[cfg(test)]
        self.strategy_phase_trace.push(phase);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_bar_resets_pass_identity() {
        let mut scheduler = StrategySchedulerState::new();
        scheduler.begin_bar(3);
        scheduler.begin_script_pass().expect("initial pass");
        scheduler.set_path_cursor(1, 10.5);
        scheduler.begin_bar(4);
        assert_eq!(scheduler.identity.bar_index, 4);
        assert_eq!(scheduler.identity.pass, 0);
        assert_eq!(
            scheduler.identity.phase,
            StrategyBarPhase::EligibleEntryFills
        );
        assert_eq!(scheduler.identity.fill_step, None);
        assert_eq!(scheduler.path_cursor, None);
        assert_eq!(scheduler.script_passes(), 1);
    }

    #[test]
    fn fill_step_is_part_of_tick_identity() {
        let mut scheduler = StrategySchedulerState::new();
        scheduler.begin_bar(1);
        scheduler.set_fill_step(HistoricalFillStep::LimitLong);
        assert_eq!(
            scheduler.identity.fill_step,
            Some(HistoricalFillStep::LimitLong)
        );
        assert_eq!(
            scheduler.identity.phase,
            StrategyBarPhase::EligibleEntryFills
        );
        scheduler.set_fill_step(HistoricalFillStep::SameBarMarketClosesAtClose);
        assert_eq!(
            scheduler.identity.phase,
            StrategyBarPhase::BarCloseMarketFills
        );
        scheduler.set_phase(StrategyBarPhase::ScriptStatements);
        assert_eq!(scheduler.identity.fill_step, None);
    }

    #[test]
    fn default_historical_path_allows_one_script_pass_per_bar() {
        let mut scheduler = StrategySchedulerState::new();
        for bar_index in 0..4 {
            scheduler.begin_bar(bar_index);
            scheduler.begin_script_pass().expect("initial pass");
        }
        assert_eq!(scheduler.script_passes(), 4);
        assert_eq!(scheduler.recalculation_passes(), 0);
        assert_eq!(scheduler.max_passes_on_bar(), 1);
        assert_eq!(
            scheduler.max_recalculation_passes(),
            DEFAULT_MAX_RECALCULATION_PASSES
        );
        assert_eq!(scheduler.identity.bar_index, 3);
        assert_eq!(scheduler.identity.pass, 0);
    }

    #[test]
    fn bounded_self_triggering_order_loop_fails_closed() {
        let mut scheduler = StrategySchedulerState::with_max_recalculation_passes(3);
        scheduler.begin_bar(4);
        scheduler.begin_script_pass().expect("initial pass");
        for extra in 1..=3 {
            scheduler
                .begin_script_pass()
                .unwrap_or_else(|_| panic!("extra pass {extra} should stay under the limit"));
            assert_eq!(scheduler.identity.pass, extra);
            assert_eq!(scheduler.identity.phase, StrategyBarPhase::ScriptStatements);
        }
        let error = scheduler
            .begin_script_pass()
            .expect_err("self-triggering extra pass should hit the guardrail");
        assert!(
            error
                .message
                .contains("strategy recalculation pass limit exceeded"),
            "{}",
            error.message
        );
        assert!(error.message.contains("bar 4"), "{}", error.message);
        assert!(error.message.contains("pass 4"), "{}", error.message);
        assert!(
            error.message.contains("limit 3 extra passes"),
            "{}",
            error.message
        );
        assert_eq!(scheduler.script_passes(), 4);
        assert_eq!(scheduler.recalculation_passes(), 3);
        assert_eq!(scheduler.max_passes_on_bar(), 4);
        assert_eq!(scheduler.identity.pass, 3);
    }

    #[test]
    fn zero_recalculation_limit_rejects_any_extra_pass() {
        let mut scheduler = StrategySchedulerState::with_max_recalculation_passes(0);
        scheduler.begin_bar(0);
        scheduler.begin_script_pass().expect("initial pass");
        let error = scheduler
            .begin_script_pass()
            .expect_err("max 0 extra passes must reject the first recalculation");
        assert!(
            error
                .message
                .contains("strategy recalculation pass limit exceeded"),
            "{}",
            error.message
        );
        assert_eq!(scheduler.recalculation_passes(), 0);
        assert_eq!(scheduler.max_passes_on_bar(), 1);
    }

    #[test]
    fn max_recalculation_passes_is_configurable() {
        let mut scheduler = StrategySchedulerState::new();
        scheduler.set_max_recalculation_passes(1);
        scheduler.begin_bar(2);
        scheduler.begin_script_pass().expect("initial pass");
        scheduler.begin_script_pass().expect("one extra pass");
        scheduler
            .begin_script_pass()
            .expect_err("configured limit of 1 extra pass");
        assert_eq!(scheduler.max_recalculation_passes(), 1);
        assert_eq!(scheduler.recalculation_passes(), 1);
    }

    #[test]
    fn host_path_cursor_is_monotonic_across_lower_bars() {
        let mut scheduler = StrategySchedulerState::new();
        scheduler.begin_bar(1);
        scheduler.set_host_path_cursor(0, StrategyPathPhase::PathLeg, 1, 10.2);
        assert_eq!(scheduler.host_bar_index(), 0);
        assert_eq!(
            scheduler.path_cursor,
            Some(StrategyPathCursor {
                host_bar_index: 0,
                path_phase: StrategyPathPhase::PathLeg,
                leg_index: 1,
                mark: 10.2,
            })
        );
        scheduler.set_host_path_cursor(0, StrategyPathPhase::PathLeg, 2, 10.6);
        scheduler.set_host_path_cursor(1, StrategyPathPhase::HostOpen, 0, 11.0);
        assert_eq!(scheduler.host_bar_index(), 1);
        assert_eq!(
            scheduler.path_cursor.map(|cursor| cursor.path_phase),
            Some(StrategyPathPhase::HostOpen)
        );
        scheduler.set_host_path_cursor(2, StrategyPathPhase::PathLeg, 0, 10.6);
        assert_eq!(scheduler.host_bar_index(), 2);
        scheduler.begin_bar(2);
        assert_eq!(scheduler.path_cursor, None);
        assert_eq!(scheduler.host_bar_index(), 0);
    }
}
