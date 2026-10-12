//! Rank recovery after non-finite elimination, keeping the ordinary path intact.
use super::scale::{normalize_columns_reversibly, normalize_rows_reversibly};

pub(in super::super) fn rank(
    values: &mut [f64],
    rows: usize,
    columns: usize,
    original: impl FnOnce() -> Vec<f64>,
) -> Option<usize> {
    let legacy_rank = eliminate(values, rows, columns);
    if values.iter().all(|value| value.is_finite()) {
        return Some(legacy_rank);
    }
    // Read original entries only on failure; ordinary calls allocate no backup.
    let original = original();
    values.copy_from_slice(&original);
    if normalize_rows_reversibly(values, rows, columns) {
        let recovered_rank = eliminate(values, rows, columns);
        if values.iter().all(|value| value.is_finite()) {
            return Some(recovered_rank);
        }
    }
    // A wide-scale row may still have independently reversible columns.
    values.copy_from_slice(&original);
    if normalize_columns_reversibly(values, rows, columns) {
        let recovered_rank = eliminate(values, rows, columns);
        if values.iter().all(|value| value.is_finite()) {
            return Some(recovered_rank);
        }
    }
    // The contaminated elimination cannot establish a finite integer rank.
    None
}

fn eliminate(values: &mut [f64], rows: usize, columns: usize) -> usize {
    let mut rank = 0_usize;
    let mut pivot_row = 0_usize;
    for column in 0..columns {
        let mut best_row = pivot_row;
        let mut best_abs = 0.0;
        for row in pivot_row..rows {
            let candidate_abs = values[row * columns + column].abs();
            if candidate_abs > best_abs {
                best_abs = candidate_abs;
                best_row = row;
            }
        }
        if best_abs == 0.0 {
            continue;
        }

        if best_row != pivot_row {
            for swap_column in 0..columns {
                values.swap(
                    pivot_row * columns + swap_column,
                    best_row * columns + swap_column,
                );
            }
        }

        let pivot_value = values[pivot_row * columns + column];
        for row in (pivot_row + 1)..rows {
            let factor = values[row * columns + column] / pivot_value;
            values[row * columns + column] = 0.0;
            for elimination_column in (column + 1)..columns {
                values[row * columns + elimination_column] -=
                    factor * values[pivot_row * columns + elimination_column];
            }
        }

        rank += 1;
        pivot_row += 1;
        if pivot_row == rows {
            break;
        }
    }
    rank
}

#[cfg(test)]
#[path = "linalg_rank_tests.rs"]
mod tests;
