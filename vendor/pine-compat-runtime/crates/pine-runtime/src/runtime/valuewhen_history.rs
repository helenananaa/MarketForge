use std::collections::{VecDeque, vec_deque};

use super::append_history::{AppendHistory, Iter as HistoryIter};

const SMALL_EVENT_LIMIT: usize = 128;

/// Short event histories retain their flat queue. Longer histories share their
/// closed leaves across execution checkpoints and copy only the write path.
pub(crate) struct ValueWhenHistory<T = crate::PineValue> {
    storage: Storage<T>,
}

#[derive(Clone)]
enum Storage<T> {
    Small(VecDeque<T>),
    Persistent(AppendHistory<T>),
}

impl<T> Default for ValueWhenHistory<T> {
    fn default() -> Self {
        Self {
            storage: Storage::Small(VecDeque::new()),
        }
    }
}

impl<T: Clone> Clone for ValueWhenHistory<T> {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        match (&mut self.storage, &source.storage) {
            (Storage::Small(values), Storage::Small(other)) => values.clone_from(other),
            (Storage::Persistent(values), Storage::Persistent(other)) => values.clone_from(other),
            _ => self.storage = source.storage.clone(),
        }
    }
}

impl<T> ValueWhenHistory<T> {
    pub(crate) fn len(&self) -> usize {
        match &self.storage {
            Storage::Small(values) => values.len(),
            Storage::Persistent(values) => values.len(),
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        match &self.storage {
            Storage::Small(values) => values.capacity(),
            Storage::Persistent(values) => values.capacity(),
        }
    }

    /// Pine occurrence zero selects the newest retained event. The internal
    /// order is chronological so persistent histories only append at the tail.
    pub(crate) fn get(&self, occurrence: usize) -> Option<&T> {
        let index = self.len().checked_sub(occurrence.checked_add(1)?)?;
        match &self.storage {
            Storage::Small(values) => values.get(index),
            Storage::Persistent(values) => values.get(index),
        }
    }

    pub(crate) fn iter(&self) -> Iter<'_, T> {
        match &self.storage {
            Storage::Small(values) => Iter::Small(values.iter()),
            Storage::Persistent(values) => Iter::Persistent(values.iter()),
        }
    }
}

impl<T: Clone> ValueWhenHistory<T> {
    pub(crate) fn push_retained(&mut self, value: T, retain: usize) {
        match &mut self.storage {
            Storage::Small(values) => {
                values.push_back(value);
                let excess = values.len().saturating_sub(retain);
                if excess != 0 {
                    values.drain(..excess);
                }
                // Trim before promoting: a fixed occurrence with at most 128
                // retained events must keep the ordinary queue's small path.
                if values.len() > SMALL_EVENT_LIMIT {
                    self.storage =
                        Storage::Persistent(AppendHistory::from_values(std::mem::take(values)));
                }
            }
            Storage::Persistent(values) => {
                values.push(value);
                values.drop_prefix(values.len().saturating_sub(retain));
            }
        }
    }
}

pub(crate) enum Iter<'a, T> {
    Small(vec_deque::Iter<'a, T>),
    Persistent(HistoryIter<'a, T>),
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.next(),
            Self::Persistent(values) => values.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Small(values) => values.size_hint(),
            Self::Persistent(values) => values.size_hint(),
        }
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {}

#[cfg(test)]
#[path = "valuewhen_history_tests.rs"]
mod tests;
