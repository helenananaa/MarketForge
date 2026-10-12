//! Compare observable window results with the pre-removal algorithm. The
//! reference uses a plain Vec and full undo snapshots, independent of the
//! production deque and its eviction log.
use std::collections::VecDeque;

use super::*;

#[test]
fn paged_scans_and_large_same_bar_eviction_log_preserve_legacy_bits() {
    let mut current = RollingWindowState::default();
    let mut legacy = LegacyWindow::default();
    for index in 0..5100 {
        current.push(ordinary_sample(index), 5000);
        legacy.samples.push(ordinary_sample(index), 5000);
    }
    assert!(current.values.has_split_storage());
    assert_same(&current, &legacy.samples, 5000);
    let committed = current.clone();
    let committed_legacy = legacy.clone();
    for (pass, length) in [1, 127, 129, 257, 5000, 3, 5000].into_iter().enumerate() {
        current.push_for_bar(ordinary_sample(5100 + pass), length, 5100);
        legacy.push_for_bar(ordinary_sample(5100 + pass), length, 5100);
        assert_same(&current, &legacy.samples, length);
        assert_hma_scan_pair(&current, &legacy.samples, length);
        if length == 1 {
            assert_eq!(current.evicted.len(), 5000);
            assert!(current.evicted.has_split_storage());
        }
        let checkpoint = current.clone();
        let previous = legacy.clone();
        current.push_for_bar(Some(-0.0), 2, 5100);
        legacy.push_for_bar(Some(-0.0), 2, 5100);
        assert_same(&current, &legacy.samples, 2);
        current = checkpoint;
        legacy = previous;
        assert_same(&current, &legacy.samples, length);
    }
    current.discard_for_bar(5100);
    legacy.discard_for_bar(5100);
    assert_same(&current, &legacy.samples, 5000);
    assert_eq!(current.values, committed.values);
    assert_eq!(current.sum.to_bits(), committed.sum.to_bits());
    assert_same(&current, &committed_legacy.samples, 5000);
}

#[derive(Clone, Default)]
struct LegacySamples {
    values: Vec<Option<f64>>,
    sum: f64,
    square_sum: f64,
    na_count: usize,
    nonzero_count: usize,
}

impl LegacySamples {
    fn remove_first(&mut self) -> Option<f64> {
        let removed = self.values.remove(0);
        if let Some(value) = removed {
            self.sum -= value;
            self.square_sum -= value * value;
            self.nonzero_count -= usize::from(value != 0.0);
        } else {
            self.na_count = self.na_count.saturating_sub(1);
        }
        self.reset_zeros();
        removed
    }

    fn append(&mut self, value: Option<f64>) {
        if let Some(value) = value {
            self.sum += value;
            self.square_sum += value * value;
            self.nonzero_count += usize::from(value != 0.0);
        } else {
            self.na_count += 1;
        }
        self.values.push(value);
    }

    fn reset_zeros(&mut self) {
        if self.nonzero_count == 0 && self.na_count == 0 {
            self.sum = 0.0;
            self.square_sum = 0.0;
        }
    }

    fn push(&mut self, value: Option<f64>, length: usize) {
        while self.values.len() >= length {
            self.remove_first();
        }
        self.append(value);
        self.reset_zeros();
    }

    fn mean(&self, length: usize) -> f64 {
        self.sum / length as f64
    }

    fn variance(&self, length: usize, biased: bool) -> f64 {
        if !biased && length < 2 {
            return f64::NAN;
        }
        let mean = self.mean(length);
        let squared_diff_sum = self
            .values
            .iter()
            .flatten()
            .map(|value| {
                let diff = *value - mean;
                diff * diff
            })
            .sum::<f64>();
        let denominator = if biased { length } else { length - 1 };
        (squared_diff_sum / denominator as f64).max(0.0)
    }

    fn weighted_mean(&self, length: usize) -> f64 {
        let numerator = self
            .values
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, value)| *value * (index + 1) as f64)
            .sum::<f64>();
        numerator / (length * (length + 1) / 2) as f64
    }
}

#[derive(Clone, Default)]
struct LegacyWindow {
    samples: LegacySamples,
    open_bar: Option<usize>,
    before_append: Option<LegacySamples>,
}

impl LegacyWindow {
    fn push_for_bar(&mut self, value: Option<f64>, length: usize, bar: usize) {
        if self.open_bar == Some(bar) {
            self.samples = self.before_append.as_ref().unwrap().clone();
        }
        self.open_bar = Some(bar);
        self.before_append = Some(self.samples.clone());
        let previous_sum = self.samples.sum;
        let mut removed = Vec::new();
        while self.samples.values.len() >= length {
            removed.push(self.samples.remove_first());
        }
        self.samples.append(value);
        if removed.len() == 1
            && let (Some(incoming), Some(outgoing)) = (value, removed[0])
            && incoming.is_sign_positive() == outgoing.is_sign_positive()
            && incoming.abs() >= outgoing.abs() * 0.5
            && outgoing.abs() >= incoming.abs() * 0.5
        {
            self.samples.sum = previous_sum + (incoming - outgoing);
        }
        self.samples.reset_zeros();
    }

