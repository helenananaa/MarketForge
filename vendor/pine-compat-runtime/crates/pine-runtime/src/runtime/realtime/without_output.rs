use super::*;

impl RealtimeRuntime<'_> {
    /// Apply a snapshot-producing update without constructing the owned snapshot.
    /// The revision, retention, cursor and last-changes semantics match `update`.
    pub fn update_without_output(&mut self, update: BarUpdate) -> Result<(), RuntimeError> {
        self.update_with_context_without_output(update, RealtimeUpdateContext::default())
    }

    pub fn update_with_context_without_output(
        &mut self,
        update: BarUpdate,
        context: RealtimeUpdateContext,
    ) -> Result<(), RuntimeError> {
        self.update_inner(update, context)?;
        self.revision += 1;
        self.apply_output_retention();
        self.sync_cursor();
        self.last_changes = None;
        Ok(())
    }

    /// Replace history without materializing a result. Reset replicas from a
    /// subsequent owned snapshot or borrowed result view after success.
    pub fn replay_historical_without_output(&mut self, bars: &[Bar]) -> Result<(), RuntimeError> {
        self.replay_historical_inner(bars, None)
    }

    pub fn replay_historical_with_execution_times_without_output(
        &mut self,
        bars: &[Bar],
        execution_times: &[i64],
    ) -> Result<(), RuntimeError> {
        self.replay_historical_inner(bars, Some(execution_times))
    }

    /// Correct confirmed history without constructing a result. Forming state
    /// is discarded on success; failures preserve the previous session.
    pub fn correct_historical_without_output(
        &mut self,
        from_time: i64,
        bars: &[Bar],
    ) -> Result<(), RuntimeError> {
        self.correct_historical_inner(from_time, bars, None)
    }

    pub fn correct_historical_with_execution_times_without_output(
        &mut self,
        from_time: i64,
        bars: &[Bar],
        execution_times: &[i64],
    ) -> Result<(), RuntimeError> {
        self.correct_historical_inner(from_time, bars, Some(execution_times))
    }
}
