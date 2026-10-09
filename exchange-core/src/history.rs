//! Append-only transaction history. Forks share sealed chunks, and an append
//! copies at most one small tail instead of every historical event. The wire
//! representation stays a plain array, including when reading old checkpoints.
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};
use std::{
    ops::Deref,
    sync::{Arc, OnceLock},
};

const CHUNK_SIZE: usize = 256;

#[derive(Clone, Debug)]
pub(crate) struct History<T> {
    chunks: Arc<Vec<Arc<Vec<T>>>>,
    tail: Arc<Vec<T>>,
    len: usize,
    contiguous: OnceLock<Arc<Vec<T>>>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            chunks: Arc::default(),
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
            Arc::make_mut(&mut self.chunks).push(std::mem::take(&mut self.tail));
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
    fn values(&self) -> impl Iterator<Item = &T> {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .chain(self.tail.iter())
    }
}

impl<T: Clone> Deref for History<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        if self.chunks.is_empty() {
            return &self.tail;
        }
        self.contiguous
            .get_or_init(|| Arc::new(self.values().cloned().collect()))
    }
}

impl<T: PartialEq> PartialEq for History<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.values().eq(other.values())
    }
}
impl<T: Eq> Eq for History<T> {}

impl<T: Serialize> Serialize for History<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.len))?;
        for value in self.values() {
            seq.serialize_element(value)?;
        }
        seq.end()
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
}
