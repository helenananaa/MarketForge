use pine_ir::HirProgram;

mod resource_limits;
mod time_protocol;
mod without_output;

use super::streaming::OutputCursor;
use crate::*;

pub struct RealtimeRuntime<'a> {
    confirmed: HistoricalRuntime<'a>,
    forming: Option<HistoricalRuntime<'a>>,
    revision: u64,
    cursor: OutputCursor,
    last_changes: Option<RuntimeChanges>,
    output_retention: OutputRetention,
    live_chart: Option<(Bar, RealtimeUpdateContext)>,
    chart_bars: Vec<Bar>,
    chart_execution_times: Option<Vec<i64>>,
}
impl<'a> RealtimeRuntime<'a> {
    #[must_use]
    pub fn new(program: &'a HirProgram) -> Self {
        Self::with_request_environment(program, RequestEnvironment::default())
    }

    #[must_use]
    pub fn with_request_environment(
        program: &'a HirProgram,
        request_environment: RequestEnvironment,
    ) -> Self {
        Self {
            confirmed: HistoricalRuntime::with_request_environment(program, request_environment),
            forming: None,
            revision: 0,
            cursor: OutputCursor::default(),
            last_changes: None,
            output_retention: OutputRetention::unlimited(),
            live_chart: None,
            chart_bars: Vec::new(),
            chart_execution_times: None,
        }
    }

    #[must_use]
    pub fn with_request_environment_and_input_overrides(
        program: &'a HirProgram,
        request_environment: RequestEnvironment,
        input_overrides: InputOverrides,
    ) -> Self {
        Self {
            confirmed: HistoricalRuntime::with_request_environment_and_input_overrides(
                program,
                request_environment,
                input_overrides,
            ),
            forming: None,
            revision: 0,
            cursor: OutputCursor::default(),
            last_changes: None,
            output_retention: OutputRetention::unlimited(),
            live_chart: None,
            chart_bars: Vec::new(),
            chart_execution_times: None,
        }
    }

    #[must_use]
    pub fn request_environment(&self) -> &RequestEnvironment {
        self.forming
            .as_ref()
            .unwrap_or(&self.confirmed)
            .request_environment()
    }

    #[must_use]
    pub fn with_execution_limits(mut self, limits: ExecutionLimits) -> Self {
        self.confirmed.execution_limits = limits;
        if let Some(forming) = &mut self.forming {
            forming.execution_limits = limits;
        }
        self
    }

    #[must_use]
    pub fn execution_limits(&self) -> ExecutionLimits {
        self.confirmed.execution_limits()
    }

    /// Apply one logical event limit to confirmed and forming states. Each
    /// execution-state tree has its own allowance; rollback copies are excluded.
    /// Rejecting a limit leaves both configuration and Pine state unchanged.
    pub fn set_valuewhen_limits(&mut self, limits: ValueWhenLimits) -> Result<(), RuntimeError> {
        self.confirmed.validate_valuewhen_limits(limits)?;
        if let Some(forming) = &self.forming {
            forming.validate_valuewhen_limits(limits)?;
        }
        self.confirmed.set_valuewhen_limits(limits)?;
        if let Some(forming) = &mut self.forming {
            forming.set_valuewhen_limits(limits)?;
        }
        Ok(())
    }

    pub fn with_valuewhen_limits(mut self, limits: ValueWhenLimits) -> Result<Self, RuntimeError> {
        self.set_valuewhen_limits(limits)?;
        Ok(self)
    }

    #[must_use]
    pub fn valuewhen_limits(&self) -> ValueWhenLimits {
        self.confirmed.valuewhen_limits()
    }

    /// Retained logical events in the currently visible execution state.
    #[must_use]
    pub fn valuewhen_retained_values(&self) -> usize {
        self.live().valuewhen_retained_values()
    }

    #[must_use]
    pub fn confirmed_valuewhen_retained_values(&self) -> usize {
        self.confirmed.valuewhen_retained_values()
    }

    #[must_use]
    pub fn with_magnifier_input(mut self, input: crate::MagnifierInput) -> Self {
        self.confirmed = self.confirmed.with_magnifier_input(input);
        self
    }

    #[must_use]
    pub fn magnifier_input(&self) -> &crate::MagnifierInput {
        self.confirmed.magnifier_input()
    }

