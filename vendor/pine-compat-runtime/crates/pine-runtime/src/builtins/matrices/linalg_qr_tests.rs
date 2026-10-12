use super::{EIGEN_TOLERANCE, eigenvalues, eigenvectors, normalize_vector_columns, qr_decompose};

fn multiply(left: &[f64], right: &[f64], size: usize) -> Vec<f64> {
    (0..size)
        .flat_map(|row| {
            (0..size).map(move |column| {
                (0..size)
                    .map(|inner| left[row * size + inner] * right[inner * size + column])
                    .sum()
            })
        })
        .collect()
}

fn assert_factorization(values: &[f64], size: usize, tolerance: f64) -> Vec<f64> {
    let (q, r) = qr_decompose(values, size).unwrap();
    let reconstructed = multiply(&q, &r, size);
    for (actual, expected) in reconstructed.iter().zip(values) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "QR reconstruction: {actual} != {expected}"
        );
    }
    for left in 0..size {
        for right in 0..size {
            let product: f64 = (0..size)
                .map(|row| q[row * size + left] * q[row * size + right])
                .sum();
            let expected = if left == right { 1.0 } else { 0.0 };
            assert!(
                (product - expected).abs() <= 1e-14,
                "Q transpose Q ({left}, {right}) = {product}"
            );
        }
    }
    r
}

#[test]
fn qr_fallback_completes_zero_columns_without_changing_the_factorization() {
    for values in [
        [1.0, 2.0, 3.0, 0.0, 0.0, 4.0, 0.0, 0.0, 5.0],
        [0.0; 9],
        [0.0, 2.0, 3.0, 0.0, 0.0, 4.0, 0.0, 0.0, 5.0],
    ] {
        let r = assert_factorization(&values, 3, 0.0);
        assert_eq!(r[4], 0.0);
        if values[0] == 0.0 {
            assert_eq!(r[0], 0.0);
        }
    }
}

#[test]
fn qr_dependent_columns_keep_reconstruction_and_an_orthogonal_completed_basis() {
    let values = [1.0, 2.0, 0.0, 2.0, 4.0, 1.0, 3.0, 6.0, 2.0];
    let r = assert_factorization(&values, 3, 1e-14);
    assert_eq!(r[4], 0.0);
}

#[test]
fn qr_existing_near_zero_tolerance_discards_only_the_original_small_residual() {
    for residual in [0.0, -0.0, 0.5 * EIGEN_TOLERANCE, -0.5 * EIGEN_TOLERANCE] {
        let values = [1.0, 2.0, 3.0, 0.0, residual, 4.0, 0.0, 0.0, 5.0];
        let r = assert_factorization(&values, 3, residual.abs());
        assert_eq!(r[4], 0.0);
        assert_eq!(eigenvalues(&values, 3).unwrap(), [1.0, 0.0, 5.0]);
    }
    for residual in [2.0 * EIGEN_TOLERANCE, -2.0 * EIGEN_TOLERANCE] {
        let values = [1.0, 2.0, 3.0, 0.0, residual, 4.0, 0.0, 0.0, 5.0];
        let r = assert_factorization(&values, 3, 0.0);
        assert_eq!(r[4], residual.abs());
        assert_eq!(eigenvalues(&values, 3).unwrap(), [1.0, residual, 5.0]);
    }
}

#[test]
fn singular_upper_triangular_eigenvalues_match_its_analytic_diagonal() {
    let values = [1.0, 2.0, 3.0, 0.0, 0.0, 4.0, 0.0, 0.0, 5.0];
    let eigenvalues = eigenvalues(&values, 3).unwrap();
    // Upper triangular characteristic polynomial is (1-lambda)*(-lambda)*(5-lambda).
    assert_eq!(eigenvalues, [1.0, 0.0, 5.0]);
    assert_eq!(eigenvalues.iter().sum::<f64>(), 6.0);
    assert_eq!(eigenvalues.iter().product::<f64>(), 0.0);
    let vectors = eigenvectors(&values, 3).unwrap();
    for column in 0..3 {
        for row in 0..3 {
            let actual: f64 = (0..3)
                .map(|inner| values[row * 3 + inner] * vectors[inner * 3 + column])
                .sum();
            let expected = eigenvalues[column] * vectors[row * 3 + column];
            assert!(
                (actual - expected).abs() <= 1e-14,
                "A v = lambda v ({row}, {column}): {actual} != {expected}"
            );
        }
    }
}

#[test]
fn column_normalization_workspace_keeps_signs_and_preserves_ineligible_columns() {
    let columns = vec![3.0, 0.0, 1e-10, 4.0, -2.0, 0.0, 0.0, 0.0, 0.0];
    let normalized = normalize_vector_columns(columns, 3);
    let expected: [f64; 9] = [0.6, -0.0, 1e-10, 0.8, 1.0, 0.0, 0.0, -0.0, 0.0];
    assert_eq!(
        normalized
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    );
    assert!(normalize_vector_columns(Vec::new(), 0).is_empty());
}

