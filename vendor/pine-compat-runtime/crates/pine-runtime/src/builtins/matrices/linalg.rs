use crate::runtime::resource_limits::MatrixWorkBudget;

const EIGEN_TOLERANCE: f64 = 1e-12;
const EIGEN_VECTOR_TOLERANCE: f64 = 1e-8;
const PSEUDO_INVERSE_TOLERANCE: f64 = 1e-12;

#[path = "linalg_rank.rs"]
mod rank;
#[path = "linalg_scale.rs"]
mod scale;
pub(super) use rank::rank;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EigenFailure {
    Numerical,
    Complex,
}

pub(super) fn eigenvalues_with_work(
    values: &[f64],
    size: usize,
    work: &mut MatrixWorkBudget,
) -> Option<Vec<f64>> {
    if !work.spend((size as u64).saturating_pow(2)) {
        return None;
    }
    if size > 1
        && let Some((normalized, scale)) = scale::extreme_eigen_normalization(values)
    {
        if let Some(result) = exact_triangular_eigenvalues(values, size) {
            return Some(result);
        }
        return recovered_eigenvalues(
            &normalized,
            size,
            scale,
            is_exactly_symmetric(values, size),
            work,
        );
    }
    let result = unscaled_eigenvalues(values, size, None, work);
    if !work.spend(0) {
        return None;
    }
    if size <= 1
        || result
            .as_ref()
            .is_ok_and(|values| values.iter().all(|value| value.is_finite()))
    {
        return result.ok();
    }
    if matches!(result, Err(EigenFailure::Complex)) {
        return None;
    }
    if let Some(result) = exact_triangular_eigenvalues(values, size) {
        return Some(result);
    }
    let (normalized, scale) = scale::reversible_eigen_normalization(values)?;
    recovered_eigenvalues(
        &normalized,
        size,
        scale,
        is_exactly_symmetric(values, size),
        work,
    )
}

fn recovered_eigenvalues(
    values: &[f64],
    size: usize,
    scale: f64,
    symmetric: bool,
    work: &mut MatrixWorkBudget,
) -> Option<Vec<f64>> {
    let mut result = unscaled_eigenvalues(values, size, Some(symmetric), work).ok()?;
    if !result.iter().all(|value| value.is_finite()) {
        return None;
    }
    for value in &mut result {
        *value *= scale;
    }
    Some(result)
}

fn unscaled_eigenvalues(
    values: &[f64],
    size: usize,
    symmetric: Option<bool>,
    work: &mut MatrixWorkBudget,
) -> Result<Vec<f64>, EigenFailure> {
    if size == 0 {
        return Ok(Vec::new());
    }
    if size == 1 {
        return Ok(vec![values[0]]);
    }
    if size == 2 && has_equal_diagonal_complex_roots(values) {
        return Err(EigenFailure::Complex);
    }
    if symmetric.unwrap_or_else(|| is_symmetric(values, size)) {
        return Ok(symmetric_eigenvalues(values, size, work));
    }
    if size == 2 {
        return two_by_two_eigenvalues(values, symmetric.is_some());
    }
    qr_eigenvalues_with_work(values, size, symmetric.is_some(), work)
}

pub(super) fn eigenvectors_with_work(
    values: &[f64],
    size: usize,
    work: &mut MatrixWorkBudget,
) -> Option<Vec<f64>> {
    if !work.spend((size as u64).saturating_pow(2)) {
        return None;
    }
    if size > 1
        && let Some((normalized, _)) = scale::extreme_eigen_normalization(values)
    {
        return recovered_eigenvectors(&normalized, size, is_exactly_symmetric(values, size), work);
    }
    let result = unscaled_eigenvectors(values, size, None, work);
    if !work.spend(0) {
        return None;
    }
    if size <= 1
        || result
            .as_ref()
            .is_ok_and(|values| values.iter().all(|value| value.is_finite()))
    {
        return result.ok();
    }
    if matches!(result, Err(EigenFailure::Complex)) {
        return None;
    }
    let (normalized, _) = scale::reversible_eigen_normalization(values)?;
    recovered_eigenvectors(&normalized, size, is_exactly_symmetric(values, size), work)
}

