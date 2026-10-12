//! Constant-time arithmetic certificates for already-qualified constant windows.
//!
//! These functions do not establish readiness, constancy, or sample-count
//! agreement. The caller must establish those properties from its window state.

/// Prove that the original weighted multiplication/addition scan returns `value`.
///
/// Write the finite nonzero input as `sign * odd * 2^exponent`, stripping every
/// trailing zero from its binary64 significand. If `odd * n * (n + 1) / 2`
/// occupies at most 53 bits, every weighted term and every same-sign partial sum
/// also has at most 53 significant bits. All their coefficients are bounded by
/// that final coefficient. The finite-product check bounds their exponents, so
/// every original multiplication and addition is exact, including subnormals.
/// Division by the exact triangular denominator then returns the original bits.
///
/// This is deliberately a sufficient condition. In particular, do not strip
/// trailing zeros from the final coefficient: they need not occur in each
/// intermediate coefficient. Signed zeros are excluded because numerically
/// equal samples do not establish a common zero sign for the original scan.
pub(super) fn exact_constant_weighted_mean(value: f64, length: usize) -> Option<f64> {
    if length == 0 || !value.is_finite() || value == 0.0 {
        return None;
    }

    let bits = value.to_bits();
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    let significand = if bits & 0x7ff0_0000_0000_0000 == 0 {
        fraction
    } else {
        fraction | (1_u64 << 52)
    };
    let odd = significand >> significand.trailing_zeros();

    // As in weighted_denominator, widen before addition and multiplication.
    // Their product fits u128 on every supported usize target (including Wasm).
    let length = length as u128;
    let triangular = length * (length + 1) / 2;
    const MAX_COEFFICIENT: u128 = (1_u128 << 53) - 1;
    if triangular > MAX_COEFFICIENT {
        return None;
    }
    // The preceding bound makes this product at most 106 bits, so it cannot
    // overflow u128. Keeping the raw coefficient bounds every intermediate.
    if u128::from(odd) * triangular > MAX_COEFFICIENT {
        return None;
    }

    // The coefficient bound also proves triangular < 2^53 and hence an exact
    // f64 denominator. It implies n <= 2^27 - 1, so all original integer weights
    // have exact f64 casts as well.
    let denominator = triangular as f64;
    (value * denominator).is_finite().then_some(value)
}

/// Prove that a same-sign constant sum cannot have a finite reconstructed result.
///
/// For `p`, the largest power of two not exceeding the positive sample count,
/// `MAX / p` is exact. A finite binary64 magnitude strictly above that bound is
/// at least `2^1024 / p`, the next representable value. Therefore even `p`
/// copies have an exact sum of magnitude at least `2^1024`; additional copies
/// cannot reduce it. The existing power-of-two reconstruction must reject that
/// result, regardless of whether it requires an exact intermediate expansion.
/// Equality is deliberately unqualified, and no rounded count cast is needed.
pub(super) fn constant_sum_overflows(value: f64, count: usize) -> bool {
    if count == 0 || !value.is_finite() {
        return false;
    }
    let lower_count = 1_usize << count.ilog2();
    value.abs() > f64::MAX / lower_count as f64
}

#[cfg(test)]
mod tests {
    use super::{constant_sum_overflows, exact_constant_weighted_mean};

    /// Independent original scan, with its multiplication and addition order.
    /// All callers keep this reference bounded; large-count checks use only the
    /// arithmetic certificate and never allocate or iterate a large window.
    fn original_constant_weighted_mean(value: f64, length: usize) -> f64 {
        assert!(length != 0 && length <= 512);
        let weighted = (1..=length)
            .map(|weight| value * weight as f64)
            .sum::<f64>();
        let length = length as u128;
        weighted / (length * (length + 1) / 2) as f64
    }

    fn assert_certified_original_bits(value: f64, length: usize) {
        let certified = exact_constant_weighted_mean(value, length)
            .unwrap_or_else(|| panic!("unqualified {value:?}, length {length}"));
        assert_eq!(certified.to_bits(), value.to_bits());
        assert_eq!(
            certified.to_bits(),
            original_constant_weighted_mean(value, length).to_bits()
        );
    }

    #[test]
    fn weighted_certificate_matches_original_dyadic_and_subnormal_bits() {
        for magnitude in [
            1.0,
            0.5,
            1.25,
            1.5,
            3.0,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            f64::from_bits(3),
            f64::from_bits(1_u64 << 51),
            f64::from_bits(1123_u64 << 52),
            f64::from_bits(423_u64 << 52),
        ] {
            for value in [magnitude, -magnitude] {
                for length in [1, 2, 3, 7, 16, 31, 128, 255, 512] {
                    assert_certified_original_bits(value, length);
                }
            }
        }
    }

    #[test]
    fn weighted_certificate_rejects_zero_nonfinite_and_empty_inputs() {
        for value in [0.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for length in [0, 1, 2, 16, usize::MAX] {
                assert_eq!(exact_constant_weighted_mean(value, length), None);
            }
        }
        assert_eq!(exact_constant_weighted_mean(1.0, 0), None);
    }

