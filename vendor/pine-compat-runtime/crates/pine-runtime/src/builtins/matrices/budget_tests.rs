//! Budget failures must precede payload copying and in-place mutations.
use super::*;
use crate::ResourceLimits;

fn program() -> pine_ir::HirProgram {
    pine_sema::analyze_source(&pine_syntax::SourceFile::new(
        "matrix-budget.pine",
        "//@version=6\nindicator(\"budget\")\nplot(close)\n",
    ))
    .hir
    .unwrap()
}

fn initialized<'a>(
    program: &'a pine_ir::HirProgram,
    rows: usize,
    columns: usize,
) -> (HistoricalRuntime<'a>, u32) {
    let mut runtime = HistoricalRuntime::new(program);
    let value = runtime.insert_matrix_storage(
        MatrixElementKind::Float,
        rows,
        columns,
        (0..rows * columns)
            .map(|index| PineValue::Float(index as f64))
            .collect(),
    );
    let PineValue::Matrix(id) = value else {
        panic!("matrix")
    };
    runtime.collection_gc_allocated_bytes = 0;
    (runtime, id)
}

fn limited(runtime: HistoricalRuntime<'_>, cells: usize) -> HistoricalRuntime<'_> {
    runtime.with_resource_limits(ResourceLimits {
        max_collection_bytes_per_bar: Some(cells * std::mem::size_of::<PineValue>()),
        max_matrix_work_per_bar: None,
    })
}

#[test]
fn reverse_reserves_the_entry_induced_page_copies_once() {
    let program = program();
    for allowance in [4096, 4097] {
        let (runtime, id) = initialized(&program, 1, 4097);
        let checkpoint = runtime.clone();
        let original = checkpoint.matrix_store.get(&id).unwrap().values.to_vec();
        let mut runtime = limited(runtime, allowance);
        runtime.matrix_reverse(id);
        assert_eq!(
            checkpoint.matrix_store.get(&id).unwrap().values.to_vec(),
            original
        );
        if allowance == 4096 {
            assert!(runtime.resource_budget.check().is_err());
            assert_eq!(
                runtime.matrix_store.get(&id).unwrap().values.to_vec(),
                original
            );
            assert_eq!(runtime.collection_gc_allocated_bytes, 0);
        } else {
            runtime.resource_budget.check().unwrap();
            assert_eq!(
                runtime.matrix_store.get(&id).unwrap().values.to_vec(),
                original.into_iter().rev().collect::<Vec<_>>()
            );
            assert_eq!(
                runtime.collection_gc_allocated_bytes,
                4097 * std::mem::size_of::<PineValue>()
            );
        }
    }
}

#[test]
fn row_swaps_reserve_each_touched_shared_page_once() {
    let program = program();
    for allowance in [255, 256] {
        let (runtime, id) = initialized(&program, 100, 100);
        let checkpoint = runtime.clone();
        let mut expected = checkpoint.matrix_store.get(&id).unwrap().values.to_vec();
        let mut runtime = limited(runtime, allowance);
        let result = runtime.matrix_swap_rows(id, 40, 41);
        if allowance == 255 {
            assert!(result.unwrap_err().message.contains("E_RESOURCE_BUDGET"));
            assert_eq!(runtime.collection_gc_allocated_bytes, 0);
        } else {
            result.unwrap();
            for column in 0..100 {
                expected.swap(4000 + column, 4100 + column);
            }
            assert_eq!(
                runtime.collection_gc_allocated_bytes,
                256 * std::mem::size_of::<PineValue>()
            );
        }
        assert_eq!(
            runtime.matrix_store.get(&id).unwrap().values.to_vec(),
            expected
        );
    }
}

#[test]
fn rejected_structural_mutations_keep_the_original_payload_and_shape() {
    let program = program();
    for operation in 0..8 {
        let (runtime, id) = initialized(&program, 2, 2);
        let checkpoint = runtime.clone();
        let original = checkpoint.matrix_store.get(&id).unwrap().clone();
        let mut runtime = limited(runtime, 0);
        let result = match operation {
            0 => runtime.matrix_reshape(id, 1, 4),
            1 => runtime.matrix_concat(id, id).map(|_| ()),
            2 => runtime.matrix_add_row(id, 0, vec![PineValue::Float(0.0); 2]),
            3 => runtime.matrix_add_col(id, 0, vec![PineValue::Float(0.0); 2]),
            4 => runtime.matrix_remove_row(id, 0),
            5 => runtime.matrix_remove_col(id, 0),
            6 => runtime.matrix_swap_columns(id, 0, 1),
            _ => runtime.matrix_sort(id, 0, false),
        };
        assert!(
            result.unwrap_err().message.contains("E_RESOURCE_BUDGET"),
            "operation {operation}"
        );
        assert_eq!(
            runtime.matrix_store.get(&id).unwrap(),
            &original,
            "operation {operation}"
        );
        assert_eq!(runtime.collection_gc_allocated_bytes, 0);
    }
}

#[test]
fn power_counts_small_clones_identity_and_each_intermediate_payload() {
    let program = program();
    for (power, copied_cells) in [(0, 4), (1, 4), (2, 16), (8, 24)] {
        for allowance in [copied_cells - 1, copied_cells] {
            let (runtime, id) = initialized(&program, 2, 2);
            let mut runtime = limited(runtime, allowance);
            let result = runtime.matrix_pow(id, power);
            if allowance < copied_cells {
                assert!(result.unwrap_err().message.contains("E_RESOURCE_BUDGET"));
                assert_eq!(runtime.matrix_store.len(), 1);
            } else {
                assert!(matches!(result.unwrap(), PineValue::Matrix(_)));
                assert_eq!(
                    runtime.collection_gc_allocated_bytes,
                    copied_cells * std::mem::size_of::<PineValue>()
                );
            }
        }
    }
    let (runtime, id) = initialized(&program, 20, 20);
    let mut runtime = limited(runtime, 0);
    assert!(matches!(
        runtime.matrix_pow(id, 1).unwrap(),
        PineValue::Matrix(_)
    ));
    assert_eq!(runtime.collection_gc_allocated_bytes, 0);
}

#[test]
fn self_concat_does_not_charge_a_unique_small_target_copy() {
    let program = program();
    let (runtime, id) = initialized(&program, 1, 4);
    let mut runtime = limited(runtime, 8);
    runtime.matrix_concat(id, id).unwrap();
    assert_eq!(
        runtime.collection_gc_allocated_bytes,
        8 * std::mem::size_of::<PineValue>()
    );
    assert_eq!(runtime.matrix_shape(id), Some((2, 4)));
}

#[test]
fn arithmetic_outputs_reserve_only_the_result_without_cloning_inputs() {
    let program = program();
    for (operation, copied_cells) in [(0, 16), (1, 4), (2, 4), (3, 2), (4, 2), (5, 1)] {
        for allowance in [0, copied_cells] {
            let (mut runtime, id) = initialized(&program, 2, 2);
            let PineValue::Array(array) = runtime
                .new_array_from_values(ArrayElementKind::Float, vec![PineValue::Float(1.0); 2])
            else {
                panic!("array")
            };
            runtime.collection_gc_allocated_bytes = 0;
            let mut runtime = limited(runtime, allowance);
            let result = match operation {
                0 => runtime.matrix_kron(id, id),
                1 => runtime.matrix_diff(id, id),
                2 => runtime.matrix_mult_scalar(id, PineValue::Float(2.0)),
                3 => runtime.matrix_mult_array(id, array),
                4 => runtime.array_mult_matrix(array, id),
                _ => runtime.array_mult_array(array, array),
            };
            if allowance == 0 {
                assert!(
                    result.unwrap_err().message.contains("E_RESOURCE_BUDGET"),
                    "operation {operation}"
                );
                assert_eq!(runtime.matrix_store.len(), 1);
                assert_eq!(runtime.array_store.len(), 1);
            } else {
                assert!(matches!(
                    result.unwrap(),
                    PineValue::Matrix(_) | PineValue::Array(_)
                ));
                assert_eq!(
                    runtime.collection_gc_allocated_bytes,
                    copied_cells * std::mem::size_of::<PineValue>()
                );
            }
        }
    }
}
