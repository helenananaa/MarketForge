//! Transaction candidates share tables until the first write. Serialization
//! remains the ordinary ordered map; sharing never crosses a mutation boundary.
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub(crate) struct SharedMap<K: Ord, V>(Arc<BTreeMap<K, V>>);

impl<K: Ord, V> Default for SharedMap<K, V> {
    fn default() -> Self {
        Self(Arc::new(BTreeMap::new()))
    }
}

impl<K: Ord + Clone, V: Clone> Clone for SharedMap<K, V> {
    fn clone(&self) -> Self {
        #[cfg(test)]
        if EAGER_CLONE.get() {
            return Self(Arc::new((**self).clone()));
        }
        Self(Arc::clone(&self.0))
    }
}

impl<K: Ord, V> Deref for SharedMap<K, V> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
impl<K: Ord, V> SharedMap<K, V> {
    pub(crate) fn shares_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<K: Ord + Clone, V: Clone> DerefMut for SharedMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
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
    fn candidates_share_then_detach_and_keep_plain_map_wire_format() {
        let original: SharedMap<u64, String> =
            serde_json::from_str(r#"{"1":"first","2":"second"}"#).unwrap();
        let mut left = original.clone();
        let mut right = original.clone();
        assert!(Arc::ptr_eq(&original.0, &left.0));
        assert!(Arc::ptr_eq(&left.0, &right.0));
        left.get_mut(&1).unwrap().push_str(" changed");
        right.remove(&2);
        right.insert(3, "third".into());
        assert!(!Arc::ptr_eq(&original.0, &left.0));
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
        assert!(!Arc::ptr_eq(&original.0, &eager.0));
        assert_eq!(*original, *eager);
    }
}
