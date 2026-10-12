use std::collections::VecDeque;

use super::*;

fn bits(value: f64) -> u64 {
    value.to_bits()
}

#[test]
fn push_still_evicts_and_updates_aggregates() {
    let mut window = RollingWindowState::default();
    window.push(Some(1.0), 2);
    window.push(Some(2.0), 2);
    window.push(Some(3.0), 2);
    assert_eq!(window.values, VecDeque::from(vec![Some(2.0), Some(3.0)]));
    assert_eq!(window.sum, 5.0);
    assert_eq!(window.na_count, 0);
    window.push(None, 2);
    assert_eq!(window.values, VecDeque::from(vec![Some(3.0), None]));
    assert_eq!(window.na_count, 1);
    window.pop_front();
    assert_eq!(window.values, VecDeque::from(vec![None]));
    assert_eq!(window.na_count, 1);
}

#[test]
fn exact_zero_window_clears_eviction_roundoff_and_restores_on_same_bar_replacement() {
    let mut window = RollingWindowState::default();
    for (bar, value) in [0.1, 0.2, 0.3, 0.0, 0.0].into_iter().enumerate() {
        window.push_for_bar(Some(value), 3, bar);
    }
    let before = window.clone();
    window.push_for_bar(Some(0.0), 3, 5);
    assert_eq!(window.values, VecDeque::from(vec![Some(0.0); 3]));
    assert_eq!(window.sum, 0.0);
    window.discard_for_bar(5);
    assert_eq!(window.values, before.values);
    assert_eq!(window.sum.to_bits(), before.sum.to_bits());
    assert_eq!(window.nonzero_count, before.nonzero_count);

    window.push_for_bar(Some(0.0), 3, 5);
    window.push_for_bar(Some(1.0), 3, 5);
    assert_eq!(
        window.values,
        VecDeque::from(vec![Some(0.0), Some(0.0), Some(1.0)])
    );
    assert_eq!(window.sum, 1.0);
}

#[test]
fn same_bar_replacement_keeps_only_latest_sample() {
    let mut window = RollingWindowState::default();
    window.push_for_bar(Some(1.0), 3, 0);
    window.push_for_bar(Some(2.0), 3, 0);
    window.push_for_bar(Some(3.0), 3, 0);
    assert_eq!(window.values, VecDeque::from(vec![Some(3.0)]));
    assert_eq!(window.sum, 3.0);
    assert_eq!(window.na_count, 0);
}

#[test]
fn nearby_replacement_avoids_two_roundings_and_restores_on_discard() {
    for sign in [1.0, -1.0] {
        let mut window = RollingWindowState::default();
        for (bar, value) in [1053.7, 1029.4, 1017.3, 1087.1].into_iter().enumerate() {
            window.push_for_bar(Some(sign * value), 4, bar);
        }
        let base = window.clone();
        window.push_for_bar(Some(sign * 1090.9), 4, 4);
        assert_eq!(window.sum.to_bits(), (sign * 4224.7_f64).to_bits());
        window.push_for_bar(Some(sign * 1088.2), 4, 4);
        let mut once = base.clone();
        once.push_for_bar(Some(sign * 1088.2), 4, 4);
        assert_eq!(window.sum.to_bits(), once.sum.to_bits());
        window.discard_for_bar(4);
        assert_eq!(window.values, base.values);
        assert_eq!(window.sum.to_bits(), base.sum.to_bits());
    }
}

#[test]
fn wide_magnitude_and_sign_changes_retain_existing_update_path() {
    for values in [[1e16, 1.0, 1.0], [1000.1, 1000.2, -1000.3]] {
        let mut ordinary = RollingWindowState::default();
        let mut per_bar = RollingWindowState::default();
        for (bar, value) in values.into_iter().enumerate() {
            ordinary.push(Some(value), 2);
            per_bar.push_for_bar(Some(value), 2, bar);
        }
        assert_eq!(ordinary.values, per_bar.values);
        assert_eq!(ordinary.sum.to_bits(), per_bar.sum.to_bits());
    }
}

#[test]
fn new_bar_commits_previous_append() {
    let mut window = RollingWindowState::default();
    window.push_for_bar(Some(1.0), 3, 0);
    window.push_for_bar(Some(2.0), 3, 1);
    window.push_for_bar(Some(3.0), 3, 1);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0), Some(3.0)]));
    assert_eq!(window.sum, 4.0);
    window.discard_for_bar(0);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0), Some(3.0)]));
}

#[test]
fn same_bar_replacement_restores_exact_sums_for_cancellation_prone_floats() {
    let mut window = RollingWindowState::default();
    window.push(Some(1.0), 2);
    let pre_sum = bits(window.sum);

    window.push_for_bar(Some(1e16), 2, 7);
    assert_eq!(window.sum, 1e16);
    assert_ne!(bits(window.sum), pre_sum);

    window.push_for_bar(Some(2.0), 2, 7);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0), Some(2.0)]));
    assert_eq!(bits(window.sum), bits(3.0));

    let mut discarded = RollingWindowState::default();
    discarded.push(Some(1.0), 2);
    discarded.push_for_bar(Some(1e16), 2, 7);
    discarded.discard_for_bar(7);
    assert_eq!(discarded.values, VecDeque::from(vec![Some(1.0)]));
    assert_eq!(bits(discarded.sum), pre_sum);
    assert_ne!(bits(discarded.sum), bits(0.0));
}

