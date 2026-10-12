use std::cell::Cell;

use super::{super::scale, rank};

#[test]
fn rank_retries_only_non_finite_elimination_and_recovers_large_mirrored_rows() {
    for magnitude in [1e308, -1e308] {
        let original = [
            magnitude, magnitude, magnitude, -magnitude, magnitude, magnitude, 0.0, magnitude,
            -magnitude,
        ];
        let mut working = original;
        let retries = Cell::new(0);
        let actual = rank(&mut working, 3, 3, || {
            retries.set(retries.get() + 1);
            original.to_vec()
        });
        // Dividing each row by magnitude gives determinant -4 and full rank.
        assert_eq!(actual, Some(3));
        assert_eq!(retries.get(), 1);
    }
}

#[test]
fn rank_ordinary_mixed_diagonal_and_tiny_pivots_keep_the_original_path() {
    for (original, expected) in [
        ([1e308, 0.0, 0.0, 1e-308], 2),
        ([1e-308, 1e308, 0.0, 1.0], 2),
        ([f64::from_bits(1), 0.0, 0.0, f64::from_bits(2)], 2),
        ([-0.0, 0.0, 0.0, -0.0], 0),
        ([1.0, 2.0, 2.0, 4.0], 1),
    ] {
        let mut values = original;
        assert_eq!(
            rank(&mut values, 2, 2, || panic!(
                "ordinary rank requested backup"
            )),
            Some(expected)
        );
    }
    assert_eq!(
        rank(&mut [], 0, 0, || panic!("empty rank requested backup")),
        Some(0)
    );
}

#[test]
fn rank_row_scaling_preserves_every_nonzero_and_rejects_irreversible_rows() {
    let original = [1e308, -0.0, 0.0, 1e-308];
    let mut reversible = original;
    assert!(scale::normalize_rows_reversibly(&mut reversible, 2, 2));
    assert!(reversible[0] > 0.0 && reversible[3] > 0.0);
    assert_eq!(reversible[1].to_bits(), original[1].to_bits());
    let mut irreversible = [1e308, 1e-308];
    assert!(!scale::normalize_rows_reversibly(&mut irreversible, 1, 2));

    let original = [
        1e308, 1e-308, 1e308, -1e308, 1e308, 1e308, 0.0, 1e308, -1e308,
    ];
    let mut legacy = original;
    super::eliminate(&mut legacy, 3, 3);
    assert!(legacy.iter().any(|value| !value.is_finite()));
    let mut values = original;
    let retries = Cell::new(0);
    assert_eq!(
        rank(&mut values, 3, 3, || {
            retries.set(retries.get() + 1);
            original.to_vec()
        }),
        None
    );
    assert_eq!(retries.get(), 1);
}

#[test]
fn rank_column_retry_recovers_wide_scale_rows_from_the_original_input() {
    for magnitude in [1e308, -1e308] {
        for tiny in [1e-308, -1e-308, f64::from_bits(1), -f64::from_bits(1)] {
            let original = [
                magnitude, magnitude, magnitude, tiny, -magnitude, magnitude, magnitude, -0.0, 0.0,
                magnitude, -magnitude, 0.0, -0.0, 0.0, 0.0, 1.0,
            ];
            let mut row_scaled = original;
            assert!(!scale::normalize_rows_reversibly(&mut row_scaled, 4, 4));
            let mut working = original;
            let retries = Cell::new(0);
            assert_eq!(
                rank(&mut working, 4, 4, || {
                    retries.set(retries.get() + 1);
                    original.to_vec()
                }),
                Some(4)
            );
            assert_eq!(retries.get(), 1);
            // The leading 3x3 block has determinant -4*magnitude^3;
            // the last row makes the whole matrix full rank for any tiny.
            assert!(working.iter().all(|value| value.is_finite()));
        }
    }
}

#[test]
fn column_scaling_preserves_signed_zeros_subnormals_and_rectangular_shapes() {
    let tiny = f64::from_bits(1);
    let original = [-0.0, tiny, 1e308, 0.0, tiny * 2.0, -1e308];
    let mut values = original;
    assert!(scale::normalize_columns_reversibly(&mut values, 2, 3));
    assert_eq!(values[0].to_bits(), original[0].to_bits());
    assert_eq!(values[3].to_bits(), original[3].to_bits());
    assert_eq!(values[1], 0.5);
    assert_eq!(values[4], 1.0);
    assert_eq!(values[2], -values[5]);
    for (index, value) in values.iter().enumerate() {
        let column = index % 3;
        let magnitude = original[column].abs().max(original[3 + column].abs());
        let round_trip = if magnitude == 0.0 {
            *value
        } else {
            *value * scale::binary_scale(magnitude)
        };
        assert_eq!(round_trip.to_bits(), original[index].to_bits());
    }
    let mut irreversible = [1e308, 0.0, 1e-308, 1.0];
    assert!(!scale::normalize_columns_reversibly(
        &mut irreversible,
        2,
        2
    ));
    assert!(scale::normalize_columns_reversibly(&mut [], 0, 3));
    assert!(scale::normalize_columns_reversibly(&mut [], 3, 0));
}
