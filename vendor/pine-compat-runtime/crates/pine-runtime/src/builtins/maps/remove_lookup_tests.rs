use super::*;

fn equal_key(left: &PineValue, right: &PineValue) -> bool {
    match (left, right) {
        (PineValue::Int(left), PineValue::Int(right)) => left == right,
        (PineValue::Float(left), PineValue::Float(right)) => left == right,
        (PineValue::Bool(left), PineValue::Bool(right)) => left == right,
        (PineValue::String(left), PineValue::String(right)) => left == right,
        (PineValue::Color(left), PineValue::Color(right)) => left == right,
        _ => false,
    }
}

#[derive(Clone, Default)]
struct OrderedVec {
    pairs: Vec<(PineValue, PineValue)>,
}

impl OrderedVec {
    fn position(&self, key: &PineValue) -> Option<usize> {
        self.pairs
            .iter()
            .position(|(stored, _)| equal_key(stored, key))
    }

    fn get(&self, key: &PineValue) -> Option<&PineValue> {
        self.position(key).map(|slot| &self.pairs[slot].1)
    }

    fn put(&mut self, key: PineValue, value: PineValue) {
        if let Some(slot) = self.position(&key) {
            self.pairs[slot].1 = value;
        } else {
            self.pairs.push((key, value));
        }
    }

    fn remove(&mut self, key: &PineValue) {
        if let Some(slot) = self.position(key) {
            self.pairs.remove(slot);
        }
    }
}