    pub fn with_session_windows(
        mut self,
        input: crate::SessionWindowInput,
    ) -> Result<Self, RuntimeError> {
        let current = self.forming.as_ref().unwrap_or(&self.confirmed);
        current
            .session_windows
            .validate_replacement(&input, current.bars)
            .map_err(crate::SessionWindowInputError::runtime_error)?;
        if let Some(forming) = &mut self.forming {
            forming.session_windows = input.clone();
        }
        self.confirmed.session_windows = input;
        Ok(self)
    }

    pub fn extend_session_windows(
        &mut self,
        input: crate::SessionWindowInput,
    ) -> Result<(), RuntimeError> {
        let current = self.forming.as_ref().unwrap_or(&self.confirmed);
        current
            .session_windows
            .validate_extension(&input, current.bars)
            .map_err(crate::SessionWindowInputError::runtime_error)?;
        if let Some(forming) = &mut self.forming {
            forming.session_windows.extend_validated(input.clone());
        }
        self.confirmed.session_windows.extend_validated(input);
        Ok(())
    }

    /// Validate the complete historical range before streaming bar-zero input.
    pub fn prepare_magnifier_chart_bar_count(
        &mut self,
        chart_bar_count: usize,
    ) -> Result<(), RuntimeError> {
        self.confirmed
            .prepare_magnifier_chart_bar_count(chart_bar_count)
    }

    pub fn update(&mut self, update: BarUpdate) -> Result<RuntimeResult, RuntimeError> {
        self.update_and_snapshot(update, RealtimeUpdateContext::default())
    }

    pub fn update_with_execution_time(
        &mut self,
        update: BarUpdate,
        execution_time: i64,
    ) -> Result<RuntimeResult, RuntimeError> {
        self.update_and_snapshot(
            update,
            RealtimeUpdateContext {
                execution_time: Some(execution_time),
                opening_update: None,
            },
        )
    }

    pub fn update_with_context(
        &mut self,
        update: BarUpdate,
        context: RealtimeUpdateContext,
    ) -> Result<RuntimeResult, RuntimeError> {
        self.update_and_snapshot(update, context)
    }

    /// Capture a result and its cursor in one call. Hosts bind the replica to
    /// this stream; create a new replica when replacing the producer session.
    #[must_use]
    pub fn replica(&self) -> RuntimeReplica {
        RuntimeReplica::with_retained_from(self.result(), self.revision, self.display_origin())
    }

    #[must_use]
    pub fn with_output_retention(mut self, retention: OutputRetention) -> Self {
        self.output_retention = retention;
        self
    }

    pub fn set_output_retention(&mut self, retention: OutputRetention) {
        self.output_retention = retention;
        self.apply_output_retention();
        self.sync_cursor();
    }

    #[must_use]
    pub fn output_retention(&self) -> OutputRetention {
        self.output_retention
    }

    #[must_use]
    pub fn display_origin(&self) -> usize {
        self.confirmed
            .display_origin
            .max(self.output_retention.origin(self.confirmed.bars))
    }

    fn apply_output_retention(&mut self) {
        let origin = self.display_origin();
        self.confirmed.apply_display_origin(origin);
        if let Some(forming) = &mut self.forming {
            forming.apply_display_origin(origin);
        }
    }

    pub fn apply_request_update(
        &mut self,
        key: RequestKey,
        update: BarUpdate,
    ) -> Result<Option<RuntimeChanges>, RuntimeError> {
        self.apply_request_update_ref(key, update)
            .map(|changes| changes.cloned())
    }

    /// Apply a provider update and borrow the resulting cached delta, if the
    /// live chart was recalculated. A feed-only update preserves `last_changes`.
    pub fn apply_request_update_ref(
        &mut self,
        key: RequestKey,
        update: BarUpdate,
    ) -> Result<Option<&RuntimeChanges>, RuntimeError> {
        let confirmed_feed = self.confirmed.request_feed.clone();
        let confirmed_cache = self.confirmed.request_cache.clone();
        let forming = self.forming.clone();
        let cursor = self.cursor.clone();
        let revision = self.revision;
        let live_chart = self.live_chart;
        let result = self.apply_request_update_inner(key, update);
        if result.is_err() {
            // Cloning a rollback checkpoint deliberately omits selection
            // scratch. Keep the live workspace when restoring Pine state.
            let scratch = self
                .forming
                .as_mut()
                .map(|runtime| std::mem::take(&mut runtime.selection_scratch));
            self.confirmed.request_feed = confirmed_feed;
            self.confirmed.request_cache = confirmed_cache;
            self.forming = forming;
            if let (Some(forming), Some(scratch)) = (&mut self.forming, scratch) {
                forming.selection_scratch = scratch;
            }
            self.cursor = cursor;
            self.revision = revision;
            self.live_chart = live_chart;
        }
        // The inner transaction commits last_changes only after all fallible
        // work. Errors leave its owned payload untouched, without a rollback copy.
        result.map(|recalculated| {
            if recalculated {
                self.last_changes.as_ref()
            } else {
                None
            }
        })
    }

