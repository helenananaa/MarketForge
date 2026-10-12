use super::*;

type Samples = SharedDeque<Option<f64>>;
type Reference = VecDeque<Option<f64>>;

fn bits(value: &Option<f64>) -> Option<u64> {
    value.map(f64::to_bits)
}

fn sample(index: usize) -> Option<f64> {
    match index % 13 {
        0 | 9 => None,
        1 => Some(-0.0),
        2 => Some(0.0),
        3 => Some(f64::from_bits(1)),
        4 => Some(-f64::from_bits(1)),
        5 => Some(1e16),
        6 => Some(1.0),
        7 => Some(-1e16),
        8 => Some(0.1),
        10 => Some(-0.3),
        _ => Some((index % 257) as f64 / 8.0 - 16.0),
    }
}

fn assert_model(values: &Samples, expected: &Reference) {
    assert_eq!(values.len(), expected.len());
    assert!(values.iter().map(bits).eq(expected.iter().map(bits)));
    assert!(
        values
            .iter()
            .rev()
            .map(bits)
            .eq(expected.iter().rev().map(bits))
    );
    assert_eq!(values.front().map(bits), expected.front().map(bits));
    assert_eq!(values.back().map(bits), expected.back().map(bits));
}

fn fixtures() -> Vec<(Samples, Reference)> {
    let mut fixtures = Vec::new();
    let mut small = Samples::from(VecDeque::with_capacity(11));
    let mut small_reference = VecDeque::with_capacity(11);
    for index in 0..75 {
        if small_reference.len() == 11 {
            assert_eq!(
                small.pop_front().as_ref().map(bits),
                small_reference.pop_front().as_ref().map(bits)
            );
        }
        small.push_back(sample(index));
        small_reference.push_back(sample(index));
        if !small_reference.as_slices().1.is_empty() {
            break;
        }
    }
    assert!(small.has_split_storage());
    fixtures.push((small, small_reference));

    for length in [129, 257, 769, 1025] {
        let mut expected = (0..length).map(sample).collect::<Reference>();
        let mut values = Samples::from(expected.clone());
        for _ in 0..17 {
            assert_eq!(
                values.pop_front().as_ref().map(bits),
                expected.pop_front().as_ref().map(bits)
            );
        }
        for _ in 0..31 {
            assert_eq!(
                values.pop_back().as_ref().map(bits),
                expected.pop_back().as_ref().map(bits)
            );
        }
        for index in 7000..7019 {
            values.push_front(sample(index));
            expected.push_front(sample(index));
        }
        for index in 8000..8013 {
            values.push_back(sample(index));
            expected.push_back(sample(index));
        }
        fixtures.push((values, expected));
    }

    let mut expected = (0..129).map(sample).collect::<Reference>();
    let mut values = Samples::from(expected.clone());
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
    assert_eq!(values.len(), 3);
    assert_eq!(values.capacity(), PAGE_SIZE);
    fixtures.push((values, expected));
    fixtures
}

fn assert_aggregates(values: &Samples, expected: &Reference, start: usize, end: usize) {
    let actual_sum = values.range(start..end).flatten().copied().sum::<f64>();
    let reference_sum = expected.range(start..end).flatten().copied().sum::<f64>();
    assert_eq!(actual_sum.to_bits(), reference_sum.to_bits());
    for initial in [-0.0, 0.0, f64::from_bits(1), -f64::from_bits(1), 0.1, -7.25] {
        let actual = values
            .range(start..end)
            .flatten()
            .fold(initial, |sum, value| sum + *value);
        let reference = expected
            .range(start..end)
            .flatten()
            .fold(initial, |sum, value| sum + *value);
        assert_eq!(actual.to_bits(), reference.to_bits());
    }
    let squared = |value: &f64| {
        let difference = *value - 0.125;
        difference * difference
    };
    assert_eq!(
        values
            .range(start..end)
            .flatten()
            .map(squared)
            .sum::<f64>()
            .to_bits(),
        expected
            .range(start..end)
            .flatten()
            .map(squared)
            .sum::<f64>()
            .to_bits()
    );
    let weighted = |(index, value): (usize, &f64)| *value * (index + 1) as f64;
    assert_eq!(
        values
            .range(start..end)
            .flatten()
            .enumerate()
            .map(weighted)
            .sum::<f64>()
            .to_bits(),
        expected
            .range(start..end)
            .flatten()
            .enumerate()
            .map(weighted)
            .sum::<f64>()
            .to_bits()
    );
    let mut actual_visits = Vec::new();
    let mut actual_weighted = -0.0;
    values.range(start..end).flatten().for_each(|value| {
        actual_visits.push(value.to_bits());
        actual_weighted += *value * actual_visits.len() as f64;
    });
    let mut reference_visits = Vec::new();
    let mut reference_weighted = -0.0;
    expected.range(start..end).flatten().for_each(|value| {
        reference_visits.push(value.to_bits());
        reference_weighted += *value * reference_visits.len() as f64;
    });
    assert_eq!(actual_visits, reference_visits);
    assert_eq!(actual_weighted.to_bits(), reference_weighted.to_bits());
}

