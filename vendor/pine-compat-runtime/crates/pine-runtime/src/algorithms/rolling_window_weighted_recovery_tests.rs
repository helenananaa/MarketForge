use super::*;

fn samples(values: &[f64]) -> RollingWindowState {
    let mut window = RollingWindowState::default();
    for &value in values {
        window.push(Some(value), values.len());
    }
    window
}

fn assert_one_ulp(actual: f64, expected: f64) {
    assert!(actual.is_finite(), "expected {expected}, got {actual}");
    if expected == 0.0 {
        assert_eq!(actual, expected);
    } else {
        assert_eq!(actual.is_sign_negative(), expected.is_sign_negative());
        assert!(
            actual.to_bits().abs_diff(expected.to_bits()) <= 1,
            "expected {expected} ({:x}), got {actual} ({:x})",
            expected.to_bits(),
            actual.to_bits()
        );
    }
}

#[test]
fn weighted_overflow_recovers_extreme_constant_bits_for_full_and_tail_scans() {
    for length in [2_usize, 3, 16, 257, 512, 5000] {
        for value in [1e308, -1e308, f64::MAX, -f64::MAX] {
            let window = samples(&vec![value; length]);
            assert_eq!(window.weighted_mean(length).to_bits(), value.to_bits());
            let (full, tail) = window.weighted_mean_with_tail(length, (length / 2).max(1));
            assert_eq!(full.to_bits(), value.to_bits());
            assert_eq!(tail.to_bits(), value.to_bits());
        }
    }
}

#[test]
fn weighted_overflow_matches_exact_float_fraction_oracles_for_nonconstant_sources() {
    // Expected bits were calculated from Fraction.from_float(input) with exact
    // integer weights and an exact triangular denominator, then rounded to f64.
    let cases: &[(&[f64], u64)] = &[
        (&[1e308, 8e307], 0x7fde_dab7_2c65_7de2),
        (&[1e308, -1e308], 0xffc7_bbef_5d3a_60d5),
        (&[1e308, 8e307, -6e307], 0x7fb2_fcbf_7dc8_4d78),
        (&[1e308, -1e308, -1e308], 0xffd7_bbef_5d3a_60d5),
        (&[1e308, 1e308, -1e308], 0),
        (&[1e308, 1e308, -1e308, 1.0], 0x3fd9_9999_9999_999a),
        (&[1e308, 1e308, 1.0, -1e308], 0xffac_7b1f_3cac_7433),
    ];
    for &(values, expected_bits) in cases {
        for sign in [1.0, -1.0] {
            let values: Vec<_> = values.iter().map(|value| sign * value).collect();
            let window = samples(&values);
            let expected = sign * f64::from_bits(expected_bits);
            assert_one_ulp(window.weighted_mean(values.len()), expected);
            let (full, tail) = window.weighted_mean_with_tail(values.len(), values.len());
            assert_one_ulp(full, expected);
            assert_one_ulp(tail, expected);
        }
    }
}

#[test]
fn weighted_recovery_keeps_unready_nonfinite_and_nonreversible_scales_unsupported() {
    let mut unready = RollingWindowState::default();
    unready.push(Some(1e308), 3);
    unready.push(Some(1e308), 3);
    assert!(!unready.is_ready(3));
    assert!(unready.weighted_mean(3).is_infinite());
    let (full, tail) = unready.weighted_mean_with_tail(3, 2);
    assert!(full.is_infinite());
    assert!(tail.is_infinite());

    for values in [
        vec![Some(1e308), None, Some(1e308)],
        vec![Some(f64::INFINITY), Some(1.0)],
        vec![Some(f64::NEG_INFINITY), Some(f64::INFINITY)],
        vec![Some(f64::NAN), Some(1.0)],
        vec![Some(1e308), Some(1e-308), Some(1e308)],
    ] {
        let length = values.len();
        let mut window = RollingWindowState::default();
        for value in values {
            window.push(value, length);
        }
        assert_eq!(window.recovered_weighted_mean(length, 0), None);
        assert!(!window.weighted_mean(length).is_finite());
    }
    let empty_tail = samples(&[1e308, 1e308]);
    assert_eq!(empty_tail.recovered_weighted_mean(2, 2), None);
    assert!(empty_tail.weighted_mean_with_tail(2, 0).1.is_nan());

    // All inputs survive normalization, but three separated nonzero components
    // cannot fit the bounded exact expansion, so this cold scan remains NA.
    let third_component = samples(&[1e308, 1e308, 2.0_f64.powi(923), 1.0, -1e308]);
    assert_eq!(third_component.recovered_weighted_mean(5, 0), None);
    assert!(!third_component.weighted_mean(5).is_finite());
}

#[test]
fn weighted_recovery_restores_dynamic_length_na_and_discarded_forming_samples() {
    let mut window = samples(&[1e308, 8e307, -6e307]);
    let checkpoint = window.clone();
    window.push_for_bar(Some(1e308), 2, 7);
    assert_one_ulp(window.weighted_mean(2), 4.666666666666667e307);

    window.push_for_bar(None, 2, 7);
    assert!(!window.is_ready(2));
    window.push_for_bar(Some(1e308), 3, 7);
    let mut once = checkpoint.clone();
    once.push_for_bar(Some(1e308), 3, 7);
    assert_eq!(window.values, once.values);
    assert_eq!(
        window.weighted_mean(3).to_bits(),
        once.weighted_mean(3).to_bits()
    );
    assert_eq!(
        window.weighted_mean_with_tail(3, 2),
        once.weighted_mean_with_tail(3, 2)
    );
    window.discard_for_bar(7);
    assert_eq!(window.values, checkpoint.values);
    assert_eq!(window.sum.to_bits(), checkpoint.sum.to_bits());
    assert_eq!(window.na_count, checkpoint.na_count);
    assert_one_ulp(
        window.weighted_mean(3),
        f64::from_bits(0x7fb2_fcbf_7dc8_4d78),
    );
}
