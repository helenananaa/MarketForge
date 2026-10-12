use super::*;
use pine_syntax::SourceFile;

fn program() -> pine_ir::HirProgram {
    pine_sema::analyze_source(&SourceFile::new(
        "matrix_checkpoint.pine",
        "//@version=6\nindicator(\"matrix checkpoint\")\ntype Item\n    int value\n",
    ))
    .hir
    .expect("HIR")
}

#[test]
fn paged_matrix_reshape_copy_and_cross_page_strings_are_independent() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let PineValue::Matrix(id) = runtime
        .new_matrix(
            MatrixElementKind::String,
            3,
            129,
            PineValue::String("seed".into()),
        )
        .unwrap()
    else {
        panic!("matrix")
    };
    runtime
        .matrix_set_value(id, 0, 127, PineValue::String("left".into()))
        .unwrap();
    runtime
        .matrix_set_value(id, 0, 128, PineValue::String("right".into()))
        .unwrap();
    let checkpoint = runtime.clone();
    let PineValue::Matrix(copy) = runtime.copy_matrix(id) else {
        panic!("copy")
    };
    runtime.matrix_reshape(id, 129, 3).unwrap();
    runtime
        .matrix_set_value(id, 42, 2, PineValue::String("changed".into()))
        .unwrap();
    runtime.matrix_swap_rows(copy, 0, 1).unwrap();
    runtime.matrix_swap_columns(copy, 127, 128).unwrap();
    assert_eq!(checkpoint.matrix_shape(id), Some((3, 129)));
    assert_eq!(runtime.matrix_shape(id), Some((129, 3)));
    assert_eq!(
        checkpoint.matrix_get_cloned(id, 0, 128).unwrap(),
        Some(PineValue::String("right".into()))
    );
    assert_eq!(
        runtime.matrix_get_cloned(id, 42, 2).unwrap(),
        Some(PineValue::String("changed".into()))
    );
    assert_eq!(
        runtime.matrix_get_cloned(copy, 1, 127).unwrap(),
        Some(PineValue::String("right".into()))
    );
    assert_eq!(
        runtime.matrix_get_cloned(copy, 1, 128).unwrap(),
        Some(PineValue::String("left".into()))
    );
}

#[test]
fn paged_udt_matrix_copy_keeps_object_aliases_and_checkpoint_rollback() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let identity = &program.user_types[0].identity;
    let object = runtime
        .allocate_object(vec![PineValue::Int(7)], identity)
        .unwrap();
    let PineValue::UserTypeRef(object_id) = object else {
        panic!("object")
    };
    let PineValue::Matrix(id) = runtime
        .new_matrix(MatrixElementKind::UserType(0), 3, 129, object.clone())
        .unwrap()
    else {
        panic!("matrix")
    };
    let checkpoint = runtime.clone();
    let PineValue::Matrix(copy) = runtime.copy_matrix(id) else {
        panic!("copy")
    };
    runtime
        .set_object_field(object_id, 0, PineValue::Int(9))
        .unwrap();
    let replacement = runtime
        .allocate_object(vec![PineValue::Int(11)], identity)
        .unwrap();
    runtime
        .matrix_set_value(copy, 0, 128, replacement.clone())
        .unwrap();
    assert_eq!(
        runtime.matrix_get_cloned(id, 0, 128).unwrap(),
        Some(object.clone())
    );
    assert_eq!(
        runtime.matrix_get_cloned(copy, 0, 127).unwrap(),
        Some(object.clone())
    );
    assert_eq!(
        runtime.matrix_get_cloned(copy, 0, 128).unwrap(),
        Some(replacement)
    );
    assert_eq!(
        runtime.materialize_object(&object).unwrap(),
        PineValue::UserType(vec![PineValue::Int(9)])
    );
    assert_eq!(
        checkpoint.materialize_object(&object).unwrap(),
        PineValue::UserType(vec![PineValue::Int(7)])
    );
    assert_eq!(
        checkpoint.matrix_get_cloned(id, 0, 128).unwrap(),
        Some(object)
    );
}

fn reset_pressure(runtime: &mut HistoricalRuntime<'_>) {
    runtime.collection_gc_allocated_bytes = 0;
    runtime.collection_gc_next_bytes = 2 * 1024 * 1024;
    runtime.collection_gc_next_id = 1024;
}

fn large_string() -> PineValue {
    PineValue::String("s".repeat(40 * 1024))
}

