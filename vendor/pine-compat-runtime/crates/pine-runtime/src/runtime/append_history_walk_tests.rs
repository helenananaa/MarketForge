use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Counted {
    value: usize,
    clones: Arc<AtomicUsize>,
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Ordering::Relaxed);
        Self {
            value: self.value,
            clones: self.clones.clone(),
        }
    }
}

#[test]
fn sequential_iteration_visits_each_subtree_once_without_cloning_payloads() {
    let clones = Arc::new(AtomicUsize::new(0));
    let mut history = AppendHistory::from_values((0..100_003).map(|value| Counted {
        value,
        clones: clones.clone(),
    }));
    history.drop_prefix(10_001);
    let mut iter = history.iter();
    for expected in 10_001..100_003 {
        assert_eq!(iter.next().unwrap().value, expected);
        assert_eq!(iter.len(), 100_002 - expected);
    }
    assert!(iter.next().is_none());
    // Includes the two boundary leaves and the initial path to the suffix.
    assert!(iter.node_visits <= 2 * history.len().div_ceil(APPEND_LEAF_SIZE) + 64);
    assert_eq!(clones.load(Ordering::Relaxed), 0);
}

#[test]
fn reverse_iteration_visits_each_subtree_once_and_seeks_to_the_tail() {
    let clones = Arc::new(AtomicUsize::new(0));
    let mut history = AppendHistory::from_values((0..100_003).map(|value| Counted {
        value,
        clones: clones.clone(),
    }));
    history.drop_prefix(10_001);
    let mut iter = history.iter_rev();
    for expected in (10_001..100_003).rev() {
        assert_eq!(iter.next().unwrap().value, expected);
        assert_eq!(iter.len(), expected - 10_001);
    }
    assert!(iter.next().is_none());
    assert!(iter.node_visits <= 2 * history.len().div_ceil(APPEND_LEAF_SIZE) + 64);
    let mut tail = history.iter_rev();
    assert_eq!(tail.next().unwrap().value, 100_002);
    assert!(tail.node_visits <= usize::BITS as usize);
    assert_eq!(clones.load(Ordering::Relaxed), 0);
}

#[test]
fn reverse_iteration_handles_repeat_leaves_pruning_and_checkpoint_replacement() {
    use crate::PineValue;
    let mut history =
        AppendHistory::from_compact_values((0..1600).map(|index| PineValue::Int(index / 131)));
    for drop in [1, 126, 1, 129, 255, 1, 512] {
        history.drop_prefix(drop);
        let checkpoint = history.clone();
        let previous = history.to_vec();
        *history.last_mut().unwrap() = PineValue::Int(-1);
        history.push_compact(PineValue::Int(-2));
        assert_eq!(
            history.iter_rev().cloned().collect::<Vec<_>>(),
            history.to_vec().into_iter().rev().collect::<Vec<_>>()
        );
        assert_eq!(
            checkpoint.iter_rev().cloned().collect::<Vec<_>>(),
            previous.into_iter().rev().collect::<Vec<_>>()
        );
    }
    history.drop_prefix(usize::MAX);
    assert!(history.iter_rev().next().is_none());
    assert_eq!(history.iter_rev().len(), 0);
    history.push(PineValue::Int(-3));
    assert_eq!(
        history.iter_rev().cloned().collect::<Vec<_>>(),
        [PineValue::Int(-3)]
    );
}

#[test]
#[allow(clippy::iter_nth_zero)] // Exercise the custom nth zero-offset branch explicitly.
fn iterator_nth_seeks_without_walking_skipped_leaves_and_can_resume() {
    let mut history = AppendHistory::from_values(0..100_003);
    history.drop_prefix(255);
    let mut iter = history.iter();
    assert_eq!(iter.next(), Some(&255));
    let before_seek = iter.node_visits;
    assert_eq!(iter.nth(90_000), Some(&90_256));
    assert!(iter.node_visits - before_seek <= usize::BITS as usize);
    assert_eq!(iter.len(), 9_746);
    assert_eq!(iter.nth(0), Some(&90_257));
    assert_eq!(iter.nth(126), Some(&90_384));
    assert_eq!(
        iter.by_ref().take(130).copied().collect::<Vec<_>>(),
        (90_385..90_515).collect::<Vec<_>>()
    );
    assert_eq!(iter.nth(usize::MAX), None);
    assert_eq!(iter.size_hint(), (0, Some(0)));
    assert_eq!(iter.nth(0), None);
    assert_eq!(iter.next(), None);
}

