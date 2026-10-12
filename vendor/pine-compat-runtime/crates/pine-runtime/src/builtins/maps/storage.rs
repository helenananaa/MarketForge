//! Ordered map entries with a persistent key index. Checkpoints share the
//! index and payload pages; replacing one value copies only its payload page.
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

use crate::runtime::collection_gc::collection_values_allocation_bytes;
use crate::{PineValue, runtime::array_values::ArrayValues};

use super::map_keys_equal;

#[derive(Debug, Clone)]
pub(crate) struct MapEntries {
    values: ArrayValues<Option<(PineValue, PineValue)>>,
    index: Option<Arc<IndexNode>>,
    len: usize,
}

// Valid for one immediate put or remove on the same logical entries, including
// a COW clone. Keep fields private and consume it so callers cannot reuse it.
pub(super) struct EntryLookup {
    hash: u64,
    slot: Option<usize>,
}

impl EntryLookup {
    pub(super) fn is_present(&self) -> bool {
        self.slot.is_some()
    }
}

impl Default for MapEntries {
    fn default() -> Self {
        Self {
            values: Vec::new().into(),
            index: None,
            len: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct IndexNode {
    hash: u64,
    slots: Vec<usize>,
    left: Option<Arc<IndexNode>>,
    right: Option<Arc<IndexNode>>,
    height: i32,
}

fn hash_key(key: &PineValue) -> u64 {
    let mut hasher = DefaultHasher::new();
    std::mem::discriminant(key).hash(&mut hasher);
    match key {
        PineValue::Int(value) => value.hash(&mut hasher),
        // Pine compares signed zeros as equal. Other accepted float keys are
        // finite, so bit identity matches their numeric equality.
        PineValue::Float(value) => {
            (if *value == 0.0 { 0 } else { value.to_bits() }).hash(&mut hasher);
        }
        PineValue::Bool(value) => value.hash(&mut hasher),
        PineValue::String(value) => value.hash(&mut hasher),
        PineValue::Color(value) => value.hash(&mut hasher),
        _ => {}
    }
    hasher.finish()
}

fn height(node: &Option<Arc<IndexNode>>) -> i32 {
    node.as_ref().map_or(0, |node| node.height)
}

fn update_height(node: &mut IndexNode) {
    node.height = 1 + height(&node.left).max(height(&node.right));
}

fn rotate_left(mut root: Arc<IndexNode>) -> Arc<IndexNode> {
    let mut next = Arc::make_mut(&mut root).right.take().expect("right child");
    Arc::make_mut(&mut root).right = Arc::make_mut(&mut next).left.take();
    update_height(Arc::make_mut(&mut root));
    Arc::make_mut(&mut next).left = Some(root);
    update_height(Arc::make_mut(&mut next));
    next
}

fn rotate_right(mut root: Arc<IndexNode>) -> Arc<IndexNode> {
    let mut next = Arc::make_mut(&mut root).left.take().expect("left child");
    Arc::make_mut(&mut root).left = Arc::make_mut(&mut next).right.take();
    update_height(Arc::make_mut(&mut root));
    Arc::make_mut(&mut next).right = Some(root);
    update_height(Arc::make_mut(&mut next));
    next
}

fn insert_index(root: &mut Option<Arc<IndexNode>>, hash: u64, slot: usize) {
    let Some(node) = root else {
        *root = Some(Arc::new(IndexNode {
            hash,
            slots: vec![slot],
            left: None,
            right: None,
            height: 1,
        }));
        return;
    };
    let node = Arc::make_mut(node);
    match hash.cmp(&node.hash) {
        std::cmp::Ordering::Less => insert_index(&mut node.left, hash, slot),
        std::cmp::Ordering::Greater => insert_index(&mut node.right, hash, slot),
        std::cmp::Ordering::Equal => {
            node.slots.push(slot);
            return;
        }
    }
    update_height(node);
    let balance = height(&node.left) - height(&node.right);
    if balance > 1 {
        if hash > node.left.as_ref().expect("left child").hash {
            node.left = Some(rotate_left(node.left.take().expect("left child")));
        }
        *root = Some(rotate_right(root.take().expect("root")));
    } else if balance < -1 {
        if hash < node.right.as_ref().expect("right child").hash {
            node.right = Some(rotate_right(node.right.take().expect("right child")));
        }
        *root = Some(rotate_left(root.take().expect("root")));
    }
}

fn remove_index_slot(root: &mut Option<Arc<IndexNode>>, hash: u64, slot: usize) {
    let Some(node) = root else {
        return;
    };
    let node = Arc::make_mut(node);
    match hash.cmp(&node.hash) {
        std::cmp::Ordering::Less => remove_index_slot(&mut node.left, hash, slot),
        std::cmp::Ordering::Greater => remove_index_slot(&mut node.right, hash, slot),
        std::cmp::Ordering::Equal => {
            node.slots.retain(|stored| *stored != slot);
        }
    }
    // Keep empty buckets until payload compaction. Reusing a removed key's
    // bucket avoids AVL deletion while keeping lookup free of dead slots.
}

impl MapEntries {
    fn payload_allocation_bytes(values: &[Option<(PineValue, PineValue)>]) -> usize {
        collection_values_allocation_bytes(
            values
                .iter()
                .filter_map(Option::as_ref)
                .flat_map(|(key, value)| [key, value]),
        )
    }

    pub(super) fn clone_allocation_bytes(&self) -> usize {
        Self::payload_allocation_bytes(self.values.clone_allocation_values())
    }

    pub(super) fn put_allocation_bytes(&self, key: &PineValue, cloned_self: bool) -> usize {
        self.put_allocation_bytes_for_lookup(&self.lookup_for_put(key), cloned_self)
    }

    pub(super) fn put_allocation_bytes_for_lookup(
        &self,
        lookup: &EntryLookup,
        cloned_self: bool,
    ) -> usize {
        let index = lookup.slot.unwrap_or(self.values.len());
        Self::payload_allocation_bytes(self.values.write_allocation_values(index, cloned_self))
    }

    #[cfg(test)]
    pub(super) fn remove_allocation_bytes(&self, key: &PineValue, cloned_self: bool) -> usize {
        self.remove_allocation_bytes_for_lookup(&self.lookup_for_remove(key), cloned_self)
    }

    pub(super) fn remove_allocation_bytes_for_lookup(
        &self,
        lookup: &EntryLookup,
        cloned_self: bool,
    ) -> usize {
        let Some(index) = lookup.slot else {
            return if cloned_self {
                self.clone_allocation_bytes()
            } else {
                0
            };
        };
        let copied =
            Self::payload_allocation_bytes(self.values.write_allocation_values(index, cloned_self));
        let remaining = self.len - 1;
        if remaining > 0 && self.values.len() - remaining >= remaining.max(128) {
            // Compaction already walks and clones the live payload. Only this
            // path needs to account for more than one bounded write page.
            copied.saturating_add(collection_values_allocation_bytes(
                self.values
                    .iter()
                    .enumerate()
                    .filter(|(slot, _)| *slot != index)
                    .filter_map(|(_, entry)| entry.as_ref())
                    .flat_map(|(key, value)| [key, value]),
            ))
        } else {
            copied
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn iter(&self) -> Iter<'_> {
        Iter {
            values: self.values.iter(),
        }
    }

    fn find(&self, key: &PineValue) -> Option<usize> {
        self.find_hashed(key, hash_key(key))
    }

    pub(super) fn lookup_for_put(&self, key: &PineValue) -> EntryLookup {
        self.lookup(key)
    }

    pub(super) fn lookup_for_remove(&self, key: &PineValue) -> EntryLookup {
        self.lookup(key)
    }

    fn lookup(&self, key: &PineValue) -> EntryLookup {
        let hash = hash_key(key);
        EntryLookup {
            hash,
            slot: self.find_hashed(key, hash),
        }
    }

    fn find_hashed(&self, key: &PineValue, hash: u64) -> Option<usize> {
        let mut node = self.index.as_deref();
        while let Some(current) = node {
            match hash.cmp(&current.hash) {
                std::cmp::Ordering::Less => node = current.left.as_deref(),
                std::cmp::Ordering::Greater => node = current.right.as_deref(),
                std::cmp::Ordering::Equal => {
                    return current.slots.iter().copied().find(|slot| {
                        self.values
                            .get(*slot)
                            .and_then(Option::as_ref)
                            .is_some_and(|(stored, _)| map_keys_equal(stored, key))
                    });
                }
            }
        }
        None
    }

    pub(crate) fn get(&self, key: &PineValue) -> Option<&PineValue> {
        self.find(key)
            .and_then(|slot| self.values.get(slot))
            .and_then(Option::as_ref)
            .map(|(_, value)| value)
    }

    pub(crate) fn put(&mut self, key: PineValue, value: PineValue) {
        let lookup = self.lookup_for_put(&key);
        self.put_with_lookup(key, value, lookup);
    }

    pub(super) fn put_with_lookup(
        &mut self,
        key: PineValue,
        value: PineValue,
        lookup: EntryLookup,
    ) {
        if let Some(slot) = lookup.slot {
            self.values
                .get_mut(slot)
                .expect("indexed slot")
                .as_mut()
                .expect("live slot")
                .1 = value;
        } else {
            let slot = self.values.len();
            insert_index(&mut self.index, lookup.hash, slot);
            self.values.insert(slot, Some((key, value)));
            self.len += 1;
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub(crate) fn remove(&mut self, key: &PineValue) {
        let lookup = self.lookup_for_remove(key);
        self.remove_with_lookup(lookup);
    }

    pub(super) fn remove_with_lookup(&mut self, lookup: EntryLookup) {
        let Some(slot) = lookup.slot else {
            return;
        };
        remove_index_slot(&mut self.index, lookup.hash, slot);
        *self.values.get_mut(slot).expect("indexed slot") = None;
        self.len -= 1;
        if self.is_empty() {
            self.clear();
        } else if self.values.len() - self.len >= self.len.max(128) {
            // Bound dead entry/index retention without shifting every entry on
            // each deletion. Reinserted keys naturally append at the end.
            *self = self.iter().cloned().collect::<Vec<_>>().into();
        }
    }
}

#[cfg(test)]
#[path = "put_lookup_tests.rs"]
mod put_lookup_tests;

#[cfg(test)]
#[path = "remove_lookup_tests.rs"]
mod remove_lookup_tests;

impl From<Vec<(PineValue, PineValue)>> for MapEntries {
    fn from(values: Vec<(PineValue, PineValue)>) -> Self {
        let mut entries = Self::default();
        for (key, value) in values {
            entries.put(key, value);
        }
        entries
    }
}

impl PartialEq for MapEntries {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

pub(crate) struct OwnedIter {
    entries: MapEntries,
    slot: usize,
}

pub(crate) struct Iter<'a> {
    values: crate::runtime::array_values::ArrayIter<'a, Option<(PineValue, PineValue)>>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a (PineValue, PineValue);
    fn next(&mut self) -> Option<Self::Item> {
        self.values.find_map(Option::as_ref)
    }
}

impl Iterator for OwnedIter {
    type Item = (PineValue, PineValue);
    fn next(&mut self) -> Option<Self::Item> {
        while self.slot < self.entries.values.len() {
            let slot = self.slot;
            self.slot += 1;
            if let Some(entry) = &self.entries.values[slot] {
                return Some(entry.clone());
            }
        }
        None
    }
}

impl IntoIterator for MapEntries {
    type Item = (PineValue, PineValue);
    type IntoIter = OwnedIter;
    fn into_iter(self) -> Self::IntoIter {
        OwnedIter {
            entries: self,
            slot: 0,
        }
    }
}

impl<'a> IntoIterator for &'a MapEntries {
    type Item = &'a (PineValue, PineValue);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_clone_pressure_counts_small_payloads_and_only_the_written_large_page() {
        let entry_bytes = 2 * std::mem::size_of::<PineValue>() + 256;
        let mut entries = MapEntries::default();
        for key in 0..128 {
            entries.put(PineValue::Int(key), PineValue::String("x".repeat(256)));
        }
        assert_eq!(entries.clone_allocation_bytes(), 128 * entry_bytes);
        assert_eq!(entries.put_allocation_bytes(&PineValue::Int(64), false), 0);
        assert_eq!(
            entries.put_allocation_bytes(&PineValue::Int(64), true),
            128 * entry_bytes
        );
        for key in 128..8192 {
            entries.put(PineValue::Int(key), PineValue::String("x".repeat(256)));
        }
        assert_eq!(entries.clone_allocation_bytes(), 0);
        let checkpoint = entries.clone();
        assert_eq!(
            entries.put_allocation_bytes(&PineValue::Int(4096), false),
            128 * entry_bytes
        );
        assert_eq!(
            entries.put_allocation_bytes(&PineValue::Int(4096), true),
            128 * entry_bytes
        );
        assert_eq!(
            entries.put_allocation_bytes(&PineValue::Int(8192), false),
            0
        );
        entries.put(PineValue::Int(4096), PineValue::String("short".into()));
        assert_eq!(
            entries.put_allocation_bytes(&PineValue::Int(4096), false),
            0
        );
        assert_eq!(
            checkpoint.get(&PineValue::Int(4096)),
            Some(&PineValue::String("x".repeat(256)))
        );
    }

    #[test]
    fn partial_tail_append_pressure_counts_one_shared_tail_and_no_incoming_value() {
        let entry_bytes = 2 * std::mem::size_of::<PineValue>() + 256;
        let mut entries = MapEntries::default();
        for key in 0..129 {
            entries.put(PineValue::Int(key), PineValue::String("x".repeat(256)));
        }
        let checkpoint = entries.clone();
        assert_eq!(
            entries.put_allocation_bytes(&PineValue::Int(129), false),
            entry_bytes
        );
        entries.put(PineValue::Int(129), PineValue::String("short".into()));
        assert_eq!(entries.put_allocation_bytes(&PineValue::Int(130), false), 0);
        assert_eq!(checkpoint.len(), 129);
        assert_eq!(checkpoint.get(&PineValue::Int(129)), None);
    }

    fn bucket_slots(entries: &MapEntries, hash: u64) -> &[usize] {
        let mut node = entries.index.as_deref();
        while let Some(current) = node {
            match hash.cmp(&current.hash) {
                std::cmp::Ordering::Less => node = current.left.as_deref(),
                std::cmp::Ordering::Greater => node = current.right.as_deref(),
                std::cmp::Ordering::Equal => return &current.slots,
            }
        }
        &[]
    }

    #[test]
    fn signed_zero_overwrite_delete_reinsert_order_and_copy_are_preserved() {
        let mut entries = MapEntries::default();
        entries.put(PineValue::Float(-0.0), PineValue::Int(1));
        entries.put(PineValue::Float(2.), PineValue::Int(2));
        let checkpoint = entries.clone();
        entries.put(PineValue::Float(0.0), PineValue::Int(3));
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries.iter().next().unwrap().0.as_f64().unwrap().to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(
            checkpoint.get(&PineValue::Float(0.0)),
            Some(&PineValue::Int(1))
        );
        entries.remove(&PineValue::Float(0.0));
        entries.put(PineValue::Float(0.0), PineValue::Int(4));
        assert_eq!(
            entries
                .iter()
                .map(|(key, _)| key.as_f64().unwrap())
                .collect::<Vec<_>>(),
            vec![2., 0.]
        );
        assert_eq!(checkpoint.len(), 2);
    }

    #[test]
    fn large_map_sparse_update_and_growth_share_unmodified_payloads() {
        let mut entries = MapEntries::default();
        for key in 0..8192 {
            entries.put(
                PineValue::Int(key),
                PineValue::String(format!("value {key}")),
            );
        }
        let checkpoint = entries.clone();
        let shared = checkpoint.get(&PineValue::Int(0)).unwrap();
        entries.put(PineValue::Int(4096), PineValue::String("changed".into()));
        entries.put(PineValue::Int(8192), PineValue::Int(8192));
        assert!(std::ptr::eq(
            shared,
            entries.get(&PineValue::Int(0)).unwrap()
        ));
        assert_eq!(
            checkpoint.get(&PineValue::Int(4096)),
            Some(&PineValue::String("value 4096".into()))
        );
        assert_eq!(checkpoint.get(&PineValue::Int(8192)), None);
        assert!(height(&entries.index) <= 20);
    }

    #[test]
    fn compaction_bounds_deleted_slots_and_preserves_snapshot_order() {
        let mut entries = MapEntries::default();
        entries.put(PineValue::String("held".into()), PineValue::Int(7));
        let checkpoint = entries.clone();
        for key in 0..4096 {
            entries.put(PineValue::Int(key), PineValue::Int(key));
            entries.remove(&PineValue::Int(key));
        }
        assert_eq!(entries, checkpoint);
        assert!(entries.values.len() <= 128);
    }

    #[test]
    fn hash_collisions_compare_the_original_keys() {
        let mut entries = MapEntries::default();
        entries.put(PineValue::Int(1), PineValue::Int(11));
        entries.put(PineValue::Int(2), PineValue::Int(22));
        // Put a nonmatching key in the same hash bucket to exercise collision
        // resolution without assuming that real SipHash collisions are easy.
        entries.index = None;
        insert_index(&mut entries.index, hash_key(&PineValue::Int(2)), 0);
        insert_index(&mut entries.index, hash_key(&PineValue::Int(2)), 1);
        assert_eq!(entries.get(&PineValue::Int(2)), Some(&PineValue::Int(22)));
    }

    #[test]
    fn repeated_key_removal_keeps_lookup_bucket_bounded_before_compaction() {
        let mut entries = MapEntries::default();
        for key in 0..8192 {
            entries.put(PineValue::Int(key), PineValue::Int(key));
        }
        let key = PineValue::Int(8192);
        entries.put(key.clone(), PineValue::Int(-1));
        let checkpoint = entries.clone();
        let hash = hash_key(&key);
        for value in 0..512 {
            entries.remove(&key);
            assert!(bucket_slots(&entries, hash).is_empty());
            entries.put(key.clone(), PineValue::Int(value));
            assert_eq!(bucket_slots(&entries, hash).len(), 1);
            assert_eq!(entries.get(&key), Some(&PineValue::Int(value)));
        }
        // This workload stays below global compaction: the lookup bound must
        // come from removing each dead index slot, not from rebuilding it.
        assert_eq!(entries.values.len(), checkpoint.values.len() + 512);
        assert_eq!(entries.len(), checkpoint.len());
        assert_eq!(checkpoint.get(&key), Some(&PineValue::Int(-1)));
        assert_eq!(bucket_slots(&checkpoint, hash), &[8192]);
    }

    #[test]
    fn removing_one_collision_slot_preserves_other_slots_and_shared_checkpoint() {
        let mut entries = MapEntries::default();
        entries.put(PineValue::Int(1), PineValue::Int(11));
        entries.put(PineValue::Int(2), PineValue::Int(22));
        let key = PineValue::Int(2);
        let hash = hash_key(&key);
        // As above, force a collision without depending on finding a real one.
        // Slot 0 must remain available for collision resolution when removing
        // and reinserting slot 1's key.
        entries.index = None;
        insert_index(&mut entries.index, hash, 0);
        insert_index(&mut entries.index, hash, 1);
        let checkpoint = entries.clone();
        assert!(Arc::ptr_eq(
            entries.index.as_ref().unwrap(),
            checkpoint.index.as_ref().unwrap()
        ));
        entries.remove(&key);
        assert_eq!(bucket_slots(&entries, hash), &[0]);
        assert_eq!(entries.get(&key), None);
        entries.put(key.clone(), PineValue::Int(33));
        assert_eq!(bucket_slots(&entries, hash), &[0, 2]);
        assert_eq!(entries.get(&key), Some(&PineValue::Int(33)));
        assert_eq!(checkpoint.get(&key), Some(&PineValue::Int(22)));
        assert_eq!(bucket_slots(&checkpoint, hash), &[0, 1]);
        assert_eq!(
            entries
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            vec![PineValue::Int(1), PineValue::Int(2)]
        );
    }
}
