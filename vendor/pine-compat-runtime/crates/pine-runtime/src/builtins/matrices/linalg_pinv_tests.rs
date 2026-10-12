use super::pseudo_inverse;

fn multiply(left: &[f64], right: &[f64], rows: usize, inner: usize, columns: usize) -> Vec<f64> {
    (0..rows)
        .flat_map(|row| {
            (0..columns).map(move |column| {
                (0..inner)
                    .map(|index| left[row * inner + index] * right[index * columns + column])
                    .sum()
            })
        })
        .collect()
}

fn assert_close(actual: &[f64], expected: &[f64], tolerance: f64) {
    assert_eq!(actual.len(), expected.len());
    let norm = expected.iter().map(|value| value.abs()).fold(0.0, f64::max);
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(
            actual.is_finite() && (actual - expected).abs() <= tolerance * norm,
            "actual {actual}, expected {expected}, norm {norm}"
        );
    }
}

#[test]
fn pseudoinverse_preserves_single_element_rank_across_finite_scales() {
    for scale in [1e-200, 1e-7, 1.0, 1e7, 1e200, -1e-200, -1e200] {
        let inverse = pseudo_inverse(&[scale], 1, 1);
        assert_close(&inverse, &[1.0 / scale], 1e-14);
        assert_close(&[scale * inverse[0] * scale], &[scale], 1e-14);
    }
    assert_eq!(pseudo_inverse(&[0.0], 1, 1), [0.0]);
    assert_eq!(pseudo_inverse(&[0.0; 6], 2, 3), [0.0; 6]);
    assert!(pseudo_inverse(&[], 0, 3).is_empty());
}

#[test]
fn pseudoinverse_rectangular_and_rank_deficient_identities_are_scale_invariant() {
    for (values, rows, columns) in [
        (vec![1.0, 0.0, 0.0, 2.0, 1.0, 1.0], 3, 2),
        (vec![1.0, 0.0, 1.0, 0.0, 2.0, 1.0], 2, 3),
        (vec![1.0, 2.0, 2.0, 4.0, 3.0, 6.0], 3, 2),
        (vec![1.0, 2.0, 3.0, 2.0, 4.0, 6.0], 2, 3),
    ] {
        let unit_inverse = pseudo_inverse(&values, rows, columns);
        for scale in [1e-150, 1e-7, 1.0, 1e7, 1e150] {
            let scaled: Vec<_> = values.iter().map(|value| value * scale).collect();
            let inverse = pseudo_inverse(&scaled, rows, columns);
            let expected: Vec<_> = unit_inverse.iter().map(|value| value / scale).collect();
            assert_close(&inverse, &expected, 1e-10);
            let left = multiply(&scaled, &inverse, rows, columns, rows);
            let reconstructed = multiply(&left, &scaled, rows, rows, columns);
            assert_close(&reconstructed, &scaled, 1e-10);
            let right = multiply(&inverse, &scaled, columns, rows, columns);
            let reconstructed_inverse = multiply(&right, &inverse, columns, columns, rows);
            assert_close(&reconstructed_inverse, &inverse, 1e-10);
        }
    }
}

#[test]
fn pseudoinverse_rank_cutoff_depends_on_relative_singular_values() {
    for scale in [1e-150, 1.0, 1e150] {
        let retained = pseudo_inverse(&[scale, 0.0, 0.0, scale * 2e-6], 2, 2);
        assert_close(&retained, &[1.0 / scale, 0.0, 0.0, 5e5 / scale], 1e-10);
        let discarded = pseudo_inverse(&[scale, 0.0, 0.0, scale * 5e-7], 2, 2);
        assert_close(&discarded, &[1.0 / scale, 0.0, 0.0, 0.0], 1e-10);
    }
}

#[test]
fn pseudoinverse_binary_scaling_retains_standard_matrix_precision() {
    let values = [58.0, 64.0, 139.0, 154.0];
    let inverse = pseudo_inverse(&values, 2, 2);
    // These are the original unscaled Gram/Jacobi results. Arbitrary maxabs
    // division introduced a 1.07e-9 error in the first cell of this fixture.
    assert_eq!(
        inverse,
        [
            4.277777777811325,
            -1.7777777777922656,
            -3.8611111111414096,
            1.611111111124196,
        ]
    );
    assert_close(
        &inverse,
        &[154.0 / 36.0, -64.0 / 36.0, -139.0 / 36.0, 58.0 / 36.0],
        1e-11,
    );
    let left = multiply(&values, &inverse, 2, 2, 2);
    assert_close(&multiply(&left, &values, 2, 2, 2), &values, 1e-12);
    let right = multiply(&inverse, &values, 2, 2, 2);
    assert_close(&multiply(&right, &inverse, 2, 2, 2), &inverse, 1e-11);
}

#[test]
fn pseudoinverse_binary_scale_handles_subnormal_inputs_without_zero_scale() {
    for scale in [
        f64::from_bits(1 << 51),
        f64::from_bits((1 << 52) - 1),
        f64::MIN_POSITIVE,
        f64::MAX,
    ] {
        assert_close(&pseudo_inverse(&[scale], 1, 1), &[1.0 / scale], 1e-14);
    }
    for scale in [f64::from_bits(1), f64::from_bits(2), -f64::from_bits(1)] {
        // The true reciprocal is unrepresentable. The runtime converts the
        // overflow to na; normalization must not produce zero or NaN instead.
        assert_eq!(pseudo_inverse(&[scale], 1, 1), [1.0 / scale]);
    }
}

#[test]
fn pseudoinverse_projection_workspace_overwrites_after_discarded_components() {
    // Both Gram branches retain the first/last components and skip the middle.
    let tall = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0];
    let tall_inverse = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0];
    assert_eq!(pseudo_inverse(&tall, 4, 3), tall_inverse);
    let wide = tall_inverse;
    assert_eq!(pseudo_inverse(&wide, 3, 4), tall);
    assert_eq!(pseudo_inverse(&[0.0; 12], 4, 3), [0.0; 12]);
    assert_eq!(pseudo_inverse(&[0.0; 12], 3, 4), [0.0; 12]);
}
