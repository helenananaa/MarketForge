use super::*;

fn paged<T>(values: &SharedDeque<T>) -> &PagedStorage<T> {
    match &values.storage {
        Storage::Paged(paged) => paged,
        Storage::Small(_) => panic!("expected paged storage"),
    }
}

// The public capacity contract counts all physical buffers, including inactive
// endpoint cells. The semantic oracle below does not use the page layout.
fn assert_storage<T>(values: &SharedDeque<T>) {
    let Storage::Paged(storage) = &values.storage else {
        return;
    };
    assert_ne!(storage.len, 0);
    assert!(storage.front_offset < storage.front.len());
    assert!(storage.back_len > 0);
    assert!(storage.back_len <= storage.page(storage.page_count() - 1).len());
    assert_eq!(
        storage.page_count(),
        storage.middle_len() + 1 + usize::from(storage.back.is_some())
    );
    assert_eq!(storage.middle.is_some(), storage.middle_len() != 0);
    assert!(storage.back.is_some() || storage.middle.is_none());
    if storage.back.is_none() {
        assert!(storage.front_offset < storage.back_len);
    }
    let mut capacity = 0_usize;
    for index in 0..storage.page_count() {
        let page = storage.page(index);
        assert!(!page.is_empty() && page.len() <= PAGE_SIZE);
        if index > 0 && index + 1 < storage.page_count() {
            assert_eq!(page.len(), PAGE_SIZE);
        }
        assert_eq!(
            page.capacity(),
            if std::mem::size_of::<T>() == 0 {
                usize::MAX
            } else {
                PAGE_SIZE
            }
        );
        capacity = capacity.saturating_add(page.capacity());
    }
    assert_eq!(values.capacity(), capacity);
    if std::mem::size_of::<T>() != 0 {
        assert!(values.capacity() <= values.len() + 2 * PAGE_SIZE);
    }
}

fn assert_model(values: &SharedDeque<usize>, expected: &VecDeque<usize>) {
    assert_storage(values);
    assert_eq!(values.len(), expected.len());
    assert_eq!(values.front(), expected.front());
    assert_eq!(values.back(), expected.back());
    assert!(values.iter().eq(expected.iter()));
    assert!(values.iter().rev().eq(expected.iter().rev()));
    for index in [
        0,
        1,
        127,
        128,
        129,
        expected.len() / 2,
        expected.len().saturating_sub(1),
        expected.len(),
        usize::MAX,
    ] {
        assert_eq!(values.get(index), expected.get(index));
    }
}

fn middle_identity<T>(storage: &PagedStorage<T>) -> Option<(usize, Vec<(usize, usize)>)> {
    storage.middle.as_ref().map(|middle| {
        (
            Arc::as_ptr(middle) as usize,
            middle
                .iter()
                .map(|page| (Arc::as_ptr(page) as usize, Arc::strong_count(page)))
                .collect(),
        )
    })
}

#[test]
fn nonboundary_endpoint_writes_preserve_middle_ownership_and_other_endpoint() {
    for length in [129, 257, 1025, 100_000] {
        let mut expected = (0..length).collect::<VecDeque<_>>();
        let mut values = SharedDeque::from(expected.clone());
        assert_eq!(values.pop_front(), expected.pop_front());
        let previous = expected.clone();
        let middle = middle_identity(paged(&values));
        let checkpoint = values.clone();
        assert_eq!(
            middle_identity(paged(&values)),
            middle,
            "clone must not clone middle page references"
        );
        let old_front = Arc::as_ptr(&paged(&values).front) as usize;
        let old_back = Arc::as_ptr(paged(&values).back.as_ref().unwrap()) as usize;

        values.push_front(length + 10);
        expected.push_front(length + 10);
        assert_ne!(Arc::as_ptr(&paged(&values).front) as usize, old_front);
        assert_eq!(
            Arc::as_ptr(paged(&values).back.as_ref().unwrap()) as usize,
            old_back
        );
        assert_eq!(middle_identity(paged(&values)), middle);
        let changed_front = Arc::as_ptr(&paged(&values).front) as usize;
        values.push_back(length + 20);
        expected.push_back(length + 20);
        assert_eq!(Arc::as_ptr(&paged(&values).front) as usize, changed_front);
        assert_ne!(
            Arc::as_ptr(paged(&values).back.as_ref().unwrap()) as usize,
            old_back
        );
        assert_eq!(middle_identity(paged(&values)), middle);
        assert_model(&values, &expected);
        assert_model(&checkpoint, &previous);
    }
}