#[test]
fn trimming_inside_the_same_leaf_does_not_detach_shared_branches() {
    let clones = Arc::new(AtomicUsize::new(0));
    let mut history = AppendHistory::from_values((0..8192).map(|value| Counted {
        value,
        clones: clones.clone(),
    }));
    let checkpoint = history.clone();
    for count in 1..APPEND_LEAF_SIZE {
        history.drop_prefix(1);
        assert!(Arc::ptr_eq(&history.root, &checkpoint.root));
        assert_eq!(history[0].value, count);
    }
    history.drop_prefix(1);
    assert!(!Arc::ptr_eq(&history.root, &checkpoint.root));
    assert_eq!(history[0].value, APPEND_LEAF_SIZE);
    assert_eq!(checkpoint[0].value, 0);
    assert_eq!(clones.load(Ordering::Relaxed), 0);
}

fn assert_no_empty_branch_chains<T>(node: &Node<T>) -> bool {
    match node {
        Node::Empty => false,
        Node::Leaf(values) => !values.is_empty(),
        Node::Repeat { len, .. } => *len != 0,
        Node::Branch { left, right, .. } => {
            let left_has_values = assert_no_empty_branch_chains(left);
            let right_has_values = right.as_deref().is_some_and(assert_no_empty_branch_chains);
            assert!(
                left_has_values || right_has_values,
                "pruned subtrees use one empty marker"
            );
            true
        }
    }
}

#[test]
fn large_pruned_subtrees_use_markers_and_survive_rerooting_and_append() {
    let mut history = AppendHistory::from_values(0..8192);
    let checkpoint = history.clone();
    history.drop_prefix(4095);
    assert_no_empty_branch_chains(&history.root);
    assert_eq!(
        history.iter().copied().collect::<Vec<_>>(),
        (4095..8192).collect::<Vec<_>>()
    );
    for value in 8192..12_500 {
        history.push(value);
        history.drop_prefix(1);
        if value % 127 == 0 {
            assert_no_empty_branch_chains(&history.root);
            assert_eq!(
                history.iter().copied().collect::<Vec<_>>(),
                ((value - 4096)..=value).collect::<Vec<_>>()
            );
            assert_eq!(
                history.to_vec(),
                history.iter().copied().collect::<Vec<_>>()
            );
        }
    }
    assert_eq!(
        checkpoint.iter().copied().collect::<Vec<_>>(),
        (0..8192).collect::<Vec<_>>()
    );
    history.drop_prefix(usize::MAX);
    assert!(history.iter().next().is_none());
    assert_eq!(history.iter().len(), 0);
    history.push(-1);
    assert_eq!(history.iter().copied().collect::<Vec<_>>(), [-1]);
}

#[test]
fn repeat_and_mixed_leaves_preserve_float_bits_and_checkpoint_branches() {
    use crate::PineValue;
    let bits = [
        0.0_f64.to_bits(),
        (-0.0_f64).to_bits(),
        0x7ff8000000000001,
        0x7ff8000000000002,
    ];
    let mut expected: Vec<_> = (0..1600)
        .map(|index| bits[(index / 131) % bits.len()])
        .collect();
    let mut history = AppendHistory::from_compact_values(
        expected
            .iter()
            .map(|bits| PineValue::Float(f64::from_bits(*bits))),
    );
    let original = history.clone();
    for count in [1, 126, 1, 129, 255, 1, 512] {
        history.drop_prefix(count);
        expected.drain(..count);
        let before_bits = expected.clone();
        let checkpoint = history.clone();
        let last_bits = ((expected.len() % 2) as f64).to_bits();
        *history.last_mut().unwrap() = PineValue::Float(f64::from_bits(last_bits));
        *expected.last_mut().unwrap() = last_bits;
        history.push_compact(PineValue::Float(f64::from_bits(bits[3])));
        expected.push(bits[3]);
        let observed: Vec<_> = history
            .iter()
            .map(|value| value.as_f64().unwrap().to_bits())
            .collect();
        assert_eq!(observed, expected);
        assert_eq!(
            history
                .iter_rev()
                .map(|value| value.as_f64().unwrap().to_bits())
                .collect::<Vec<_>>(),
            expected.iter().rev().copied().collect::<Vec<_>>()
        );
        assert_eq!(checkpoint.len() + 1, history.len());
        assert_eq!(
            checkpoint
                .iter()
                .map(|value| value.as_f64().unwrap().to_bits())
                .collect::<Vec<_>>(),
            before_bits
        );
        let mut iter = checkpoint.iter();
        let offset = checkpoint.len() / 2;
        assert_eq!(
            iter.nth(offset)
                .map(|value| value.as_f64().unwrap().to_bits()),
            checkpoint
                .get(offset)
                .map(|value| value.as_f64().unwrap().to_bits())
        );
        assert_no_empty_branch_chains(&history.root);
    }
    let original_bits: Vec<_> = original
        .iter()
        .map(|value| value.as_f64().unwrap().to_bits())
        .collect();
    assert_eq!(
        original_bits,
        (0..1600)
            .map(|index| bits[(index / 131) % bits.len()])
            .collect::<Vec<_>>()
    );
}