#[test]
fn empty_and_signed_zero_scans_preserve_std_sum_and_fold_initial_bits() {
    for length in [0, 1, 4, 127, 128, 129, 257, 769] {
        for pattern in 0..4 {
            let expected = (0..length)
                .map(|index| match pattern {
                    0 => None,
                    1 => Some(-0.0),
                    2 => Some(0.0),
                    _ => match index % 3 {
                        0 => None,
                        1 => Some(-0.0),
                        _ => Some(0.0),
                    },
                })
                .collect::<Reference>();
            let values = Samples::from(expected.clone());
            assert_aggregates(&values, &expected, 0, length);
            if pattern == 0 || pattern == 1 {
                assert_eq!(
                    values.iter().flatten().copied().sum::<f64>().to_bits(),
                    (-0.0_f64).to_bits()
                );
            }
            for initial_bits in [
                0,
                1_u64 << 63,
                1,
                0x7ff8_0000_0000_0011,
                0xfff8_0000_0000_0022,
            ] {
                let initial = f64::from_bits(initial_bits);
                for endpoint in [0, length] {
                    let mut called = 0;
                    let result = values.range(endpoint..endpoint).fold(initial, |_, _| {
                        called += 1;
                        1.0
                    });
                    assert_eq!(result.to_bits(), initial_bits);
                    assert_eq!(called, 0);
                }
                for reverse in [false, true] {
                    let mut actual = values.iter();
                    let mut reference = expected.iter();
                    let exhausted = if reverse {
                        actual.nth_back(usize::MAX)
                    } else {
                        actual.nth(usize::MAX)
                    };
                    let expected_exhausted = if reverse {
                        reference.nth_back(usize::MAX)
                    } else {
                        reference.nth(usize::MAX)
                    };
                    assert_eq!(exhausted.map(bits), expected_exhausted.map(bits));
                    assert_eq!(actual.len(), 0);
                    assert_eq!(actual.next(), None);
                    assert_eq!(actual.next_back(), None);
                    let mut called = 0;
                    let result = actual.fold(initial, |_, _| {
                        called += 1;
                        1.0
                    });
                    assert_eq!(result.to_bits(), initial_bits);
                    assert_eq!(called, 0);
                }
                if pattern == 0 {
                    let mut called = 0;
                    let result = values.iter().flatten().fold(initial, |_, _| {
                        called += 1;
                        1.0
                    });
                    assert_eq!(result.to_bits(), initial_bits);
                    assert_eq!(called, 0);
                }
            }
        }
    }
}

#[test]
fn float_adapter_scans_match_vecdeque_across_partial_and_ghost_pages() {
    for (values, expected) in fixtures() {
        assert_model(&values, &expected);
        let len = expected.len();
        for start in [
            0,
            1.min(len),
            3.min(len),
            127.min(len),
            128.min(len),
            129.min(len),
            len / 2,
            len,
        ] {
            for end in [start, (start + 1).min(len), (start + 129).min(len), len] {
                assert_aggregates(&values, &expected, start, end);
                assert!(
                    values
                        .range(start..end)
                        .map(bits)
                        .eq(expected.range(start..end).map(bits))
                );
            }
        }
        assert!(
            values
                .range(..=127.min(len - 1))
                .map(bits)
                .eq(expected.range(..=127.min(len - 1)).map(bits))
        );
        assert!(
            values
                .range((Bound::Excluded(0), Bound::Unbounded))
                .map(bits)
                .eq(expected.range(1..).map(bits))
        );
    }
}

fn consume_mixed(
    actual: &mut Iter<'_, Option<f64>>,
    reference: &mut std::collections::vec_deque::Iter<'_, Option<f64>>,
    skip: usize,
) {
    assert_eq!(actual.next().map(bits), reference.next().map(bits));
    assert_eq!(
        actual.next_back().map(bits),
        reference.next_back().map(bits)
    );
    assert_eq!(actual.nth(skip).map(bits), reference.nth(skip).map(bits));
    assert_eq!(
        actual.nth_back(skip / 2).map(bits),
        reference.nth_back(skip / 2).map(bits)
    );
    assert_eq!(
        actual.next_back().map(bits),
        reference.next_back().map(bits)
    );
    assert_eq!(actual.next().map(bits), reference.next().map(bits));
    assert_eq!(actual.len(), reference.len());
    assert_eq!(actual.size_hint(), reference.size_hint());
}