#[test]
fn overflowing_qr_column_norm_recovers_repeated_triangular_eigenvalues() {
    let magnitude = 1e154;
    let values = [
        magnitude, 0.0, 0.0, magnitude, magnitude, 0.0, magnitude, 0.0, magnitude,
    ];
    assert!(super::scale::extreme_eigen_normalization(&values).is_none());
    assert!(qr_decompose(&values, 3).is_none());
    let roots = eigenvalues(&values, 3).unwrap();
    for root in roots {
        assert_eq!(root.to_bits(), magnitude.to_bits());
    }
}

#[test]
fn qr_exact_fixed_point_preserves_the_full_iteration_complex_classification() {
    let values = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 2.0];
    let (q, r) = qr_decompose(&values, 3).unwrap();
    let next = super::multiply_square(&r, &q, 3);
    assert!(super::lower_off_diagonal_norm(&next, 3) > EIGEN_TOLERANCE);
    assert!(super::finite_matrix_bits_unchanged(&values, &next));

    // Independent original iteration budget: every subsequent step is the
    // same matrix, so stopping here must still reject its +/-i pair.
    let mut reference = values.to_vec();
    for _ in 0..3 * 3 * 128 {
        let (q, r) = qr_decompose(&reference, 3).unwrap();
        reference = super::multiply_square(&r, &q, 3);
    }
    assert_eq!(
        reference
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        next.iter().map(|value| value.to_bits()).collect::<Vec<_>>()
    );
    assert_eq!(
        super::qr_eigenvalues(&values, 3, false),
        Err(super::EigenFailure::Complex)
    );
    assert!(eigenvalues(&values, 3).is_none());
    assert!(eigenvectors(&values, 3).is_none());
}

#[test]
fn qr_fixed_point_policy_distinguishes_signed_zero_rounding_and_nonfinite_states() {
    let values = [1.0, -0.0, 2.0, 0.0];
    assert!(super::finite_matrix_bits_unchanged(&values, &values));
    assert!(!super::finite_matrix_bits_unchanged(
        &values,
        &[1.0, 0.0, 2.0, 0.0]
    ));
    assert!(!super::finite_matrix_bits_unchanged(
        &values,
        &[1.0 + f64::EPSILON, -0.0, 2.0, 0.0]
    ));
    assert!(!super::finite_matrix_bits_unchanged(&values, &values[..3]));
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let values = [1.0, invalid];
        assert!(!super::finite_matrix_bits_unchanged(&values, &values));
    }
    let triangular = [1.0, 2.0, 3.0, -0.0, -0.0, 4.0, 0.0, -0.0, 5.0];
    let roots = eigenvalues(&triangular, 3).unwrap();
    assert_eq!(roots[0].to_bits(), 1.0_f64.to_bits());
    assert_eq!(roots[1].to_bits(), 0.0_f64.to_bits());
    assert_eq!(roots[2].to_bits(), 5.0_f64.to_bits());
}

#[test]
fn fixed_point_complex_and_signed_zero_control_run_through_the_public_interpreter() {
    let source = pine_syntax::SourceFile::new(
        "qr_fixedpoint.pine",
        r#"//@version=6
indicator("QR fixed point")
var a = matrix.new<float>(3, 3, 0)
var control = matrix.new<float>(3, 3, 0)
if barstate.isfirst
    matrix.set(a, 0, 1, -1)
    matrix.set(a, 1, 0, 1)
    matrix.set(a, 2, 2, 2)
    matrix.set(control, 0, 0, 1)
    matrix.set(control, 0, 1, 2)
    matrix.set(control, 0, 2, 3)
    matrix.set(control, 1, 0, -0.0)
    matrix.set(control, 1, 1, -0.0)
    matrix.set(control, 1, 2, 4)
    matrix.set(control, 2, 1, -0.0)
    matrix.set(control, 2, 2, 5)
plot(na(matrix.eigenvalues(a)) ? 1 : 0)
plot(na(matrix.eigenvectors(a)) ? 1 : 0)
roots = matrix.eigenvalues(control)
plot(array.get(roots, 0))
plot(array.get(roots, 1))
plot(array.get(roots, 2))
"#,
    );
    let analysis = pine_sema::analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.unwrap();
    let mut runtime = crate::HistoricalRuntime::new(&program);
    for index in 0..3 {
        runtime
            .append_bar(crate::Bar {
                time: index * 60_000,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            })
            .unwrap();
    }
    let result = runtime.result();
    for (plot, expected) in result.plots.iter().zip([
        crate::PineValue::Int(1),
        crate::PineValue::Int(1),
        crate::PineValue::Float(1.0),
        crate::PineValue::Float(0.0),
        crate::PineValue::Float(5.0),
    ]) {
        assert_eq!(plot.values, vec![expected; 3]);
    }
    assert_eq!(result.plots.len(), 5);
}
