use std::{cell::Cell, rc::Rc};

use super::*;

#[derive(Debug)]
struct Counted {
    value: usize,
    clones: Rc<Cell<usize>>,
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        self.clones.set(self.clones.get() + 1);
        Self {
            value: self.value,
            clones: self.clones.clone(),
        }
    }
}

fn counted_queue(len: usize, clones: &Rc<Cell<usize>>) -> SharedDeque<Counted> {
    let mut values = SharedDeque::default();
    for value in 0..len {
        values.push_back(Counted {
            value,
            clones: clones.clone(),
        });
    }
    values
}

#[test]
fn large_checkpoint_and_append_copy_only_a_bounded_tail_page() {
    for len in [129, 257, 5000, 100_000] {
        let clones = Rc::new(Cell::new(0));
        let mut values = counted_queue(len, &clones);
        let checkpoint = values.clone();
        assert_eq!(clones.get(), 0, "checkpoint length {len}");
        values.push_back(Counted {
            value: len,
            clones: clones.clone(),
        });
        assert!(clones.get() <= PAGE_SIZE, "tail page length {len}");
        assert_eq!(checkpoint.len(), len);
        assert_eq!(checkpoint.back().unwrap().value, len - 1);
        assert_eq!(values.back().unwrap().value, len);
        assert_eq!(
            checkpoint.iter().map(|value| value.value).sum::<usize>(),
            (len - 1) * len / 2
        );
    }
}

#[test]
fn shared_writes_to_both_endpoints_copy_at_most_two_pages() {
    let clones = Rc::new(Cell::new(0));
    let mut values = counted_queue(100_000, &clones);
    assert_eq!(values.pop_front().unwrap().value, 0);
    clones.set(0);
    let checkpoint = values.clone();
    assert_eq!(clones.get(), 0);
    values.push_front(Counted {
        value: 300_000,
        clones: clones.clone(),
    });
    values.push_back(Counted {
        value: 400_000,
        clones: clones.clone(),
    });
    assert!(clones.get() <= 2 * PAGE_SIZE);
    assert_eq!(checkpoint.front().unwrap().value, 1);
    assert_eq!(checkpoint.back().unwrap().value, 99_999);
    assert_eq!(values.front().unwrap().value, 300_000);
    assert_eq!(values.back().unwrap().value, 400_000);
}

#[test]
fn short_checkpoint_copies_at_most_one_page_and_preserves_wrapped_storage() {
    let clones = Rc::new(Cell::new(0));
    let mut values = SharedDeque::from(VecDeque::with_capacity(7));
    let mut wrapped = false;
    for value in 0..1024 {
        if values.len() == 7 {
            values.pop_front();
        }
        values.push_back(Counted {
            value,
            clones: clones.clone(),
        });
        wrapped |= values.has_split_storage();
    }
    clones.set(0);
    let checkpoint = values.clone();
    assert_eq!(clones.get(), 7);
    assert!(clones.get() <= PAGE_SIZE);
    assert!(wrapped, "keep the ordinary deque's wrapped backing slices");
    values.clear();
    assert_eq!(checkpoint.len(), 7);
    assert_eq!(checkpoint.front().unwrap().value, 1017);
}

fn assert_capacity_reports_backing_storage<T>(values: &SharedDeque<T>) {
    let actual_buffer_capacity = match &values.storage {
        Storage::Small(buffer) => buffer.capacity(),
        Storage::Paged(pages) => (0..pages.page_count()).fold(0_usize, |capacity, index| {
            let page = pages.page(index);
            assert!(page.len() <= PAGE_SIZE, "a page must not grow its buffer");
            capacity.saturating_add(page.capacity())
        }),
    };
    assert_eq!(values.capacity(), actual_buffer_capacity);
}