// Recompute exclusively from owned value buffers and repeat payloads. Never use
// Node::allocated_slots as an oracle; also check each intermediate branch cache.
fn assert_capacity_matches_allocations<T>(history: &AppendHistory<T>) {
    fn actual_slots<T>(node: &Node<T>) -> usize {
        match node {
            Node::Empty => 0,
            Node::Leaf(values) => values.capacity(),
            Node::Repeat { .. } => 1,
            Node::Branch {
                left,
                right,
                allocated_slots,
            } => {
                let actual =
                    actual_slots(left).saturating_add(right.as_deref().map_or(0, actual_slots));
                assert_eq!(*allocated_slots, actual);
                actual
            }
        }
    }
    assert_eq!(history.capacity(), actual_slots(&history.root));
}

#[test]
fn capacity_tracks_vec_clone_and_mutation_across_branch_paths() {
    let mut leaf = AppendHistory::from_values(0..5);
    let leaf_checkpoint = leaf.clone();
    assert!(leaf.capacity() > leaf.len());
    *leaf.last_mut().unwrap() = -1;
    assert_capacity_matches_allocations(&leaf);
    assert_capacity_matches_allocations(&leaf_checkpoint);
    assert!(leaf.capacity() < leaf_checkpoint.capacity());
    assert_eq!(leaf_checkpoint.to_vec(), [0, 1, 2, 3, 4]);
    leaf.truncate(3);
    assert_capacity_matches_allocations(&leaf);
    leaf.push(10);
    assert_capacity_matches_allocations(&leaf);

    let mut history = AppendHistory::from_values(0..385);
    let original = history.clone();
    *history.last_mut().unwrap() = -1;
    assert_capacity_matches_allocations(&history);
    assert_capacity_matches_allocations(&original);
    assert!(history.capacity() < original.capacity());

    history.truncate(259);
    let truncated = history.clone();
    *history.get_mut(258).unwrap() = -258;
    assert_capacity_matches_allocations(&history);
    assert_capacity_matches_allocations(&truncated);
    // A shared three-value tail clones its live values rather than its old
    // full-leaf reservation. Every parent must observe that smaller Vec.
    assert!(history.capacity() < truncated.capacity());
    history.push(9000);
    assert_capacity_matches_allocations(&history);
    assert_eq!(truncated[258], 258);
    assert_eq!(original.to_vec(), (0..385).collect::<Vec<_>>());

    history.drop_prefix(129);
    assert_capacity_matches_allocations(&history);
    history.truncate(1);
    assert_capacity_matches_allocations(&history);
    history.truncate(0);
    assert_eq!(history.capacity(), 0);
    assert_capacity_matches_allocations(&history);
    history.push(99);
    assert_capacity_matches_allocations(&history);
    history.drop_prefix(usize::MAX);
    assert_eq!(history.capacity(), 0);
    assert_capacity_matches_allocations(&history);
}

