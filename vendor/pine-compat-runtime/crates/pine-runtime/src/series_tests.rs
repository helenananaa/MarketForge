use super::*;

#[test]
fn scalar_history_is_skipped_by_gc_and_later_reference_writes_are_tracked() {
    let mut store = SeriesStore::new();
    let id = SeriesId(17);
    for value in 0..100000 {
        store.commit(id, PineValue::Int(value), None);
    }
    assert_eq!(store.collection_root_buffers().count(), 0);
    let checkpoint = store.clone();
    store.commit(id, PineValue::Tuple(vec![PineValue::Map(7)]), Some(1));
    assert_eq!(store.collection_root_buffers().count(), 1);
    assert_eq!(checkpoint.collection_root_buffers().count(), 0);
    store.commit(id, PineValue::Int(1), Some(1));
    let mut equivalent = SeriesStore::new();
    equivalent.commit(id, PineValue::Int(1), Some(1));
    assert_eq!(store, equivalent); // Conservative GC metadata is not store content.
    store.commit(id, PineValue::Na, Some(0));
    assert_eq!(store.collection_root_buffers().count(), 0);
}

#[test]
fn checkpoint_sharing_preserves_nan_equality_and_float_bits() {
    for count in [1, 129] {
        let mut store = SeriesStore::new();
        let id = SeriesId(1);
        for _ in 0..count {
            store.commit(
                id,
                PineValue::Float(f64::from_bits(0x7ff8000000000001)),
                None,
            );
        }
        let checkpoint = store.clone();
        assert_ne!(store, checkpoint);
        let PineValue::Float(value) = store.read(id, count) else {
            panic!("float")
        };
        assert_eq!(value.to_bits(), 0x7ff8000000000001);
    }
}

#[test]
fn bounded_history_matches_a_window_across_ring_and_tree_boundaries() {
    for depth in [1, 7, 127, 128, 129, 257, 5000] {
        let mut store = SeriesStore::new();
        let id = SeriesId(17);
        for value in 0..(depth * 3 + 500) {
            store.commit(id, PineValue::Int(value as i64), Some(depth));
            assert_eq!(store.len(id), (value + 1).min(depth));
            assert_eq!(store.read(id, 1), PineValue::Int(value as i64));
            let retained = store.len(id);
            assert_eq!(
                store.read(id, retained),
                PineValue::Int((value + 1 - retained) as i64)
            );
            assert_eq!(store.read(id, retained + 1), PineValue::Na);
            assert_eq!(store.read(id, 0), PineValue::Na);
        }
        let buffer = &store.buffers[&id];
        assert!(buffer.capacity() <= depth * 2 + APPEND_LEAF_SIZE * 3);
        assert_eq!(buffer.iter().count(), depth);
        if depth == 1 {
            assert!(buffer.capacity() <= 4, "one-value history stays small");
        }
    }
}

#[test]
fn unbounded_history_and_branches_preserve_all_values() {
    let id = SeriesId(2);
    let mut store = SeriesStore::new();
    for value in 0..10_003 {
        store.commit(id, PineValue::Int(value), None);
    }
    store.set_current_bar(10_002);
    let checkpoint = store.clone();
    // A deep checkpoint shares the old leaves without cloning PineValues.
    assert!(std::ptr::eq(
        store.buffers[&id].get(0).unwrap(),
        checkpoint.buffers[&id].get(0).unwrap()
    ));
    store.commit(id, PineValue::String("tentative".into()), None);
    store.set_current_bar(10_003);
    assert!(std::ptr::eq(
        store.buffers[&id].get(0).unwrap(),
        checkpoint.buffers[&id].get(0).unwrap()
    ));
    assert_eq!(checkpoint.current_bar(), 10_002);
    assert_eq!(checkpoint.len(id), 10_003);
    for offset in 1..=10_003 {
        assert_eq!(
            checkpoint.read(id, offset),
            PineValue::Int((10_003 - offset) as i64)
        );
    }
    assert_eq!(store.read(id, 1), PineValue::String("tentative".into()));
    assert_eq!(store.read(id, 10_004), PineValue::Int(0));
    let mut replacement = checkpoint.clone();
    replacement.commit(id, PineValue::Int(-1), None);
    assert_eq!(replacement.read(id, 1), PineValue::Int(-1));
    assert_eq!(checkpoint.read(id, 1), PineValue::Int(10_002));
}

#[test]
fn bounded_checkpoint_append_shares_unchanged_middle_leaves() {
    let id = SeriesId(0);
    let mut store = SeriesStore::new();
    for value in 0..5000 {
        store.commit(id, PineValue::Int(value), Some(5000));
    }
    let checkpoint = store.clone();
    store.commit(id, PineValue::Int(5000), Some(5000));
    assert!(std::ptr::eq(
        store.buffers[&id].get(255).unwrap(),
        checkpoint.buffers[&id].get(256).unwrap()
    ));
    assert_eq!(checkpoint.read(id, 5000), PineValue::Int(0));
    assert_eq!(store.read(id, 5000), PineValue::Int(1));
    assert_eq!(store.values_len(), 5000);
}

#[test]
fn retention_changes_keep_exact_logical_roots_and_checkpoint_values() {
    let id = SeriesId(7);
    let mut store = SeriesStore::new();
    let mut expected = VecDeque::new();
    for (step, limit) in [None, Some(257), Some(127), Some(0), Some(1), None]
        .into_iter()
        .enumerate()
    {
        for value in 0..300 {
            let value = PineValue::Array((step * 300 + value) as u32);
            let checkpoint = store.clone();
            let previous: Vec<_> = expected.iter().cloned().collect();
            store.commit(id, value.clone(), limit);
            expected.push_back(value);
            if let Some(limit) = limit {
                while expected.len() > limit {
                    expected.pop_front();
                }
            }
            let roots: Vec<_> = store
                .buffers
                .values()
                .flat_map(|values| values.iter())
                .cloned()
                .collect();
            assert_eq!(roots, expected.iter().cloned().collect::<Vec<_>>());
            assert_eq!(
                checkpoint
                    .buffers
                    .values()
                    .flat_map(|values| values.iter())
                    .cloned()
                    .collect::<Vec<_>>(),
                previous
            );
            assert_eq!(store.values_len(), expected.len());
        }
    }
}

#[test]
fn borrowed_history_windows_match_offsets_across_retention_and_tree_boundaries() {
    let mut store = SeriesStore::new();
    let id = SeriesId(123);
    for depth in [Some(1), Some(128), Some(129), Some(257), None, Some(0)] {
        for value in 0..700 {
            store.commit(id, PineValue::Int(value), depth);
        }
        for length in [0, 1, 127, 128, 129, 1000] {
            let expected: Vec<_> = (1..=length.min(store.len(id)))
                .map(|offset| store.read(id, offset))
                .collect();
            assert_eq!(
                store
                    .history_window(id, length)
                    .cloned()
                    .collect::<Vec<_>>(),
                expected
            );
        }
        let checkpoint = store.clone();
        store.commit(id, PineValue::Int(-1), depth);
        assert_eq!(
            checkpoint
                .history_window(id, usize::MAX)
                .cloned()
                .collect::<Vec<_>>(),
            (1..=checkpoint.len(id))
                .map(|offset| checkpoint.read(id, offset))
                .collect::<Vec<_>>()
        );
    }
    assert!(store.history_window(SeriesId(999), 3).next().is_none());
}
