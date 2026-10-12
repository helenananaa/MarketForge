use super::{eigenvalues, eigenvectors, scale};

#[test]
fn exact_binary_scale_handles_subnormals_and_guards_irreversible_mixed_inputs() {
    for magnitude in [
        f64::from_bits(1),
        f64::from_bits(2),
        f64::from_bits((1 << 52) - 1),
        f64::MIN_POSITIVE,
        1e-100,
        1e200,
        f64::MAX,
    ] {
        let factor = scale::binary_scale(magnitude);
        assert!(factor.is_finite() && factor > 0.0 && factor <= magnitude);
        let ratio = magnitude / factor;
        assert!((1.0..2.0).contains(&ratio));
        assert_eq!((ratio * factor).to_bits(), magnitude.to_bits());
    }
    assert!(scale::extreme_eigen_normalization(&[1e308, 1e-308]).is_none());
    assert!(scale::extreme_eigen_normalization(&[1.0, 2.0, 3.0]).is_none());
    assert!(scale::extreme_eigen_normalization(&[-0.0, 0.0]).is_none());
}

#[test]
fn extreme_rank_one_eigenvalues_and_vectors_satisfy_scale_invariant_equations() {
    for magnitude in [f64::from_bits(1), 1e-200, 1e-100, 1e200] {
        let values = [magnitude; 4];
        let eigenvalues = eigenvalues(&values, 2).unwrap();
        assert!((eigenvalues[0] / magnitude).abs() <= 1e-14);
        assert!((eigenvalues[1] / magnitude - 2.0).abs() <= 1e-14);
        let vectors = eigenvectors(&values, 2).unwrap();
        for column in 0..2 {
            let norm_squared: f64 = (0..2).map(|row| vectors[row * 2 + column].powi(2)).sum();
            assert!((norm_squared - 1.0).abs() <= 1e-14);
            for row in 0..2 {
                let projected: f64 = (0..2)
                    .map(|inner| {
                        (values[row * 2 + inner] / magnitude) * vectors[inner * 2 + column]
                    })
                    .sum();
                let expected = (eigenvalues[column] / magnitude) * vectors[row * 2 + column];
                assert!((projected - expected).abs() <= 1e-14);
            }
        }
    }
}

#[test]
fn extreme_non_symmetric_triangular_eigenvalues_preserve_the_analytic_diagonal() {
    let magnitude = 1e200;
    let values = [2.0 * magnitude, magnitude, 0.0, magnitude];
    let eigenvalues = eigenvalues(&values, 2).unwrap();
    assert!((eigenvalues[0] / magnitude - 2.0).abs() <= 1e-14);
    assert!((eigenvalues[1] / magnitude - 1.0).abs() <= 1e-14);
    let vectors = eigenvectors(&values, 2).unwrap();
    for column in 0..2 {
        for row in 0..2 {
            let projected: f64 = (0..2)
                .map(|inner| (values[row * 2 + inner] / magnitude) * vectors[inner * 2 + column])
                .sum();
            let expected = (eigenvalues[column] / magnitude) * vectors[row * 2 + column];
            assert!((projected - expected).abs() <= 1e-14);
        }
    }
}

#[test]
fn eigen_normalization_keeps_mixed_diagonals_singletons_and_signed_zeros() {
    for magnitude in [1e-100, 1e200, f64::MAX] {
        let original = [magnitude];
        assert_eq!(
            eigenvalues(&original, 1).unwrap()[0].to_bits(),
            magnitude.to_bits()
        );
        assert_eq!(eigenvectors(&original, 1).unwrap(), [1.0]);
    }
    let mixed = [1e308, 0.0, 0.0, 1e-308];
    let result = eigenvalues(&mixed, 2).unwrap();
    assert_eq!(result[0].to_bits(), mixed[0].to_bits());
    assert_eq!(result[1].to_bits(), mixed[3].to_bits());
    assert_eq!(eigenvectors(&mixed, 2).unwrap(), [1.0, 0.0, 0.0, 1.0]);
    let zero = [-0.0, 0.0, 0.0, -0.0];
    assert!(
        eigenvalues(&zero, 2)
            .unwrap()
            .iter()
            .all(|value| value.to_bits() == (-0.0_f64).to_bits())
    );
    assert_eq!(eigenvectors(&zero, 2).unwrap(), [1.0, 0.0, 0.0, 1.0]);
    assert!(eigenvalues(&[], 0).unwrap().is_empty());
}

#[test]
fn failed_two_by_two_formula_recovers_real_finite_roots_and_vectors() {
    let magnitude = 1e154;
    let values = [magnitude, magnitude, 0.5 * magnitude, magnitude];
    assert!(scale::extreme_eigen_normalization(&values).is_none());
    // The original trace and determinant are finite, but both terms of the
    // discriminant overflow. NaN.max(0) used to fabricate two equal roots.
    assert_eq!(
        super::two_by_two_eigenvalues(&values, false),
        Err(super::EigenFailure::Numerical)
    );
    let roots = eigenvalues(&values, 2).unwrap();
    let expected = [1.0 + 0.5_f64.sqrt(), 1.0 - 0.5_f64.sqrt()];
    for (actual, expected) in roots.iter().zip(expected) {
        assert!((actual / magnitude - expected).abs() <= 1e-14);
    }
    let vectors = eigenvectors(&values, 2).unwrap();
    let unit = [1.0, 1.0, 0.5, 1.0];
    for column in 0..2 {
        for row in 0..2 {
            let projected: f64 = (0..2)
                .map(|inner| unit[row * 2 + inner] * vectors[inner * 2 + column])
                .sum();
            assert!((projected - expected[column] * vectors[row * 2 + column]).abs() <= 1e-14);
        }
    }
}