#[test]
fn matrix_string_writes_and_fill_collect_short_lived_payloads_before_id_limit() {
    let program = program();
    for fill in [false, true] {
        let mut runtime = HistoricalRuntime::new(&program);
        let PineValue::Matrix(id) = runtime
            .new_matrix(
                MatrixElementKind::String,
                1,
                100,
                PineValue::String(String::new()),
            )
            .unwrap()
        else {
            panic!("matrix")
        };
        reset_pressure(&mut runtime);
        if fill {
            runtime.matrix_fill_value(id, large_string());
        } else {
            for column in 0..100 {
                runtime
                    .matrix_set_value(id, 0, column, large_string())
                    .unwrap();
            }
        }
        assert!(runtime.next_matrix_id < 1024);
        runtime.collect_temporary_collections();
        assert_eq!(runtime.matrix_store_profile().slots, 0, "fill={fill}");
    }
}

#[test]
fn small_matrix_string_copies_collect_without_waiting_for_1024_handles() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let PineValue::Matrix(id) = runtime
        .new_matrix(MatrixElementKind::String, 1, 100, large_string())
        .unwrap()
    else {
        panic!("matrix")
    };
    runtime
        .current_symbols
        .insert(pine_ir::SymbolId(0), PineValue::Matrix(id));
    reset_pressure(&mut runtime);
    for _ in 0..3 {
        assert!(matches!(runtime.copy_matrix(id), PineValue::Matrix(_)));
        runtime.collect_temporary_collections();
        assert_eq!(runtime.matrix_store_profile().slots, 1);
        assert_eq!(
            runtime.matrix_get_cloned(id, 0, 99).unwrap(),
            Some(large_string())
        );
    }
    assert_eq!(runtime.next_matrix_id, 4);
}

#[test]
fn shared_string_matrix_page_write_counts_neighbors_and_keeps_checkpoint() {
    let program = program();
    for cells in [100, 257] {
        let mut runtime = HistoricalRuntime::new(&program);
        let PineValue::Matrix(id) = runtime
            .new_matrix(MatrixElementKind::String, 1, cells, large_string())
            .unwrap()
        else {
            panic!("matrix")
        };
        let checkpoint = runtime.clone();
        reset_pressure(&mut runtime);
        let last = cells - 1;
        runtime
            .matrix_set_value(id, 0, last, PineValue::String(String::new()))
            .unwrap();
        let copied_cells = if cells == 100 { 100 } else { 1 };
        let expected = value_allocation_bytes(&large_string()) * copied_cells
            + value_allocation_bytes(&PineValue::String(String::new()));
        assert_eq!(runtime.collection_gc_allocated_bytes, expected);
        assert_eq!(
            checkpoint.matrix_get_cloned(id, 0, last).unwrap(),
            Some(large_string())
        );
    }
}

#[test]
fn paged_matrix_copy_and_reshape_do_not_charge_shared_string_cells() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let PineValue::Matrix(id) = runtime
        .new_matrix(MatrixElementKind::String, 1, 256, large_string())
        .unwrap()
    else {
        panic!("matrix")
    };
    reset_pressure(&mut runtime);
    let PineValue::Matrix(copy) = runtime.copy_matrix(id) else {
        panic!("copy")
    };
    runtime.matrix_reshape(copy, 2, 128).unwrap();
    assert_eq!(runtime.collection_gc_allocated_bytes, 0);
    assert_eq!(
        runtime.matrix_get_cloned(copy, 1, 127).unwrap(),
        Some(large_string())
    );
    assert_eq!(runtime.matrix_shape(id), Some((1, 256)));
}

#[test]
fn matrix_string_structural_growth_collects_new_payloads() {
    let program = program();
    for columns in [false, true] {
        let mut runtime = HistoricalRuntime::new(&program);
        let PineValue::Matrix(id) = runtime
            .new_matrix(
                MatrixElementKind::String,
                0,
                0,
                PineValue::String(String::new()),
            )
            .unwrap()
        else {
            panic!("matrix")
        };
        reset_pressure(&mut runtime);
        let values = (0..100).map(|_| large_string()).collect();
        if columns {
            runtime.matrix_add_col(id, 0, values).unwrap();
        } else {
            runtime.matrix_add_row(id, 0, values).unwrap();
        }
        runtime.collect_temporary_collections();
        assert_eq!(runtime.matrix_store_profile().slots, 0, "columns={columns}");
    }
}
