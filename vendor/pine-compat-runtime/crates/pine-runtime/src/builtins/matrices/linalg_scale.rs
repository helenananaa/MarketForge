//! Exact binary scaling for bounded numerical recovery paths.

/// Highest represented power of two at or below a positive finite magnitude.
pub(super) fn binary_scale(magnitude: f64) -> f64 {
    let bits = magnitude.to_bits();
    let exponent = bits & 0x7ff0_0000_0000_0000;
    f64::from_bits(if exponent != 0 {
        exponent
    } else {
        // Subnormal powers cannot be constructed with powi without underflow.
        1_u64 << bits.ilog2()
    })
}

fn reversible_scaled(value: f64, scale: f64) -> Option<f64> {
    let normalized = value / scale;
    (normalized.is_finite() && (normalized * scale).to_bits() == value.to_bits())
        .then_some(normalized)
}

pub(super) fn extreme_eigen_normalization(values: &[f64]) -> Option<(Vec<f64>, f64)> {
    let magnitude = values.iter().map(|value| value.abs()).fold(0.0, f64::max);
    if magnitude == 0.0 || (magnitude > super::EIGEN_TOLERANCE && magnitude <= f64::MAX.sqrt()) {
        return None;
    }
    eigen_normalization_at_magnitude(values, magnitude)
}

/// Retry a failed eigensolver only if binary normalization preserves every bit.
pub(super) fn reversible_eigen_normalization(values: &[f64]) -> Option<(Vec<f64>, f64)> {
    let magnitude = values.iter().map(|value| value.abs()).fold(0.0, f64::max);
    if magnitude == 0.0 || !magnitude.is_finite() {
        return None;
    }
    eigen_normalization_at_magnitude(values, magnitude)
}

fn eigen_normalization_at_magnitude(values: &[f64], magnitude: f64) -> Option<(Vec<f64>, f64)> {
    let scale = binary_scale(magnitude);
    let normalized = values
        .iter()
        .map(|value| reversible_scaled(*value, scale))
        .collect::<Option<Vec<_>>>()?;
    Some((normalized, scale))
}

/// Return false instead of losing a small nonzero entry in a wide-scale row.
pub(super) fn normalize_rows_reversibly(values: &mut [f64], rows: usize, columns: usize) -> bool {
    for row in 0..rows {
        let values = &mut values[row * columns..(row + 1) * columns];
        let magnitude = values.iter().map(|value| value.abs()).fold(0.0, f64::max);
        if magnitude == 0.0 {
            continue;
        }
        let scale = binary_scale(magnitude);
        for value in values {
            let Some(normalized) = reversible_scaled(*value, scale) else {
                return false;
            };
            *value = normalized;
        }
    }
    true
}

/// Column scaling is a second cold-path rank recovery option when row scaling
/// cannot retain a small element. A failed attempt may partly modify the input.
pub(super) fn normalize_columns_reversibly(
    values: &mut [f64],
    rows: usize,
    columns: usize,
) -> bool {
    for column in 0..columns {
        let mut magnitude = 0.0_f64;
        for row in 0..rows {
            let value = values[row * columns + column];
            if !value.is_finite() {
                return false;
            }
            magnitude = magnitude.max(value.abs());
        }
        if magnitude == 0.0 {
            continue;
        }
        let scale = binary_scale(magnitude);
        for row in 0..rows {
            let value = &mut values[row * columns + column];
            let Some(normalized) = reversible_scaled(*value, scale) else {
                return false;
            };
            *value = normalized;
        }
    }
    true
}