    fn discard_for_bar(&mut self, bar: usize) {
        if self.open_bar == Some(bar) {
            self.samples = self.before_append.take().unwrap();
            self.open_bar = None;
        }
    }
}

fn sample(index: usize) -> Option<f64> {
    match index % 13 {
        0 => None,
        1 => Some(-0.0),
        2 => Some(0.0),
        3 => Some(f64::from_bits(1)),
        4 => Some(-f64::from_bits(1)),
        5 => Some(1053.7),
        6 => Some(1029.4),
        7 => Some(1e16),
        8 => Some(1.0),
        9 => Some(-1e16),
        // The dead square aggregate overflows on this finite source value.
        10 => Some(1e200),
        11 => Some(-1e200),
        _ => Some(-1088.2),
    }
}

// Keep ordinary arithmetic controls away from the proven lost-tail correction
// cases. The original extreme sample sequence remains in the independent HMA
// scan/output tests, and recovery has separate mathematical regression tests.
fn ordinary_sample(index: usize) -> Option<f64> {
    match index % 13 {
        0 => None,
        1 => Some(-0.0),
        2 => Some(0.0),
        3 => Some(1.25),
        4 => Some(-1.25),
        5 => Some(1053.7),
        6 => Some(1029.4),
        7 => Some(1041.3),
        8 => Some(1.0),
        9 => Some(-1041.3),
        10 => Some(1e4),
        11 => Some(-1e4),
        _ => Some(-1088.2),
    }
}

fn assert_same_samples(current: &RollingWindowState, legacy: &LegacySamples, length: usize) {
    assert_eq!(
        current
            .values
            .iter()
            .map(|value| value.map(f64::to_bits))
            .collect::<Vec<_>>(),
        legacy
            .values
            .iter()
            .map(|value| value.map(f64::to_bits))
            .collect::<Vec<_>>()
    );
    assert_eq!(current.na_count, legacy.na_count);
    assert_eq!(current.nonzero_count, legacy.nonzero_count);
    assert_eq!(
        current.is_ready(length),
        legacy.values.len() == length && legacy.na_count == 0
    );
}

fn assert_same(current: &RollingWindowState, legacy: &LegacySamples, length: usize) {
    assert_same_samples(current, legacy, length);
    assert_eq!(current.sum.to_bits(), legacy.sum.to_bits());
    assert_eq!(
        current.mean(length).to_bits(),
        legacy.mean(length).to_bits()
    );
    for biased in [false, true] {
        assert_eq!(
            current.variance(length, biased).to_bits(),
            legacy.variance(length, biased).to_bits()
        );
    }
    assert_eq!(
        current.weighted_mean(length).to_bits(),
        legacy.weighted_mean(length).to_bits()
    );
    let mean = legacy.mean(length);
    let deviation = legacy
        .values
        .iter()
        .flatten()
        .map(|value| (*value - mean).abs())
        .sum::<f64>()
        / length as f64;
    assert_eq!(
        current.mean_absolute_deviation(length).to_bits(),
        deviation.to_bits()
    );
    let gravity = -legacy
        .values
        .iter()
        .flatten()
        .enumerate()
        .map(|(index, value)| *value * (length - index) as f64)
        .sum::<f64>()
        / legacy.sum;
    assert_eq!(
        current.center_of_gravity(length).to_bits(),
        gravity.to_bits()
    );
}

fn assert_hma_scan_pair(current: &RollingWindowState, legacy: &LegacySamples, length: usize) {
    let half_length = (length / 2).max(1).min(legacy.values.len());
    let old_half = LegacySamples {
        values: legacy.values[legacy.values.len() - half_length..].to_vec(),
        ..Default::default()
    };
    let (full_mean, half_mean) = current.weighted_mean_with_tail(length, half_length);
    let assert_scan = |actual: f64, old: f64, values: &[Option<f64>]| {
        if old.is_finite() || !current.is_ready(length) || !actual.is_finite() {
            assert_eq!(actual.to_bits(), old.to_bits());
        } else {
            // A complete finite source may now recover an overflowing legacy
            // scan. Independent exact Fraction oracles qualify the recovered
            // values in weighted_recovery_tests; retain the convex bound here.
            assert!(values.iter().all(|value| value.is_some_and(f64::is_finite)));
            let minimum = values.iter().flatten().copied().reduce(f64::min).unwrap();
            let maximum = values.iter().flatten().copied().reduce(f64::max).unwrap();
            assert!((minimum..=maximum).contains(&actual));
        }
    };
    assert_scan(full_mean, legacy.weighted_mean(length), &legacy.values);
    assert_scan(
        half_mean,
        old_half.weighted_mean(half_length),
        &old_half.values,
    );
}

