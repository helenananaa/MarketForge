use std::cmp::Ordering;

/// Reusable workspace, with no Pine-visible state. Runtime checkpoints start
/// with an empty workspace rather than copying a potentially large selection.
#[derive(Debug, Default)]
pub(crate) struct SelectionScratch {
    values: Vec<(f64, usize)>,
    ranks: Vec<f64>,
}

impl Clone for SelectionScratch {
    fn clone(&self) -> Self {
        Self::default()
    }
}

fn compare_samples(left: &(f64, usize), right: &(f64, usize)) -> Ordering {
    // Samples are finite. Preserve the former stable sort's tie order, including
    // signed zeros, even though selection itself is unstable.
    left.0
        .partial_cmp(&right.0)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.1.cmp(&right.1))
}

impl SelectionScratch {
    pub(crate) fn capacity(&self) -> usize {
        self.values.capacity() + self.ranks.capacity()
    }

    pub(crate) fn mode(&mut self, samples: impl Iterator<Item = f64>) -> f64 {
        self.ranks.clear();
        let mut first_zero = None;
        self.ranks.extend(samples.inspect(|value| {
            if *value == 0.0 && first_zero.is_none() {
                first_zero = Some(*value);
            }
        }));
        // Equal finite floats have identical bits except signed zero. Sorting
        // only by value keeps the duplicate-key fast path; restore the original
        // stable zero representative separately when it anchors the winning group.
        self.ranks
            .sort_unstable_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
        let mut best_value = self.ranks[0];
        let mut best_count = 0;
        let mut current_value = best_value;
        let mut current_count = 0;
        for &value in &self.ranks {
            if (value - current_value).abs() < f64::EPSILON {
                current_count += 1;
            } else {
                if current_count > best_count {
                    best_value = current_value;
                    best_count = current_count;
                }
                current_value = value;
                current_count = 1;
            }
        }
        if current_count > best_count {
            best_value = current_value;
        }
        self.ranks.clear();
        if best_value == 0.0 {
            first_zero.unwrap_or(best_value)
        } else {
            best_value
        }
    }

    pub(crate) fn rci(&mut self, samples: impl Iterator<Item = f64>) -> f64 {
        self.values.clear();
        self.values
            .extend(samples.enumerate().map(|(index, value)| (value, index)));
        // Every member of an equal-price group receives the same average rank,
        // then ranks are mapped back to input order before floating reduction.
        // Tie order is unobservable, and leaving keys equal avoids sorting n
        // distinct (price, index) keys when the window has only a few prices.
        self.values
            .sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
        let length = self.values.len();
        self.ranks.resize(length, 0.0);
        let mut start = 0;
        while start < length {
            let mut end = start + 1;
            while end < length && self.values[end].0 == self.values[start].0 {
                end += 1;
            }
            let average_rank = (start + 1 + end) as f64 / 2.0;
            for &(_, original_index) in &self.values[start..end] {
                self.ranks[original_index] = average_rank;
            }
            start = end;
        }
        let squared_rank_difference = self
            .ranks
            .iter()
            .enumerate()
            .map(|(index, price_rank)| {
                let difference = *price_rank - (index + 1) as f64;
                difference * difference
            })
            .sum::<f64>();
        let length = length as f64;
        let result =
            (1.0 - 6.0 * squared_rank_difference / (length * (length * length - 1.0))) * 100.0;
        self.values.clear();
        self.ranks.clear();
        result
    }

