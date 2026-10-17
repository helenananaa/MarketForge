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
    // Fresh on reconstruction, inherited only by an in-memory clone. This also
    // distinguishes reconstructed short histories with no sealed chunk yet.
    origin: Arc<()>,
    chunks: im::Vector<Arc<Chunk<T>>>,
    tail: Arc<Chunk<T>>,
    len: usize,
    contiguous: OnceLock<Arc<Vec<T>>>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            origin: Arc::new(()),
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
    /// Conservative append witness for immutable value histories. Sealed chunks
    /// can only be inherited by cloning an ancestor: no API can replace, splice,
    /// or reorder them. Matching the last old sealed chunk therefore pins its
    /// entire ancestry. Only the old (at most 32 element) tail needs comparison.
    /// A reconstructed history deliberately fails this witness even if equal.
    pub(crate) fn shares_append_prefix(&self, previous: &Self) -> bool
    where
        T: PartialEq,
    {
        if !Arc::ptr_eq(&self.origin, &previous.origin) || self.len < previous.len {
            return false;
        }
        if let Some(index) = previous.chunks.len().checked_sub(1)
            && !self
                .chunks
                .get(index)
                .is_some_and(|chunk| Arc::ptr_eq(chunk, &previous.chunks[index]))
        {
            return false;
        }
        let offset = previous.len - previous.tail.len();
        previous
            .tail
            .iter()
            .enumerate()
            .all(|(index, value)| self.at(offset + index) == Some(value))
    }

    /// Exact append proof: shared sealed chunks need no element traversal;
    /// detached/recovered chunks and the short old tail are compared fully.
    pub(crate) fn extends(&self, previous: &Self) -> bool
    where
        T: PartialEq,
    {
        if self.len < previous.len {
            return false;
        }
        for (index, chunk) in previous.chunks.iter().enumerate() {
            if !self
                .chunks
                .get(index)
                .is_some_and(|next| Arc::ptr_eq(chunk, next) || chunk.0 == next.0)
            {
                return false;
            }
        }
        let offset = previous.len - previous.tail.len();
        previous
            .tail
            .iter()
            .enumerate()
            .all(|(i, value)| self.at(offset + i) == Some(value))
    }

    /// Indexed immutable reads without flattening shared history chunks.
    pub(crate) fn at(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        let sealed = self.len - self.tail.len();
        if index < sealed {
            self.chunks.get(index / CHUNK_SIZE)?.get(index % CHUNK_SIZE)
        } else {
            self.tail.get(index - sealed)
        }
    }
    /// Slice-compatible partition point using indexed reads, so a search never
    /// flattens the history. The predicate must hold for a prefix only.
    pub(crate) fn partition_point(&self, mut pred: impl FnMut(&T) -> bool) -> usize {
        let (mut low, mut high) = (0, self.len);
        while low < high {
            let mid = low + (high - low) / 2;
            if self.at(mid).is_some_and(&mut pred) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }
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
    fn observation_append_witness_is_bounded_and_rejects_divergent_or_rebuilt_prefixes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Clone, Debug)]
        struct Counted(usize, Arc<AtomicUsize>);
        impl PartialEq for Counted {
            fn eq(&self, other: &Self) -> bool {
                self.1.fetch_add(1, Ordering::Relaxed);
                self.0 == other.0
            }
        }
        for length in [0, 1, 31, 32, 33, 63, 64, 65, 100_003] {
            let comparisons = Arc::new(AtomicUsize::new(0));
            let mut old = History::default();
            old.extend((0..length).map(|i| Counted(i, comparisons.clone())));
            let mut next = old.clone();
            next.extend((length..length + 75).map(|i| Counted(i, comparisons.clone())));
            assert!(next.shares_append_prefix(&old));
            assert!(comparisons.load(Ordering::Relaxed) <= CHUNK_SIZE);
            assert!(old.contiguous.get().is_none() && next.contiguous.get().is_none());
            if length > 32 {
                let rebuilt: History<_> = old.iter().cloned().collect::<Vec<_>>().into();
                assert!(!next.shares_append_prefix(&rebuilt));
            }
            if length > 0 {
                let mut divergent: History<_> = old
                    .iter()
                    .take(length - 1)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into();
                divergent.push(Counted(usize::MAX, comparisons.clone()));
                assert!(!next.shares_append_prefix(&divergent));
                assert!(!old.shares_append_prefix(&next));
            }
            let mut a = old.clone();
            let mut b = old.clone();
            a.push(Counted(7, comparisons.clone()));
            b.push(Counted(8, comparisons.clone()));
            assert!(!a.shares_append_prefix(&b));
        }
    }

    #[test]
    fn append_proof_checks_changed_prefixes_across_sealed_and_tail_boundaries() {
        for length in [0, 1, 31, 32, 33, 64, 65, 1000] {
            let mut previous = History::default();
            previous.extend(0..length);
            let mut next = previous.clone();
            next.extend(length..length + 70);
            assert!(next.extends(&previous));
            let mut values: Vec<_> = next.iter().copied().collect();
            let restored: History<usize> =
                serde_json::from_value(serde_json::json!(values)).unwrap();
            assert!(restored.extends(&previous));
            if length > 0 {
                values[0] = 99999;
                let corrected: History<usize> =
                    serde_json::from_value(serde_json::json!(values)).unwrap();
                assert!(!corrected.extends(&previous));
                assert!(!History::<usize>::default().extends(&previous));
            }
        }
    }
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
        for bound in [0, 1, 31, 32, 33, 450, 899, 900, 901] {
            let expected = (0..900usize)
                .collect::<Vec<_>>()
                .partition_point(|v| *v < bound);
            assert_eq!(history.partition_point(|v| *v < bound), expected);
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
