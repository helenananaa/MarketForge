use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering::Relaxed},
};

#[derive(Debug)]
struct Counted {
    value: usize,
    clones: Arc<AtomicUsize>,
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Relaxed);
        Self {
            value: self.value,
            clones: self.clones.clone(),
        }
    }
}

#[test]
fn large_checkpoints_share_payloads_and_first_write_clones_only_one_leaf() {
    let clones = Arc::new(AtomicUsize::new(0));
    let mut history = ValueWhenHistory::default();
    for value in 0..100_003 {
        history.push_retained(
            Counted {
                value,
                clones: clones.clone(),
            },
            1_000_000,
        );
    }
    assert_eq!(clones.load(Relaxed), 0);
    let checkpoint = history.clone();
    assert_eq!(clones.load(Relaxed), 0);
    history.push_retained(
        Counted {
            value: 100_003,
            clones: clones.clone(),
        },
        1_000_000,
    );
    assert!(clones.load(Relaxed) <= super::super::append_history::APPEND_LEAF_SIZE);
    assert_eq!(checkpoint.len(), 100_003);
    assert_eq!(checkpoint.get(0).unwrap().value, 100_002);
    assert_eq!(checkpoint.get(100_002).unwrap().value, 0);
    assert_eq!(history.get(0).unwrap().value, 100_003);
}

#[test]
fn fixed_small_windows_keep_standard_queue_capacity_and_clone_costs() {
    for retain in [0, 1, 2, 128] {
        let clones = Arc::new(AtomicUsize::new(0));
        let mut history = ValueWhenHistory::default();
        let mut reference = VecDeque::new();
        for value in 0..515 {
            history.push_retained(
                Counted {
                    value,
                    clones: clones.clone(),
                },
                retain,
            );
            reference.push_front(value);
            reference.truncate(retain);
            assert_eq!(history.len(), reference.len());
            assert_eq!(history.capacity(), reference.capacity());
            assert!(matches!(history.storage, Storage::Small(_)));
        }
        let before = clones.load(Relaxed);
        let checkpoint = history.clone();
        assert_eq!(clones.load(Relaxed) - before, reference.len());
        assert_eq!(
            checkpoint.get(0).map(|value| value.value),
            reference.front().copied()
        );
        history.push_retained(
            Counted {
                value: 515,
                clones: clones.clone(),
            },
            retain,
        );
        assert_eq!(clones.load(Relaxed) - before, reference.len());
    }
}

#[test]
fn retained_occurrences_match_newest_first_reference_after_pruning_and_checkpoint() {
    let mut history = ValueWhenHistory::default();
    let mut reference = VecDeque::new();
    for value in 0..600 {
        history.push_retained(value, 129);
        reference.push_front(value);
        reference.truncate(129);
    }
    let checkpoint = history.clone();
    let original = reference.clone();
    for (value, retain) in (600..628).zip([0, 1, 2, 127, 128, 129, 257].into_iter().cycle()) {
        history.push_retained(value, retain);
        reference.push_front(value);
        reference.truncate(retain);
        assert_eq!(history.len(), reference.len());
        for occurrence in 0..=reference.len() {
            assert_eq!(history.get(occurrence), reference.get(occurrence));
        }
        assert_eq!(history.get(usize::MAX), None);
        assert_eq!(
            history.iter().copied().collect::<Vec<_>>(),
            reference.iter().rev().copied().collect::<Vec<_>>()
        );
        assert!(matches!(history.storage, Storage::Persistent(_)));
    }
    assert_eq!(
        checkpoint.iter().copied().collect::<Vec<_>>(),
        original.iter().rev().copied().collect::<Vec<_>>()
    );
    assert_eq!(checkpoint.get(0), original.front());
}

#[test]
fn owned_string_values_remain_independent_after_checkpoint_tail_replacement() {
    let mut history = ValueWhenHistory::default();
    for index in 0..300 {
        history.push_retained(crate::PineValue::String(format!("event {index} 汉🙂")), 129);
    }
    let checkpoint = history.clone();
    history.push_retained(crate::PineValue::String("replacement".into()), 129);
    assert_eq!(
        checkpoint.get(0),
        Some(&crate::PineValue::String("event 299 汉🙂".into()))
    );
    assert_eq!(
        history.get(0),
        Some(&crate::PineValue::String("replacement".into()))
    );
    assert_eq!(
        checkpoint.get(128),
        Some(&crate::PineValue::String("event 171 汉🙂".into()))
    );
    assert_eq!(
        history.get(128),
        Some(&crate::PineValue::String("event 172 汉🙂".into()))
    );
}
