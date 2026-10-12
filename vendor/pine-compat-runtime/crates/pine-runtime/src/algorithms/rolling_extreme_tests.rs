use super::*;

fn update(
    state: &mut RollingExtremeState,
    values: &[Option<f64>],
    bar: usize,
    length: usize,
    mode: WindowExtreme,
    available: usize,
) -> (Option<(f64, usize)>, usize) {
    let missing = state.prepare(SeriesId(0), length, mode, bar, available);
    for (index, &value) in values.iter().enumerate().take(bar).skip(bar - missing) {
        state.push(index, value);
    }
    (state.best(values[bar], bar), missing)
}

fn reference(
    values: &[Option<f64>],
    bar: usize,
    length: usize,
    mode: WindowExtreme,
    available: usize,
) -> Option<(f64, usize)> {
    let start = bar - available.min(length - 1).min(bar);
    let mut best: Option<(f64, usize)> = None;
    for index in (start..=bar).rev() {
        let Some(value) = values[index].filter(|value| value.is_finite()) else {
            continue;
        };
        if best.is_none_or(|(previous, _)| match mode {
            WindowExtreme::Highest => value > previous,
            WindowExtreme::Lowest => value < previous,
        }) {
            best = Some((value, bar - index));
        }
    }
    best
}

#[test]
fn steady_deep_windows_match_scan_and_consume_each_history_sample_once() {
    let values: Vec<_> = (0..12_000)
        .map(|index| match index % 17 {
            0 => None,
            1 => Some(f64::INFINITY),
            2 => Some(f64::NAN),
            3 => Some(-0.0),
            4 => Some(0.0),
            _ => Some((index * 137 % 71) as f64 - 35.0),
        })
        .collect();
    for mode in [WindowExtreme::Highest, WindowExtreme::Lowest] {
        let mut state = RollingExtremeState::new(SeriesId(0), 513, mode);
        let mut samples = 0;
        for bar in 0..values.len() {
            let (best, missing) = update(&mut state, &values, bar, 513, mode, bar);
            samples += missing;
            assert_eq!(best, reference(&values, bar, 513, mode, bar), "bar {bar}");
            assert!(state.retained_values() <= 512);
        }
        assert_eq!(samples, values.len() - 1);
    }
}

#[test]
fn sparse_dynamic_and_limited_retention_windows_rebuild_from_actual_history() {
    let values: Vec<_> = (0..4096)
        .map(|index| (index % 13 != 0).then_some((index * 137 % 23) as f64))
        .collect();
    for mode in [WindowExtreme::Highest, WindowExtreme::Lowest] {
        let mut state = RollingExtremeState::new(SeriesId(0), 257, mode);
        for (bar, length, available) in [
            (3, 257, 3),
            (10, 257, 10),
            (999, 257, 999),
            (1000, 33, 1000),
            (1001, 1025, 1001),
            (1002, 1025, 7),
            (1003, 1025, 8),
            (3000, 513, 3000),
            (3001, 513, 3001),
            (30, 513, 30),
        ] {
            let (best, _) = update(&mut state, &values, bar, length, mode, available);
            assert_eq!(best, reference(&values, bar, length, mode, available));
            assert!(state.retained_values() <= available.min(length - 1).min(bar));
        }
        assert_eq!(state.prepare(SeriesId(99), 513, mode, 30, 30), 30);
    }
}

#[test]
fn current_bar_changes_never_replace_committed_candidates() {
    let mut state = RollingExtremeState::new(SeriesId(0), 33, WindowExtreme::Highest);
    assert_eq!(
        state.prepare(SeriesId(0), 33, WindowExtreme::Highest, 2, 2),
        2
    );
    state.push(0, Some(10.0));
    state.push(1, Some(5.0));
    for value in [20.0, 10.0, -5.0] {
        assert_eq!(
            state.prepare(SeriesId(0), 33, WindowExtreme::Highest, 2, 2),
            0
        );
        assert_eq!(
            state.best(Some(value), 2),
            Some((value.max(10.0), usize::from(value < 10.0) * 2))
        );
    }
    assert_eq!(
        state.prepare(SeriesId(0), 33, WindowExtreme::Highest, 3, 3),
        1
    );
    state.push(2, Some(7.0));
    assert_eq!(state.best(Some(-5.0), 3), Some((10.0, 3)));
}

#[test]
fn long_monotone_checkpoints_share_closed_leaves_and_mutate_independently() {
    let mut original = RollingExtremeState::new(SeriesId(0), 10_001, WindowExtreme::Highest);
    assert_eq!(
        original.prepare(SeriesId(0), 10_001, WindowExtreme::Highest, 10_000, 10_000),
        10_000
    );
    for bar in 0..10_000 {
        original.push(bar, Some(20_000.0 - bar as f64));
    }
    let mut copy = original.clone();
    assert!(std::ptr::eq(
        original.candidates.get(129).unwrap(),
        copy.candidates.get(129).unwrap()
    ));
    assert_eq!(
        copy.prepare(SeriesId(0), 10_001, WindowExtreme::Highest, 10_001, 10_001),
        1
    );
    copy.push(10_000, Some(10_000.0));
    assert!(std::ptr::eq(
        original.candidates.get(129).unwrap(),
        copy.candidates.get(128).unwrap()
    ));
    assert_eq!(original.best(None, 10_000), Some((20_000.0, 10_000)));
    assert_eq!(copy.best(None, 10_001), Some((19_999.0, 10_000)));
    for bar in 10_001..50_000 {
        assert_eq!(
            copy.prepare(
                SeriesId(0),
                10_001,
                WindowExtreme::Highest,
                bar + 1,
                bar + 1
            ),
            1
        );
        copy.push(bar, Some(20_000.0 - bar as f64));
        assert_eq!(copy.retained_values(), 10_000);
    }
    assert!(copy.retained_capacity() <= 10_256);
    assert_eq!(original.retained_values(), 10_000);
}
