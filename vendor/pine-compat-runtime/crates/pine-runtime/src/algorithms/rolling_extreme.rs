use pine_ir::SeriesId;

use super::rolling_window::WindowExtreme;
use crate::runtime::append_history::AppendHistory;

/// A monotone queue of committed history samples. The current bar is deliberately
/// excluded: a call can run repeatedly with different sources before that bar's
/// final source is committed. Checkpoints share bounded leaves and branch paths.
#[derive(Debug, Clone)]
pub(crate) struct RollingExtremeState {
    series_id: SeriesId,
    length: usize,
    mode: WindowExtreme,
    processed_until: usize,
    candidates: AppendHistory<(usize, f64)>,
}

impl RollingExtremeState {
    pub(crate) fn external_checkpoint(&self) -> serde_json::Value {
        use crate::runtime::external_checkpoint::float;
        serde_json::json!({"series": self.series_id.0, "length": self.length,
            "highest": self.mode == WindowExtreme::Highest, "processed": self.processed_until,
            "candidates": self.candidates.iter().map(|(i, v)| serde_json::json!([i, float(*v)])).collect::<Vec<_>>()})
    }

    pub(crate) fn restore_external_checkpoint(
        value: &serde_json::Value,
    ) -> Result<Self, crate::RuntimeError> {
        use crate::runtime::external_checkpoint::{
            array, boolean, read_float, u32_field, usize_field,
        };
        let length = usize_field(&value["length"])?;
        let processed_until = usize_field(&value["processed"])?;
        let mode = if boolean(&value["highest"])? {
            WindowExtreme::Highest
        } else {
            WindowExtreme::Lowest
        };
        let mut candidates = AppendHistory::default();
        for item in array(&value["candidates"])? {
            let index = usize_field(&item[0])?;
            let sample = read_float(&item[1])?;
            if !sample.is_finite()
                || index >= processed_until
                || candidates.last().is_some_and(|(i, _)| *i >= index)
            {
                return Err(crate::external::external_error(
                    "invalid extreme checkpoint candidate",
                ));
            }
            candidates.push((index, sample));
        }
        if length == 0 || candidates.len() > length {
            return Err(crate::external::external_error(
                "invalid extreme checkpoint length",
            ));
        }
        Ok(Self {
            series_id: SeriesId(u32_field(&value["series"])?),
            length,
            mode,
            processed_until,
            candidates,
        })
    }

    pub(crate) fn new(series_id: SeriesId, length: usize, mode: WindowExtreme) -> Self {
        Self {
            series_id,
            length,
            mode,
            processed_until: 0,
            candidates: AppendHistory::default(),
        }
    }

    /// Return the number of newest committed samples the caller must feed in
    /// chronological order. Changing a window or moving backwards rebuilds it
    /// from the actual retained history, including conditionally executed calls.
    pub(crate) fn prepare(
        &mut self,
        series_id: SeriesId,
        length: usize,
        mode: WindowExtreme,
        bar: usize,
        available_history: usize,
    ) -> usize {
        let count = available_history.min(length - 1).min(bar);
        let oldest = bar - count;
        if self.series_id != series_id
            || self.length != length
            || self.mode != mode
            || self.processed_until > bar
            || self.processed_until < oldest
        {
            *self = Self::new(series_id, length, mode);
            self.processed_until = oldest;
        }
        if self
            .candidates
            .get(0)
            .is_some_and(|&(index, _)| index < oldest)
        {
            let expired = self
                .candidates
                .partition_point(|&(index, _)| index < oldest);
            self.candidates.drop_prefix(expired);
        }
        let missing = bar - self.processed_until;
        self.processed_until = bar;
        missing
    }

    pub(crate) fn push(&mut self, bar: usize, value: Option<f64>) {
        let Some(value) = value.filter(|value| value.is_finite()) else {
            return;
        };
        // Equal candidates are removed so the newest tie determines bar offsets.
        // Search and prune a whole suffix once instead of copying a branch path
        // separately for every dominated candidate.
        if self
            .candidates
            .last()
            .is_some_and(|&(_, previous)| !self.strictly_better(previous, value))
        {
            let retained = if self
                .candidates
                .get(0)
                .is_some_and(|&(_, previous)| !self.strictly_better(previous, value))
            {
                0
            } else {
                self.candidates
                    .partition_point(|&(_, previous)| self.strictly_better(previous, value))
            };
            self.candidates.truncate(retained);
        }
        self.candidates.push((bar, value));
    }

    pub(crate) fn best(&self, current: Option<f64>, bar: usize) -> Option<(f64, usize)> {
        let current = current.filter(|value| value.is_finite());
        match (current, self.candidates.get(0).copied()) {
            (Some(value), Some((index, previous))) if self.strictly_better(previous, value) => {
                Some((previous, bar - index))
            }
            (Some(value), _) => Some((value, 0)),
            (None, Some((index, value))) => Some((value, bar - index)),
            (None, None) => None,
        }
    }

    pub(crate) fn retained_values(&self) -> usize {
        self.candidates.len()
    }

    pub(crate) fn retained_capacity(&self) -> usize {
        self.candidates.capacity()
    }

    fn strictly_better(&self, left: f64, right: f64) -> bool {
        match self.mode {
            WindowExtreme::Highest => left > right,
            WindowExtreme::Lowest => left < right,
        }
    }
}

#[cfg(test)]
#[path = "rolling_extreme_tests.rs"]
mod tests;
