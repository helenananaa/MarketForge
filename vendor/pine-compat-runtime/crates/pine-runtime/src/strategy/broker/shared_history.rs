use std::ops::Index;

use crate::runtime::append_history::{AppendHistory, Iter};

/// Immutable history is shared by broker checkpoints. An append or tail
/// replacement copies at most one bounded leaf after a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SharedHistory<T>(AppendHistory<T>);

impl<T> Default for SharedHistory<T> {
    fn default() -> Self {
        Self(AppendHistory::default())
    }
}

impl<T> SharedHistory<T> {
    pub(super) fn history(&self) -> &AppendHistory<T> {
        &self.0
    }

    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn get(&self, index: usize) -> Option<&T> {
        self.0.get(index)
    }

    #[cfg(test)]
    pub(super) fn first(&self) -> Option<&T> {
        self.0.get(0)
    }

    pub(super) fn last(&self) -> Option<&T> {
        self.0.last()
    }

    #[cfg(test)]
    pub(super) fn clear(&mut self) {
        self.0 = AppendHistory::default();
    }

    pub(super) fn iter(&self) -> Iter<'_, T> {
        self.0.iter()
    }

    pub(super) fn tail(&self, start: usize) -> Vec<T>
    where
        T: Clone,
    {
        self.0.tail(start)
    }

    pub(super) fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.0.to_vec()
    }
}

impl<T: Clone> SharedHistory<T> {
    pub(super) fn push(&mut self, value: T) {
        self.0.push(value);
    }

    pub(super) fn last_mut(&mut self) -> Option<&mut T> {
        self.0.last_mut()
    }
}

impl<T> Index<usize> for SharedHistory<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl<'a, T> IntoIterator for &'a SharedHistory<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn shared_checkpoints_detach_on_append_and_existing_record_mutation() {
        let mut history = SharedHistory::default();
        history.push(String::from("confirmed"));
        let checkpoint = history.clone();
        history.push(String::from("tentative"));
        assert_eq!(checkpoint.to_vec(), ["confirmed"]);
        assert_eq!(history.to_vec(), ["confirmed", "tentative"]);
        let mut replacement = checkpoint.clone();
        replacement
            .last_mut()
            .expect("confirmed row")
            .push_str(" changed");
        assert_eq!(checkpoint.to_vec(), ["confirmed"]);
        assert_eq!(replacement.to_vec(), ["confirmed changed"]);
    }

    #[test]
    fn checkpoint_append_copies_only_a_bounded_leaf() {
        #[derive(Debug)]
        struct Counted(Arc<AtomicUsize>);
        impl Clone for Counted {
            fn clone(&self) -> Self {
                self.0.fetch_add(1, Ordering::Relaxed);
                Self(self.0.clone())
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let mut history = SharedHistory::default();
        for _ in 0..10_001 {
            history.push(Counted(count.clone()));
        }
        let checkpoint = history.clone();
        count.store(0, Ordering::Relaxed);
        history.push(Counted(count.clone()));
        assert!(count.load(Ordering::Relaxed) <= 128);
        assert_eq!(checkpoint.len(), 10_001);
        assert_eq!(history.len(), 10_002);
    }
}