#[test]
fn single_leaf_with_both_inactive_ends_grows_through_two_and_many_pages() {
    for initial_length in [129, 257, 1025] {
        let original = (0..initial_length).collect::<VecDeque<_>>();
        let mut expected = original.clone();
        let mut values = SharedDeque::from(original.clone());
        let original_checkpoint = values.clone();
        while expected.len() > 10 {
            assert_eq!(values.pop_back(), expected.pop_back());
        }
        for _ in 0..7 {
            assert_eq!(values.pop_front(), expected.pop_front());
        }
        let storage = paged(&values);
        assert_eq!(storage.page_count(), 1);
        assert_eq!(storage.front.len(), PAGE_SIZE);
        assert_eq!(storage.front_offset, 7);
        assert_eq!(storage.back_len, 10);
        let ghost_checkpoint = values.clone();
        let ghost_expected = expected.clone();
        for value in 1000..1010 {
            values.push_front(value);
            expected.push_front(value);
            assert_model(&values, &expected);
        }
        assert_eq!(paged(&values).page_count(), 2);
        for value in 2000..2000 + PAGE_SIZE + 1 {
            values.push_back(value);
            expected.push_back(value);
            assert_model(&values, &expected);
        }
        assert!(paged(&values).page_count() >= 3);
        let grown_checkpoint = values.clone();
        let grown_expected = expected.clone();
        while expected.len() > 1 {
            if expected.len() % 2 == 0 {
                assert_eq!(values.pop_front(), expected.pop_front());
            } else {
                assert_eq!(values.pop_back(), expected.pop_back());
            }
            assert_model(&values, &expected);
        }
        assert_model(&original_checkpoint, &original);
        assert_model(&ghost_checkpoint, &ghost_expected);
        assert_model(&grown_checkpoint, &grown_expected);
    }
}

#[test]
fn expired_endpoint_pages_preserve_surviving_bounds_and_branch_snapshots() {
    for length in [129, 256, 257, 385, 1025] {
        let original = (0..length).collect::<VecDeque<_>>();
        let mut expected = original.clone();
        let mut values = SharedDeque::from(original.clone());
        let checkpoint = values.clone();
        for _ in 0..PAGE_SIZE {
            assert_eq!(values.pop_front(), expected.pop_front());
            assert_model(&values, &expected);
        }
        for _ in 0..PAGE_SIZE.min(expected.len() - 1) {
            assert_eq!(values.pop_back(), expected.pop_back());
            assert_model(&values, &expected);
        }
        let surviving_checkpoint = values.clone();
        let surviving_expected = expected.clone();
        for value in 3000..3000 + PAGE_SIZE + 1 {
            values.push_front(value);
            expected.push_front(value);
            values.push_back(value + 1000);
            expected.push_back(value + 1000);
            assert_model(&values, &expected);
        }
        while !expected.is_empty() {
            assert_eq!(values.pop_back(), expected.pop_back());
            assert_model(&values, &expected);
        }
        assert_eq!(values.pop_front(), None);
        assert_eq!(values.pop_back(), None);
        assert_model(&checkpoint, &original);
        assert_model(&surviving_checkpoint, &surviving_expected);
    }
}

#[test]
fn mixed_range_consumption_and_folds_match_vecdeque_across_three_sections() {
    for length in [1, 127, 128, 129, 257, 769, 1025] {
        let mut expected = (0..length).collect::<VecDeque<_>>();
        let mut values = SharedDeque::from(expected.clone());
        for _ in 0..17.min(expected.len() - 1) {
            assert_eq!(values.pop_front(), expected.pop_front());
        }
        for _ in 0..23.min(expected.len() - 1) {
            assert_eq!(values.pop_back(), expected.pop_back());
        }
        for value in 4000..4009 {
            values.push_front(value);
            expected.push_front(value);
        }
        for value in 5000..5011 {
            values.push_back(value);
            expected.push_back(value);
        }
        let len = expected.len();
        for start in [0, 1, 127.min(len), 128.min(len), 129.min(len), len / 3, len] {
            for end in [start, (start + 131).min(len), len] {
                assert!(
                    values
                        .range(start..end)
                        .rev()
                        .eq(expected.range(start..end).rev())
                );
                for skip in [0, 1, 17, 127, 128, 129, 257, usize::MAX] {
                    let mut actual = values.range(start..end);
                    let mut reference = expected.range(start..end);
                    assert_eq!(actual.next(), reference.next());
                    assert_eq!(actual.next_back(), reference.next_back());
                    assert_eq!(actual.nth(skip), reference.nth(skip));
                    assert_eq!(actual.nth_back(skip / 2), reference.nth_back(skip / 2));
                    assert_eq!(actual.len(), reference.len());
                    assert_eq!(actual.size_hint(), reference.size_hint());
                    let append = |mut output: Vec<usize>, value: &usize| {
                        output.push(*value);
                        output
                    };
                    assert_eq!(
                        actual.fold(Vec::new(), append),
                        reference.fold(Vec::new(), append)
                    );

                    // Cancellation makes regrouping or reversing a fold observable.
                    let numeric = |sum: f64, value: &usize| {
                        sum + match value % 3 {
                            0 => 1e16,
                            1 => 1.0,
                            _ => -1e16,
                        }
                    };
                    let mut actual = values.range(start..end);
                    let mut reference = expected.range(start..end);
                    assert_eq!(actual.nth(1), reference.nth(1));
                    assert_eq!(actual.nth_back(skip), reference.nth_back(skip));
                    assert_eq!(
                        actual.fold(0.0, numeric).to_bits(),
                        reference.fold(0.0, numeric).to_bits()
                    );
                }
            }
        }
        assert!(
            values
                .range(..=127.min(len - 1))
                .eq(expected.range(..=127.min(len - 1)))
        );
        assert!(
            values
                .range((Bound::Excluded(0), Bound::Unbounded))
                .eq(expected.range(1..))
        );
        let mut exhausted = values.range(len..len);
        assert_eq!(exhausted.nth(usize::MAX), None);
        assert_eq!(exhausted.nth_back(usize::MAX), None);
        assert_eq!(exhausted.next(), None);
        assert_eq!(exhausted.next_back(), None);
    }
}

