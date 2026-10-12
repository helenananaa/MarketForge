use super::*;

impl RealtimeRuntime<'_> {
    /// Timestamp of the currently forming chart bar, if one is visible.
    #[must_use]
    pub fn forming_bar_time(&self) -> Option<i64> {
        self.forming.as_ref()?;
        self.live_chart.map(|(bar, _)| bar.time)
    }

    // Validate before cloning or executing a candidate. Input errors leave the
    // confirmed state, intrabar persistence, cached delta and revision intact.
    pub(super) fn validate_chart_update_time(&self, update: BarUpdate) -> Result<(), RuntimeError> {
        if let Some(last_confirmed_time) = self.last_confirmed_bar_time()
            && update.bar.time <= last_confirmed_time
        {
            return Err(RuntimeError {
                message: format!(
                    "realtime bar time `{}` must be later than confirmed time `{last_confirmed_time}`",
                    update.bar.time
                ),
            });
        }
        // Historical discovery replaces any speculative bar. Forming and
        // confirmed observations must refer to the same already-open bar.
        if update.kind != BarUpdateKind::Historical
            && let Some(forming_time) = self.forming_bar_time()
            && update.bar.time != forming_time
        {
            let action = if update.kind == BarUpdateKind::Forming {
                "replace"
            } else {
                "confirm"
            };
            return Err(RuntimeError {
                message: format!(
                    "realtime {action} time `{}` does not match forming time `{forming_time}`",
                    update.bar.time
                ),
            });
        }
        Ok(())
    }

    pub(super) fn validate_history_times(
        bars: &[Bar],
        previous: Option<i64>,
    ) -> Result<(), RuntimeError> {
        let mut previous = previous;
        for bar in bars {
            if let Some(previous) = previous
                && bar.time <= previous
            {
                return Err(RuntimeError {
                    message: format!(
                        "historical bar times must be strictly increasing; time `{}` must be later than `{previous}`",
                        bar.time
                    ),
                });
            }
            previous = Some(bar.time);
        }
        Ok(())
    }
}