#[test]
fn hma_fused_scan_matches_old_double_scan_for_wrapped_na_zero_and_extreme_samples() {
    let mut current = RollingWindowState {
        values: VecDeque::with_capacity(7).into(),
        ..Default::default()
    };
    let mut legacy = LegacySamples::default();
    let mut wrapped = false;
    let mut overflow_seen = false;
    let mut nan_seen = false;
    for index in 0..2048 {
        let length = [1, 2, 3, 7, 8, 31, 32, 127][(index / 128) % 8];
        let value = match index % 29 {
            0 => Some(f64::MAX),
            1 => Some(-f64::MAX),
            2 => Some(f64::MIN_POSITIVE),
            _ => sample(index),
        };
        current.push(value, length);
        legacy.push(value, length);
        wrapped |= current.values.has_split_storage();
        overflow_seen |= legacy.weighted_mean(length).is_infinite();
        nan_seen |= legacy.weighted_mean(length).is_nan();
        assert_hma_scan_pair(&current, &legacy, length);
    }
    assert!(wrapped, "exercise both backing slices of the deque");
    assert!(overflow_seen, "exercise overflowing weighted terms");
    assert!(nan_seen, "exercise opposite infinite partial sums");

    for value in [
        None,
        Some(0.0),
        Some(-0.0),
        Some(f64::from_bits(1)),
        Some(-f64::from_bits(1)),
    ] {
        let mut current = RollingWindowState::default();
        let mut legacy = LegacySamples::default();
        for _ in 0..8 {
            current.push(value, 8);
            legacy.push(value, 8);
            assert_hma_scan_pair(&current, &legacy, 8);
        }
    }
}

#[test]
fn hma_fused_scan_keeps_old_bits_after_same_bar_undo_discard_and_checkpoint_restore() {
    let mut current = RollingWindowState::default();
    let mut legacy = LegacyWindow::default();
    for bar in 0..128 {
        for (pass, length) in [17, 3, 29, 1, 11].into_iter().enumerate() {
            let value = match (bar * 5 + pass) % 31 {
                0 => Some(f64::MAX),
                1 => Some(-f64::MAX),
                _ => sample(bar * 5 + pass),
            };
            current.push_for_bar(value, length, bar);
            legacy.push_for_bar(value, length, bar);
            assert_hma_scan_pair(&current, &legacy.samples, length);
            if pass == 2 {
                let checkpoint = current.clone();
                let legacy_checkpoint = legacy.clone();
                current.push_for_bar(Some(999.0), 2, bar);
                legacy.push_for_bar(Some(999.0), 2, bar);
                assert_hma_scan_pair(&current, &legacy.samples, 2);
                current = checkpoint;
                legacy = legacy_checkpoint;
                assert_hma_scan_pair(&current, &legacy.samples, length);
            }
        }
        current.discard_for_bar(bar + 1);
        legacy.discard_for_bar(bar + 1);
        assert_hma_scan_pair(&current, &legacy.samples, 11);
        if bar.is_multiple_of(3) {
            current.discard_for_bar(bar);
            legacy.discard_for_bar(bar);
            assert_hma_scan_pair(&current, &legacy.samples, 11);
        }
    }
}

#[test]
fn ordinary_push_outputs_match_legacy_bits_through_wrapped_deque_na_and_length_changes() {
    let mut current = RollingWindowState {
        values: VecDeque::with_capacity(7).into(),
        ..Default::default()
    };
    let mut legacy = LegacySamples::default();
    let mut wrapped = false;
    for index in 0..2048 {
        let length = match (index / 64) % 4 {
            0 => 7,
            1 => 3,
            2 => 11,
            _ => 1,
        };
        current.push(ordinary_sample(index), length);
        legacy.push(ordinary_sample(index), length);
        wrapped |= current.values.has_split_storage();
        assert_same(&current, &legacy, length);
        if index.is_multiple_of(19) {
            current.pop_front();
            legacy.remove_first();
            assert_same(&current, &legacy, length);
        }
    }
    assert!(wrapped, "exercise both backing slices of the deque");
}