    fn apply_request_update_inner(
        &mut self,
        key: RequestKey,
        update: BarUpdate,
    ) -> Result<bool, RuntimeError> {
        self.confirmed.apply_request_update(key.clone(), update)?;
        if let Some(forming) = &mut self.forming {
            forming.apply_request_update(key, update)?;
        }
        if self.forming.is_none() {
            return Ok(false);
        }
        let Some((bar, mut context)) = self.live_chart else {
            return Ok(false);
        };
        // Provider updates re-execute the existing forming bar; they are not a
        // second opening observation. Keep its latest execution timestamp.
        context.opening_update = Some(false);
        self.update_inner(BarUpdate::forming(bar), context)?;
        self.revision += 1;
        self.apply_output_retention();
        let mut changes =
            self.cursor
                .diff(self.live(), self.revision, StreamingVisibility::Preview);
        changes.retained_from = self.display_origin();
        self.sync_cursor();
        self.last_changes = Some(changes);
        Ok(true)
    }

    pub fn apply_update(&mut self, update: BarUpdate) -> Result<RuntimeChanges, RuntimeError> {
        self.apply_update_with_context(update, RealtimeUpdateContext::default())
    }

    pub fn apply_update_with_execution_time(
        &mut self,
        update: BarUpdate,
        execution_time: i64,
    ) -> Result<RuntimeChanges, RuntimeError> {
        self.apply_update_with_context(
            update,
            RealtimeUpdateContext {
                execution_time: Some(execution_time),
                opening_update: None,
            },
        )
    }

    pub fn apply_update_with_context(
        &mut self,
        update: BarUpdate,
        context: RealtimeUpdateContext,
    ) -> Result<RuntimeChanges, RuntimeError> {
        self.apply_update_with_context_ref(update, context).cloned()
    }

    /// Apply an update and borrow its delta from the runtime cache. The borrow
    /// ends before the next mutable operation on this runtime; clone explicitly
    /// when the caller needs to retain an independent owned delta.
    pub fn apply_update_ref(&mut self, update: BarUpdate) -> Result<&RuntimeChanges, RuntimeError> {
        self.apply_update_with_context_ref(update, RealtimeUpdateContext::default())
    }

    pub fn apply_update_with_execution_time_ref(
        &mut self,
        update: BarUpdate,
        execution_time: i64,
    ) -> Result<&RuntimeChanges, RuntimeError> {
        self.apply_update_with_context_ref(
            update,
            RealtimeUpdateContext {
                execution_time: Some(execution_time),
                opening_update: None,
            },
        )
    }

    pub fn apply_update_with_context_ref(
        &mut self,
        update: BarUpdate,
        context: RealtimeUpdateContext,
    ) -> Result<&RuntimeChanges, RuntimeError> {
        let kind = update.kind;
        self.update_inner(update, context)?;
        self.revision += 1;
        let visibility = if kind == BarUpdateKind::Forming {
            StreamingVisibility::Preview
        } else {
            StreamingVisibility::Confirmed
        };
        self.apply_output_retention();
        let mut changes = self.cursor.diff(self.live(), self.revision, visibility);
        changes.retained_from = self.display_origin();
        self.sync_cursor();
        self.last_changes = Some(changes);
        Ok(self.last_changes.as_ref().expect("delta committed"))
    }

    fn update_and_snapshot(
        &mut self,
        update: BarUpdate,
        context: RealtimeUpdateContext,
    ) -> Result<RuntimeResult, RuntimeError> {
        self.update_with_context_without_output(update, context)?;
        Ok(self.result())
    }

