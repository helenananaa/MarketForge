//! Append-only transaction history. Forks share sealed chunks, and an append
//! copies at most one small tail instead of every historical event. The wire
//! representation stays a plain array, including when reading old checkpoints.
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, OnceLock},
};

const CHUNK_SIZE: usize = 32;

#[derive(Debug)]
struct Chunk<T>(Vec<T>);

impl<T> Default for Chunk<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T: Clone> Clone for Chunk<T> {
    fn clone(&self) -> Self {
        if self.is_empty() {
            return Self::default();
        }
        // A detached tail will immediately be appended to. Preserve spare
        // capacity so that cloning does not force another allocation/copy.
        let mut values = Vec::with_capacity(CHUNK_SIZE);
        values.extend_from_slice(&self.0);
        Self(values)
    }
}

impl<T> Deref for Chunk<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Chunk<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct History<T> {
    chunks: im::Vector<Arc<Chunk<T>>>,
    tail: Arc<Chunk<T>>,
    len: usize,
    contiguous: OnceLock<Arc<Vec<T>>>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            chunks: im::Vector::new(),
            tail: Arc::default(),
            len: 0,
            contiguous: OnceLock::new(),
        }
    }
}

impl<T: Clone> History<T> {
    pub(crate) fn push(&mut self, value: T) {
        self.contiguous.take();
        if self.tail.len() == CHUNK_SIZE {
            self.chunks.push_back(std::mem::take(&mut self.tail));
        }
        Arc::make_mut(&mut self.tail).push(value);
        self.len += 1;
    }

    pub(crate) fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        for value in values {
            self.push(value);
        }
    }
}

impl<T> History<T> {
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .chain(self.tail.iter())
    }

    /// Read a suffix directly from its first chunk, without flattening or
    /// walking the preceding history. This is the transaction journal path.
    pub(crate) fn iter_from(&self, start: usize) -> impl DoubleEndedIterator<Item = &T> {
        let start = start.min(self.len);
        let sealed_len = self.len - self.tail.len();
        let first_chunk = (start / CHUNK_SIZE).min(self.chunks.len());
        let offset = start % CHUNK_SIZE;
        (first_chunk..self.chunks.len())
            .flat_map(move |index| {
                self.chunks[index][if index == first_chunk { offset } else { 0 }..].iter()
            })
            .chain(self.tail[start.saturating_sub(sealed_len)..].iter())
    }
}

impl<T: Clone> Deref for History<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        if self.chunks.is_empty() {
            return &self.tail;
        }
        self.contiguous
            .get_or_init(|| Arc::new(self.iter().cloned().collect()))
    }
}

impl<T: PartialEq> PartialEq for History<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}
impl<T: Eq> Eq for History<T> {}

impl<T: Serialize> Serialize for History<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.len))?;
        for value in self.iter() {
            seq.serialize_element(value)?;
        }
        seq.end()
    }
}

impl<T: Clone> From<Vec<T>> for History<T> {
    fn from(values: Vec<T>) -> Self {
        let mut history = Self::default();
        history.extend(values);
        history
    }
}

impl<'de, T: Deserialize<'de> + Clone> Deserialize<'de> for History<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<T>::deserialize(deserializer)?;
        let mut history = Self::default();
        history.extend(values);
        Ok(history)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appending_to_long_forks_copies_a_small_tail_and_keeps_old_exports() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Debug)]
        struct Counted(usize, Arc<AtomicUsize>);
        impl Clone for Counted {
            fn clone(&self) -> Self {
                self.1.fetch_add(1, Ordering::Relaxed);
                Self(self.0, self.1.clone())
            }
        }
        let copies = Arc::new(AtomicUsize::new(0));
        let mut history = History::default();
        history.extend((0..10_175).map(|i| Counted(i, copies.clone())));
        let exported = history.deref().as_ptr();
        copies.store(0, Ordering::Relaxed);
        let mut fork = history.clone();
        assert_eq!(copies.load(Ordering::Relaxed), 0);
        fork.push(Counted(20_000, copies.clone()));
        assert!(copies.load(Ordering::Relaxed) < CHUNK_SIZE);
        assert_eq!(history.deref().as_ptr(), exported);
        assert_eq!(history.last().unwrap().0, 10_174);
        assert_eq!(
            fork.iter_from(10_174).map(|v| v.0).collect::<Vec<_>>(),
            [10_174, 20_000]
        );
        // Sealing the shared full tail preserves earlier chunks; appending
        // another branch does not change either the old export or the fork.
        let checkpoint = fork.clone();
        fork.push(Counted(20_001, copies.clone()));
        assert!(Arc::ptr_eq(&fork.chunks[0], &history.chunks[0]));
        assert_eq!(checkpoint.len(), 10_176);
        assert_eq!(fork.len(), 10_177);
        assert_eq!(history.deref().as_ptr(), exported);
        assert_eq!(
            fork.iter_from(10_174)
                .rev()
                .map(|v| v.0)
                .collect::<Vec<_>>(),
            [20_001, 20_000, 10_174]
        );
    }

    #[test]
    fn append_forks_share_old_chunks_and_keep_array_compatibility() {
        let mut original = History::default();
        original.extend(0..1000u64);
        let mut fork = original.clone();
        // Materialized exports also remain isolated when either branch appends.
        assert_eq!(&*fork, &(0..1000).collect::<Vec<_>>());
        original.push(1000);
        fork.extend(2000..2300);
        assert!(Arc::ptr_eq(&original.chunks[0], &fork.chunks[0]));
        assert_eq!(original.last(), Some(&1000));
        assert_eq!(fork[1000], 2000);
        assert_eq!(fork.last(), Some(&2299));
        let value = serde_json::to_value(&fork).unwrap();
        assert!(value.is_array());
        let restored: History<u64> = serde_json::from_value(value).unwrap();
        assert_eq!(restored, fork);
        assert_eq!(&*restored, &*fork);
    }

    #[test]
    fn suffix_reads_cross_chunks_without_materializing_or_copying_old_entries() {
        let mut history = History::default();
        history.extend(0..900usize);
        for start in [0, 1, 255, 256, 257, 511, 512, 767, 768, 899, 900, 901] {
            assert_eq!(
                history.iter_from(start).copied().collect::<Vec<_>>(),
                (start.min(900)..900).collect::<Vec<_>>()
            );
            assert_eq!(
                history.iter_from(start).rev().copied().collect::<Vec<_>>(),
                (start.min(900)..900).rev().collect::<Vec<_>>()
            );
        }
        assert!(history.contiguous.get().is_none());
        let mut fork = history.clone();
        fork.push(900);
        assert!(history.chunks.ptr_eq(&fork.chunks));
        assert_eq!(history.len(), 900);
        assert_eq!(fork.iter_from(899).copied().collect::<Vec<_>>(), [899, 900]);
        let empty = History::<usize>::default();
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.iter_from(usize::MAX).next(), None);
    }
}
