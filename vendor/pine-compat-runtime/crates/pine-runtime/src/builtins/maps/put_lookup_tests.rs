use super::*;

fn same_key(left: &PineValue, right: &PineValue) -> bool {
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
struct VecOracle {
    entries: Vec<(PineValue, PineValue)>,
}

impl VecOracle {
    fn position(&self, key: &PineValue) -> Option<usize> {
        self.entries
            .iter()
            .position(|(stored, _)| same_key(stored, key))
    }

    fn get(&self, key: &PineValue) -> Option<&PineValue> {
        self.position(key).map(|index| &self.entries[index].1)
    }

    fn put(&mut self, key: PineValue, value: PineValue) {
        if let Some(index) = self.position(&key) {
            self.entries[index].1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    fn remove(&mut self, key: &PineValue) {
        if let Some(index) = self.position(key) {
            self.entries.remove(index);
        }
    }
}

fn assert_value_bits(actual: &PineValue, expected: &PineValue) {
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

fn assert_order(entries: &MapEntries, oracle: &VecOracle) {
    assert_eq!(entries.len(), oracle.entries.len());
    let mut actual = entries.iter();
    for (key, value) in &oracle.entries {
        let (actual_key, actual_value) = actual.next().expect("ordered entry");
        assert_value_bits(actual_key, key);
        assert_value_bits(actual_value, value);
    }
    assert!(actual.next().is_none());
    assert!(actual.next().is_none());
}

fn assert_get(entries: &MapEntries, oracle: &VecOracle, key: &PineValue) {
    match (entries.get(key), oracle.get(key)) {
        (Some(actual), Some(expected)) => assert_value_bits(actual, expected),
        (None, None) => {}
        (actual, expected) => panic!("lookup {key:?}: {actual:?} != {expected:?}"),
    }
}

fn apply(entries: &mut MapEntries, oracle: &mut VecOracle, key: PineValue, value: PineValue) {
    let lookup = entries.lookup_for_put(&key);
    assert_eq!(lookup.is_present(), oracle.get(&key).is_some());
    oracle.put(key.clone(), value.clone());
    entries.put_with_lookup(key.clone(), value, lookup);
    assert_order(entries, oracle);
    assert_get(entries, oracle, &key);
}

#[test]
fn put_lookup_ordered_values_and_signed_zero_match_vec_oracle() {
    let mut entries = MapEntries::default();
    let mut oracle = VecOracle::default();
    for (key, value) in [
        (PineValue::Float(-0.0), PineValue::Float(-0.0)),
        (PineValue::Float(2.0), PineValue::Int(2)),
        (PineValue::Int(0), PineValue::Na),
        (PineValue::Bool(false), PineValue::Bool(true)),
        (PineValue::Color(0), PineValue::Color(u64::MAX)),
        (
            PineValue::String("键\0value".into()),
            PineValue::String("first".into()),
        ),
        (
            PineValue::Float(0.0),
            PineValue::Float(f64::from_bits(0x7ff8_0000_0000_0042)),
        ),
    ] {
        apply(&mut entries, &mut oracle, key, value);
    }
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    for index in 0..300 {
        let key = PineValue::Int(index % 31);
        let value = if index % 4 == 0 {
            PineValue::Float(-0.0)
        } else {
            PineValue::String(format!("value {index}"))
        };
        apply(&mut entries, &mut oracle, key.clone(), value);
        if index % 23 == 0 {
            entries.remove(&key);
            oracle.remove(&key);
            assert_order(&entries, &oracle);
            apply(
                &mut entries,
                &mut oracle,
                key,
                PineValue::Float(index as f64),
            );
        }
    }
    entries.remove(&PineValue::Float(0.0));
    oracle.remove(&PineValue::Float(0.0));
    apply(
        &mut entries,
        &mut oracle,
        PineValue::Float(0.0),
        PineValue::Int(99),
    );
    assert_order(&checkpoint, &saved);
    assert_get(&checkpoint, &saved, &PineValue::Float(0.0));
}

#[test]
fn put_lookup_collision_bucket_uses_original_key_equality() {
    let mut entries = MapEntries::default();
    let mut oracle = VecOracle::default();
    for key in 1..=3 {
        apply(
            &mut entries,
            &mut oracle,
            PineValue::Int(key),
            PineValue::Int(key * 11),
        );
    }
    let key = PineValue::Int(2);
    let hash = hash_key(&key);
    // Force one lookup bucket containing unequal keys. The ordered Vec oracle
    // has no hash index, so it must still select only the equal original key.
    entries.index = None;
    for slot in 0..3 {
        insert_index(&mut entries.index, hash, slot);
    }
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    apply(&mut entries, &mut oracle, key, PineValue::Float(-0.0));
    assert_order(&checkpoint, &saved);
    assert_get(&checkpoint, &saved, &PineValue::Int(2));

    let missing = PineValue::Int(4);
    let hash = hash_key(&missing);
    entries.index = None;
    for slot in 0..3 {
        insert_index(&mut entries.index, hash, slot);
    }
    apply(
        &mut entries,
        &mut oracle,
        missing.clone(),
        PineValue::Int(44),
    );
    entries.remove(&missing);
    oracle.remove(&missing);
    apply(&mut entries, &mut oracle, missing, PineValue::Int(444));
    assert_order(&checkpoint, &saved);
}

fn string_with_capacity(text: &str, capacity: usize) -> String {
    let mut value = String::with_capacity(capacity);
    value.push_str(text);
    value
}

fn original_scalar_bytes(value: &PineValue) -> usize {
    std::mem::size_of::<PineValue>()
        + match value {
            PineValue::String(value) => value.capacity(),
            _ => 0,
        }
}

fn pressure_fixture(length: usize) -> (MapEntries, VecOracle, Vec<usize>) {
    let mut entries = MapEntries::default();
    let mut oracle = VecOracle::default();
    let mut original_bytes = Vec::new();
    for index in 0..length {
        let key = PineValue::String(string_with_capacity(&format!("key-{index}"), 1024 + index));
        let value = PineValue::String(string_with_capacity("original", 2048 + index));
        // Capture original allocation capacities before any String clone.
        original_bytes.push(original_scalar_bytes(&key) + original_scalar_bytes(&value));
        oracle.put(key.clone(), value.clone());
        entries.put(key, value);
    }
    (entries, oracle, original_bytes)
}

// Independent old copy-pressure policy over the fixture's flat byte ledger.
// This does not call ArrayValues' pressure helpers or the new lookup path.
fn copy_pressure_oracle(
    original_bytes: &[usize],
    index: usize,
    cloned_self: bool,
    payload_shared: bool,
) -> usize {
    let length = original_bytes.len();
    if length <= 128 {
        return if cloned_self {
            original_bytes.iter().sum()
        } else {
            0
        };
    }
    if !(cloned_self || payload_shared) || (index == length && length.is_multiple_of(128)) {
        return 0;
    }
    let start = index / 128 * 128;
    original_bytes[start..(start + 128).min(length)]
        .iter()
        .sum()
}

#[test]
fn put_lookup_shared_pages_and_original_capacity_match_pressure_oracle() {
    for length in [0, 1, 127, 128, 129, 255, 256, 257] {
        let queries = if length == 0 {
            vec![0]
        } else {
            vec![0, length / 2, length]
        };
        for index in queries {
            for (cloned_self, payload_shared) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let (entries, mut oracle, original_bytes) = pressure_fixture(length);
                let checkpoint = payload_shared.then(|| entries.clone());
                let saved = oracle.clone();
                let key = PineValue::String(format!("key-{index}"));
                let lookup = entries.lookup_for_put(&key);
                assert_eq!(lookup.is_present(), oracle.get(&key).is_some());
                let physical_index = oracle.position(&key).unwrap_or(length);
                let expected = copy_pressure_oracle(
                    &original_bytes,
                    physical_index,
                    cloned_self,
                    payload_shared,
                );
                assert_eq!(
                    entries.put_allocation_bytes_for_lookup(&lookup, cloned_self),
                    expected
                );
                assert_eq!(entries.put_allocation_bytes(&key, cloned_self), expected);

                // Model store-entry COW after measuring the original payload.
                // Logical slots survive it, while cloned String capacities may
                // differ, so measuring the copy afterwards would be incorrect.
                let mut target = if cloned_self {
                    entries.clone()
                } else {
                    entries
                };
                let value = PineValue::String("replacement".into());
                oracle.put(key.clone(), value.clone());
                target.put_with_lookup(key.clone(), value, lookup);
                assert_order(&target, &oracle);
                assert_get(&target, &oracle, &key);
                if let Some(checkpoint) = checkpoint {
                    assert_order(&checkpoint, &saved);
                }
            }
        }
    }

    // After one sparse COW write, that page is unique while other payload
    // pages remain shared with the checkpoint. Both states need fresh lookups.
    let (mut entries, mut oracle, original_bytes) = pressure_fixture(257);
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    apply(
        &mut entries,
        &mut oracle,
        PineValue::String("key-0".into()),
        PineValue::String("changed".into()),
    );
    let same_page = entries.lookup_for_put(&PineValue::String("key-64".into()));
    assert_eq!(
        entries.put_allocation_bytes_for_lookup(&same_page, false),
        0
    );
    let other_page = entries.lookup_for_put(&PineValue::String("key-128".into()));
    assert_eq!(
        entries.put_allocation_bytes_for_lookup(&other_page, false),
        copy_pressure_oracle(&original_bytes, 128, false, true)
    );
    assert_order(&checkpoint, &saved);
}

#[test]
fn put_lookup_after_deletion_compaction_and_snapshot_matches_vec_oracle() {
    let mut entries = MapEntries::default();
    let mut oracle = VecOracle::default();
    for key in 0..256 {
        apply(
            &mut entries,
            &mut oracle,
            PineValue::Int(key),
            PineValue::String(format!("value {key}")),
        );
    }
    let checkpoint = entries.clone();
    let saved = oracle.clone();
    for key in 0..127 {
        entries.remove(&PineValue::Int(key));
        oracle.remove(&PineValue::Int(key));
    }
    assert_eq!(entries.values.len(), 256);
    apply(
        &mut entries,
        &mut oracle,
        PineValue::Int(200),
        PineValue::String("before compaction".into()),
    );
    entries.remove(&PineValue::Int(127));
    oracle.remove(&PineValue::Int(127));
    assert_eq!(entries.values.len(), 128);
    // Lookups are created only now, after deletion has moved the live slots.
    apply(
        &mut entries,
        &mut oracle,
        PineValue::Int(128),
        PineValue::Float(-0.0),
    );
    apply(
        &mut entries,
        &mut oracle,
        PineValue::Int(0),
        PineValue::String("reinserted".into()),
    );
    apply(
        &mut entries,
        &mut oracle,
        PineValue::Int(1),
        PineValue::Int(1),
    );
    assert_order(&checkpoint, &saved);

    let branch = entries.clone();
    let branch_oracle = oracle.clone();
    entries.clear();
    oracle.entries.clear();
    apply(
        &mut entries,
        &mut oracle,
        PineValue::String("after clear".into()),
        PineValue::Na,
    );
    assert_order(&branch, &branch_oracle);
    assert_order(&checkpoint, &saved);
}
