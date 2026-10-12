use super::*;

type Samples = SharedDeque<Option<f64>>;
type Reference = VecDeque<Option<f64>>;

fn sample(index: usize) -> Option<f64> {
    match index % 13 {
        0 => None,
        1 => Some(-0.0),
        2 => Some(0.0),
        3 => Some(f64::from_bits(1)),
        4 => Some(-f64::from_bits(1)),
        5 => Some(1e16),
        6 => Some(1.0),
        7 => Some(-1e16),
        8 => Some(f64::from_bits(0x7ff8_0000_0000_0042)),
        9 => Some(f64::INFINITY),
        10 => Some(f64::NEG_INFINITY),
        _ => Some(index as f64 * 0.125 - 19.0),
    }
}

fn bits(value: &Option<f64>) -> Option<u64> {
    value.map(f64::to_bits)
}

fn points(len: usize) -> Vec<usize> {
    let mut points = [0, 1, 2, 17, 126, 127, 128, 129, 255, 256, 257, len / 2, len]
        .into_iter()
        .map(|point| point.min(len))
        .collect::<Vec<_>>();
    points.extend([len.saturating_sub(2), len.saturating_sub(1)]);
    points.sort_unstable();
    points.dedup();
    points
}

fn fixtures() -> Vec<(Samples, Reference)> {
    let mut fixtures = Vec::new();
    for len in [0, 1, 17, 127, 128, 129, 257, 513, 769, 1025] {
        let reference = (0..len).map(sample).collect::<Reference>();
        fixtures.push((Samples::from(reference.clone()), reference));
    }
    for len in [129, 257, 769, 1025] {
        let mut reference = (0..len).map(sample).collect::<Reference>();
        let mut values = Samples::from(reference.clone());
        for _ in 0..5 {
            assert_eq!(
                values.pop_front().as_ref().map(bits),
                reference.pop_front().as_ref().map(bits)
            );
        }
        for _ in 0..9 {
            assert_eq!(
                values.pop_back().as_ref().map(bits),
                reference.pop_back().as_ref().map(bits)
            );
        }
        fixtures.push((values, reference));
    }
    let mut values = Samples::from(VecDeque::with_capacity(19));
    let mut reference = VecDeque::with_capacity(19);
    for index in 0..17 {
        values.push_back(sample(index));
        reference.push_back(sample(index));
    }
    for index in 17..28 {
        values.pop_front();
        reference.pop_front();
        values.push_back(sample(index));
        reference.push_back(sample(index));
    }
    assert!(values.has_split_storage());
    fixtures.push((values, reference));
    fixtures
}

fn assert_reversed(values: &Samples, expected: &Reference, start: usize, end: usize) {
    let mut actual = values.range(start..end);
    let mut reference = expected.range(start..end);
    loop {
        assert_eq!(actual.len(), reference.len());
        assert_eq!(actual.size_hint(), reference.size_hint());
        let value = actual.next_back().map(bits);
        assert_eq!(value, reference.next_back().map(bits));
        if value.is_none() {
            break;
        }
    }
    assert_eq!(actual.next().map(bits), reference.next().map(bits));
    assert_eq!(
        actual.nth_back(usize::MAX).map(bits),
        reference.nth_back(usize::MAX).map(bits)
    );
    assert_eq!(
        actual.nth(usize::MAX).map(bits),
        reference.nth(usize::MAX).map(bits)
    );
}