#[test]
fn recovery_keeps_tiny_nonsymmetric_classification_and_rejects_complex_roots() {
    let magnitude = 1e-200;
    let values = [0.0, magnitude, (1.0 - 2e-13) * magnitude, 0.0];
    let (normalized, _) = scale::extreme_eigen_normalization(&values).unwrap();
    assert!(super::is_symmetric(&normalized, 2));
    let roots = eigenvalues(&values, 2).unwrap();
    let positive = ((values[1] / magnitude) * (values[2] / magnitude)).sqrt();
    assert!((roots[0] / magnitude - positive).abs() <= 2e-15);
    assert!((roots[1] / magnitude + positive).abs() <= 2e-15);
    let vectors = eigenvectors(&values, 2).unwrap();
    for column in 0..2 {
        for row in 0..2 {
            let projected: f64 = (0..2)
                .map(|inner| (values[row * 2 + inner] / magnitude) * vectors[inner * 2 + column])
                .sum();
            assert!(
                (projected - (roots[column] / magnitude) * vectors[row * 2 + column]).abs()
                    <= 1e-14
            );
        }
    }
    let complex = [0.0, magnitude, -magnitude, 0.0];
    assert!(eigenvalues(&complex, 2).is_none());
    assert!(eigenvectors(&complex, 2).is_none());
    let tiny_off_diagonal = [1e200, 1e185, -1e185, 1e200];
    assert!(eigenvalues(&tiny_off_diagonal, 2).is_none());
    assert!(eigenvectors(&tiny_off_diagonal, 2).is_none());
}

#[test]
fn ordinary_finite_complex_failure_is_not_reinterpreted_by_rescaling() {
    let values = [1000.0, 1e-5, -1e-5, 1000.0];
    assert!(scale::extreme_eigen_normalization(&values).is_none());
    assert_eq!(
        super::two_by_two_eigenvalues(&values, false),
        Err(super::EigenFailure::Complex)
    );
    assert!(eigenvalues(&values, 2).is_none());
    assert!(eigenvectors(&values, 2).is_none());
}

#[test]
fn equal_diagonal_complex_roots_are_rejected_before_cancellation_or_approximate_symmetry() {
    for diagonal in [0.0, -0.0, 1.0, -1.0, 1000.0, 1e154, 1e200] {
        for (upper, lower) in [
            (1e-8, -1e-8),
            (-1e-8, 1e-8),
            (1e-200, -1e-200),
            (f64::from_bits(1), -f64::from_bits(1)),
            (1e8, -1e-300),
        ] {
            let values = [diagonal, upper, lower, diagonal];
            // In exact arithmetic D = 4*upper*lower < 0. Computing their
            // product, or trace^2 - 4*det, may round this fact away.
            assert_eq!(
                super::two_by_two_eigenvalues(&values, false),
                Err(super::EigenFailure::Complex)
            );
            assert!(eigenvalues(&values, 2).is_none(), "{values:?}");
            assert!(eigenvectors(&values, 2).is_none(), "{values:?}");
        }
    }
    let tiny = [1.0, 1e-200, -1e-200, 1.0];
    assert!(super::is_symmetric(&tiny, 2));
    let ordinary = [1.0, 1e-8, -1e-8, 1.0];
    let trace = ordinary[0] + ordinary[3];
    let determinant = ordinary[0] * ordinary[3] - ordinary[1] * ordinary[2];
    assert_eq!(trace * trace - 4.0 * determinant, 0.0);
}

#[test]
fn exact_complex_predicate_keeps_zero_off_diagonals_and_rejects_nonfinite_proofs() {
    for zero in [0.0, -0.0] {
        for values in [[1.0, zero, -1e-8, 1.0], [1.0, 1e-8, zero, 1.0]] {
            assert!(!super::has_equal_diagonal_complex_roots(&values));
            assert_eq!(
                super::two_by_two_eigenvalues(&values, false).unwrap(),
                [1.0, 1.0]
            );
        }
    }
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for values in [[invalid, 1.0, -1.0, invalid], [1.0, invalid, -1.0, 1.0]] {
            assert!(!super::has_equal_diagonal_complex_roots(&values));
        }
    }
    assert!(!super::has_equal_diagonal_complex_roots(&[
        1.0,
        1e-8,
        -1e-8,
        1.0 + f64::EPSILON,
    ]));
}

#[test]
fn failed_triangular_kernel_preserves_mixed_diagonal_without_scaling() {
    for values in [[1e308, 1e308, 0.0, 1e-308], [1e-308, 0.0, 1e308, 1e308]] {
        assert!(scale::reversible_eigen_normalization(&values).is_none());
        let roots = eigenvalues(&values, 2).unwrap();
        assert_eq!(roots[0].to_bits(), 1e308_f64.to_bits());
        assert_eq!(roots[1].to_bits(), 1e-308_f64.to_bits());
        // The vector solver has no lossless normalized retry for these inputs.
        assert!(eigenvectors(&values, 2).is_none());
    }
}

#[test]
fn failed_nontriangular_kernel_rejects_irreversible_normalization() {
    let values = [1e154, 1e-200, 1e154, 1e154, 1e154, 0.0, 0.0, 1e154, 1e154];
    assert!(scale::extreme_eigen_normalization(&values).is_none());
    assert!(scale::reversible_eigen_normalization(&values).is_none());
    assert!(eigenvalues(&values, 3).is_none());
    assert!(eigenvectors(&values, 3).is_none());
}
