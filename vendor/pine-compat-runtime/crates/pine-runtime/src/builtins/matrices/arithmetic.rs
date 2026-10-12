use pine_ir::HirCallArg;

use super::*;

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_matrix_kron(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let left = self.eval_expr(&args[0].value)?;
        let right = self.eval_expr(&args[1].value)?;
        let (PineValue::Matrix(left_id), PineValue::Matrix(right_id)) = (left, right) else {
            return Ok(PineValue::Na);
        };
        self.matrix_kron(left_id, right_id)
    }

    pub(crate) fn eval_matrix_mult(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let left = self.eval_expr(&args[0].value)?;
        let right = self.eval_expr(&args[1].value)?;
        match (left, right) {
            (PineValue::Matrix(left_id), PineValue::Matrix(right_id)) => {
                self.matrix_mult(left_id, right_id)
            }
            (PineValue::Matrix(left_id), PineValue::Array(right_id)) => {
                self.matrix_mult_array(left_id, right_id)
            }
            (PineValue::Array(left_id), PineValue::Matrix(right_id)) => {
                self.array_mult_matrix(left_id, right_id)
            }
            (PineValue::Array(left_id), PineValue::Array(right_id)) => {
                self.array_mult_array(left_id, right_id)
            }
            (PineValue::Matrix(left_id), scalar) => self.matrix_mult_scalar(left_id, scalar),
            (scalar, PineValue::Matrix(right_id)) => self.matrix_mult_scalar(right_id, scalar),
            _ => Ok(PineValue::Na),
        }
    }

    pub(crate) fn eval_matrix_diff(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let left = self.eval_expr(&args[0].value)?;
        let right = self.eval_expr(&args[1].value)?;
        match (left, right) {
            (PineValue::Matrix(left_id), PineValue::Matrix(right_id)) => {
                self.matrix_diff(left_id, right_id)
            }
            (PineValue::Matrix(left_id), scalar) => self.matrix_diff_scalar(left_id, scalar),
            (scalar, PineValue::Matrix(right_id)) => {
                self.matrix_map_scalar(right_id, scalar, |cell, scalar| scalar - cell)
            }
            _ => Ok(PineValue::Na),
        }
    }

    pub(crate) fn eval_matrix_pow(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let power = matrix_power_value(self.eval_expr(&args[1].value)?)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Na);
        };
        self.matrix_pow(id, power)
    }

    pub(crate) fn matrix_kron(
        &mut self,
        left_id: u32,
        right_id: u32,
    ) -> Result<PineValue, RuntimeError> {
        let (Some(left), Some(right)) = (
            self.matrix_store.get(&left_id),
            self.matrix_store.get(&right_id),
        ) else {
            return Ok(PineValue::Na);
        };
        let rows = left
            .rows
            .checked_mul(right.rows)
            .ok_or_else(|| RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            })?;
        let columns = left
            .columns
            .checked_mul(right.columns)
            .ok_or_else(|| RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            })?;
        let cells = rows.checked_mul(columns).ok_or_else(|| RuntimeError {
            message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
        })?;
        if cells > MAX_MATRIX_CELLS {
            return Err(RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            });
        }

        if !self.record_collection_allocation(cells) {
            return Err(self.resource_budget.collection_error());
        }
        let left = self.matrix_store.get(&left_id).expect("validated matrix");
        let right = self.matrix_store.get(&right_id).expect("validated matrix");
        let mut values = Vec::with_capacity(cells);
        for left_row in 0..left.rows {
            for right_row in 0..right.rows {
                for left_column in 0..left.columns {
                    let left_value = &left.values[left_row * left.columns + left_column];
                    for right_column in 0..right.columns {
                        let right_value = &right.values[right_row * right.columns + right_column];
                        values.push(match (left_value.as_f64(), right_value.as_f64()) {
                            (Some(left), Some(right)) if left.is_finite() && right.is_finite() => {
                                finite_float_or_na(left * right)
                            }
                            _ => PineValue::Na,
                        });
                    }
                }
            }
        }

        Ok(self.insert_matrix_payload(MatrixElementKind::Float, rows, columns, values.into()))
    }

    pub(crate) fn matrix_mult(
        &mut self,
        left_id: u32,
        right_id: u32,
    ) -> Result<PineValue, RuntimeError> {
        let (Some(left), Some(right)) = (
            self.matrix_store.get(&left_id),
            self.matrix_store.get(&right_id),
        ) else {
            return Ok(PineValue::Na);
        };
        let result = matrix_multiply_storage(
            left,
            right,
            &mut self.resource_budget,
            &mut self.collection_gc_allocated_bytes,
        )?;
        Ok(self.insert_matrix_payload(
            MatrixElementKind::Float,
            result.rows,
            result.columns,
            result.values,
        ))
    }

    pub(crate) fn matrix_mult_scalar(
        &mut self,
        id: u32,
        scalar: PineValue,
    ) -> Result<PineValue, RuntimeError> {
        self.matrix_map_scalar(id, scalar, |left, right| left * right)
    }

    pub(crate) fn matrix_mult_array(
        &mut self,
        left_id: u32,
        right_id: u32,
    ) -> Result<PineValue, RuntimeError> {
        let Some(left) = self.matrix_store.get(&left_id) else {
            return Ok(PineValue::Na);
        };
        let Some(right) = self.array_values(right_id)? else {
            return Ok(PineValue::Na);
        };
        if left.columns != right.len() {
            return Err(RuntimeError {
                message: "matrix multiplication requires matrix column count to match array size"
                    .to_owned(),
            });
        }

        let rows = left.rows;
        if !self.record_collection_allocation(rows) {
            return Err(self.resource_budget.collection_error());
        }
        let left = self.matrix_store.get(&left_id).expect("validated matrix");
        let right = self.array_values(right_id)?.expect("validated array");
        let mut values = Vec::with_capacity(rows);
        for row in 0..rows {
            let mut sum = 0.0;
            let mut valid = true;
            for (column, right_value) in right.iter().enumerate() {
                let left_value = &left.values[row * left.columns + column];
                let (Some(left_number), Some(right_number)) =
                    (left_value.as_f64(), right_value.as_f64())
                else {
                    valid = false;
                    break;
                };
                if !left_number.is_finite() || !right_number.is_finite() {
                    valid = false;
                    break;
                }
                sum += left_number * right_number;
            }
            values.push(if valid {
                finite_float_or_na(sum)
            } else {
                PineValue::Na
            });
        }

        Ok(self.insert_precharged_array_values(ArrayElementKind::Float, values))
    }

    pub(crate) fn array_mult_matrix(
        &mut self,
        left_id: u32,
        right_id: u32,
    ) -> Result<PineValue, RuntimeError> {
        let Some(left) = self.array_values(left_id)? else {
            return Ok(PineValue::Na);
        };
        let Some(right) = self.matrix_store.get(&right_id) else {
            return Ok(PineValue::Na);
        };
        if left.len() != right.rows {
            return Err(RuntimeError {
                message: "matrix multiplication requires array size to match matrix row count"
                    .to_owned(),
            });
        }

        let columns = right.columns;
        if !self.record_collection_allocation(columns) {
            return Err(self.resource_budget.collection_error());
        }
        let left = self.array_values(left_id)?.expect("validated array");
        let right = self.matrix_store.get(&right_id).expect("validated matrix");
        let mut values = Vec::with_capacity(columns);
        for column in 0..columns {
            let mut sum = 0.0;
            let mut valid = true;
            for (row, left_value) in left.iter().enumerate() {
                let right_value = &right.values[row * right.columns + column];
                let (Some(left_number), Some(right_number)) =
                    (left_value.as_f64(), right_value.as_f64())
                else {
                    valid = false;
                    break;
                };
                if !left_number.is_finite() || !right_number.is_finite() {
                    valid = false;
                    break;
                }
                sum += left_number * right_number;
            }
            values.push(if valid {
                finite_float_or_na(sum)
            } else {
                PineValue::Na
            });
        }

        Ok(self.insert_precharged_array_values(ArrayElementKind::Float, values))
    }

    pub(crate) fn array_mult_array(
        &mut self,
        left_id: u32,
        right_id: u32,
    ) -> Result<PineValue, RuntimeError> {
        let Some(left) = self.array_values(left_id)? else {
            return Ok(PineValue::Na);
        };
        let Some(right) = self.array_values(right_id)? else {
            return Ok(PineValue::Na);
        };
        if left.len() != right.len() {
            return Err(RuntimeError {
                message: "matrix multiplication requires left array size to match right array size"
                    .to_owned(),
            });
        }

        if !self.record_collection_allocation(1) {
            return Err(self.resource_budget.collection_error());
        }
        let left = self.array_values(left_id)?.expect("validated array");
        let right = self.array_values(right_id)?.expect("validated array");
        let mut sum = 0.0;
        let mut valid = true;
        for (left_value, right_value) in left.iter().zip(right.iter()) {
            let (Some(left_number), Some(right_number)) =
                (left_value.as_f64(), right_value.as_f64())
            else {
                valid = false;
                break;
            };
            if !left_number.is_finite() || !right_number.is_finite() {
                valid = false;
                break;
            }
            sum += left_number * right_number;
        }

        let value = if valid {
            finite_float_or_na(sum)
        } else {
            PineValue::Na
        };
        Ok(self.insert_precharged_array_values(ArrayElementKind::Float, vec![value]))
    }

    pub(crate) fn matrix_diff(
        &mut self,
        left_id: u32,
        right_id: u32,
    ) -> Result<PineValue, RuntimeError> {
        let (Some(left), Some(right)) = (
            self.matrix_store.get(&left_id),
            self.matrix_store.get(&right_id),
        ) else {
            return Ok(PineValue::Na);
        };
        if left.rows != right.rows || left.columns != right.columns {
            return Err(RuntimeError {
                message: "matrix difference requires matching row and column counts".to_owned(),
            });
        }

        let (rows, columns) = (left.rows, left.columns);
        if !self.record_collection_allocation(left.values.len()) {
            return Err(self.resource_budget.collection_error());
        }
        let left = self.matrix_store.get(&left_id).expect("validated matrix");
        let right = self.matrix_store.get(&right_id).expect("validated matrix");
        let values = left
            .values
            .iter()
            .zip(&right.values)
            .map(|(left_value, right_value)| {
                let (Some(left_number), Some(right_number)) =
                    (left_value.as_f64(), right_value.as_f64())
                else {
                    return PineValue::Na;
                };
                if left_number.is_finite() && right_number.is_finite() {
                    finite_float_or_na(left_number - right_number)
                } else {
                    PineValue::Na
                }
            })
            .collect::<Vec<_>>();

        Ok(self.insert_matrix_payload(MatrixElementKind::Float, rows, columns, values.into()))
    }

    pub(crate) fn matrix_diff_scalar(
        &mut self,
        id: u32,
        scalar: PineValue,
    ) -> Result<PineValue, RuntimeError> {
        self.matrix_map_scalar(id, scalar, |left, right| left - right)
    }

    fn matrix_map_scalar(
        &mut self,
        id: u32,
        scalar: PineValue,
        operation: impl Fn(f64, f64) -> f64,
    ) -> Result<PineValue, RuntimeError> {
        let Some(source) = self.matrix_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        let scalar = scalar.as_f64().filter(|value| value.is_finite());
        let (rows, columns) = (source.rows, source.columns);
        if !self.record_collection_allocation(source.values.len()) {
            return Err(self.resource_budget.collection_error());
        }
        let source = self.matrix_store.get(&id).expect("validated matrix");
        let values = source
            .values
            .iter()
            .map(|value| {
                let (Some(left), Some(right)) = (value.as_f64(), scalar) else {
                    return PineValue::Na;
                };
                if left.is_finite() {
                    finite_float_or_na(operation(left, right))
                } else {
                    PineValue::Na
                }
            })
            .collect::<Vec<_>>();

        Ok(self.insert_matrix_payload(MatrixElementKind::Float, rows, columns, values.into()))
    }

    pub(crate) fn matrix_pow(&mut self, id: u32, power: usize) -> Result<PineValue, RuntimeError> {
        let Some(source) = self.matrix_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        if source.rows != source.columns {
            return Err(RuntimeError {
                message: "matrix power requires a square matrix".to_owned(),
            });
        }
        let size = source.rows;
        if power == 0 {
            if !self.record_collection_allocation(size * size) {
                return Err(self.resource_budget.collection_error());
            }
            return Ok(self.insert_matrix_payload(
                MatrixElementKind::Float,
                size,
                size,
                identity_matrix_values(size).into(),
            ));
        }
        let cloned = collection_values_allocation_bytes(source.values.clone_allocation_values());
        let identity = if power > 1 {
            (size * size).saturating_mul(std::mem::size_of::<PineValue>())
        } else {
            0
        };
        if !self.record_collection_bytes(cloned.saturating_add(identity)) {
            return Err(self.resource_budget.collection_error());
        }
        let source = self
            .matrix_store
            .get(&id)
            .expect("validated matrix")
            .clone();
        if power == 1 {
            return Ok(self.insert_matrix_payload(
                MatrixElementKind::Float,
                source.rows,
                source.columns,
                source.values,
            ));
        }

        let mut result = MatrixStorage {
            kind: MatrixElementKind::Float,
            rows: source.rows,
            columns: source.columns,
            values: identity_matrix_values(source.rows).into(),
        };
        let mut base = source;
        let mut exponent = power;
        while exponent > 0 {
            if exponent % 2 == 1 {
                result = matrix_multiply_storage(
                    &result,
                    &base,
                    &mut self.resource_budget,
                    &mut self.collection_gc_allocated_bytes,
                )?;
            }
            exponent /= 2;
            if exponent > 0 {
                base = matrix_multiply_storage(
                    &base,
                    &base,
                    &mut self.resource_budget,
                    &mut self.collection_gc_allocated_bytes,
                )?;
            }
        }

        Ok(self.insert_matrix_payload(
            MatrixElementKind::Float,
            result.rows,
            result.columns,
            result.values,
        ))
    }
}