#[test]
fn several_checkpoint_branches_keep_independent_endpoint_mutations_and_clear() {
    for length in [129, 257, 1025] {
        let original = (0..length).collect::<VecDeque<_>>();
        let source = SharedDeque::from(original.clone());
        let mut branches = [
            source.clone(),
            source.clone(),
            source.clone(),
            source.clone(),
        ];
        let mut models = [
            original.clone(),
            original.clone(),
            original.clone(),
            original.clone(),
        ];
        for step in 0..512 {
            for branch in 0..branches.len() {
                let values = &mut branches[branch];
                let expected = &mut models[branch];
                let incoming = 6000 + step * 4 + branch;
                match (step + branch) % 7 {
                    0 | 1 => {
                        values.push_front(incoming);
                        expected.push_front(incoming);
                    }
                    2 | 3 => {
                        values.push_back(incoming);
                        expected.push_back(incoming);
                    }
                    4 => assert_eq!(values.pop_front(), expected.pop_front()),
                    5 => assert_eq!(values.pop_back(), expected.pop_back()),
                    _ if step % 43 == 0 => {
                        values.clear();
                        expected.clear();
                    }
                    _ => {
                        let other = values.clone();
                        assert_model(&other, expected);
                    }
                }
            }
            for (values, expected) in branches.iter().zip(&models) {
                assert_model(values, expected);
            }
        }
        assert_model(&source, &original);
    }
}

#[test]
fn expired_pages_and_middle_roots_release_after_their_last_snapshot() {
    let mut values = (0..1025).collect::<SharedDeque<_>>();
    let old_pages = (0..paged(&values).page_count())
        .map(|index| Arc::downgrade(paged(&values).page(index)))
        .collect::<Vec<_>>();
    let old_middle = Arc::downgrade(paged(&values).middle.as_ref().unwrap());
    let checkpoint = values.clone();
    for expected in 0..PAGE_SIZE {
        assert_eq!(values.pop_front(), Some(expected));
    }
    assert!(old_pages[0].upgrade().is_some());
    assert!(old_middle.upgrade().is_some());
    drop(checkpoint);
    assert!(old_pages[0].upgrade().is_none());
    assert!(old_middle.upgrade().is_none());
    assert!(old_pages[1..].iter().all(|page| page.upgrade().is_some()));
    values.clear();
    assert!(old_pages.iter().all(|page| page.upgrade().is_none()));

    for length in [129, 257, 1025] {
        let values = (0..length).collect::<SharedDeque<_>>();
        let pages = (0..paged(&values).page_count())
            .map(|index| Arc::downgrade(paged(&values).page(index)))
            .collect::<Vec<_>>();
        let mut survivor = values.clone();
        drop(values);
        assert!(pages.iter().all(|page| page.upgrade().is_some()));
        survivor.clear();
        assert!(pages.iter().all(|page| page.upgrade().is_none()));
        survivor.push_front(7);
        survivor.push_back(8);
        assert_model(&survivor, &VecDeque::from([7, 8]));
    }
}