fn assert_model(values: &SharedDeque<usize>, expected: &VecDeque<usize>) {
    assert_capacity_reports_backing_storage(values);
    assert_eq!(values.len(), expected.len());
    assert_eq!(values.front(), expected.front());
    assert_eq!(values.back(), expected.back());
    assert!(values.iter().eq(expected.iter()));
    for index in [
        0,
        expected.len() / 2,
        expected.len().saturating_sub(1),
        expected.len(),
    ] {
        assert_eq!(values.get(index), expected.get(index));
    }
    let len = expected.len();
    for (start, end) in [
        (0, 0),
        (0, len),
        (len, len),
        (len / 3, len * 2 / 3),
        (127.min(len), 129.min(len)),
    ] {
        let range = values.range(start..end);
        assert_eq!(range.len(), end - start);
        assert!(range.eq(expected.range(start..end)));
    }
}

#[test]
fn capacity_matches_allocated_buffers_across_promotion_ghosts_and_checkpoints() {
    let initial = VecDeque::with_capacity(4096);
    let flat_capacity = initial.capacity();
    let mut values = SharedDeque::from(initial);
    let mut expected = VecDeque::new();
    for value in 0..128 {
        values.push_back(value);
        expected.push_back(value);
        assert_eq!(values.capacity(), flat_capacity);
        assert_model(&values, &expected);
    }
    let flat_checkpoint = values.clone();
    assert_capacity_reports_backing_storage(&flat_checkpoint);
    for value in 128..1025 {
        values.push_back(value);
        expected.push_back(value);
        assert_model(&values, &expected);
    }
    let paged_checkpoint = values.clone();
    let previous = expected.clone();
    // Shrink both endpoints without changing their physical buffers, including
    // leaving a single page whose physical tail still contains removed cells.
    for _ in 0..37 {
        assert_eq!(values.pop_front(), expected.pop_front());
        assert_model(&values, &expected);
    }
    while values.len() > 1 {
        assert_eq!(values.pop_back(), expected.pop_back());
        assert_model(&values, &expected);
    }
    let ghost_checkpoint = values.clone();
    for value in 2000..2257 {
        values.push_front(value);
        expected.push_front(value);
        values.push_back(value + 1000);
        expected.push_back(value + 1000);
        assert_model(&values, &expected);
    }
    assert_model(&paged_checkpoint, &previous);
    assert!(flat_checkpoint.iter().copied().eq(0..128));
    assert!(ghost_checkpoint.iter().copied().eq([37]));
    assert_capacity_reports_backing_storage(&ghost_checkpoint);
    values.clear();
    expected.clear();
    assert_model(&values, &expected);
    values.push_front(5000);
    expected.push_front(5000);
    assert_model(&values, &expected);
    assert_model(&paged_checkpoint, &previous);
}

#[test]
fn zero_sized_capacity_matches_std_deque_across_shared_pages_and_drain() {
    let mut values = SharedDeque::default();
    let mut expected = VecDeque::new();
    let assert_model = |values: &SharedDeque<()>, expected: &VecDeque<()>| {
        assert_eq!(values.len(), expected.len());
        assert!(values.iter().eq(expected.iter()));
        assert_eq!(values.capacity(), expected.capacity());
        assert_capacity_reports_backing_storage(values);
    };
    assert_model(&values, &expected);
    for _ in 0..1025 {
        values.push_back(());
        expected.push_back(());
        assert_model(&values, &expected);
    }
    let checkpoint = values.clone();
    for _ in 0..129 {
        assert_eq!(values.pop_front(), expected.pop_front());
        assert_eq!(values.pop_back(), expected.pop_back());
        assert_model(&values, &expected);
    }
    values.push_front(());
    expected.push_front(());
    values.push_back(());
    expected.push_back(());
    assert_model(&values, &expected);
    while !expected.is_empty() {
        assert_eq!(values.pop_front(), expected.pop_front());
        assert_model(&values, &expected);
    }
    values.clear();
    expected.clear();
    assert_model(&values, &expected);
    assert_eq!(checkpoint.len(), 1025);
    assert_eq!(checkpoint.capacity(), expected.capacity());
    assert_capacity_reports_backing_storage(&checkpoint);
}

