use super::*;

type FloatDeque = SharedDeque<Option<f64>>;
type Directory = Arc<VecDeque<Arc<Page<Option<f64>>>>>;

fn bits(value: Option<f64>) -> Option<u64> {
    value.map(f64::to_bits)
}

fn paged(values: &FloatDeque) -> &PagedStorage<Option<f64>> {
    match &values.storage {
        Storage::Paged(pages) => pages,
        Storage::Small(_) => panic!("expected paged storage"),
    }
}

fn same_directory(values: &FloatDeque, expected: &Option<Directory>) -> bool {
    match (&paged(values).middle, expected) {
        (None, None) => true,
        (Some(actual), Some(expected)) => Arc::ptr_eq(actual, expected),
        _ => false,
    }
}

fn assert_model(values: &FloatDeque, expected: &VecDeque<Option<f64>>) {
    assert_eq!(values.len(), expected.len());
    assert_eq!(
        values.front().copied().map(bits),
        expected.front().copied().map(bits)
    );
    assert_eq!(
        values.back().copied().map(bits),
        expected.back().copied().map(bits)
    );
    assert!(
        values
            .iter()
            .copied()
            .map(bits)
            .eq(expected.iter().copied().map(bits))
    );
    for index in [
        0,
        expected.len() / 2,
        expected.len().saturating_sub(1),
        expected.len(),
    ] {
        assert_eq!(
            values.get(index).copied().map(bits),
            expected.get(index).copied().map(bits)
        );
    }
    let len = expected.len();
    for (start, end) in [
        (0, len),
        (len, len),
        (len / 3, len * 2 / 3),
        (127.min(len), 129.min(len)),
    ] {
        let actual = values.range(start..end);
        assert_eq!(actual.len(), end - start);
        assert!(
            actual
                .copied()
                .map(bits)
                .eq(expected.range(start..end).copied().map(bits))
        );
        assert!(
            values
                .range(start..end)
                .rev()
                .copied()
                .map(bits)
                .eq(expected.range(start..end).rev().copied().map(bits))
        );
    }
    if let Storage::Paged(pages) = &values.storage {
        assert!(pages.front_offset < pages.front.len());
        assert!(pages.back_len != 0 && pages.back_len <= pages.page(pages.page_count() - 1).len());
        let logical = if pages.back.is_none() {
            assert!(pages.middle.is_none());
            assert!(pages.front_offset < pages.back_len);
            pages.back_len - pages.front_offset
        } else {
            pages.front.len() - pages.front_offset + pages.middle_len() * PAGE_SIZE + pages.back_len
        };
        assert_eq!(pages.len, logical);
        assert!(
            (0..pages.page_count())
                .map(|index| pages.page(index))
                .all(|page| page.len() <= PAGE_SIZE && page.capacity() == PAGE_SIZE)
        );
        assert_eq!(values.capacity(), pages.page_count() * PAGE_SIZE);
    }
}

#[test]
fn unchanged_prefix_restoration_keeps_shared_directory_and_page_identity() {
    for length in [129, 257, 5000, 100_000] {
        for removed in [1, 17, PAGE_SIZE - 1] {
            let original = (0..length)
                .map(|value| Some(value as f64))
                .collect::<VecDeque<_>>();
            let mut expected = original.clone();
            let mut values = FloatDeque::from(original.clone());
            let before = values.clone();
            let mut evicted = Vec::new();
            for _ in 0..removed {
                let value = values.pop_front().unwrap();
                assert_eq!(bits(value), bits(expected.pop_front().unwrap()));
                evicted.push(value);
            }
            let after_pop = values.clone();
            let after_pop_expected = expected.clone();
            let root = paged(&values).middle.clone();
            let page = paged(&values).front.clone();
            for value in evicted.into_iter().rev() {
                values.restore_front(value);
                expected.push_front(value);
                assert!(same_directory(&values, &root));
                assert!(Arc::ptr_eq(&paged(&values).front, &page));
            }
            assert_model(&values, &original);
            assert_model(&values, &expected);
            assert_model(&before, &original);
            assert_model(&after_pop, &after_pop_expected);
        }
    }
}