#[test]
fn mixed_iterator_directions_and_skips_preserve_remaining_float_fold_order() {
    for (values, expected) in fixtures() {
        let len = expected.len();
        for start in [0, 1.min(len), 127.min(len), 128.min(len), 129.min(len), len] {
            for end in [start, (start + 257).min(len), len] {
                for skip in [0, 1, 17, 127, 128, 129, 257, usize::MAX] {
                    let mut actual = values.range(start..end);
                    let mut reference = expected.range(start..end);
                    consume_mixed(&mut actual, &mut reference, skip);
                    let remaining = reference.len();
                    let append = |mut output: Vec<Option<u64>>, value: &Option<f64>| {
                        output.push(bits(value));
                        output
                    };
                    let visited = actual.fold(Vec::new(), append);
                    assert_eq!(visited, reference.fold(Vec::new(), append));
                    assert_eq!(visited.len(), remaining);
                    for initial in [-0.0, 0.0, 0.1] {
                        let mut actual = values.range(start..end);
                        let mut reference = expected.range(start..end);
                        consume_mixed(&mut actual, &mut reference, skip);
                        let add = |sum, value: &f64| sum + *value;
                        assert_eq!(
                            actual.flatten().fold(initial, add).to_bits(),
                            reference.flatten().fold(initial, add).to_bits()
                        );
                    }
                    let mut actual = values.range(start..end);
                    let mut reference = expected.range(start..end);
                    consume_mixed(&mut actual, &mut reference, skip);
                    assert_eq!(
                        actual.flatten().copied().sum::<f64>().to_bits(),
                        reference.flatten().copied().sum::<f64>().to_bits()
                    );
                }
            }
        }
    }
}

fn stop_scan<'a>(
    iter: &mut impl Iterator<Item = &'a Option<f64>>,
    stop: usize,
) -> (Result<f64, usize>, usize) {
    let mut visits = 0;
    let result = iter.try_fold(-0.0, |sum, value| {
        visits += 1;
        if visits == stop {
            Err(visits)
        } else {
            Ok(value.map_or(sum, |value| sum + value))
        }
    });
    (result, visits)
}

#[test]
fn checkpoint_scans_and_short_circuit_resume_preserve_logical_bounds() {
    let original = (0..129).map(sample).collect::<Reference>();
    let mut expected = original.clone();
    let mut values = Samples::from(original.clone());
    let before = values.clone();
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
    let ghost = values.clone();
    let ghost_expected = expected.clone();
    for index in 9000..9259 {
        values.push_front(sample(index));
        expected.push_front(sample(index));
        values.push_back(sample(index + 1000));
        expected.push_back(sample(index + 1000));
    }
    let grown = values.clone();
    let grown_expected = expected.clone();
    values.clear();
    expected.clear();
    for value in [None, Some(-0.0), Some(0.1)] {
        values.push_back(value);
        expected.push_back(value);
    }
    for (snapshot, model) in [
        (&before, &original),
        (&ghost, &ghost_expected),
        (&grown, &grown_expected),
        (&values, &expected),
    ] {
        assert_model(snapshot, model);
        assert_aggregates(snapshot, model, 0, model.len());
        for start in [0, 1, model.len()] {
            for stop in [1, 2, 3, 17, 127, 128, 129, 257, usize::MAX] {
                let mut actual = snapshot.range(start..);
                let mut reference = model.range(start..);
                let (result, visits) = stop_scan(&mut actual, stop);
                let (expected_result, expected_visits) = stop_scan(&mut reference, stop);
                assert_eq!(result.map(f64::to_bits), expected_result.map(f64::to_bits));
                assert_eq!(visits, expected_visits);
                assert_eq!(actual.len(), reference.len());
                assert_eq!(actual.next().map(bits), reference.next().map(bits));
                assert_eq!(
                    actual.next_back().map(bits),
                    reference.next_back().map(bits)
                );
                assert_eq!(actual.nth(1).map(bits), reference.nth(1).map(bits));
                assert_eq!(
                    actual.nth_back(1).map(bits),
                    reference.nth_back(1).map(bits)
                );
                assert_eq!(actual.size_hint(), reference.size_hint());
                let append = |mut output: Vec<Option<u64>>, value: &Option<f64>| {
                    output.push(bits(value));
                    output
                };
                assert_eq!(
                    actual.fold(Vec::new(), append),
                    reference.fold(Vec::new(), append)
                );
            }
        }
    }
}