#[test]
fn reverse_ranges_match_vecdeque_at_endpoint_and_page_boundaries() {
    for (values, expected) in fixtures() {
        for start in points(expected.len()) {
            for end in points(expected.len())
                .into_iter()
                .filter(|end| *end >= start)
            {
                assert_reversed(&values, &expected, start, end);
                assert_eq!(
                    values.range(start..end).rev().map(bits).collect::<Vec<_>>(),
                    expected
                        .range(start..end)
                        .rev()
                        .map(bits)
                        .collect::<Vec<_>>()
                );
            }
        }
        if !expected.is_empty() {
            let last = expected.len() - 1;
            assert_eq!(
                values.range(..=last).rev().map(bits).collect::<Vec<_>>(),
                expected.range(..=last).rev().map(bits).collect::<Vec<_>>()
            );
            assert_eq!(
                values
                    .range((Bound::Excluded(0), Bound::Included(last)))
                    .rev()
                    .map(bits)
                    .collect::<Vec<_>>(),
                expected
                    .range((Bound::Excluded(0), Bound::Included(last)))
                    .rev()
                    .map(bits)
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn reverse_take_skip_and_nth_back_match_vecdeque_logical_bounds() {
    for (values, expected) in fixtures() {
        let len = expected.len();
        for start in points(len) {
            for end in [start, (start + 129).min(len), len] {
                for limit in [0, 1, 17, 127, 128, 129, len / 2, len, len + 1, usize::MAX] {
                    assert_eq!(
                        values
                            .range(start..end)
                            .take(limit)
                            .rev()
                            .map(bits)
                            .collect::<Vec<_>>(),
                        expected
                            .range(start..end)
                            .take(limit)
                            .rev()
                            .map(bits)
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        values
                            .range(start..end)
                            .skip(limit)
                            .rev()
                            .map(bits)
                            .collect::<Vec<_>>(),
                        expected
                            .range(start..end)
                            .skip(limit)
                            .rev()
                            .map(bits)
                            .collect::<Vec<_>>()
                    );
                    let mut actual = values.range(start..end).rev();
                    let mut reference = expected.range(start..end).rev();
                    assert_eq!(actual.nth(limit).map(bits), reference.nth(limit).map(bits));
                    assert_eq!(actual.next().map(bits), reference.next().map(bits));
                    assert_eq!(
                        actual.next_back().map(bits),
                        reference.next_back().map(bits)
                    );
                    assert_eq!(actual.len(), reference.len());
                    assert_eq!(
                        actual.map(bits).collect::<Vec<_>>(),
                        reference.map(bits).collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}

#[test]
fn reverse_mixed_directions_and_skips_preserve_remaining_fold_order() {
    for (values, expected) in fixtures() {
        let len = expected.len();
        for start in [0, 1.min(len), 127.min(len), len] {
            for end in [start, (start + 257).min(len), len] {
                for skip in [0, 1, 17, 127, 128, 129, usize::MAX] {
                    let mut actual = values.range(start..end);
                    let mut reference = expected.range(start..end);
                    for step in 0..160 {
                        let (a, b) = match step % 11 {
                            3 => (actual.next(), reference.next()),
                            5 => (actual.nth(skip), reference.nth(skip)),
                            7 => (actual.nth_back(skip), reference.nth_back(skip)),
                            _ => (actual.next_back(), reference.next_back()),
                        };
                        assert_eq!(a.map(bits), b.map(bits));
                        assert_eq!(actual.len(), reference.len());
                        assert_eq!(actual.size_hint(), reference.size_hint());
                    }
                    let remaining = reference.len();
                    let append = |mut visited: Vec<_>, value| {
                        visited.push(bits(value));
                        visited
                    };
                    let visited = actual.fold(Vec::new(), append);
                    assert_eq!(visited, reference.fold(Vec::new(), append));
                    assert_eq!(visited.len(), remaining);
                }
            }
        }
    }
}

#[test]
fn reverse_snapshots_preserve_bits_after_endpoint_shrink_and_regrowth() {
    for len in [129, 257, 1025] {
        let mut expected = (0..len).map(sample).collect::<Reference>();
        let mut values = Samples::from(expected.clone());
        let original = values.clone();
        let original_expected = expected.clone();
        while expected.len() > 10 {
            assert_eq!(
                values.pop_back().as_ref().map(bits),
                expected.pop_back().as_ref().map(bits)
            );
        }
        for _ in 0..7 {
            assert_eq!(
                values.pop_front().as_ref().map(bits),
                expected.pop_front().as_ref().map(bits)
            );
        }
        let thin = values.clone();
        let thin_expected = expected.clone();
        for index in 5000..5259 {
            values.push_front(sample(index));
            expected.push_front(sample(index));
            values.push_back(sample(index + 1000));
            expected.push_back(sample(index + 1000));
        }
        let grown = values.clone();
        let grown_expected = expected.clone();
        values.clear();
        expected.clear();
        for index in 9000..9017 {
            values.push_back(sample(index));
            expected.push_back(sample(index));
        }
        for (snapshot, model) in [
            (&original, &original_expected),
            (&thin, &thin_expected),
            (&grown, &grown_expected),
            (&values, &expected),
        ] {
            for start in points(model.len()) {
                assert_reversed(snapshot, model, start, model.len());
                assert_reversed(snapshot, model, start, (start + 129).min(model.len()));
            }
        }
    }
}

#[test]
fn reverse_zst_ranges_and_exhaustion_match_vecdeque() {
    for len in [0, 1, 17, 127, 128, 129, 513] {
        let mut expected = std::iter::repeat_n((), len).collect::<VecDeque<_>>();
        let mut values = SharedDeque::from(expected.clone());
        if len > 128 {
            for _ in 0..17 {
                assert_eq!(values.pop_front(), expected.pop_front());
            }
            for _ in 0..31 {
                assert_eq!(values.pop_back(), expected.pop_back());
            }
        }
        for start in points(expected.len()) {
            for end in [start, (start + 129).min(expected.len()), expected.len()] {
                let mut actual = values.range(start..end);
                let mut reference = expected.range(start..end);
                for step in 0..260 {
                    let (a, b) = match step % 5 {
                        0 => (actual.next(), reference.next()),
                        1 => (actual.nth_back(0), reference.nth_back(0)),
                        2 => (actual.nth(1), reference.nth(1)),
                        _ => (actual.next_back(), reference.next_back()),
                    };
                    assert_eq!(a, b);
                    assert_eq!(actual.len(), reference.len());
                    assert_eq!(actual.size_hint(), reference.size_hint());
                }
                assert_eq!(actual.nth_back(usize::MAX), reference.nth_back(usize::MAX));
                assert_eq!(actual.next(), reference.next());
                assert_eq!(actual.next_back(), reference.next_back());
                assert_eq!(
                    values.range(start..end).take(127).rev().count(),
                    expected.range(start..end).take(127).rev().count()
                );
            }
        }
    }
}

fn stop_reverse<'a>(
    iter: &mut impl Iterator<Item = &'a Option<f64>>,
    stop: usize,
) -> (Result<usize, usize>, Vec<Option<u64>>) {
    let mut visited = Vec::new();
    let result = iter.try_fold(0, |count, value| {
        visited.push(bits(value));
        if visited.len() == stop {
            Err(count)
        } else {
            Ok(count + 1)
        }
    });
    (result, visited)
}

#[test]
fn reverse_short_circuit_resume_matches_vecdeque_remaining_sequence() {
    for (values, expected) in fixtures() {
        let len = expected.len();
        for start in [0, 1.min(len), 127.min(len), len] {
            for stop in [1, 2, 17, 127, 128, 129, 257, usize::MAX] {
                let mut actual = values.range(start..).rev();
                let mut reference = expected.range(start..).rev();
                assert_eq!(
                    stop_reverse(&mut actual, stop),
                    stop_reverse(&mut reference, stop)
                );
                assert_eq!(actual.len(), reference.len());
                assert_eq!(actual.next().map(bits), reference.next().map(bits));
                assert_eq!(actual.nth(1).map(bits), reference.nth(1).map(bits));
                assert_eq!(
                    actual.next_back().map(bits),
                    reference.next_back().map(bits)
                );
                assert_eq!(
                    actual.nth_back(1).map(bits),
                    reference.nth_back(1).map(bits)
                );
                assert_eq!(actual.size_hint(), reference.size_hint());
                assert_eq!(
                    actual.map(bits).collect::<Vec<_>>(),
                    reference.map(bits).collect::<Vec<_>>()
                );
            }
        }
    }
}