#[test]
fn exact_float_bits_restore_but_changed_zeros_nan_payloads_and_na_use_cow() {
    let nan_a = Some(f64::from_bits(0x7ff8_0000_0000_0011));
    let nan_b = Some(f64::from_bits(0x7ff8_0000_0000_0022));
    let negative_nan = Some(f64::from_bits(0xfff8_0000_0000_0011));
    let signaling_nan = Some(f64::from_bits(0x7ff0_0000_0000_0001));
    for (outgoing, incoming) in [
        (Some(0.0), Some(0.0)),
        (Some(-0.0), Some(-0.0)),
        (nan_a, nan_a),
        (negative_nan, negative_nan),
        (signaling_nan, signaling_nan),
        (None, None),
        (Some(12.5), Some(12.5)),
        (Some(0.0), Some(-0.0)),
        (Some(-0.0), Some(0.0)),
        (nan_a, nan_b),
        (nan_a, negative_nan),
        (nan_a, None),
        (None, nan_a),
        (Some(12.5), Some(12.75)),
    ] {
        let mut expected = (0..257)
            .map(|value| Some(value as f64))
            .collect::<VecDeque<_>>();
        expected[0] = outgoing;
        let mut values = FloatDeque::from(expected.clone());
        assert_eq!(
            bits(values.pop_front().unwrap()),
            bits(expected.pop_front().unwrap())
        );
        let checkpoint = values.clone();
        let checkpoint_expected = expected.clone();
        let root = paged(&values).middle.clone();
        let page = paged(&values).front.clone();
        values.restore_front(incoming);
        expected.push_front(incoming);
        let unchanged = bits(outgoing) == bits(incoming);
        // A changed restore copies the front page while the interior directory
        // stays shared; an exact restore keeps both identities.
        assert!(same_directory(&values, &root));
        assert_eq!(Arc::ptr_eq(&paged(&values).front, &page), unchanged);
        assert_model(&values, &expected);
        assert_model(&checkpoint, &checkpoint_expected);
    }
}

#[test]
fn ordinary_front_write_invalidates_the_old_prefix_and_restore_uses_cow() {
    let original = (0..257)
        .map(|value| Some(value as f64))
        .collect::<VecDeque<_>>();
    let mut values = FloatDeque::from(original.clone());
    let outgoing = values.pop_front().unwrap();
    values.push_front(Some(20.5));
    assert_eq!(bits(values.pop_front().unwrap()), bits(Some(20.5)));
    let checkpoint = values.clone();
    let checkpoint_expected = original.iter().skip(1).copied().collect::<VecDeque<_>>();
    let root = paged(&values).middle.clone();
    let page = paged(&values).front.clone();
    values.restore_front(outgoing);
    assert!(same_directory(&values, &root));
    assert!(!Arc::ptr_eq(&paged(&values).front, &page));
    assert_model(&values, &original);
    assert_model(&checkpoint, &checkpoint_expected);
}

#[test]
fn reverse_restore_across_removed_pages_preserves_both_checkpoints() {
    for removed in [PAGE_SIZE + 1, PAGE_SIZE * 3 + 1, PAGE_SIZE * 8] {
        let original = (0..1025)
            .map(|value| Some(value as f64))
            .collect::<VecDeque<_>>();
        let mut expected = original.clone();
        let mut values = FloatDeque::from(original.clone());
        let before = values.clone();
        let mut evicted = Vec::new();
        for _ in 0..removed {
            let value = values.pop_front().unwrap();
            assert_eq!(bits(value), bits(expected.pop_front().unwrap()));
            evicted.push(value);
        }
        let after = values.clone();
        let after_expected = expected.clone();
        for value in evicted.into_iter().rev() {
            values.restore_front(value);
            expected.push_front(value);
            assert_model(&values, &expected);
        }
        assert_model(&values, &original);
        assert_model(&before, &original);
        assert_model(&after, &after_expected);
    }
}

#[test]
fn single_paged_leaf_restoration_preserves_logical_back_and_stale_tail() {
    let mut expected = (0..129)
        .map(|value| Some(value as f64))
        .collect::<VecDeque<_>>();
    let mut values = FloatDeque::from(expected.clone());
    while expected.len() > 10 {
        assert_eq!(values.pop_back().map(bits), expected.pop_back().map(bits));
    }
    let mut last = None;
    for _ in 0..7 {
        last = values.pop_front();
        assert_eq!(last.map(bits), expected.pop_front().map(bits));
    }
    let checkpoint = values.clone();
    let checkpoint_expected = expected.clone();
    let root = paged(&values).middle.clone();
    let page = paged(&values).front.clone();
    assert_eq!(paged(&values).page_count(), 1);
    let restored = last.unwrap();
    values.restore_front(restored);
    expected.push_front(restored);
    assert!(same_directory(&values, &root));
    assert!(Arc::ptr_eq(&paged(&values).front, &page));
    assert!(matches!(
        &values.storage,
        Storage::Paged(PagedStorage {
            front_offset: 6,
            back_len: 10,
            len: 4,
            ..
        })
    ));
    assert_model(&values, &expected);
    assert_eq!(values.pop_back().map(bits), expected.pop_back().map(bits));
    values.push_back(Some(123.0));
    expected.push_back(Some(123.0));
    assert_model(&values, &expected);
    assert_model(&checkpoint, &checkpoint_expected);
}