    pub(crate) fn select_pair(
        &mut self,
        samples: impl Iterator<Item = f64>,
        lower: usize,
        upper: usize,
    ) -> (f64, f64) {
        self.values.clear();
        self.values
            .extend(samples.enumerate().map(|(index, value)| (value, index)));
        debug_assert!(lower <= upper && upper < self.values.len());
        // Median and interpolated percentile need one rank or adjacent ranks.
        debug_assert!(upper - lower <= 1);
        let (left, selected, _) = self.values.select_nth_unstable_by(upper, compare_samples);
        let high = selected.0;
        let low = if lower == upper {
            high
        } else {
            left.iter().max_by(|a, b| compare_samples(a, b)).unwrap().0
        };
        self.values.clear();
        (low, high)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stable_mode(mut values: Vec<f64>) -> f64 {
        values.sort_by(|left, right| left.partial_cmp(right).unwrap());
        let mut best = values[0];
        let mut best_count = 0;
        let mut start = 0;
        while start < values.len() {
            let mut end = start + 1;
            while end < values.len() && (values[end] - values[start]).abs() < f64::EPSILON {
                end += 1;
            }
            if end - start > best_count {
                best = values[start];
                best_count = end - start;
            }
            start = end;
        }
        best
    }

    fn pairwise_rci(values: &[f64]) -> f64 {
        let sum = values
            .iter()
            .enumerate()
            .map(|(index, price)| {
                let less = values.iter().filter(|value| *value < price).count();
                let equal = values.iter().filter(|value| *value == price).count();
                let rank = (2 * less + equal + 1) as f64 / 2.0;
                let difference = rank - (index + 1) as f64;
                difference * difference
            })
            .sum::<f64>();
        let length = values.len() as f64;
        (1.0 - 6.0 * sum / (length * (length * length - 1.0))) * 100.0
    }

    #[test]
    fn duplicate_key_sorting_matches_stable_mode_and_pairwise_rci_bits() {
        let palette = [
            -0.0,
            0.0,
            -f64::EPSILON,
            -f64::EPSILON / 2.0,
            f64::EPSILON / 2.0,
            f64::EPSILON,
            1.0,
            2.0,
            f64::MIN,
            f64::MAX,
            -1e300,
            1e300,
            f64::from_bits(1),
        ];
        let mut scratch = SelectionScratch::default();
        for length in [2, 3, 16, 31, 129, 513] {
            for seed in 0_u64..16 {
                let mut state = seed + 1;
                let values: Vec<_> = (0..length)
                    .map(|_| {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        palette[((state >> 32) as usize) % palette.len()]
                    })
                    .collect();
                assert_eq!(
                    scratch.mode(values.iter().copied()).to_bits(),
                    stable_mode(values.clone()).to_bits()
                );
                assert_eq!(
                    scratch.rci(values.iter().copied()).to_bits(),
                    pairwise_rci(&values).to_bits()
                );
            }
        }
    }

    #[test]
    fn zero_representative_survives_large_duplicate_groups_and_epsilon_boundaries() {
        let mut scratch = SelectionScratch::default();
        for zero in [-0.0_f64, 0.0] {
            let mut values = vec![zero];
            values.extend((0..2048).map(|index| match index % 3 {
                0 => -zero,
                1 => f64::EPSILON / 2.0,
                _ => 1.0,
            }));
            assert_eq!(
                scratch.mode(values.iter().copied()).to_bits(),
                zero.to_bits()
            );
            values.push(-f64::EPSILON / 2.0);
            assert_eq!(
                scratch.mode(values.iter().copied()).to_bits(),
                stable_mode(values).to_bits()
            );
        }
    }

    #[test]
    fn mode_preserves_stable_zero_order_epsilon_groups_and_lowest_ties() {
        let mut scratch = SelectionScratch::default();
        assert_eq!(
            scratch.mode([-0.0, 0.0].into_iter()).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(
            scratch.mode([0.0, -0.0].into_iter()).to_bits(),
            0.0_f64.to_bits()
        );
        assert_eq!(scratch.mode([3.0, 2.0, 3.0, 2.0].into_iter()), 2.0);
        assert_eq!(
            scratch.mode([0.0, f64::EPSILON / 2.0, f64::EPSILON].into_iter()),
            0.0
        );
        assert_eq!(scratch.mode([1e300, -1e300, 1e300].into_iter()), 1e300);
    }

    #[test]
    fn rci_preserves_tied_ranks_and_reuses_noncheckpoint_scratch() {
        let mut scratch = SelectionScratch::default();
        assert_eq!(scratch.rci([1.0, 2.0, 3.0].into_iter()), 100.0);
        assert_eq!(scratch.rci([3.0, 2.0, 1.0].into_iter()), -100.0);
        assert_eq!(scratch.rci([-0.0, 1.0, 0.0].into_iter()), 12.5);
        scratch.rci((0..1024).map(|index| (index % 17) as f64));
        let capacity = scratch.capacity();
        assert!(capacity >= 2048);
        assert_eq!(scratch.clone().capacity(), 0);
        assert_eq!(scratch.rci([1.0, 2.0, 3.0].into_iter()), 100.0);
        assert_eq!(scratch.capacity(), capacity);
    }

    #[test]
    fn selection_matches_stable_sort_bits_for_every_adjacent_rank() {
        let mut scratch = SelectionScratch::default();
        for length in [1_usize, 2, 7, 16, 129, 1024] {
            let values: Vec<_> = (0..length)
                .map(|index| match index % 10 {
                    0 => 0.0,
                    1 => -0.0,
                    2 => 1e300,
                    3 => -1e300,
                    4 => f64::MAX,
                    5 => f64::MIN,
                    6 => f64::from_bits(1),
                    _ => (index * 137 % 31) as f64,
                })
                .collect();
            let mut sorted = values.clone();
            sorted.sort_by(|left, right| left.partial_cmp(right).unwrap());
            for upper in 0..length {
                for lower in [upper.saturating_sub(1), upper] {
                    let (low, high) = scratch.select_pair(values.iter().copied(), lower, upper);
                    assert_eq!(low.to_bits(), sorted[lower].to_bits());
                    assert_eq!(high.to_bits(), sorted[upper].to_bits());
                }
            }
        }
        assert!(scratch.capacity() >= 1024);
        assert_eq!(scratch.clone().capacity(), 0);
        let capacity = scratch.capacity();
        assert_eq!(
            scratch.select_pair([3.0, 1.0, 2.0].into_iter(), 1, 1),
            (2.0, 2.0)
        );
        assert_eq!(scratch.capacity(), capacity);
    }
}
