use std::collections::VecDeque;

use super::RollingWindowState;

// The original algorithm uses two independent passes over an ordinary deque.
// It does not call the production range/extreme methods or the paged iterator.
fn original_range(values: &VecDeque<Option<f64>>) -> Option<f64> {
    let highest = values
        .iter()
        .flatten()
        .copied()
        .reduce(|current, value| current.max(value))?;
    let lowest = values
        .iter()
        .flatten()
        .copied()
        .reduce(|current, value| current.min(value))?;
    Some(highest - lowest)
}

fn assert_range(window: &RollingWindowState, expected: &VecDeque<Option<f64>>) {
    assert_eq!(
        window.range().map(f64::to_bits),
        original_range(expected).map(f64::to_bits),
        "range differs for {expected:?}"
    );
    assert_eq!(
        window
            .values
            .iter()
            .map(|value| value.map(f64::to_bits))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|value| value.map(f64::to_bits))
            .collect::<Vec<_>>()
    );
}

#[derive(Clone, Default)]
struct ReferenceWindow {
    values: VecDeque<Option<f64>>,
    open: Option<(usize, VecDeque<Option<f64>>)>,
}

impl ReferenceWindow {
    fn push(&mut self, value: Option<f64>, length: usize) {
        if length == 0 {
            return;
        }
        while self.values.len() >= length {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    fn push_for_bar(&mut self, value: Option<f64>, length: usize, bar: usize) {
        if length == 0 {
            return;
        }
        if let Some((open_bar, before)) = &self.open
            && *open_bar == bar
        {
            self.values = before.clone();
        }
        self.open = Some((bar, self.values.clone()));
        self.push(value, length);
    }

    fn discard_for_bar(&mut self, bar: usize) {
        if self
            .open
            .as_ref()
            .is_some_and(|(open_bar, _)| *open_bar == bar)
        {
            self.values = self.open.take().unwrap().1;
        }
    }
}

fn finite_sample(index: usize) -> Option<f64> {
    match index % 17 {
        0 => None,
        1 => Some(-0.0),
        2 => Some(0.0),
        3 => Some(f64::MAX),
        4 => Some(-f64::MAX),
        5 => Some(f64::from_bits(1)),
        6 => Some(-f64::from_bits(1)),
        _ => Some((index % 113) as f64 * 0.25 - 9.0),
    }
}

#[test]
fn range_matches_original_two_pass_bits_for_raw_sample_combinations() {
    let samples = [
        None,
        Some(-0.0),
        Some(0.0),
        Some(-1.0),
        Some(1.0),
        Some(-f64::MAX),
        Some(f64::MAX),
        Some(f64::from_bits(1)),
        Some(-f64::from_bits(1)),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
        Some(f64::from_bits(0x7ff8_0000_0000_0042)),
        Some(f64::from_bits(0xfff8_0000_0000_0063)),
    ];
    for length in 0..=4 {
        for mut encoded in 0..samples.len().pow(length) {
            let mut expected = VecDeque::new();
            for _ in 0..length {
                expected.push_back(samples[encoded % samples.len()]);
                encoded /= samples.len();
            }
            let window = RollingWindowState {
                values: expected.clone().into(),
                ..RollingWindowState::default()
            };
            assert_range(&window, &expected);
        }
    }
}

#[test]
fn range_preserves_empty_na_filter_readiness_and_overflow_behavior() {
    let mut window = RollingWindowState::default();
    let mut reference = ReferenceWindow::default();
    assert_range(&window, &reference.values);
    assert_eq!(window.range(), None);
    for value in [None, None, None, Some(2.0), None, Some(-1.0)] {
        window.push(value, 3);
        reference.push(value, 3);
        assert_range(&window, &reference.values);
        assert!(!window.is_ready(3));
    }
    assert_eq!(window.range(), Some(3.0));
    for value in [Some(-f64::MAX), Some(f64::MAX), Some(0.0)] {
        window.push(value, 3);
        reference.push(value, 3);
        assert_range(&window, &reference.values);
    }
    assert!(window.is_ready(3));
    assert!(!window.is_ready(4));
    assert_eq!(
        window.range().map(f64::to_bits),
        Some(f64::INFINITY.to_bits())
    );
    window.push(None, 0);
    reference.push(None, 0);
    assert_range(&window, &reference.values);
}

#[test]
fn range_preserves_partial_paged_heads_and_snapshot_branches() {
    let mut window = RollingWindowState::default();
    let mut reference = ReferenceWindow::default();
    for index in 0..1700 {
        window.push(finite_sample(index), 769);
        reference.push(finite_sample(index), 769);
    }
    for _ in 0..51 {
        window.pop_front();
        reference.values.pop_front();
    }
    assert!(window.values.has_split_storage());
    assert_range(&window, &reference.values);
    let snapshot = window.clone();
    let snapshot_reference = reference.clone();
    for (branch, length) in [1, 127, 128, 129, 257, 513, 769, 1025]
        .into_iter()
        .enumerate()
    {
        let mut fork = snapshot.clone();
        let mut fork_reference = snapshot_reference.clone();
        for index in 0..257 {
            let value = finite_sample(1700 + branch * 257 + index);
            fork.push(value, length);
            fork_reference.push(value, length);
            assert_range(&fork, &fork_reference.values);
        }
        assert_range(&snapshot, &snapshot_reference.values);
        assert_range(&window, &reference.values);
    }
}

#[test]
fn range_preserves_dynamic_lengths_replacement_discard_and_checkpoint_bits() {
    let mut window = RollingWindowState::default();
    let mut reference = ReferenceWindow::default();
    for index in 0..1100 {
        window.push(finite_sample(index), 513);
        reference.push(finite_sample(index), 513);
    }
    let before = window.clone();
    let before_reference = reference.clone();
    for (pass, length) in [1, 127, 129, 257, 513, 3, 513].into_iter().enumerate() {
        let value = finite_sample(1100 + pass);
        window.push_for_bar(value, length, 1100);
        reference.push_for_bar(value, length, 1100);
        assert_range(&window, &reference.values);
        let checkpoint = window.clone();
        let checkpoint_reference = reference.clone();
        for (value, replacement_length) in [(Some(-0.0), 2), (None, 129), (Some(0.0), 0)] {
            window.push_for_bar(value, replacement_length, 1100);
            reference.push_for_bar(value, replacement_length, 1100);
            assert_range(&window, &reference.values);
            assert_range(&checkpoint, &checkpoint_reference.values);
        }
        window = checkpoint;
        reference = checkpoint_reference;
    }
    window.discard_for_bar(1099);
    reference.discard_for_bar(1099);
    assert_range(&window, &reference.values);
    window.discard_for_bar(1100);
    reference.discard_for_bar(1100);
    assert_range(&window, &before_reference.values);
    assert_range(&before, &before_reference.values);
    for index in 0..260 {
        let length = [129, 257, 513][index % 3];
        let value = finite_sample(1200 + index);
        window.push(value, length);
        reference.push(value, length);
        assert_range(&window, &reference.values);
        if index % 23 == 0 {
            let bar = 1200 + index;
            for replacement_length in [1, 129, 513] {
                window.push_for_bar(Some(-0.0), replacement_length, bar);
                reference.push_for_bar(Some(-0.0), replacement_length, bar);
                assert_range(&window, &reference.values);
            }
            window.discard_for_bar(bar);
            reference.discard_for_bar(bar);
            assert_range(&window, &reference.values);
        }
    }
    for bar in 1500..1505 {
        window.push_for_bar(finite_sample(bar), 513, bar);
        reference.push_for_bar(finite_sample(bar), 513, bar);
        assert_range(&window, &reference.values);
    }
    window.discard_for_bar(1504);
    reference.discard_for_bar(1504);
    assert_range(&window, &reference.values);
}
