use super::*;
use pine_syntax::SourceFile;

fn program() -> pine_ir::HirProgram {
    pine_sema::analyze_source(&SourceFile::new(
        "matrix_linalg.pine",
        "//@version=6\nindicator(\"matrix linalg\")\n",
    ))
    .hir
    .expect("HIR")
}

fn matrix_values(runtime: &HistoricalRuntime<'_>, id: u32) -> Vec<PineValue> {
    runtime
        .matrix_store
        .get(&id)
        .unwrap()
        .values
        .iter()
        .cloned()
        .collect()
}

#[test]
fn matrix_numeric_results_remain_independent_of_small_input_and_result_aliases() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let PineValue::Matrix(input) = runtime
        .new_matrix(MatrixElementKind::Float, 2, 2, PineValue::Float(0.0))
        .unwrap()
    else {
        panic!("matrix")
    };
    runtime
        .matrix_set_value(input, 0, 0, PineValue::Float(2.0))
        .unwrap();
    runtime
        .matrix_set_value(input, 1, 1, PineValue::Float(4.0))
        .unwrap();
    let input_alias = input;
    let input_values = matrix_values(&runtime, input);
    let PineValue::Matrix(inverse) = runtime.matrix_inv(input_alias).unwrap() else {
        panic!("inverse")
    };
    let PineValue::Matrix(pinv) = runtime.matrix_pinv(input_alias) else {
        panic!("pseudo inverse")
    };
    let PineValue::Array(eigenvalues) = runtime.matrix_eigenvalues(input_alias).unwrap() else {
        panic!("eigenvalues")
    };
    let PineValue::Matrix(eigenvectors) = runtime.matrix_eigenvectors(input_alias).unwrap() else {
        panic!("eigenvectors")
    };
    assert_ne!(input, inverse);
    assert_ne!(input, pinv);
    assert_ne!(input, eigenvectors);
    assert_eq!(matrix_values(&runtime, input), input_values);
    let expected_inverse = vec![
        PineValue::Float(0.5),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(0.25),
    ];
    assert_eq!(matrix_values(&runtime, inverse), expected_inverse);
    assert_eq!(matrix_values(&runtime, pinv), expected_inverse);
    let expected_vectors = vec![
        PineValue::Float(1.0),
        PineValue::Float(0.0),
        PineValue::Float(0.0),
        PineValue::Float(1.0),
    ];
    assert_eq!(matrix_values(&runtime, eigenvectors), expected_vectors);
    let expected_eigenvalues = vec![PineValue::Float(2.0), PineValue::Float(4.0)];
    assert_eq!(
        runtime.array_values_clone(eigenvalues).unwrap().unwrap(),
        expected_eigenvalues
    );

    runtime
        .matrix_set_value(input_alias, 0, 0, PineValue::Float(99.0))
        .unwrap();
    assert_eq!(matrix_values(&runtime, inverse), expected_inverse);
    assert_eq!(matrix_values(&runtime, pinv), expected_inverse);
    assert_eq!(matrix_values(&runtime, eigenvectors), expected_vectors);
    assert_eq!(
        runtime.array_values_clone(eigenvalues).unwrap().unwrap(),
        expected_eigenvalues
    );
    runtime
        .matrix_set_value(inverse, 1, 1, PineValue::Float(77.0))
        .unwrap();
    assert_eq!(
        runtime.matrix_get_cloned(input, 1, 1).unwrap(),
        Some(PineValue::Float(4.0))
    );
    assert_eq!(matrix_values(&runtime, pinv), expected_inverse);
}

#[test]
fn matrix_rank_maps_unrecoverable_elimination_to_na() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let PineValue::Matrix(input) = runtime
        .new_matrix(MatrixElementKind::Float, 3, 3, PineValue::Float(1e308))
        .unwrap()
    else {
        panic!("matrix")
    };
    for (row, column, value) in [(0, 1, 1e-308), (1, 0, -1e308), (2, 0, 0.0), (2, 2, -1e308)] {
        runtime
            .matrix_set_value(input, row, column, PineValue::Float(value))
            .unwrap();
    }
    let original = matrix_values(&runtime, input);
    assert_eq!(runtime.matrix_rank(input), Some(PineValue::Na));
    assert_eq!(matrix_values(&runtime, input), original);
}