#[test]
fn removed_pages_empty_storage_and_short_queues_restore_through_push_front() {
    let mut expected = (0..257)
        .map(|value| Some(value as f64))
        .collect::<VecDeque<_>>();
    let mut values = FloatDeque::from(expected.clone());
    for _ in 0..PAGE_SIZE {
        assert_eq!(values.pop_front().map(bits), expected.pop_front().map(bits));
    }
    assert!(matches!(
        &values.storage,
        Storage::Paged(PagedStorage {
            front_offset: 0,
            ..
        })
    ));
    let checkpoint = values.clone();
    let checkpoint_expected = expected.clone();
    let root = paged(&values).middle.clone();
    values.restore_front(Some(127.0));
    expected.push_front(Some(127.0));
    assert!(!same_directory(&values, &root));
    assert_eq!(paged(&values).front.len(), 1);
    values.restore_front(Some(126.0));
    expected.push_front(Some(126.0));
    assert_model(&values, &expected);
    assert_model(&checkpoint, &checkpoint_expected);
    while values.pop_front().is_some() {}
    values.restore_front(None);
    assert_model(&values, &VecDeque::from([None]));
    assert!(matches!(&values.storage, Storage::Small(_)));
    for length in [0, 1, 2, 127, 128] {
        let mut expected = (0..length)
            .map(|value| Some(value as f64))
            .collect::<VecDeque<_>>();
        let mut values = FloatDeque::from(expected.clone());
        let value = values.pop_front().unwrap_or(None);
        expected.pop_front();
        let checkpoint = values.clone();
        let checkpoint_expected = expected.clone();
        values.restore_front(value);
        expected.push_front(value);
        assert_model(&values, &expected);
        assert_model(&checkpoint, &checkpoint_expected);
    }
}

#[test]
fn clone_and_drop_keep_own_logical_bounds_and_release_shared_storage() {
    let (weak_root, weak_page) = {
        let mut expected = (0..257)
            .map(|value| Some(value as f64))
            .collect::<VecDeque<_>>();
        let mut values = FloatDeque::from(expected.clone());
        let mut evicted = Vec::new();
        for _ in 0..17 {
            evicted.push(values.pop_front().unwrap());
            expected.pop_front();
        }
        let checkpoint = values.clone();
        let checkpoint_expected = expected.clone();
        let root = paged(&values).middle.as_ref().unwrap().clone();
        let weak_root = Arc::downgrade(&root);
        let weak_page = Arc::downgrade(&paged(&values).front);
        let value = evicted.pop().unwrap();
        values.restore_front(value);
        expected.push_front(value);
        let restored_clone = values.clone();
        let restored_expected = expected.clone();
        values.restore_front(evicted.pop().unwrap());
        assert!(Arc::ptr_eq(paged(&values).middle.as_ref().unwrap(), &root));
        drop(values);
        assert_model(&checkpoint, &checkpoint_expected);
        assert_model(&restored_clone, &restored_expected);
        drop(checkpoint);
        assert!(weak_root.upgrade().is_some());
        drop(restored_clone);
        drop(root);
        (weak_root, weak_page)
    };
    assert!(weak_root.upgrade().is_none());
    assert!(weak_page.upgrade().is_none());
}

fn next_random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

fn random_value(state: &mut u64) -> Option<f64> {
    let random = next_random(state);
    match (random >> 32) % 7 {
        0 => None,
        1 => Some(0.0),
        2 => Some(-0.0),
        3 => Some(f64::from_bits(0x7ff8_0000_0000_0000 | (random & 0xffff))),
        _ => Some(((random & 1023) as i64 - 512) as f64 / 8.0),
    }
}

#[test]
fn restore_and_mixed_mutations_match_vecdeque_bit_model() {
    let mut expected = (0..300)
        .map(|value| Some(value as f64))
        .collect::<VecDeque<_>>();
    let mut values = FloatDeque::from(expected.clone());
    let mut state = 0x65fe_149b_7272_86a3;
    for step in 0..4000 {
        match next_random(&mut state) >> 32 & 7 {
            0 => {
                let value = random_value(&mut state);
                values.push_back(value);
                expected.push_back(value);
            }
            1 | 7 => {
                let value = random_value(&mut state);
                values.restore_front(value);
                expected.push_front(value);
            }
            2 => assert_eq!(values.pop_front().map(bits), expected.pop_front().map(bits)),
            3 => assert_eq!(values.pop_back().map(bits), expected.pop_back().map(bits)),
            4 => {
                let value = values.pop_front();
                assert_eq!(value.map(bits), expected.pop_front().map(bits));
                if let Some(value) = value {
                    values.restore_front(value);
                    expected.push_front(value);
                }
            }
            5 => {
                let before = values.clone();
                let before_expected = expected.clone();
                let removed =
                    ((next_random(&mut state) >> 32) as usize % 140 + 1).min(expected.len());
                let mut evicted = Vec::new();
                for _ in 0..removed {
                    let value = values.pop_front().unwrap();
                    assert_eq!(bits(value), bits(expected.pop_front().unwrap()));
                    evicted.push(value);
                }
                let after = values.clone();
                let after_expected = expected.clone();
                for value in evicted.into_iter().rev() {
                    values.restore_front(value);
                    expected.push_front(value);
                }
                assert_model(&before, &before_expected);
                assert_model(&after, &after_expected);
            }
            _ => {
                if step % 127 == 0 {
                    values.clear();
                    expected.clear();
                } else {
                    let value = random_value(&mut state);
                    values.push_front(value);
                    expected.push_front(value);
                }
            }
        }
        assert_model(&values, &expected);
    }
}