#[test]
fn mixed_endpoint_operations_and_checkpoints_match_independent_deque() {
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    for initial_len in [0, 1, 127, 128, 129, 257, 5000] {
        let mut expected = (0..initial_len).collect::<VecDeque<_>>();
        let mut values = SharedDeque::from(expected.clone());
        for step in 0..4096 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            match state % 10 {
                0..=2 => {
                    values.push_back(step);
                    expected.push_back(step);
                }
                3..=5 => {
                    values.push_front(step);
                    expected.push_front(step);
                }
                6 => assert_eq!(values.pop_front(), expected.pop_front()),
                7 => assert_eq!(values.pop_back(), expected.pop_back()),
                8 => {
                    let checkpoint = values.clone();
                    let previous = expected.clone();
                    values.push_front(step);
                    values.push_back(step + 1);
                    values.pop_front();
                    values.pop_back();
                    assert_model(&checkpoint, &previous);
                }
                _ if step % 97 == 0 => {
                    values.clear();
                    expected.clear();
                }
                _ => {}
            }
            assert_model(&values, &expected);
        }
    }
}

#[test]
fn ranges_preserve_order_across_partial_boundary_pages_and_page_expiry() {
    let mut values = SharedDeque::default();
    let mut expected = VecDeque::new();
    for value in 0..1024 {
        values.push_back(value);
        expected.push_back(value);
    }
    for _ in 0..37 {
        assert_eq!(values.pop_front(), expected.pop_front());
    }
    for _ in 0..91 {
        assert_eq!(values.pop_back(), expected.pop_back());
    }
    for value in 2000..2117 {
        values.push_front(value);
        expected.push_front(value);
    }
    for start in 0..=values.len() {
        for end in [
            start,
            (start + 1).min(values.len()),
            (start + 129).min(values.len()),
            values.len(),
        ] {
            assert!(
                values.range(start..end).eq(expected.range(start..end)),
                "range {start}..{end}"
            );
        }
    }
    assert!(values.range(..=128).eq(expected.range(..=128)));
    assert!(
        values
            .range((Bound::Excluded(127), Bound::Unbounded))
            .eq(expected.range(128..))
    );
    let mut iter = values.iter();
    for remaining in (1..=values.len()).rev() {
        assert_eq!(iter.len(), remaining);
        assert!(iter.next().is_some());
    }
    assert_eq!(iter.len(), 0);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next(), None);
}

#[test]
fn single_paged_leaf_never_exposes_cells_removed_from_its_logical_tail() {
    let mut expected = (0..129).collect::<VecDeque<_>>();
    let mut values = SharedDeque::from(expected.clone());
    for _ in 0..128 {
        assert_eq!(values.pop_back(), expected.pop_back());
    }
    // The remaining physical leaf still has 128 cells, but its live tail is 1.
    let checkpoint = values.clone();
    values.push_front(200);
    expected.push_front(200);
    assert_model(&values, &expected);
    assert_eq!(values.pop_back(), expected.pop_back());
    assert_model(&values, &expected);
    for value in [201, 202] {
        values.push_back(value);
        expected.push_back(value);
    }
    assert_eq!(values.pop_back(), expected.pop_back());
    // This shorter physical leaf has a stale tail cell; insert at its front,
    // then overwrite the stale tail and move the logical front independently.
    values.push_front(203);
    expected.push_front(203);
    assert_model(&values, &expected);
    values.push_back(204);
    expected.push_back(204);
    assert_eq!(values.pop_front(), expected.pop_front());
    let second_checkpoint = values.clone();
    values.push_front(205);
    expected.push_front(205);
    values.push_back(206);
    expected.push_back(206);
    assert_model(&values, &expected);
    assert!(checkpoint.iter().copied().eq([0]));
    assert!(second_checkpoint.iter().copied().eq([200, 201, 204]));
}