    fn live(&self) -> &HistoricalRuntime<'a> {
        self.forming.as_ref().unwrap_or(&self.confirmed)
    }

    fn sync_cursor(&mut self) {
        self.cursor = OutputCursor::capture(self.live());
    }

    fn update_inner(
        &mut self,
        update: BarUpdate,
        context: RealtimeUpdateContext,
    ) -> Result<(), RuntimeError> {
        self.validate_chart_update_time(update)?;
        if update.kind == BarUpdateKind::Historical && context.opening_update == Some(false) {
            return Err(RuntimeError {
                message: "historical bars always have an opening update".to_owned(),
            });
        }
        if update.kind != BarUpdateKind::Historical
            && self.forming.is_some()
            && context.opening_update == Some(true)
        {
            return Err(RuntimeError {
                message: "opening_update cannot repeat for an already forming bar".to_owned(),
            });
        }
        let execution_time = context.execution_time;
        if matches!(
            update.kind,
            BarUpdateKind::Forming | BarUpdateKind::Confirmed
        ) && self
            .confirmed
            .magnifier_input()
            .bars_for_chart_bar(self.confirmed.bars)
            .is_some()
        {
            return Err(MagnifierInputError::FormingBar {
                chart_bar_index: self.confirmed.bars,
            }
            .runtime_error());
        }
        match update.kind {
            BarUpdateKind::Historical => {
                self.validate_confirmed_clock(execution_time)?;
                let mut runtime = self.confirmed.clone();
                runtime.append_bar_with_context(update.bar, update.kind, true, execution_time)?;
                self.confirmed = runtime;
                self.forming = None;
                self.live_chart = None;
                self.push_confirmed_bar(update.bar, execution_time);
                Ok(())
            }
            BarUpdateKind::Confirmed => {
                self.validate_confirmed_clock(execution_time)?;
                let runtime = self.replay_from_confirmed(update, context)?;
                self.confirmed = runtime;
                self.forming = None;
                self.live_chart = None;
                self.push_confirmed_bar(update.bar, execution_time);
                Ok(())
            }
            BarUpdateKind::Forming => {
                let runtime = self.replay_from_confirmed(update, context)?;
                self.forming = Some(runtime);
                self.live_chart = Some((update.bar, context));
                Ok(())
            }
        }
    }

    fn replay_from_confirmed(
        &mut self,
        update: BarUpdate,
        context: RealtimeUpdateContext,
    ) -> Result<HistoricalRuntime<'a>, RuntimeError> {
        if update.kind == BarUpdateKind::Forming
            && self.confirmed.program.script_mode == pine_ir::ScriptMode::Strategy
            && !self.confirmed.program.strategy_settings.calc_on_every_tick
            && !self.confirmed.program.strategy_settings.calc_on_order_fills
        {
            // No script can execute on this observation. Clone its last visible
            // state once, advance the broker transactionally, and retain all
            // user state instead of restoring then cloning it again.
            let mut runtime = self.forming.as_ref().unwrap_or(&self.confirmed).clone();
            runtime.advance_broker_only_forming(update.bar)?;
            return Ok(runtime);
        }
        let is_new_bar = context.opening_update.unwrap_or(self.forming.is_none());
        // User state rolls back, except varip. Orders and fills belong to the
        // live broker and survive successful updates of the same open bar.
        let mut runtime = self.confirmed.clone();
        // Intrabar overlays can copy collection payloads. Start this update's
        // allowance before preparation and retain its charges through execution.
        runtime.reset_execution_budget();
        if let Some(previous_forming) = &self.forming {
            runtime.seed_intrabar_persistence_from(previous_forming)?;
            // Compiled patterns are pure caches, independent of Pine rollback.
            runtime
                .regex_cache
                .clone_from(&previous_forming.regex_cache);
            // Gaussian weights depend only on their checked parameter key.
            runtime
                .alma_weights
                .clone_from(&previous_forming.alma_weights);
            runtime
                .strategy_broker
                .clone_from(&previous_forming.strategy_broker);
            runtime
                .strategy_scheduler
                .clone_from(&previous_forming.strategy_scheduler);
        }
        if let Some(previous_forming) = &mut self.forming {
            // Selection scratch is not script state; reuse its allocation while
            // evaluating repeated replacements of the same forming bar.
            runtime.selection_scratch = std::mem::take(&mut previous_forming.selection_scratch);
        }
        let script_passes = runtime.strategy_scheduler.script_passes();
        if let Err(error) = runtime.append_bar_with_prepared_budget(
            update.bar,
            update.kind,
            is_new_bar,
            context.execution_time,
        ) {
            if let Some(previous_forming) = &mut self.forming {
                previous_forming.selection_scratch = std::mem::take(&mut runtime.selection_scratch);
            }
            return Err(error);
        }
        if update.kind == BarUpdateKind::Forming
            && runtime.program.script_mode == pine_ir::ScriptMode::Strategy
            && runtime.strategy_scheduler.script_passes() == script_passes
        {
            // A price observation without script execution cannot replace the
            // last visible plots/drawings or change user state. Only the broker
            // and its scheduling state advanced in the speculative runtime.
            let mut retained = self.forming.as_ref().unwrap_or(&self.confirmed).clone();
            retained.strategy_broker = runtime.strategy_broker;
            retained.strategy_scheduler = runtime.strategy_scheduler;
            return Ok(retained);
        }
        Ok(runtime)
    }

    fn validate_confirmed_clock(&self, execution_time: Option<i64>) -> Result<(), RuntimeError> {
        if self.chart_execution_times.is_some() && execution_time.is_none() {
            return Err(RuntimeError {
                message: "E_HISTORY_CLOCK: confirmed update missing execution timestamp".to_owned(),
            });
        }
        Ok(())
    }

    fn push_confirmed_bar(&mut self, bar: Bar, execution_time: Option<i64>) {
        self.chart_bars.push(bar);
        if let Some(times) = &mut self.chart_execution_times {
            times.push(execution_time.expect("confirmed clock validated"));
        }
    }

    /// Replace confirmed history from `from_time` forward. Bars with `time <
    /// from_time` are kept; `bars` is the corrected suffix. Forming state is
    /// discarded. Replicas must reset from the returned snapshot.
    pub fn correct_historical(
        &mut self,
        from_time: i64,
        bars: &[Bar],
    ) -> Result<RuntimeResult, RuntimeError> {
        self.correct_historical_inner(from_time, bars, None)?;
        Ok(self.confirmed.result())
    }

    pub fn correct_historical_with_execution_times(
        &mut self,
        from_time: i64,
        bars: &[Bar],
        execution_times: &[i64],
    ) -> Result<RuntimeResult, RuntimeError> {
        self.correct_historical_inner(from_time, bars, Some(execution_times))?;
        Ok(self.confirmed.result())
    }

    fn correct_historical_inner(
        &mut self,
        from_time: i64,
        bars: &[Bar],
        execution_times: Option<&[i64]>,
    ) -> Result<(), RuntimeError> {
        let (combined, combined_times) =
            self.corrected_history(from_time, bars, execution_times)?;
        match combined_times {
            Some(times) => self.replay_historical_inner(&combined, Some(&times)),
            None => self.replay_historical_inner(&combined, None),
        }
    }

    fn corrected_history(
        &self,
        from_time: i64,
        bars: &[Bar],
        execution_times: Option<&[i64]>,
    ) -> Result<(Vec<Bar>, Option<Vec<i64>>), RuntimeError> {
        if let Some(times) = execution_times
            && times.len() != bars.len()
        {
            return Err(RuntimeError {
                message: format!(
                    "execution timestamp count {} does not match bar count {}",
                    times.len(),
                    bars.len()
                ),
            });
        }
        if let Some(last) = self.chart_bars.last()
            && from_time > last.time
        {
            return Err(RuntimeError {
                message: format!(
                    "E_HISTORY_CORRECT: from_time `{from_time}` is after confirmed time `{}`",
                    last.time
                ),
            });
        }
        if let Some(first) = bars.first()
            && first.time < from_time
        {
            return Err(RuntimeError {
                message: format!(
                    "E_HISTORY_CORRECT: corrected bars start at `{}` before from_time `{from_time}`",
                    first.time
                ),
            });
        }
        let cut = self
            .chart_bars
            .iter()
            .position(|bar| bar.time >= from_time)
            .unwrap_or(self.chart_bars.len());
        if let (Some(prefix_last), Some(suffix_first)) = (
            self.chart_bars
                .get(cut.saturating_sub(1))
                .filter(|_| cut > 0),
            bars.first(),
        ) && suffix_first.time <= prefix_last.time
        {
            return Err(RuntimeError {
                message: format!(
                    "E_HISTORY_CORRECT: corrected bars must follow retained time `{}`",
                    prefix_last.time
                ),
            });
        }
        let combined_times = match (&self.chart_execution_times, execution_times) {
            (Some(stored), Some(suffix)) => {
                let mut times = stored[..cut].to_vec();
                times.extend_from_slice(suffix);
                Some(times)
            }
            (Some(stored), None) if bars.is_empty() => Some(stored[..cut].to_vec()),
            (None, None) => None,
            (None, Some(suffix)) if cut == 0 => Some(suffix.to_vec()),
            (None, Some(_)) => {
                return Err(RuntimeError {
                    message: "E_HISTORY_CLOCK: confirmed prefix was recorded without execution timestamps".to_owned(),
                });
            }
            (Some(_), None) => {
                return Err(RuntimeError {
                    message: "E_HISTORY_CLOCK: corrected suffix must include execution timestamps"
                        .to_owned(),
                });
            }
        };
        let mut combined = self.chart_bars[..cut].to_vec();
        combined.extend_from_slice(bars);
        Ok((combined, combined_times))
    }

    /// Replace confirmed history by re-executing `bars` from a blank runtime.
    /// Forming state is discarded. Request feed, inputs, magnifier and session
    /// windows are kept. Replicas cannot apply this as a delta; reset them from
    /// the returned snapshot and `revision`.
    pub fn replay_historical(&mut self, bars: &[Bar]) -> Result<RuntimeResult, RuntimeError> {
        self.replay_historical_inner(bars, None)?;
        Ok(self.confirmed.result())
    }

    pub fn replay_historical_with_execution_times(
        &mut self,
        bars: &[Bar],
        execution_times: &[i64],
    ) -> Result<RuntimeResult, RuntimeError> {
        self.replay_historical_inner(bars, Some(execution_times))?;
        Ok(self.confirmed.result())
    }

    fn replay_historical_inner(
        &mut self,
        bars: &[Bar],
        execution_times: Option<&[i64]>,
    ) -> Result<(), RuntimeError> {
        Self::validate_history_times(bars, None)?;
        if let Some(times) = execution_times
            && times.len() != bars.len()
        {
            return Err(RuntimeError {
                message: format!(
                    "execution timestamp count {} does not match bar count {}",
                    times.len(),
                    bars.len()
                ),
            });
        }
        // All fallible execution happens on the candidate. The old session stays
        // untouched until it is ready, so no second rollback copy is needed.
        let mut runtime = self.confirmed.blank_for_replay();
        let chart_close = bars.last().map(|bar| {
            runtime
                .request_environment
                .chart()
                .timeframe()
                .nominal_close(bar.time)
        });
        runtime.request_feed.trim_after(chart_close);
        match execution_times {
            Some(times) => runtime.append_bars_with_execution_times(bars, times)?,
            None => runtime.append_bars(bars)?,
        }
        self.confirmed = runtime;
        self.forming = None;
        self.live_chart = None;
        self.chart_bars = bars.to_vec();
        self.chart_execution_times = execution_times.map(Vec::from);
        self.revision += 1;
        self.apply_output_retention();
        self.sync_cursor();
        self.last_changes = None;
        Ok(())
    }

    pub fn seed_historical(&mut self, bars: &[Bar]) -> Result<RuntimeResult, RuntimeError> {
        self.seed_historical_without_output(bars)?;
        Ok(self.confirmed.result())
    }

    pub fn seed_historical_with_execution_times(
        &mut self,
        bars: &[Bar],
        execution_times: &[i64],
    ) -> Result<RuntimeResult, RuntimeError> {
        self.seed_historical_with_execution_times_without_output(bars, execution_times)?;
        Ok(self.confirmed.result())
    }

    /// Commit known historical bars without materializing an owned snapshot.
    /// Revision, rollback, retention and subsequent replicas match `seed_historical`.
    pub fn seed_historical_without_output(&mut self, bars: &[Bar]) -> Result<(), RuntimeError> {
        self.seed_historical_inner(bars, None)
    }

    pub fn seed_historical_with_execution_times_without_output(
        &mut self,
        bars: &[Bar],
        execution_times: &[i64],
    ) -> Result<(), RuntimeError> {
        self.seed_historical_inner(bars, Some(execution_times))
    }

    fn seed_historical_inner(
        &mut self,
        bars: &[Bar],
        execution_times: Option<&[i64]>,
    ) -> Result<(), RuntimeError> {
        self.validate_seed_clocks(bars.len(), execution_times)?;
        Self::validate_history_times(bars, self.last_confirmed_bar_time())?;
        let mut runtime = self.confirmed.clone();
        match execution_times {
            Some(times) => runtime.append_bars_with_execution_times(bars, times)?,
            None => runtime.append_bars(bars)?,
        }
        self.confirmed = runtime;
        self.forming = None;
        self.live_chart = None;
        let prior_len = self.chart_bars.len();
        self.chart_bars.extend_from_slice(bars);
        match (&mut self.chart_execution_times, execution_times) {
            (Some(stored), Some(times)) => stored.extend_from_slice(times),
            (None, Some(times)) if prior_len == 0 => {
                self.chart_execution_times = Some(times.to_vec());
            }
            _ => {}
        }
        self.revision += 1;
        self.apply_output_retention();
        self.sync_cursor();
        self.last_changes = None;
        Ok(())
    }

    fn validate_seed_clocks(
        &self,
        bar_count: usize,
        execution_times: Option<&[i64]>,
    ) -> Result<(), RuntimeError> {
        match (&self.chart_execution_times, execution_times) {
            (Some(_), None) => Err(RuntimeError {
                message: "E_HISTORY_CLOCK: historical seed missing execution timestamps".to_owned(),
            }),
            (None, Some(_)) if !self.chart_bars.is_empty() => Err(RuntimeError {
                message: "E_HISTORY_CLOCK: cannot add execution timestamps after history recorded without them".to_owned(),
            }),
            (Some(_), Some(times)) | (None, Some(times)) if times.len() != bar_count => {
                Err(RuntimeError {
                    message: format!(
                        "execution timestamp count {} does not match bar count {bar_count}",
                        times.len()
                    ),
                })
            }
            _ => Ok(()),
        }
    }

    #[must_use]
    pub fn confirmed_bar_count(&self) -> usize {
        self.chart_bars.len()
    }

    #[must_use]
    pub fn last_confirmed_bar_time(&self) -> Option<i64> {
        self.chart_bars.last().map(|bar| bar.time)
    }

    /// Borrow visible output without copying historical values.
    #[must_use]
    pub fn result_view(&self) -> RuntimeResultView<'_> {
        self.live().result_view()
    }

    /// Borrow confirmed output, excluding the forming bar.
    #[must_use]
    pub fn confirmed_result_view(&self) -> RuntimeResultView<'_> {
        self.confirmed.result_view()
    }

    #[must_use]
    pub fn result(&self) -> RuntimeResult {
        self.live().result()
    }

    #[must_use]
    pub fn confirmed_result(&self) -> RuntimeResult {
        self.confirmed.result()
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn last_changes(&self) -> Option<&RuntimeChanges> {
        self.last_changes.as_ref()
    }

    #[must_use]
    pub fn profile(&self) -> RuntimeProfile {
        self.forming.as_ref().unwrap_or(&self.confirmed).profile()
    }

    #[must_use]
    pub fn confirmed_profile(&self) -> RuntimeProfile {
        self.confirmed.profile()
    }
}

