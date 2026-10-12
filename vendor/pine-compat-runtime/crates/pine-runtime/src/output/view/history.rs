use crate::runtime::append_history::{AppendHistory, Iter};

/// A retained suffix of a public output history, without materializing a Vec.
pub struct HistoryView<'a, T> {
    source: HistorySource<'a, T>,
    skip: usize,
}
enum HistorySource<'a, T> {
    Persistent(&'a AppendHistory<T>),
    Slice(&'a [T]),
}
impl<T> Copy for HistoryView<'_, T> {}
impl<T> Clone for HistoryView<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for HistorySource<'_, T> {}
impl<T> Clone for HistorySource<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, T> HistoryView<'a, T> {
    pub(crate) fn persistent(history: &'a AppendHistory<T>, skip: usize) -> Self {
        Self {
            source: HistorySource::Persistent(history),
            skip: skip.min(history.len()),
        }
    }
    #[must_use]
    pub fn from_slice(values: &'a [T]) -> Self {
        Self {
            source: HistorySource::Slice(values),
            skip: 0,
        }
    }
    #[must_use]
    pub fn len(&self) -> usize {
        match self.source {
            HistorySource::Persistent(h) => h.len(),
            HistorySource::Slice(h) => h.len(),
        }
        .saturating_sub(self.skip)
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[must_use]
    pub fn iter(&self) -> HistoryViewIter<'a, T> {
        HistoryViewIter {
            source: match self.source {
                HistorySource::Persistent(history) => {
                    HistoryIterSource::Persistent(history.iter().skip(self.skip))
                }
                HistorySource::Slice(history) => {
                    HistoryIterSource::Slice(history[self.skip..].iter())
                }
            },
        }
    }
}
/// An exact-size borrowed walk of a history, including compact repeated values.
pub struct HistoryViewIter<'a, T> {
    source: HistoryIterSource<'a, T>,
}
enum HistoryIterSource<'a, T> {
    Persistent(std::iter::Skip<Iter<'a, T>>),
    Slice(std::slice::Iter<'a, T>),
}
impl<'a, T> Iterator for HistoryViewIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.source {
            HistoryIterSource::Persistent(h) => h.next(),
            HistoryIterSource::Slice(h) => h.next(),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.source {
            HistoryIterSource::Persistent(h) => h.size_hint(),
            HistoryIterSource::Slice(h) => h.size_hint(),
        }
    }
}
impl<T> ExactSizeIterator for HistoryViewIter<'_, T> {}
impl<'a, T> IntoIterator for HistoryView<'a, T> {
    type Item = &'a T;
    type IntoIter = HistoryViewIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a, T> IntoIterator for &HistoryView<'a, T> {
    type Item = &'a T;
    type IntoIter = HistoryViewIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