#[test]
fn capacity_tracks_repeat_expansion_compaction_and_pruned_subtrees() {
    use crate::PineValue;
    let mut history = AppendHistory::from_compact_values(std::iter::repeat_n(PineValue::Na, 1024));
    let original = history.clone();
    assert_eq!(history.capacity(), 8);
    *history.get_mut(129).unwrap() = PineValue::Int(7);
    assert_eq!(history.capacity(), 7 + APPEND_LEAF_SIZE);
    assert_capacity_matches_allocations(&history);
    assert_capacity_matches_allocations(&original);

    history.drop_prefix(300);
    assert_eq!(history.capacity(), 6);
    assert_capacity_matches_allocations(&history);
    history.truncate(129);
    let truncated = history.clone();
    assert_eq!(history.capacity(), 2);
    *history.last_mut().unwrap() = PineValue::Int(8);
    assert_capacity_matches_allocations(&history);
    assert!(history.capacity() > truncated.capacity());
    history.push_compact(PineValue::Na);
    assert_capacity_matches_allocations(&history);
    *history.get_mut(0).unwrap() = PineValue::Int(9);
    assert_capacity_matches_allocations(&history);
    assert_eq!(truncated.capacity(), 2);
    assert_capacity_matches_allocations(&truncated);
    assert_eq!(original.capacity(), 8);
    assert!(original.iter().all(|value| value == &PineValue::Na));

    let mut small = AppendHistory::default();
    small.push_compact(PineValue::String("constant".into()));
    let uncompressed = small.clone();
    small.push_compact(PineValue::String("constant".into()));
    assert_eq!(small.capacity(), 1);
    assert_capacity_matches_allocations(&small);
    assert_capacity_matches_allocations(&uncompressed);
    small.push_compact(PineValue::String("different".into()));
    assert_capacity_matches_allocations(&small);
    assert_eq!(uncompressed.len(), 1);
}

#[test]
fn capacity_matches_all_allocations_during_mixed_checkpoint_edits() {
    use crate::PineValue;
    let mut history = AppendHistory::default();
    let mut expected = Vec::new();
    let mut checkpoints = Vec::new();
    for index in 0..4096 {
        if index % 127 == 0 {
            checkpoints.push((history.clone(), expected.clone()));
            if checkpoints.len() > 4 {
                checkpoints.remove(0);
            }
        }
        let value = match (index / 141) % 3 {
            0 => PineValue::Na,
            1 => PineValue::String("repeat".into()),
            _ => PineValue::Int(index),
        };
        if index % 13 == 0 {
            history.push(value.clone());
        } else {
            history.push_compact(value.clone());
        }
        expected.push(value);
        if index % 19 == 0 {
            let changed = expected.len() / 3;
            *history.get_mut(changed).unwrap() = PineValue::Bool(true);
            expected[changed] = PineValue::Bool(true);
        }
        if index % 23 == 0 {
            *history.last_mut().unwrap() = PineValue::Int(-index);
            *expected.last_mut().unwrap() = PineValue::Int(-index);
        }
        if index % 97 == 0 && expected.len() > 200 {
            let retained = expected.len() - 137;
            history.truncate(retained);
            expected.truncate(retained);
        }
        if expected.len() > 400 {
            let count = (index % 11 + 1) as usize;
            history.drop_prefix(count);
            expected.drain(..count);
        }
        assert_capacity_matches_allocations(&history);
        assert_eq!(history.to_vec(), expected);
        if index % 127 == 0 {
            for (checkpoint, values) in &checkpoints {
                assert_capacity_matches_allocations(checkpoint);
                assert_eq!(checkpoint.to_vec(), *values);
            }
        }
    }
}

#[test]
fn zero_sized_capacity_saturates_across_shared_leaf_paths() {
    let mut history = AppendHistory::from_values(std::iter::repeat_n((), 1025));
    let checkpoint = history.clone();
    assert_eq!(history.capacity(), usize::MAX);
    assert_capacity_matches_allocations(&history);
    *history.get_mut(129).unwrap() = ();
    history.truncate(513);
    history.drop_prefix(256);
    history.push(());
    *history.last_mut().unwrap() = ();
    assert_capacity_matches_allocations(&history);
    assert_capacity_matches_allocations(&checkpoint);
    assert_eq!(history.capacity(), usize::MAX);
    assert_eq!(checkpoint.len(), 1025);
    history.truncate(0);
    assert_eq!(history.capacity(), Vec::<()>::new().capacity());
    assert_capacity_matches_allocations(&history);
}