impl RealtimeRuntime<'static> {
    #[must_use]
    pub fn from_program(program: HirProgram) -> Self {
        Self::from_program_with_request_environment_and_input_overrides(
            program,
            RequestEnvironment::default(),
            InputOverrides::new(),
        )
    }

    #[must_use]
    pub fn from_program_with_request_environment_and_input_overrides(
        program: HirProgram,
        request_environment: RequestEnvironment,
        input_overrides: InputOverrides,
    ) -> Self {
        Self::from_prepared_with_request_environment_and_input_overrides(
            &PreparedProgram::new(program),
            request_environment,
            input_overrides,
        )
    }

    #[must_use]
    pub fn from_prepared(program: &PreparedProgram) -> Self {
        Self::from_prepared_with_request_environment_and_input_overrides(
            program,
            RequestEnvironment::default(),
            InputOverrides::new(),
        )
    }

    #[must_use]
    pub fn from_prepared_with_request_environment_and_input_overrides(
        program: &PreparedProgram,
        request_environment: RequestEnvironment,
        input_overrides: InputOverrides,
    ) -> Self {
        Self {
            confirmed:
                HistoricalRuntime::from_prepared_with_request_environment_and_input_overrides(
                    program,
                    request_environment,
                    input_overrides,
                ),
            forming: None,
            revision: 0,
            cursor: OutputCursor::default(),
            last_changes: None,
            output_retention: OutputRetention::unlimited(),
            live_chart: None,
            chart_bars: Vec::new(),
            chart_execution_times: None,
        }
    }
}
