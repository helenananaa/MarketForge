use crate::{Bar, BarUpdateKind, HistoricalRuntime, RuntimeError};

/// A borrowed, sequential execution of a known historical dataset.
///
/// Created by [`HistoricalRuntime::historical_dataset`]. Hosts may time or pause
/// individual steps without changing Pine's view of the dataset endpoint.
#[must_use = "the dataset is executed only when the iterator is advanced"]
pub struct HistoricalDataset<'runtime, 'bars, 'program> {
    pub(super) runtime: &'runtime mut HistoricalRuntime<'program>,
    pub(super) bars: &'bars [Bar],
    pub(super) execution_times: Option<&'bars [i64]>,
    pub(super) index: usize,
    pub(super) previous_historical_end: Option<usize>,
}

impl Iterator for HistoricalDataset<'_, '_, '_> {
    type Item = Result<(), RuntimeError>;

    fn next(&mut self) -> Option<Self::Item> {
        let bar = *self.bars.get(self.index)?;
        let result = self.runtime.append_bar_with_context(
            bar,
            BarUpdateKind::Historical,
            true,
            self.execution_times.map(|times| times[self.index]),
        );
        self.index += 1;
        if result.is_err() {
            self.index = self.bars.len();
        }
        Some(result)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // An execution error can end the iterator before the remaining bars.
        (0, Some(self.bars.len() - self.index))
    }
}

impl std::iter::FusedIterator for HistoricalDataset<'_, '_, '_> {}

impl Drop for HistoricalDataset<'_, '_, '_> {
    fn drop(&mut self) {
        self.runtime.historical_end = self.previous_historical_end;
    }
}
