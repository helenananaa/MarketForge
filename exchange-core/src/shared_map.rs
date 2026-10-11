//! Persistent transaction tables. Candidates share a root; writes copy only
//! the affected B-tree path, rather than detaching the entire table. Iteration
//! and serialization retain the ordinary key-ordered map contract.
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(transparent)]
#[serde(bound(deserialize = "K: Ord + Clone + Deserialize<'de>, V: Clone + Deserialize<'de>"))]
pub(crate) struct SharedMap<K: Ord + Clone, V: Clone>(im::OrdMap<K, V>);

impl<K: Ord + Clone, V: Clone> From<BTreeMap<K, V>> for SharedMap<K, V> {
    fn from(entries: BTreeMap<K, V>) -> Self {
        Self(entries.into_iter().collect())
    }
}

impl<K: Ord + Clone, V: Clone> Default for SharedMap<K, V> {
    fn default() -> Self {
        Self(im::OrdMap::new())
    }
}

impl<K: Ord + Clone, V: Clone> Clone for SharedMap<K, V> {
    fn clone(&self) -> Self {
        #[cfg(test)]
        if EAGER_CLONE.get() {
            return Self(self.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        }
        Self(self.0.clone())
    }
}

impl<K: Ord + Clone, V: Clone> Deref for SharedMap<K, V> {
    type Target = im::OrdMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
impl<K: Ord + Clone, V: Clone> SharedMap<K, V> {
    pub(crate) fn shares_storage(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl<K: Ord + Clone, V: Clone> DerefMut for SharedMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static EAGER_CLONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_candidate_write_does_not_copy_all_account_values() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        #[derive(Debug)]
        struct Counted(usize, Arc<AtomicUsize>);
        impl Clone for Counted {
            fn clone(&self) -> Self {
                self.1.fetch_add(1, Ordering::Relaxed);
                Self(self.0, self.1.clone())
            }
        }
        let copies = Arc::new(AtomicUsize::new(0));
        let mut original = SharedMap::default();
        for id in 0..16_384 {
            original.insert(id, Counted(id, copies.clone()));
        }
        copies.store(0, Ordering::Relaxed);
        let mut candidate = original.clone();
        assert_eq!(copies.load(Ordering::Relaxed), 0);
        candidate.get_mut(&8000).unwrap().0 = 42;
        assert_eq!(original[&8000].0, 8000);
        assert_eq!(candidate[&8000].0, 42);
        assert!(
            copies.load(Ordering::Relaxed) < original.len() / 16,
            "a single write must detach a bounded tree path, not the entire table"
        );
    }

    #[test]
    fn candidates_share_then_detach_and_keep_plain_map_wire_format() {
        let original: SharedMap<u64, String> =
            serde_json::from_str(r#"{"1":"first","2":"second"}"#).unwrap();
        let mut left = original.clone();
        let mut right = original.clone();
        assert!(original.shares_storage(&left));
        assert!(left.shares_storage(&right));
        left.get_mut(&1).unwrap().push_str(" changed");
        right.remove(&2);
        right.insert(3, "third".into());
        assert!(!original.shares_storage(&left));
        assert_eq!(original[&1], "first");
        assert_eq!(original[&2], "second");
        assert_eq!(left.len(), 2);
        assert_eq!(right[&1], "first");
        assert!(!right.contains_key(&2));
        let wire = serde_json::to_value(&original).unwrap();
        assert_eq!(wire, serde_json::json!({"1":"first","2":"second"}));
        let mut restored: SharedMap<u64, String> = serde_json::from_value(wire).unwrap();
        restored.clear();
        assert_eq!(original.len(), 2);
        EAGER_CLONE.set(true);
        let eager = original.clone();
        EAGER_CLONE.set(false);
        assert!(!original.shares_storage(&eager));
        assert_eq!(*original, *eager);
    }
}