fn recovered_eigenvectors(
    values: &[f64],
    size: usize,
    symmetric: bool,
    work: &mut MatrixWorkBudget,
) -> Option<Vec<f64>> {
    unscaled_eigenvectors(values, size, Some(symmetric), work)
        .ok()
        .filter(|vectors| vectors.iter().all(|value| value.is_finite()))
}

fn unscaled_eigenvectors(
    values: &[f64],
    size: usize,
    symmetric: Option<bool>,
    work: &mut MatrixWorkBudget,
) -> Result<Vec<f64>, EigenFailure> {
    if size == 0 {
        return Ok(Vec::new());
    }
    if size == 1 {
        return Ok(vec![1.0]);
    }
    if size == 2 && has_equal_diagonal_complex_roots(values) {
        return Err(EigenFailure::Complex);
    }
    if symmetric.unwrap_or_else(|| is_symmetric(values, size)) {
        let (_, vectors) = jacobi_eigen_decomposition(values.to_vec(), size, work);
        if !work.spend(0) {
            return Err(EigenFailure::Numerical);
        }
        return Ok(normalize_vector_columns(vectors, size));
    }

    let eigenvalues = unscaled_eigenvalues(values, size, symmetric, work)?;
    let mut result = vec![0.0; size * size];
    let mut previous = Vec::new();
    for (column, eigenvalue) in eigenvalues.iter().copied().enumerate() {
        if !work.spend((size as u64).saturating_pow(3).saturating_mul(3)) {
            return Err(EigenFailure::Numerical);
        }
        let matching_previous = previous
            .iter()
            .filter_map(|(value, vector): &(f64, Vec<f64>)| {
                if (value - eigenvalue).abs() <= EIGEN_VECTOR_TOLERANCE {
                    Some(vector.as_slice())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let vector = eigenvector_for(values, size, eigenvalue, &matching_previous)
            .ok_or(EigenFailure::Numerical)?;
        for row in 0..size {
            result[row * size + column] = vector[row];
        }
        previous.push((eigenvalue, vector));
    }
    Ok(result)
}

pub(super) fn pseudo_inverse_with_work(
    values: &[f64],
    rows: usize,
    columns: usize,
    work: &mut MatrixWorkBudget,
) -> Vec<f64> {
    if !work.spend((rows as u64).saturating_mul(columns as u64)) {
        return Vec::new();
    }
    if rows == 0 || columns == 0 {
        return Vec::new();
    }

    // Forming the Gram matrix squares the input scale. Normalize first so
    // ordinary small values retain their rank and large values do not overflow.
    // The caller supplies finite numbers; zero matrices have a zero inverse.
    let scale = values.iter().map(|value| value.abs()).fold(0.0, f64::max);
    if scale == 0.0 {
        return vec![0.0; rows * columns];
    }
    // Power-of-two scaling avoids an extra rounding step for ordinary inputs.
    // Construct the highest represented power directly, including subnormals
    // whose biased exponent is zero and whose scale would underflow via powi.
    let scale = scale::binary_scale(scale);
    let normalized: Vec<_> = values.iter().map(|value| value / scale).collect();
    let mut result = normalized_pseudo_inverse(&normalized, rows, columns, work);
    for value in &mut result {
        *value /= scale;
    }
    result
}

fn normalized_pseudo_inverse(
    values: &[f64],
    rows: usize,
    columns: usize,
    work: &mut MatrixWorkBudget,
) -> Vec<f64> {
    if !work.spend(
        (rows as u64)
            .saturating_mul(columns as u64)
            .saturating_mul(rows.min(columns) as u64),
    ) {
        return Vec::new();
    }
    if columns <= rows {
        let gram = right_gram(values, rows, columns);
        let (eigenvalues, eigenvectors) = jacobi_eigen_decomposition(gram, columns, work);
        if !work.spend(
            (rows as u64)
                .saturating_mul(columns as u64)
                .saturating_mul(columns as u64)
                .saturating_mul(2),
        ) {
            return Vec::new();
        }
        let cutoff = eigen_cutoff(&eigenvalues);
        let mut result = vec![0.0; columns * rows];
        let mut projected_rows = vec![0.0; rows];

        for eigen_index in 0..columns {
            let lambda = eigenvalues[eigen_index];
            if lambda <= cutoff {
                continue;
            }
            for row in 0..rows {
                let mut total = 0.0;
                for column in 0..columns {
                    total += values[row * columns + column]
                        * eigenvectors[column * columns + eigen_index];
                }
                projected_rows[row] = total;
            }
            for column in 0..columns {
                let scaled_vector = eigenvectors[column * columns + eigen_index] / lambda;
                for row in 0..rows {
                    result[column * rows + row] += scaled_vector * projected_rows[row];
                }
            }
        }
        result
    } else {
        let gram = left_gram(values, rows, columns);
        let (eigenvalues, eigenvectors) = jacobi_eigen_decomposition(gram, rows, work);
        if !work.spend(
            (rows as u64)
                .saturating_mul(columns as u64)
                .saturating_mul(rows as u64)
                .saturating_mul(2),
        ) {
            return Vec::new();
        }
        let cutoff = eigen_cutoff(&eigenvalues);
        let mut result = vec![0.0; columns * rows];
        let mut projected_columns = vec![0.0; columns];

        for eigen_index in 0..rows {
            let lambda = eigenvalues[eigen_index];
            if lambda <= cutoff {
                continue;
            }
            for column in 0..columns {
                let mut total = 0.0;
                for row in 0..rows {
                    total +=
                        values[row * columns + column] * eigenvectors[row * rows + eigen_index];
                }
                projected_columns[column] = total;
            }
            for column in 0..columns {
                let scaled_projection = projected_columns[column] / lambda;
                for row in 0..rows {
                    result[column * rows + row] +=
                        scaled_projection * eigenvectors[row * rows + eigen_index];
                }
            }
        }
        result
    }
}

fn right_gram(values: &[f64], rows: usize, columns: usize) -> Vec<f64> {
    let mut gram = vec![0.0; columns * columns];
    for left in 0..columns {
        for right in left..columns {
            let mut total = 0.0;
            for row in 0..rows {
                total += values[row * columns + left] * values[row * columns + right];
            }
            gram[left * columns + right] = total;
            gram[right * columns + left] = total;
        }
    }
    gram
}

fn left_gram(values: &[f64], rows: usize, columns: usize) -> Vec<f64> {
    let mut gram = vec![0.0; rows * rows];
    for upper in 0..rows {
        for lower in upper..rows {
            let mut total = 0.0;
            for column in 0..columns {
                total += values[upper * columns + column] * values[lower * columns + column];
            }
            gram[upper * rows + lower] = total;
            gram[lower * rows + upper] = total;
        }
    }
    gram
}

fn eigen_cutoff(eigenvalues: &[f64]) -> f64 {
    let max = eigenvalues.iter().copied().fold(0.0, f64::max);
    max * PSEUDO_INVERSE_TOLERANCE
}

#[cfg(test)]
#[path = "linalg_pinv_tests.rs"]
mod pinv_tests;

#[cfg(test)]
#[path = "linalg_qr_tests.rs"]
mod qr_tests;

#[cfg(test)]
#[path = "linalg_scale_tests.rs"]
mod scale_tests;

fn is_symmetric(values: &[f64], size: usize) -> bool {
    for row in 0..size {
        for column in (row + 1)..size {
            if (values[row * size + column] - values[column * size + row]).abs() > EIGEN_TOLERANCE {
                return false;
            }
        }
    }
    true
}

fn is_exactly_symmetric(values: &[f64], size: usize) -> bool {
    for row in 0..size {
        for column in (row + 1)..size {
            if values[row * size + column] != values[column * size + row] {
                return false;
            }
        }
    }
    true
}

/// Use the triangular characteristic polynomial only on a recovery path. This
/// also retains exact diagonal values when no single scaling preserves all
/// entries, and avoids QR splitting a repeated eigenvalue of a Jordan block.
fn exact_triangular_eigenvalues(values: &[f64], size: usize) -> Option<Vec<f64>> {
    let mut upper = true;
    let mut lower = true;
    for row in 0..size {
        for column in (row + 1)..size {
            upper &= values[column * size + row] == 0.0;
            lower &= values[row * size + column] == 0.0;
        }
    }
    if !upper && !lower {
        return None;
    }
    let mut result: Vec<_> = (0..size)
        .map(|index| values[index * size + index])
        .collect();
    // The nonsymmetric 2x2 formula returns the larger root first. Symmetric
    // diagonal matrices and larger QR matrices keep their diagonal order.
    if size == 2 && !is_exactly_symmetric(values, size) && result[0] < result[1] {
        result.swap(0, 1);
    }
    Some(result)
}

fn symmetric_eigenvalues(values: &[f64], size: usize, work: &mut MatrixWorkBudget) -> Vec<f64> {
    let (eigenvalues, _) = jacobi_eigen_decomposition(values.to_vec(), size, work);
    eigenvalues
}

/// For equal real diagonal entries, the discriminant is exactly 4*b*c.
/// Compare signs directly so cancellation or underflow cannot invent real
/// roots. Run this before approximate symmetry can hide tiny off-diagonals.
fn has_equal_diagonal_complex_roots(values: &[f64]) -> bool {
    values[0] == values[3]
        && values[1] != 0.0
        && values[2] != 0.0
        && values[1].is_sign_negative() != values[2].is_sign_negative()
        && values.iter().all(|value| value.is_finite())
}

fn two_by_two_eigenvalues(values: &[f64], recovery: bool) -> Result<Vec<f64>, EigenFailure> {
    if recovery {
        return recovered_two_by_two_eigenvalues(values);
    }
    if has_equal_diagonal_complex_roots(values) {
        return Err(EigenFailure::Complex);
    }
    let trace = values[0] + values[3];
    let determinant = values[0] * values[3] - values[1] * values[2];
    let discriminant = trace * trace - 4.0 * determinant;
    if !trace.is_finite() || !determinant.is_finite() || !discriminant.is_finite() {
        return Err(EigenFailure::Numerical);
    }
    if discriminant < -EIGEN_TOLERANCE {
        return Err(EigenFailure::Complex);
    }
    let root = discriminant.max(0.0).sqrt();
    Ok(vec![(trace + root) / 2.0, (trace - root) / 2.0])
}

fn recovered_two_by_two_eigenvalues(values: &[f64]) -> Result<Vec<f64>, EigenFailure> {
    let trace = values[0] + values[3];
    let difference = values[0] - values[3];
    let product = values[1] * values[2];
    // Equal diagonal entries with oppositely signed nonzero off-diagonals have
    // complex roots even if their product underflows after normalization.
    if has_equal_diagonal_complex_roots(values) {
        return Err(EigenFailure::Complex);
    }
    let discriminant = difference * difference + 4.0 * product;
    if !trace.is_finite() || !discriminant.is_finite() {
        return Err(EigenFailure::Numerical);
    }
    if discriminant < 0.0 {
        return Err(EigenFailure::Complex);
    }
    let root = discriminant.sqrt();
    Ok(vec![(trace + root) / 2.0, (trace - root) / 2.0])
}

fn qr_eigenvalues_with_work(
    values: &[f64],
    size: usize,
    recovery: bool,
    work: &mut MatrixWorkBudget,
) -> Result<Vec<f64>, EigenFailure> {
    let mut matrix = values.to_vec();
    for _ in 0..(size * size * 128).max(1) {
        if !work.spend((size as u64).saturating_pow(3).saturating_mul(4)) {
            return Err(EigenFailure::Numerical);
        }
        let (q, r) = qr_decompose_with_work(&matrix, size, work).ok_or(EigenFailure::Numerical)?;
        let next = multiply_square(&r, &q, size);
        // Keep the original convergence check first. A finite bit-identical
        // state will repeat every remaining deterministic QR step; it still
        // goes through the original real/complex block classification below.
        if lower_off_diagonal_norm(&next, size) <= EIGEN_TOLERANCE {
            matrix = next;
            break;
        }
        let fixed_point = finite_matrix_bits_unchanged(&matrix, &next);
        matrix = next;
        if fixed_point {
            break;
        }
    }

    let mut result = Vec::with_capacity(size);
    let mut index = 0;
    while index < size {
        if index + 1 < size && matrix[(index + 1) * size + index].abs() > 1e-8 {
            let block = [
                matrix[index * size + index],
                matrix[index * size + index + 1],
                matrix[(index + 1) * size + index],
                matrix[(index + 1) * size + index + 1],
            ];
            result.extend(two_by_two_eigenvalues(&block, recovery)?);
            index += 2;
        } else {
            result.push(matrix[index * size + index]);
            index += 1;
        }
    }
    Ok(result)
}

fn finite_matrix_bits_unchanged(previous: &[f64], next: &[f64]) -> bool {
    previous.len() == next.len()
        && previous.iter().zip(next).all(|(previous, next)| {
            previous.is_finite() && next.is_finite() && previous.to_bits() == next.to_bits()
        })
}

fn eigenvector_for(
    values: &[f64],
    size: usize,
    eigenvalue: f64,
    previous: &[&[f64]],
) -> Option<Vec<f64>> {
    let mut matrix = values.to_vec();
    for index in 0..size {
        matrix[index * size + index] -= eigenvalue;
    }
    let pivot_columns = rref(&mut matrix, size);

    for free_column in 0..size {
        if pivot_columns.contains(&free_column) {
            continue;
        }
        let mut vector = vec![0.0; size];
        vector[free_column] = 1.0;
        for (row, pivot_column) in pivot_columns.iter().copied().enumerate().rev() {
            let mut value = 0.0;
            for column in (pivot_column + 1)..size {
                value -= matrix[row * size + column] * vector[column];
            }
            vector[pivot_column] = value;
        }
        normalize_vector(&mut vector)?;
        if previous
            .iter()
            .any(|candidate| vectors_are_collinear(candidate, &vector))
        {
            continue;
        }
        if eigen_residual_norm(values, size, eigenvalue, &vector) <= EIGEN_VECTOR_TOLERANCE {
            return Some(vector);
        }
    }
    None
}

fn rref(matrix: &mut [f64], size: usize) -> Vec<usize> {
    let mut pivot_columns = Vec::new();
    let mut row = 0;
    for column in 0..size {
        let mut pivot_row = row;
        let mut pivot_abs = matrix[pivot_row * size + column].abs();
        for candidate in (row + 1)..size {
            let candidate_abs = matrix[candidate * size + column].abs();
            if candidate_abs > pivot_abs {
                pivot_abs = candidate_abs;
                pivot_row = candidate;
            }
        }
        if pivot_abs <= EIGEN_VECTOR_TOLERANCE {
            continue;
        }
        if pivot_row != row {
            for swap_column in 0..size {
                matrix.swap(row * size + swap_column, pivot_row * size + swap_column);
            }
        }
        let pivot_value = matrix[row * size + column];
        for normalize_column in column..size {
            matrix[row * size + normalize_column] /= pivot_value;
        }
        for eliminate_row in 0..size {
            if eliminate_row == row {
                continue;
            }
            let factor = matrix[eliminate_row * size + column];
            if factor.abs() <= EIGEN_VECTOR_TOLERANCE {
                continue;
            }
            matrix[eliminate_row * size + column] = 0.0;
            for eliminate_column in (column + 1)..size {
                matrix[eliminate_row * size + eliminate_column] -=
                    factor * matrix[row * size + eliminate_column];
            }
        }
        pivot_columns.push(column);
        row += 1;
        if row == size {
            break;
        }
    }
    pivot_columns
}

fn eigen_residual_norm(values: &[f64], size: usize, eigenvalue: f64, vector: &[f64]) -> f64 {
    let mut total = 0.0;
    for row in 0..size {
        let mut projected = 0.0;
        for column in 0..size {
            projected += values[row * size + column] * vector[column];
        }
        let residual = projected - eigenvalue * vector[row];
        total += residual * residual;
    }
    total.sqrt()
}

fn normalize_vector_columns(mut vectors: Vec<f64>, size: usize) -> Vec<f64> {
    let mut vector = vec![0.0; size];
    for column in 0..size {
        for row in 0..size {
            vector[row] = vectors[row * size + column];
        }
        if normalize_vector(&mut vector).is_some() {
            for row in 0..size {
                vectors[row * size + column] = vector[row];
            }
        }
    }
    vectors
}

fn normalize_vector(vector: &mut [f64]) -> Option<()> {
    let norm = vector_norm(vector);
    if !norm.is_finite() || norm <= EIGEN_VECTOR_TOLERANCE {
        return None;
    }
    for value in vector.iter_mut() {
        *value /= norm;
        if value.abs() <= EIGEN_VECTOR_TOLERANCE {
            *value = 0.0;
        }
    }
    if vector
        .iter()
        .copied()
        .max_by(|left, right| left.abs().partial_cmp(&right.abs()).unwrap())
        .is_some_and(|anchor| anchor < 0.0)
    {
        for value in vector.iter_mut() {
            *value = -*value;
        }
    }
    Some(())
}

fn vectors_are_collinear(left: &[f64], right: &[f64]) -> bool {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum::<f64>()
        .abs()
        >= 1.0 - EIGEN_VECTOR_TOLERANCE
}

fn qr_decompose_with_work(
    values: &[f64],
    size: usize,
    work: &mut MatrixWorkBudget,
) -> Option<(Vec<f64>, Vec<f64>)> {
    let mut q = vec![0.0; size * size];
    let mut r = vec![0.0; size * size];

    for column in 0..size {
        let mut vector = (0..size)
            .map(|row| values[row * size + column])
            .collect::<Vec<_>>();

        for previous in 0..column {
            let projection = dot_column(&q, size, previous, &vector);
            r[previous * size + column] = projection;
            for row in 0..size {
                vector[row] -= projection * q[row * size + previous];
            }
        }

        let mut norm = vector_norm(&vector);
        if !norm.is_finite() {
            return None;
        }
        let mut diagonal = norm;
        if norm <= EIGEN_TOLERANCE {
            // Completing Q does not create a component in the original column.
            // Its numerically dependent residual is discarded at the existing
            // tolerance; only the fallback basis vector needs normalization.
            diagonal = 0.0;
            vector = orthogonal_fallback(&q, size, column, work)?;
            norm = vector_norm(&vector);
        }
        if !norm.is_finite() || norm <= EIGEN_TOLERANCE {
            return None;
        }

        r[column * size + column] = diagonal;
        for row in 0..size {
            q[row * size + column] = vector[row] / norm;
        }
    }

    Some((q, r))
}

fn orthogonal_fallback(
    q: &[f64],
    size: usize,
    column: usize,
    work: &mut MatrixWorkBudget,
) -> Option<Vec<f64>> {
    for candidate in 0..size {
        // The ordinary QR phase is cubic. Completing dependent columns can
        // scan every basis candidate for every column, making this path
        // quartic; charge each candidate before its projection and norm loops.
        let candidate_work =
            (size as u64).saturating_mul((column as u64).saturating_mul(2).saturating_add(1));
        if !work.spend(candidate_work) {
            return None;
        }
        let mut vector = vec![0.0; size];
        vector[candidate] = 1.0;
        for previous in 0..column {
            let projection = dot_column(q, size, previous, &vector);
            for row in 0..size {
                vector[row] -= projection * q[row * size + previous];
            }
        }
        if vector_norm(&vector) > EIGEN_TOLERANCE {
            return Some(vector);
        }
    }
    None
}

fn dot_column(matrix: &[f64], size: usize, column: usize, vector: &[f64]) -> f64 {
    (0..size)
        .map(|row| matrix[row * size + column] * vector[row])
        .sum()
}

fn vector_norm(vector: &[f64]) -> f64 {
    vector.iter().map(|value| value * value).sum::<f64>().sqrt()
}

fn multiply_square(left: &[f64], right: &[f64], size: usize) -> Vec<f64> {
    let mut result = vec![0.0; size * size];
    for row in 0..size {
        for column in 0..size {
            let mut total = 0.0;
            for inner in 0..size {
                total += left[row * size + inner] * right[inner * size + column];
            }
            result[row * size + column] = total;
        }
    }
    result
}

fn lower_off_diagonal_norm(values: &[f64], size: usize) -> f64 {
    let mut total = 0.0;
    for row in 1..size {
        for column in 0..row {
            total += values[row * size + column].abs();
        }
    }
    total
}

fn jacobi_eigen_decomposition(
    mut matrix: Vec<f64>,
    size: usize,
    work: &mut MatrixWorkBudget,
) -> (Vec<f64>, Vec<f64>) {
    let mut eigenvectors = vec![0.0; size * size];
    for index in 0..size {
        eigenvectors[index * size + index] = 1.0;
    }

    for _ in 0..(size * size * 16).max(1) {
        if !work.spend(
            (size as u64)
                .saturating_pow(2)
                .saturating_add((size as u64).saturating_mul(4)),
        ) {
            break;
        }
        let Some((pivot, partner, off_diagonal)) = largest_off_diagonal(&matrix, size) else {
            break;
        };
        let scale = matrix[pivot * size + pivot]
            .abs()
            .max(matrix[partner * size + partner].abs())
            .max(1.0);
        if off_diagonal <= EIGEN_TOLERANCE * scale {
            break;
        }

        rotate(&mut matrix, &mut eigenvectors, size, pivot, partner);
    }

    let eigenvalues = (0..size)
        .map(|index| matrix[index * size + index])
        .collect();
    (eigenvalues, eigenvectors)
}

fn largest_off_diagonal(matrix: &[f64], size: usize) -> Option<(usize, usize, f64)> {
    let mut best = None;
    let mut best_abs = 0.0;
    for row in 0..size {
        for column in (row + 1)..size {
            let value_abs = matrix[row * size + column].abs();
            if value_abs > best_abs {
                best_abs = value_abs;
                best = Some((row, column, value_abs));
            }
        }
    }
    best
}

fn rotate(matrix: &mut [f64], eigenvectors: &mut [f64], size: usize, pivot: usize, partner: usize) {
    let pivot_value = matrix[pivot * size + pivot];
    let partner_value = matrix[partner * size + partner];
    let off_diagonal = matrix[pivot * size + partner];
    if off_diagonal == 0.0 {
        return;
    }

    let tau = (partner_value - pivot_value) / (2.0 * off_diagonal);
    let tangent = tau.signum() / (tau.abs() + (1.0 + tau * tau).sqrt());
    let cosine = 1.0 / (1.0 + tangent * tangent).sqrt();
    let sine = tangent * cosine;

    for index in 0..size {
        if index == pivot || index == partner {
            continue;
        }
        let pivot_cell = matrix[index * size + pivot];
        let partner_cell = matrix[index * size + partner];
        let rotated_pivot = cosine * pivot_cell - sine * partner_cell;
        let rotated_partner = sine * pivot_cell + cosine * partner_cell;
        matrix[index * size + pivot] = rotated_pivot;
        matrix[pivot * size + index] = rotated_pivot;
        matrix[index * size + partner] = rotated_partner;
        matrix[partner * size + index] = rotated_partner;
    }

    matrix[pivot * size + pivot] = cosine * cosine * pivot_value
        - 2.0 * sine * cosine * off_diagonal
        + sine * sine * partner_value;
    matrix[partner * size + partner] = sine * sine * pivot_value
        + 2.0 * sine * cosine * off_diagonal
        + cosine * cosine * partner_value;
    matrix[pivot * size + partner] = 0.0;
    matrix[partner * size + pivot] = 0.0;

    for row in 0..size {
        let pivot_vector = eigenvectors[row * size + pivot];
        let partner_vector = eigenvectors[row * size + partner];
        eigenvectors[row * size + pivot] = cosine * pivot_vector - sine * partner_vector;
        eigenvectors[row * size + partner] = sine * pivot_vector + cosine * partner_vector;
    }
}

#[cfg(test)]
pub(super) fn eigenvalues(values: &[f64], size: usize) -> Option<Vec<f64>> {
    eigenvalues_with_work(values, size, &mut MatrixWorkBudget::new(None))
}
#[cfg(test)]
pub(super) fn eigenvectors(values: &[f64], size: usize) -> Option<Vec<f64>> {
    eigenvectors_with_work(values, size, &mut MatrixWorkBudget::new(None))
}
#[cfg(test)]
pub(super) fn pseudo_inverse(values: &[f64], rows: usize, columns: usize) -> Vec<f64> {
    pseudo_inverse_with_work(values, rows, columns, &mut MatrixWorkBudget::new(None))
}

#[cfg(test)]
fn qr_eigenvalues(values: &[f64], size: usize, recovery: bool) -> Result<Vec<f64>, EigenFailure> {
    qr_eigenvalues_with_work(values, size, recovery, &mut MatrixWorkBudget::new(None))
}

#[cfg(test)]
fn qr_decompose(values: &[f64], size: usize) -> Option<(Vec<f64>, Vec<f64>)> {
    qr_decompose_with_work(values, size, &mut MatrixWorkBudget::new(None))
}