fn assert_bits(actual: &PineValue, expected: &PineValue) {
    assert_eq!(
        std::mem::discriminant(actual),
        std::mem::discriminant(expected)
    );
    match (actual, expected) {
        (PineValue::Float(actual), PineValue::Float(expected)) => {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        _ => assert_eq!(actual, expected),
    }
}

fn assert_order(actual: &MapEntries, expected: &OrderedVec) {
    assert_eq!(actual.len(), expected.pairs.len());
    let mut iter = actual.iter();
    for (key, value) in &expected.pairs {
        let (actual_key, actual_value) = iter.next().expect("live ordered pair");
        assert_bits(actual_key, key);
        assert_bits(actual_value, value);
    }
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
}

fn assert_get(actual: &MapEntries, expected: &OrderedVec, key: &PineValue) {
    match (actual.get(key), expected.get(key)) {
        (Some(actual), Some(expected)) => assert_bits(actual, expected),
        (None, None) => {}
        (actual, expected) => panic!("get {key:?}: {actual:?} != {expected:?}"),
    }
}

fn put(entries: &mut MapEntries, oracle: &mut OrderedVec, key: PineValue, value: PineValue) {
    oracle.put(key.clone(), value.clone());
    entries.put(key, value);
    assert_order(entries, oracle);
}

fn remove(entries: &mut MapEntries, oracle: &mut OrderedVec, key: &PineValue) {
    // Every token is created after prior mutations, then consumed immediately.
    let lookup = entries.lookup_for_remove(key);
    assert_eq!(lookup.is_present(), oracle.position(key).is_some());
    oracle.remove(key);
    entries.remove_with_lookup(lookup);
    assert_order(entries, oracle);
    assert_get(entries, oracle, key);
}

#[test]
fn remove_lookup_ordered_keys_and_signed_zero_match_vec_oracle() {
    let mut entries = MapEntries::default();
    let mut oracle = OrderedVec::default();
    for (key, value) in [
        (PineValue::Float(-0.0), PineValue::Float(-0.0)),
        (PineValue::Float(2.0), PineValue::Na),
        (
            PineValue::Int(0),
            PineValue::Float(f64::from_bits(0x7ff8_0000_0000_0042)),
        ),
        (PineValue::Bool(false), PineValue::Bool(true)),
        (PineValue::Color(0), PineValue::Color(u64::MAX)),
        (
            PineValue::String("键\0value".into()),
            PineValue::String("held".into()),
        ),
        (PineValue::Float(0.0), PineValue::Int(9)),
    ] {
        put(&mut entries, &mut oracle, key, value);
    }
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    remove(&mut entries, &mut oracle, &PineValue::Float(0.0));
    // Removing +0 must remove the stored -0 key but leave Int(0) untouched.
    assert_get(&entries, &oracle, &PineValue::Int(0));
    put(
        &mut entries,
        &mut oracle,
        PineValue::Float(0.0),
        PineValue::Float(-0.0),
    );
    remove(&mut entries, &mut oracle, &PineValue::Float(-0.0));
    remove(&mut entries, &mut oracle, &PineValue::Float(-0.0));
    put(
        &mut entries,
        &mut oracle,
        PineValue::Float(-0.0),
        PineValue::Int(10),
    );
    for index in 0..300 {
        let key = PineValue::Int(index % 29);
        put(
            &mut entries,
            &mut oracle,
            key.clone(),
            PineValue::String(format!("value {index}")),
        );
        if index % 3 == 0 {
            remove(&mut entries, &mut oracle, &key);
            remove(&mut entries, &mut oracle, &key);
        }
        remove(&mut entries, &mut oracle, &PineValue::Int(1000 + index));
    }
    for key in [
        PineValue::Bool(false),
        PineValue::Color(0),
        PineValue::String("键\0value".into()),
    ] {
        remove(&mut entries, &mut oracle, &key);
        put(&mut entries, &mut oracle, key, PineValue::Na);
    }
    assert_order(&checkpoint, &saved);
    assert_get(&checkpoint, &saved, &PineValue::Float(0.0));
}

fn force_bucket(entries: &mut MapEntries, hash: u64, slots: &[usize]) {
    entries.index = None;
    for slot in slots {
        insert_index(&mut entries.index, hash, *slot);
    }
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
fn remove_lookup_collision_bucket_preserves_other_keys_and_snapshot() {
    let mut entries = MapEntries::default();
    let mut oracle = OrderedVec::default();
    for key in 1..=3 {
        put(
            &mut entries,
            &mut oracle,
            PineValue::Int(key),
            PineValue::Int(key * 11),
        );
    }
    let key = PineValue::Int(2);
    let hash = hash_key(&key);
    // Synthetic collisions avoid assuming a readily available SipHash collision.
    // Only the target hash is queried; the Vec oracle has no index or buckets.
    force_bucket(&mut entries, hash, &[0, 1, 2]);
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    remove(&mut entries, &mut oracle, &key);
    assert_eq!(bucket_slots(&entries, hash), &[0, 2]);
    assert_eq!(bucket_slots(&checkpoint, hash), &[0, 1, 2]);
    assert_get(&checkpoint, &saved, &key);
    remove(&mut entries, &mut oracle, &key);
    put(
        &mut entries,
        &mut oracle,
        key.clone(),
        PineValue::Float(-0.0),
    );
    assert_eq!(bucket_slots(&entries, hash), &[0, 2, 3]);
    remove(&mut entries, &mut oracle, &key);
    assert_eq!(bucket_slots(&entries, hash), &[0, 2]);

    let missing = PineValue::Int(4);
    let hash = hash_key(&missing);
    force_bucket(&mut entries, hash, &[0, 2]);
    remove(&mut entries, &mut oracle, &missing);
    assert_eq!(bucket_slots(&entries, hash), &[0, 2]);
    assert_order(&checkpoint, &saved);
}

fn reserved_string(text: &str, capacity: usize) -> String {
    let mut value = String::with_capacity(capacity);
    value.push_str(text);
    value
}

fn original_bytes(value: &PineValue) -> usize {
    std::mem::size_of::<PineValue>()
        + match value {
            PineValue::String(value) => value.capacity(),
            _ => 0,
        }
}

fn pressure_fixture(
    length: usize,
    deleted_prefix: usize,
) -> (MapEntries, OrderedVec, Vec<Option<usize>>) {
    let mut entries = MapEntries::default();
    let mut oracle = OrderedVec::default();
    let mut ledger = Vec::new();
    for index in 0..length {
        let key = PineValue::String(reserved_string(&format!("key-{index}"), 1024 + index));
        let value = PineValue::String(reserved_string("original", 2048 + index));
        // Preserve original String capacities before any store/page COW.
        ledger.push(Some(original_bytes(&key) + original_bytes(&value)));
        oracle.put(key.clone(), value.clone());
        entries.put(key, value);
    }
    for (index, slot) in ledger.iter_mut().enumerate().take(deleted_prefix) {
        let key = PineValue::String(format!("key-{index}"));
        entries.remove(&key);
        oracle.remove(&key);
        *slot = None;
    }
    assert_eq!(entries.values.len(), ledger.len());
    (entries, oracle, ledger)
}

// Original copy-pressure policy, evaluated only against an independent flat
// ledger. No ArrayValues/GC helper or candidate token is used by this oracle.
fn copy_pressure_oracle(
    ledger: &[Option<usize>],
    index: Option<usize>,
    cloned_self: bool,
    payload_shared: bool,
) -> usize {
    let all_bytes: usize = ledger.iter().flatten().sum();
    let small = ledger.len() <= 128;
    let Some(index) = index else {
        return if small && cloned_self { all_bytes } else { 0 };
    };
    let copied = if small {
        if cloned_self { all_bytes } else { 0 }
    } else if cloned_self || payload_shared {
        let start = index / 128 * 128;
        ledger[start..(start + 128).min(ledger.len())]
            .iter()
            .flatten()
            .sum()
    } else {
        0
    };
    let remaining = ledger.iter().filter(|slot| slot.is_some()).count() - 1;
    if remaining > 0 && ledger.len() - remaining >= remaining.max(128) {
        copied
            + ledger
                .iter()
                .enumerate()
                .filter(|(slot, _)| *slot != index)
                .filter_map(|(_, bytes)| *bytes)
                .sum::<usize>()
    } else {
        copied
    }
}

#[test]
fn remove_lookup_original_capacity_pressure_matches_copy_oracle() {
    for (length, deleted_prefix) in [
        (0, 0),
        (1, 0),
        (127, 0),
        (128, 0),
        (129, 0),
        (255, 0),
        (256, 0),
        (257, 0),
        (256, 127),
    ] {
        let queries = if length == 0 {
            vec![0]
        } else {
            vec![
                0,
                deleted_prefix,
                length / 2,
                length - 1,
                length,
                length + 1,
            ]
        };
        for index in queries {
            for (cloned_self, payload_shared) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let (entries, mut oracle, ledger) = pressure_fixture(length, deleted_prefix);
                let checkpoint = payload_shared.then(|| entries.clone());
                let saved = oracle.clone();
                let key = PineValue::String(format!("key-{index}"));
                let lookup = entries.lookup_for_remove(&key);
                let present = oracle.position(&key).is_some();
                assert_eq!(lookup.is_present(), present);
                let expected = copy_pressure_oracle(
                    &ledger,
                    present.then_some(index),
                    cloned_self,
                    payload_shared,
                );
                assert_eq!(
                    entries.remove_allocation_bytes_for_lookup(&lookup, cloned_self),
                    expected
                );
                assert_eq!(entries.remove_allocation_bytes(&key, cloned_self), expected);
                // Absence still models entry COW before the no-op remove.
                // A token remains valid across that unchanged logical clone.
                let mut target = if cloned_self {
                    entries.clone()
                } else {
                    entries
                };
                oracle.remove(&key);
                target.remove_with_lookup(lookup);
                assert_order(&target, &oracle);
                assert_get(&target, &oracle, &key);
                if let Some(checkpoint) = checkpoint {
                    assert_order(&checkpoint, &saved);
                    assert_get(&checkpoint, &saved, &key);
                }
            }
        }
    }

    let (mut entries, mut oracle, mut ledger) = pressure_fixture(257, 0);
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    remove(
        &mut entries,
        &mut oracle,
        &PineValue::String("key-0".into()),
    );
    ledger[0] = None;
    // The written page is unique; another page still shares original payloads.
    let same_page = entries.lookup_for_remove(&PineValue::String("key-64".into()));
    assert_eq!(
        entries.remove_allocation_bytes_for_lookup(&same_page, false),
        0
    );
    let other_page = entries.lookup_for_remove(&PineValue::String("key-128".into()));
    assert_eq!(
        entries.remove_allocation_bytes_for_lookup(&other_page, false),
        copy_pressure_oracle(&ledger, Some(128), false, true)
    );
    assert_order(&checkpoint, &saved);
}

#[test]
fn remove_lookup_compaction_clear_and_fresh_reinsert_preserve_snapshots() {
    let mut entries = MapEntries::default();
    let mut oracle = OrderedVec::default();
    for key in 0..256 {
        put(
            &mut entries,
            &mut oracle,
            PineValue::Int(key),
            PineValue::String(format!("value {key}")),
        );
    }
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    for key in 0..127 {
        remove(&mut entries, &mut oracle, &PineValue::Int(key));
    }
    assert_eq!(entries.values.len(), 256);
    remove(&mut entries, &mut oracle, &PineValue::Int(127));
    assert_eq!(entries.values.len(), 128);
    // Compaction moved old slot 128 to the front. Resolve it only afterwards.
    remove(&mut entries, &mut oracle, &PineValue::Int(128));
    put(
        &mut entries,
        &mut oracle,
        PineValue::Int(128),
        PineValue::Float(-0.0),
    );
    put(&mut entries, &mut oracle, PineValue::Int(0), PineValue::Na);
    assert_eq!(entries.values.len(), 130);
    let branch = entries.clone();
    let branch_oracle = oracle.clone();
    for (key, _) in branch_oracle.pairs.clone() {
        remove(&mut entries, &mut oracle, &key);
    }
    assert!(entries.is_empty());
    assert_eq!(entries.values.len(), 0);
    remove(&mut entries, &mut oracle, &PineValue::Int(128));
    put(
        &mut entries,
        &mut oracle,
        PineValue::String("after clear".into()),
        PineValue::Float(-0.0),
    );
    assert_order(&branch, &branch_oracle);
    assert_order(&checkpoint, &saved);
}