#[test]
fn reverse_and_mixed_iterator_ends_preserve_ranges_and_pivot_prefixes() {
    let mut expected = (0..1025).collect::<VecDeque<_>>();
    let mut values = expected.iter().copied().collect::<SharedDeque<_>>();
    for _ in 0..91 {
        values.pop_front();
        expected.pop_front();
    }
    for _ in 0..37 {
        values.pop_back();
        expected.pop_back();
    }
    let checkpoint = values.clone();
    for prefix in [0, 1, 127, 128, 129, 257, values.len() - 1, values.len()] {
        assert!(
            values
                .iter()
                .take(prefix)
                .rev()
                .eq(expected.iter().take(prefix).rev())
        );
        assert!(values.iter().skip(prefix).eq(expected.iter().skip(prefix)));
    }
    for (start, end) in [(0, 0), (0, values.len()), (17, 129), (127, 513), (257, 850)] {
        assert!(
            values
                .range(start..end)
                .rev()
                .eq(expected.range(start..end).rev())
        );
        for pass in 0..32 {
            let mut fold_actual = values.range(start..end);
            let mut fold_reference = expected.range(start..end);
            assert_eq!(fold_actual.next(), fold_reference.next());
            assert_eq!(fold_actual.next_back(), fold_reference.next_back());
            assert_eq!(fold_actual.nth(pass), fold_reference.nth(pass));
            assert_eq!(
                fold_actual.nth_back(pass / 2),
                fold_reference.nth_back(pass / 2)
            );
            let append = |mut collected: Vec<usize>, value: &usize| {
                collected.push(*value);
                collected
            };
            assert_eq!(
                fold_actual.fold(Vec::new(), append),
                fold_reference.fold(Vec::new(), append)
            );
            let mut actual = values.range(start..end);
            let mut reference = expected.range(start..end);
            let mut step = 0;
            while actual.len() != 0 {
                let skip = (pass + step * 37) % 129;
                match (pass + step) % 4 {
                    0 => assert_eq!(actual.next(), reference.next()),
                    1 => assert_eq!(actual.next_back(), reference.next_back()),
                    2 => assert_eq!(actual.nth(skip), reference.nth(skip)),
                    _ => assert_eq!(actual.nth_back(skip), reference.nth_back(skip)),
                }
                assert_eq!(actual.len(), reference.len());
                step += 1;
            }
            assert_eq!(actual.next(), None);
            assert_eq!(actual.next_back(), None);
            assert_eq!(actual.nth(usize::MAX), None);
            assert_eq!(actual.nth_back(usize::MAX), None);
        }
    }
    values.push_front(2000);
    values.push_back(3000);
    assert!(checkpoint.iter().rev().eq(expected.iter().rev()));
}

#[test]
fn repeated_checkpoint_and_sliding_writes_keep_fixed_page_capacity() {
    let length = 100_000;
    let mut values = (0..length).collect::<SharedDeque<_>>();
    for next in length..length + 4096 {
        let checkpoint = values.clone();
        assert_eq!(values.pop_front(), Some(next - length));
        values.push_back(next);
        assert_eq!(checkpoint.front(), Some(&(next - length)));
        assert_eq!(checkpoint.back(), Some(&(next - 1)));
        assert_eq!(values.len(), length);
        assert!(values.capacity() <= length + 2 * PAGE_SIZE);
        assert!(values.page_directory_capacity() <= 4 * length.div_ceil(PAGE_SIZE) + 16);
    }
}

#[test]
fn sliding_queue_and_shrinking_directory_remain_bounded_by_live_window() {
    let mut values = SharedDeque::default();
    for value in 0..5000 {
        values.push_back(value);
    }
    for next in 5000..405_000 {
        assert_eq!(values.pop_front(), Some(next - 5000));
        values.push_back(next);
        if next % 1024 == 0 {
            assert_eq!(values.len(), 5000);
            assert!(values.capacity() <= 5000 + 2 * PAGE_SIZE);
            assert!(values.page_directory_capacity() <= 4 * 5000_usize.div_ceil(PAGE_SIZE) + 16);
        }
    }
    for remaining in (2..=5000).rev() {
        values.pop_front();
        assert!(values.capacity() <= remaining + 2 * PAGE_SIZE);
        assert!(values.page_directory_capacity() <= 4 * remaining.div_ceil(PAGE_SIZE) + 16);
    }
    assert_eq!(values.len(), 1);
    values.clear();
    values.push_front(10);
    values.push_back(11);
    assert!(values.iter().copied().eq([10, 11]));
}