    #[test]
    fn weighted_certificate_keeps_decimal_rounding_in_the_original_scan() {
        for value in [0.1_f64, -0.1_f64] {
            assert_certified_original_bits(value, 1);
            assert_eq!(exact_constant_weighted_mean(value, 2), None);
            assert_eq!(exact_constant_weighted_mean(value, 3), None);
            let original = original_constant_weighted_mean(value, 3);
            let expected_bits = value.to_bits() + 1;
            assert_eq!(original.to_bits(), expected_bits);
            assert_ne!(original.to_bits(), value.to_bits());
        }
    }

    #[test]
    fn weighted_certificate_checks_overflow_after_the_significand_bound() {
        for value in [f64::MAX, -f64::MAX] {
            assert_certified_original_bits(value, 1);
            assert_eq!(exact_constant_weighted_mean(value, 2), None);
        }
        // The odd significand is one, so coefficient precision alone passes.
        // A weighted numerator of 3 * 2^1023 still overflows.
        let huge = f64::from_bits(2046_u64 << 52);
        for value in [huge, -huge] {
            assert_certified_original_bits(value, 1);
            assert_eq!(exact_constant_weighted_mean(value, 2), None);
        }
        let finite = f64::from_bits(2045_u64 << 52);
        for value in [finite, -finite] {
            assert_certified_original_bits(value, 2);
        }
    }

    #[test]
    fn weighted_certificate_checks_large_count_boundaries_without_scanning() {
        if let Ok(last) = usize::try_from((1_u128 << 27) - 1) {
            for value in [1.0_f64, -1.0_f64, f64::from_bits(1)] {
                assert_eq!(
                    exact_constant_weighted_mean(value, last).map(f64::to_bits),
                    Some(value.to_bits())
                );
            }
        }
        if let Ok(first) = usize::try_from(1_u128 << 27) {
            let length = first as u128;
            let triangular = length * (length + 1) / 2;
            // T is exactly representable despite having 54 raw bits. That is
            // insufficient for the stricter partial-coefficient certificate.
            assert_eq!((triangular as f64) as u128, triangular);
            assert_eq!(exact_constant_weighted_mean(1.0, first), None);
        }
        assert_eq!(exact_constant_weighted_mean(1.0, usize::MAX), None);
        assert_eq!(exact_constant_weighted_mean(f64::MAX, usize::MAX), None);
    }

    #[test]
    fn weighted_certificate_matches_bounded_original_scans_for_generated_inputs() {
        let mut random = 0x92b4_03c8_d9e7_6a15_u64;
        let mut certified_count = 0;
        for index in 0..2048 {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let sign = random & (1_u64 << 63);
            let exponent = (random >> 32) % 2047;
            let fraction = random & 0x000f_ffff_ffff_ffff;
            let bits = match index % 4 {
                0 => sign | (exponent << 52) | fraction,
                1 => sign | (exponent << 52) | (fraction & !((1_u64 << 40) - 1)),
                2 => {
                    let shift = (random >> 24) % 37;
                    sign | (((random & 0xffff) | 1) << shift)
                }
                _ => sign | ((exponent.max(1)) << 52),
            };
            let value = f64::from_bits(bits);
            let length = ((random >> 16) % 32 + 1) as usize;
            if let Some(certified) = exact_constant_weighted_mean(value, length) {
                assert_eq!(certified.to_bits(), value.to_bits());
                assert_eq!(
                    certified.to_bits(),
                    original_constant_weighted_mean(value, length).to_bits(),
                    "value bits {bits:x}, length {length}"
                );
                certified_count += 1;
            }
        }
        assert!(certified_count > 512);
    }

    #[test]
    fn sum_overflow_certificate_is_strict_at_exact_power_of_two_thresholds() {
        for exponent in 0..usize::BITS {
            let count = 1_usize << exponent;
            let threshold = f64::MAX / count as f64;
            assert_eq!((threshold * count as f64).to_bits(), f64::MAX.to_bits());
            let below = f64::from_bits(threshold.to_bits() - 1);
            let above = f64::from_bits(threshold.to_bits() + 1);
            for sign in [1.0, -1.0] {
                assert!(!constant_sum_overflows(sign * below, count));
                assert!(!constant_sum_overflows(sign * threshold, count));
                if above.is_finite() {
                    assert!(constant_sum_overflows(sign * above, count));
                    assert!((above * count as f64).is_infinite());
                } else {
                    assert!(!constant_sum_overflows(sign * above, count));
                }
            }
        }
    }

    #[test]
    fn sum_overflow_certificate_keeps_unsupported_and_conservative_cases() {
        for value in [0.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for count in [0, 1, 2, 3, usize::MAX] {
                assert!(!constant_sum_overflows(value, count));
            }
        }
        assert!(!constant_sum_overflows(f64::MAX, 0));
        assert!(!constant_sum_overflows(f64::MAX, 1));
        assert!(constant_sum_overflows(f64::MAX, 2));
        assert!(constant_sum_overflows(-f64::MAX, 2));

        // Count three uses the lower power two. Equality is unqualified even
        // though the third addition really does overflow; false is conservative.
        let threshold = f64::MAX / 2.0;
        assert!(!constant_sum_overflows(threshold, 3));
        assert!((0..3).map(|_| threshold).sum::<f64>().is_infinite());
        let above = f64::from_bits(threshold.to_bits() + 1);
        assert!(constant_sum_overflows(above, 3));
        assert!(constant_sum_overflows(-above, 3));
    }
}
