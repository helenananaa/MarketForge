use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use pine_ir::SeriesId;

use crate::PineValue;
use crate::runtime::append_history::{APPEND_LEAF_SIZE, AppendHistory};

/// Small windows use a ring; deeper histories share immutable tree leaves with
/// realtime checkpoints. Neither path shifts the surviving history on commit.
#[derive(Debug, Clone)]
pub(crate) enum SeriesBuffer {
    Small(VecDeque<PineValue>),
    Shared(AppendHistory<PineValue>),
}

impl PartialEq for SeriesBuffer {
    fn eq(&self, other: &Self) -> bool {
        // Preserve PineValue comparison, including non-reflexive NaN values
        // accepted by the public store, even when checkpoints share leaves.
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Default for SeriesBuffer {
    fn default() -> Self {
        Self::Small(VecDeque::new())
    }
}

impl SeriesBuffer {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Small(values) => values.len(),
            Self::Shared(values) => values.len(),
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        match self {
            Self::Small(values) => values.capacity(),
            Self::Shared(values) => values.capacity(),
        }
    }

    fn get(&self, index: usize) -> Option<&PineValue> {
        match self {
            Self::Small(values) => values.get(index),
            Self::Shared(values) => values.get(index),
        }
    }

    fn commit(&mut self, value: PineValue, max_depth: Option<usize>) {
        debug_assert_ne!(max_depth, Some(0), "zero-depth series have no buffer");
        match self {
            Self::Small(values) => {
                // Pop first so a full bounded ring does not grow to hold the
                // transient extra value, including after a checkpoint clone.
                if let Some(max_depth) = max_depth {
                    while values.len() >= max_depth {
                        values.pop_front();
                    }
                }
                values.push_back(value);
                if values.len() > APPEND_LEAF_SIZE {
                    *self = Self::Shared(AppendHistory::from_values(values.drain(..)));
                }
            }
            Self::Shared(values) => {
                values.push(value);
                if let Some(max_depth) = max_depth {
                    values.drop_prefix(values.len().saturating_sub(max_depth));
                    if max_depth <= APPEND_LEAF_SIZE {
                        *self = Self::Small(values.iter().cloned().collect());
                    }
                }
            }
        }
    }

    pub(crate) fn iter(&self) -> SeriesIter<'_> {
        match self {
            Self::Small(values) => SeriesIter::Small(values.iter()),
            Self::Shared(values) => SeriesIter::Shared(values.iter()),
        }
    }

    pub(crate) fn iter_rev(&self) -> SeriesRevIter<'_> {
        match self {
            Self::Small(values) => SeriesRevIter::Small(values.iter().rev()),
            Self::Shared(values) => SeriesRevIter::Shared(values.iter_rev()),
        }
    }
}

#[cfg(test)]
#[path = "series_tests.rs"]
mod tests;

pub(crate) enum SeriesIter<'a> {
    Small(std::collections::vec_deque::Iter<'a, PineValue>),
    Shared(crate::runtime::append_history::Iter<'a, PineValue>),
}

impl<'a> Iterator for SeriesIter<'a> {
    type Item = &'a PineValue;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.next(),
            Self::Shared(values) => values.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Small(values) => values.size_hint(),
            Self::Shared(values) => values.size_hint(),
        }
    }
}

impl ExactSizeIterator for SeriesIter<'_> {}

pub(crate) enum SeriesRevIter<'a> {
    Small(std::iter::Rev<std::collections::vec_deque::Iter<'a, PineValue>>),
    Shared(crate::runtime::append_history::RevIter<'a, PineValue>),
}

impl<'a> Iterator for SeriesRevIter<'a> {
    type Item = &'a PineValue;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.next(),
            Self::Shared(values) => values.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Small(values) => values.size_hint(),
            Self::Shared(values) => values.size_hint(),
        }
    }
}

impl ExactSizeIterator for SeriesRevIter<'_> {}

impl<'a> IntoIterator for &'a SeriesBuffer {
    type Item = &'a PineValue;
    type IntoIter = SeriesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Default, Clone)]
pub struct SeriesStore {
    current_bar: usize,
    pub(crate) buffers: HashMap<SeriesId, SeriesBuffer>,
    // Conservative: once a buffer held references, it remains a possible GC
    // root until explicitly removed. Scalar-only histories never need a walk.
    collection_root_series: Arc<HashSet<SeriesId>>,
}

impl PartialEq for SeriesStore {
    fn eq(&self, other: &Self) -> bool {
        self.current_bar == other.current_bar && self.buffers == other.buffers
    }
}

impl SeriesStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_current_bar(&mut self, current_bar: usize) {
        self.current_bar = current_bar;
    }

    #[must_use]
    pub fn current_bar(&self) -> usize {
        self.current_bar
    }

    pub fn commit(&mut self, series_id: SeriesId, value: PineValue, max_depth: Option<usize>) {
        if matches!(max_depth, Some(0)) {
            self.buffers.remove(&series_id);
            if self.collection_root_series.contains(&series_id) {
                Arc::make_mut(&mut self.collection_root_series).remove(&series_id);
            }
            return;
        }

        if value.can_reference_collection() && !self.collection_root_series.contains(&series_id) {
            Arc::make_mut(&mut self.collection_root_series).insert(series_id);
        }

        let buffer = self.buffers.entry(series_id).or_default();
        buffer.commit(value, max_depth);
    }

    pub(crate) fn collection_root_buffers(&self) -> impl Iterator<Item = &SeriesBuffer> {
        self.collection_root_series
            .iter()
            .filter_map(|id| self.buffers.get(id))
    }

    #[must_use]
    pub fn values_len(&self) -> usize {
        self.buffers.values().map(SeriesBuffer::len).sum()
    }

    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.buffers
            .values()
            .map(SeriesBuffer::len)
            .max()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn len(&self, series_id: SeriesId) -> usize {
        self.buffers
            .get(&series_id)
            .map(SeriesBuffer::len)
            .unwrap_or(0)
    }

    #[must_use]
    pub fn read(&self, series_id: SeriesId, offset: usize) -> PineValue {
        if offset == 0 {
            return PineValue::Na;
        }

        let Some(buffer) = self.buffers.get(&series_id) else {
            return PineValue::Na;
        };
        if offset > buffer.len() {
            return PineValue::Na;
        }

        buffer
            .get(buffer.len() - offset)
            .expect("valid history offset")
            .clone()
    }

    pub(crate) fn history_window(
        &self,
        series_id: SeriesId,
        length: usize,
    ) -> impl Iterator<Item = &PineValue> {
        self.buffers
            .get(&series_id)
            .into_iter()
            .flat_map(SeriesBuffer::iter_rev)
            .take(length)
    }
}
