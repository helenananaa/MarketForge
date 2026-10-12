use super::*;

fn directory(count: usize) -> PageDirectory<usize> {
    PageDirectory::from_pages((0..count).map(|index| Arc::new(vec![index])).collect())
}

fn changed_nodes<T>(before: &Arc<Node<T>>, after: &Arc<Node<T>>) -> usize {
    if Arc::ptr_eq(before, after) {
        return 0;
    }
    1 + match (before.as_ref(), after.as_ref()) {
        (
            Node::Branch { children: left, .. },
            Node::Branch {
                children: right, ..
            },
        ) => left
            .iter()
            .zip(right)
            .map(|(left, right)| changed_nodes(left, right))
            .sum(),
        _ => 0,
    }
}

#[test]
fn sparse_writes_copy_only_one_bounded_radix_path_and_one_payload_page() {
    for count in [2, 32, 33, 1024, 1025, 4097] {
        let source = directory(count);
        let mut branch = source.clone();
        let index = count / 2;
        assert_eq!(branch.write_page(index, false), Some([index].as_slice()));
        Arc::make_mut(branch.get_mut(index))[0] = usize::MAX;
        let depth = if count <= 32 {
            1
        } else if count <= 1024 {
            2
        } else {
            3
        };
        assert_eq!(changed_nodes(&source.root, &branch.root), depth);
        assert_eq!(branch.write_page(index, false), None);
        assert_eq!(
            branch.write_page(index, true),
            Some([usize::MAX].as_slice())
        );
        for other in 0..count {
            assert_eq!(source.get(other).as_slice(), [other]);
            if other != index {
                assert!(Arc::ptr_eq(source.get(other), branch.get(other)));
                assert_eq!(branch.write_page(other, false), Some([other].as_slice()));
            }
        }
        drop(source);
        assert!(branch.copied_values(false).next().is_none());
    }
}

#[test]
fn growth_shrink_and_root_collapse_match_independent_page_list() {
    for count in [0, 1, 31, 32, 33, 1023, 1024, 1025] {
        let original = directory(count);
        let mut branch = original.clone();
        let mut expected = (0..count).collect::<Vec<_>>();
        for index in count..count + 65 {
            branch.push(Arc::new(vec![index]));
            expected.push(index);
            assert_eq!(
                branch.iter().map(|page| page[0]).collect::<Vec<_>>(),
                expected
            );
        }
        while !expected.is_empty() {
            branch.pop();
            expected.pop();
            assert_eq!(
                branch.iter().map(|page| page[0]).collect::<Vec<_>>(),
                expected
            );
        }
        branch.push(Arc::new(vec![42]));
        assert_eq!(branch.get(0).as_slice(), [42]);
        assert_eq!(
            original.iter().map(|page| page[0]).collect::<Vec<_>>(),
            (0..count).collect::<Vec<_>>()
        );
    }
}

#[test]
fn two_page_mutation_forks_common_ancestors_once_and_preserves_other_branches() {
    let original = directory(1025);
    for (left, right) in [(0, 1), (31, 32), (0, 1024)] {
        let mut branch = original.clone();
        let (first, second) = branch.two_mut(left, right);
        std::mem::swap(&mut Arc::make_mut(first)[0], &mut Arc::make_mut(second)[0]);
        assert_eq!(branch.get(left).as_slice(), [right]);
        assert_eq!(branch.get(right).as_slice(), [left]);
        for index in 0..1025 {
            assert_eq!(original.get(index).as_slice(), [index]);
            if index != left && index != right {
                assert!(Arc::ptr_eq(original.get(index), branch.get(index)));
            }
        }
    }
}

#[test]
fn leaf_and_ancestor_sharing_are_both_accounted_before_bulk_mutation() {
    let original = directory(1025);
    let mut branch = original.clone();
    Arc::make_mut(branch.get_mut(32))[0] = 1234;
    assert_eq!(branch.copied_values(false).count(), 1024);
    branch.for_each_mut(|page| Arc::make_mut(page)[0] += 10000);
    assert_eq!(branch.copied_values(false).count(), 0);
    assert_eq!(
        original.iter().map(|page| page[0]).collect::<Vec<_>>(),
        (0..1025).collect::<Vec<_>>()
    );
    assert_eq!(branch.get(32).as_slice(), [11234]);
    assert_eq!(branch.get(1024).as_slice(), [11024]);
}