#[test]
fn zero_sized_endpoint_pages_match_vecdeque_during_shrink_grow_and_clear() {
    for length in [129, 257, 1025] {
        let mut expected = VecDeque::from(vec![(); length]);
        let mut values = SharedDeque::from(expected.clone());
        let checkpoint = values.clone();
        let check = |values: &SharedDeque<()>, expected: &VecDeque<()>| {
            assert_storage(values);
            assert_eq!(values.len(), expected.len());
            assert_eq!(values.capacity(), expected.capacity());
            assert!(values.iter().eq(expected.iter()));
            assert!(values.iter().rev().eq(expected.iter().rev()));
            let mut actual = values.iter();
            let mut reference = expected.iter();
            assert_eq!(actual.nth(127), reference.nth(127));
            assert_eq!(actual.nth_back(129), reference.nth_back(129));
            assert_eq!(actual.len(), reference.len());
            assert!(actual.eq(reference));
        };
        while expected.len() > 1 {
            if expected.len() % 2 == 0 {
                assert_eq!(values.pop_front(), expected.pop_front());
            } else {
                assert_eq!(values.pop_back(), expected.pop_back());
            }
            check(&values, &expected);
        }
        for _ in 0..1025 {
            values.push_front(());
            expected.push_front(());
            values.push_back(());
            expected.push_back(());
            check(&values, &expected);
        }
        values.clear();
        expected.clear();
        check(&values, &expected);
        assert_eq!(checkpoint.len(), length);
        assert_eq!(checkpoint.capacity(), usize::MAX);
        assert_storage(&checkpoint);
    }
}

fn bits(value: Option<f64>) -> Option<u64> {
    value.map(f64::to_bits)
}

fn assert_float_model(values: &SharedDeque<Option<f64>>, expected: &VecDeque<Option<f64>>) {
    assert_storage(values);
    assert_eq!(values.len(), expected.len());
    assert!(
        values
            .iter()
            .copied()
            .map(bits)
            .eq(expected.iter().copied().map(bits))
    );
    assert!(
        values
            .iter()
            .rev()
            .copied()
            .map(bits)
            .eq(expected.iter().rev().copied().map(bits))
    );
    assert_eq!(
        values.front().copied().map(bits),
        expected.front().copied().map(bits)
    );
    assert_eq!(
        values.back().copied().map(bits),
        expected.back().copied().map(bits)
    );
    for index in [
        0,
        127,
        128,
        expected.len().saturating_sub(1),
        expected.len(),
    ] {
        assert_eq!(
            values.get(index).copied().map(bits),
            expected.get(index).copied().map(bits)
        );
    }
}

#[test]
fn exact_bit_restore_and_changed_samples_preserve_the_shared_middle() {
    let nan_a = Some(f64::from_bits(0x7ff8_0000_0000_0011));
    let nan_b = Some(f64::from_bits(0x7ff8_0000_0000_0022));
    let signaling_nan = Some(f64::from_bits(0x7ff0_0000_0000_0001));
    for length in [129, 257, 1025] {
        for (outgoing, incoming) in [
            (Some(0.0), Some(0.0)),
            (Some(-0.0), Some(-0.0)),
            (nan_a, nan_a),
            (signaling_nan, signaling_nan),
            (None, None),
            (Some(0.0), Some(-0.0)),
            (Some(-0.0), Some(0.0)),
            (nan_a, nan_b),
            (None, nan_a),
            (nan_a, None),
        ] {
            let mut expected = (0..length)
                .map(|index| Some(index as f64))
                .collect::<VecDeque<_>>();
            expected[0] = outgoing;
            let mut values = SharedDeque::from(expected.clone());
            assert_eq!(values.pop_front().map(bits), expected.pop_front().map(bits));
            let previous = expected.clone();
            let checkpoint = values.clone();
            let front = Arc::as_ptr(&paged(&values).front) as usize;
            let back = Arc::as_ptr(paged(&values).back.as_ref().unwrap()) as usize;
            let middle = middle_identity(paged(&values));
            values.restore_front(incoming);
            expected.push_front(incoming);
            assert_eq!(
                Arc::as_ptr(&paged(&values).front) as usize == front,
                bits(outgoing) == bits(incoming)
            );
            assert_eq!(
                Arc::as_ptr(paged(&values).back.as_ref().unwrap()) as usize,
                back
            );
            assert_eq!(middle_identity(paged(&values)), middle);
            assert_float_model(&values, &expected);
            assert_float_model(&checkpoint, &previous);
        }
        let original = (0..length)
            .map(|index| match index % 5 {
                0 => None,
                1 => Some(-0.0),
                2 => nan_a,
                3 => signaling_nan,
                _ => Some(index as f64),
            })
            .collect::<VecDeque<_>>();
        let mut expected = original.clone();
        let mut values = SharedDeque::from(original.clone());
        let before = values.clone();
        let mut removed = Vec::new();
        for _ in 0..(PAGE_SIZE + 1).min(length) {
            let sample = values.pop_front().unwrap();
            assert_eq!(bits(sample), bits(expected.pop_front().unwrap()));
            removed.push(sample);
        }
        let after = values.clone();
        let after_expected = expected.clone();
        for sample in removed.into_iter().rev() {
            values.restore_front(sample);
            expected.push_front(sample);
            assert_float_model(&values, &expected);
        }
        assert_float_model(&values, &original);
        assert_float_model(&before, &original);
        assert_float_model(&after, &after_expected);
    }
}