fn matrix_power_value(value: PineValue) -> Result<usize, RuntimeError> {
    let power = value.as_i64().ok_or_else(|| RuntimeError {
        message: "matrix power cannot be na".to_owned(),
    })?;
    if power < 0 {
        return Err(RuntimeError {
            message: "matrix power cannot be negative".to_owned(),
        });
    }
    Ok(power as usize)
}

fn identity_matrix_values(size: usize) -> Vec<PineValue> {
    let mut values = Vec::with_capacity(size * size);
    for row in 0..size {
        for column in 0..size {
            values.push(PineValue::Float(if row == column { 1.0 } else { 0.0 }));
        }
    }
    values
}

fn matrix_multiply_storage(
    left: &MatrixStorage,
    right: &MatrixStorage,
    budget: &mut crate::runtime::resource_limits::ResourceBudget,
    collection_gc_allocated_bytes: &mut usize,
) -> Result<MatrixStorage, RuntimeError> {
    if left.columns != right.rows {
        return Err(RuntimeError {
            message: "matrix multiplication requires left column count to match right row count"
                .to_owned(),
        });
    }
    let cells = left
        .rows
        .checked_mul(right.columns)
        .ok_or_else(|| RuntimeError {
            message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
        })?;
    if cells > MAX_MATRIX_CELLS {
        return Err(RuntimeError {
            message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
        });
    }

    if !budget.matrix.spend(
        (left.rows as u64)
            .saturating_mul(right.columns as u64)
            .saturating_mul(left.columns as u64),
    ) {
        return Err(budget.check().expect_err("matrix work was rejected"));
    }
    let bytes = cells.saturating_mul(std::mem::size_of::<PineValue>());
    if !budget.reserve_collection(bytes) {
        return Err(budget.collection_error());
    }
    *collection_gc_allocated_bytes = collection_gc_allocated_bytes.saturating_add(bytes);
    let mut values = Vec::with_capacity(cells);
    for row in 0..left.rows {
        for column in 0..right.columns {
            let mut total = 0.0;
            let mut has_na = false;
            for index in 0..left.columns {
                let left_value = &left.values[row * left.columns + index];
                let right_value = &right.values[index * right.columns + column];
                let (Some(left_number), Some(right_number)) =
                    (left_value.as_f64(), right_value.as_f64())
                else {
                    has_na = true;
                    break;
                };
                if !left_number.is_finite() || !right_number.is_finite() {
                    has_na = true;
                    break;
                }
                total += left_number * right_number;
            }
            values.push(if has_na {
                PineValue::Na
            } else {
                finite_float_or_na(total)
            });
        }
    }

    Ok(MatrixStorage {
        kind: MatrixElementKind::Float,
        rows: left.rows,
        columns: right.columns,
        values: values.into(),
    })
}