#[test]
fn same_bar_length_shrink_then_grow_restores_pre_bar_tail() {
    let mut window = RollingWindowState::default();
    for value in [1.0, 2.0, 3.0, 4.0, 5.0] {
        window.push(Some(value), 5);
    }
    let pre_bar = window.clone();

    window.push_for_bar(Some(99.0), 3, 4);
    assert_eq!(
        window.values,
        VecDeque::from(vec![Some(4.0), Some(5.0), Some(99.0)])
    );

    window.push_for_bar(Some(100.0), 5, 4);
    let mut expected = pre_bar.clone();
    expected.push_for_bar(Some(100.0), 5, 4);
    assert_eq!(window.values, expected.values);
    assert_eq!(
        window.values,
        VecDeque::from(vec![
            Some(2.0),
            Some(3.0),
            Some(4.0),
            Some(5.0),
            Some(100.0)
        ])
    );
    assert_eq!(bits(window.sum), bits(expected.sum));
    assert_eq!(window, expected);

    let mut shrunk = pre_bar.clone();
    shrunk.push_for_bar(Some(99.0), 3, 4);
    shrunk.discard_for_bar(4);
    assert_eq!(shrunk.values, pre_bar.values);
    assert_eq!(bits(shrunk.sum), bits(pre_bar.sum));
    assert_eq!(shrunk.na_count, pre_bar.na_count);
}

#[test]
fn discard_then_repush_on_same_bar() {
    let mut window = RollingWindowState::default();
    window.push_for_bar(Some(1.0), 3, 0);
    window.push_for_bar(Some(2.0), 3, 1);
    window.discard_for_bar(1);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0)]));
    assert_eq!(window.sum, 1.0);
    assert_eq!(window.na_count, 0);

    window.push_for_bar(Some(3.0), 3, 1);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0), Some(3.0)]));
    assert_eq!(window.sum, 4.0);

    window.discard_for_bar(9);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0), Some(3.0)]));
}

#[test]
fn cloned_checkpoint_restores_open_append_undo() {
    let mut live = RollingWindowState::default();
    live.push_for_bar(Some(1.0), 2, 0);
    live.push_for_bar(Some(2.0), 2, 1);
    let checkpoint = live.clone();
    assert_eq!(checkpoint, live);

    live.push_for_bar(Some(3.0), 2, 1);
    assert_eq!(live.values, VecDeque::from(vec![Some(1.0), Some(3.0)]));
    assert_ne!(live, checkpoint);

    let mut restored = checkpoint.clone();
    assert_eq!(restored, checkpoint);
    restored.push_for_bar(Some(4.0), 2, 1);
    assert_eq!(restored.values, VecDeque::from(vec![Some(1.0), Some(4.0)]));
    assert_eq!(bits(restored.sum), bits(5.0));
}

#[test]
fn none_is_ordinary_push_not_discard() {
    let mut window = RollingWindowState::default();
    window.push_for_bar(Some(1.0), 3, 0);
    window.push_for_bar(None, 3, 1);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0), None]));
    assert_eq!(window.na_count, 1);
    window.push_for_bar(Some(2.0), 3, 1);
    assert_eq!(window.values, VecDeque::from(vec![Some(1.0), Some(2.0)]));
    assert_eq!(window.na_count, 0);
}

#[test]
fn partial_eq_and_clone_include_undo_metadata() {
    let mut via_push = RollingWindowState::default();
    via_push.push(Some(1.0), 3);
    let mut via_bar = RollingWindowState::default();
    via_bar.push_for_bar(Some(1.0), 3, 0);
    assert_eq!(via_bar.values, via_push.values);
    assert_eq!(bits(via_bar.sum), bits(via_push.sum));
    assert_ne!(via_bar, via_push);
    assert_eq!(via_bar.clone(), via_bar);
    assert_eq!(RollingWindowState::default(), RollingWindowState::default());
}

#[test]
fn every_bar_final_discard_leaves_no_committed_sample() {
    let mut discarded = RollingWindowState::default();
    for bar in 0..3 {
        discarded.push_for_bar(Some(1.0), 3, bar);
        discarded.push_for_bar(Some(2.0), 3, bar);
        discarded.discard_for_bar(bar);
    }
    assert!(discarded.values.is_empty());
    assert_eq!(discarded.sum, 0.0);
    assert_eq!(discarded.na_count, 0);

    let mut committed = RollingWindowState::default();
    committed.push_for_bar(Some(1.0), 3, 0);
    committed.push_for_bar(Some(2.0), 3, 0);
    committed.push_for_bar(Some(3.0), 3, 0);
    committed.push_for_bar(Some(4.0), 3, 1);
    assert_eq!(committed.values, VecDeque::from(vec![Some(3.0), Some(4.0)]));
}