#[test]
fn bar_replacement_discard_and_checkpoints_match_legacy_bits_after_length_changes() {
    let mut current = RollingWindowState::default();
    let mut legacy = LegacyWindow::default();
    for bar in 0..512 {
        let original = current.clone();
        let legacy_original = legacy.clone();
        for (pass, length) in [17, 3, 29, 1, 11].into_iter().enumerate() {
            let value = ordinary_sample(bar * 5 + pass);
            current.push_for_bar(value, length, bar);
            legacy.push_for_bar(value, length, bar);
            assert_same(&current, &legacy.samples, length);
            let evicted = legacy
                .before_append
                .as_ref()
                .unwrap()
                .values
                .len()
                .saturating_add(1)
                .saturating_sub(legacy.samples.values.len());
            assert_eq!(current.retained_values(), current.values.len() + evicted);
            if pass == 2 {
                let checkpoint = current.clone();
                let legacy_checkpoint = legacy.clone();
                current.push_for_bar(Some(999.0), 2, bar);
                legacy.push_for_bar(Some(999.0), 2, bar);
                assert_same(&current, &legacy.samples, 2);
                current = checkpoint;
                legacy = legacy_checkpoint;
            }
        }
        current.discard_for_bar(bar + 1);
        legacy.discard_for_bar(bar + 1);
        assert_same(&current, &legacy.samples, 11);
        if bar.is_multiple_of(3) {
            current.discard_for_bar(bar);
            legacy.discard_for_bar(bar);
            assert_same(&current, &legacy.samples, 11);
            assert_eq!(current.values, original.values);
            assert_eq!(current.sum.to_bits(), original.sum.to_bits());
            assert_eq!(legacy.samples.values, legacy_original.samples.values);
        }
    }
}

#[test]
fn hma_half_ready_window_is_exact_full_window_suffix_under_dynamic_lengths() {
    let mut half = LegacySamples::default();
    let mut full = LegacySamples::default();
    let lengths = [1, 2, 3, 7, 8, 31, 32, 127, 4, 65];
    let mut ready = 0;
    for index in 0..4096 {
        // Multiple samples represent repeated calls even within one script bar.
        let length = lengths[(index / 128) % lengths.len()];
        let half_length = (length / 2).max(1);
        let source = if index.is_multiple_of(131) {
            None
        } else {
            sample(index).or(Some(-0.0))
        };
        half.push(source, half_length);
        full.push(source, length);
        assert!(full.values.len() <= 2 * half.values.len() + 1);
        assert_eq!(
            half.values,
            full.values[full.values.len() - half.values.len()..]
        );
        if full.values.len() == length && full.na_count == 0 {
            assert_eq!(half.values.len(), half_length);
            assert_eq!(half.na_count, 0);
            let numerator = full.values[full.values.len() - half_length..]
                .iter()
                .flatten()
                .enumerate()
                .map(|(index, value)| *value * (index + 1) as f64)
                .sum::<f64>();
            let derived = numerator / (half_length * (half_length + 1) / 2) as f64;
            assert_eq!(derived.to_bits(), half.weighted_mean(half_length).to_bits());
            ready += 1;
        }
    }
    assert!(ready > 1000);
}

#[test]
fn hma_tail_reuse_matches_legacy_three_windows_for_every_output_bit() {
    let mut old_half = LegacySamples::default();
    let mut old_full = LegacySamples::default();
    let mut old_smooth = LegacySamples::default();
    let mut full = RollingWindowState::default();
    let mut smooth = RollingWindowState::default();
    for index in 0..8192 {
        let length = [1, 2, 3, 7, 8, 31, 32, 127, 4, 65][(index / 256) % 10];
        let half_length = (length / 2).max(1);
        let smooth_length = (length as f64).sqrt().round().max(1.0) as usize;
        let source = if index.is_multiple_of(257) {
            None
        } else {
            sample(index).or(Some(-0.0))
        };
        old_half.push(source, half_length);
        old_full.push(source, length);
        full.push(source, length);
        let old_diff = (old_half.values.len() == half_length
            && old_half.na_count == 0
            && old_full.values.len() == length
            && old_full.na_count == 0)
            .then(|| 2.0 * old_half.weighted_mean(half_length) - old_full.weighted_mean(length));
        let diff = full.is_ready(length).then(|| {
            let (full_mean, half_mean) = full.weighted_mean_with_tail(length, half_length);
            2.0 * half_mean - full_mean
        });
        assert_eq!(old_diff.map(f64::to_bits), diff.map(f64::to_bits));
        // The runtime's common update helper converts non-finite results to NA.
        old_smooth.push(old_diff.filter(|value| value.is_finite()), smooth_length);
        smooth.push(diff.filter(|value| value.is_finite()), smooth_length);
        assert_same_samples(&full, &old_full, length);
        assert_hma_scan_pair(&full, &old_full, length);
        assert_same_samples(&smooth, &old_smooth, smooth_length);
        assert_eq!(
            smooth.is_ready(smooth_length),
            old_smooth.values.len() == smooth_length && old_smooth.na_count == 0
        );
        if smooth.is_ready(smooth_length) {
            assert_eq!(
                smooth.weighted_mean(smooth_length).to_bits(),
                old_smooth.weighted_mean(smooth_length).to_bits()
            );
        }
    }
}
