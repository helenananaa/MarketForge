#![allow(dead_code)]

use std::cmp::Ordering;

use pine_ir::HirCallArg;

use crate::runtime::array_values::ArrayValues;
use crate::runtime::collection_gc::{collection_values_allocation_bytes, value_allocation_bytes};
use crate::*;

mod arithmetic;
#[cfg(test)]
#[path = "matrices/budget_tests.rs"]
mod budget_tests;
#[cfg(test)]
mod checkpoint_tests;
mod linalg;
mod linear_algebra;
mod mutation;
mod user_types;

#[cfg(test)]
#[path = "matrix_stochastic_tests.rs"]
mod stochastic_tests;

const MAX_MATRIX_CELLS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatrixElementKind {
    Float,
    Int,
    Bool,
    String,
    Color,
    UserType(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MatrixStorage {
    pub(crate) kind: MatrixElementKind,
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    pub(crate) values: ArrayValues<PineValue>,
}

impl MatrixStorage {
    fn cloned_entry_allocation_bytes(&self, cloned_entry: bool) -> usize {
        if cloned_entry {
            collection_values_allocation_bytes(self.values.clone_allocation_values())
        } else {
            0
        }
    }

    fn swap_allocation_bytes(
        &self,
        pairs: impl Iterator<Item = (usize, usize)>,
        cloned_entry: bool,
    ) -> usize {
        let mut copied_pages = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for (left, right) in pairs {
            for index in [left, right] {
                let values = self.values.write_allocation_values(index, cloned_entry);
                if !values.is_empty() && copied_pages.insert(values.as_ptr()) {
                    bytes = bytes.saturating_add(collection_values_allocation_bytes(values));
                }
            }
        }
        bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MatrixStoreProfile {
    pub(crate) slots: usize,
    pub(crate) capacity: usize,
    pub(crate) cells: usize,
    pub(crate) cell_capacity: usize,
}

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_matrix_call(
        &mut self,
        callee: &str,
        args: &[HirCallArg],
    ) -> Option<Result<PineValue, RuntimeError>> {
        if !callee.starts_with("matrix.") {
            return None;
        }
        if let Some(name) = callee
            .strip_prefix("matrix.new<")
            .and_then(|name| name.strip_suffix('>'))
            && let Some(index) = self
                .program
                .user_types
                .iter()
                .position(|ty| ty.identity.type_name == name)
        {
            return Some(self.eval_udt_matrix_new(index, args));
        }

        Some(match callee {
            "matrix.new<float>" => self.eval_matrix_new_float(args),
            "matrix.new<int>" => self.eval_matrix_new_int(args),
            "matrix.new<bool>" => self.eval_matrix_new_bool(args),
            "matrix.new<string>" => self.eval_matrix_new_string(args),
            "matrix.new<color>" => self.eval_matrix_new_color(args),
            "matrix.get" => self.eval_matrix_get(args),
            "matrix.set" => self.eval_matrix_set(args),
            "matrix.fill" => self.eval_matrix_fill(args),
            "matrix.concat" => self.eval_matrix_concat(args),
            "matrix.copy" => self.eval_matrix_copy(args),
            "matrix.transpose" => self.eval_matrix_transpose(args),
            "matrix.reverse" => self.eval_matrix_reverse(args),
            "matrix.reshape" => self.eval_matrix_reshape(args),
            "matrix.kron" => self.eval_matrix_kron(args),
            "matrix.mult" => self.eval_matrix_mult(args),
            "matrix.diff" => self.eval_matrix_diff(args),
            "matrix.pow" => self.eval_matrix_pow(args),
            "matrix.add_row" => self.eval_matrix_add_row(args),
            "matrix.add_col" => self.eval_matrix_add_col(args),
            "matrix.remove_col" => self.eval_matrix_remove_col(args),
            "matrix.remove_row" => self.eval_matrix_remove_row(args),
            "matrix.swap_columns" => self.eval_matrix_swap_columns(args),
            "matrix.swap_rows" => self.eval_matrix_swap_rows(args),
            "matrix.sort" => self.eval_matrix_sort(args),
            "matrix.submatrix" => self.eval_matrix_submatrix(args),
            "matrix.rows" => self.eval_matrix_rows(args),
            "matrix.columns" => self.eval_matrix_columns(args),
            "matrix.elements_count" => self.eval_matrix_elements_count(args),
            "matrix.is_square" => self.eval_matrix_is_square(args),
            "matrix.is_binary" => self.eval_matrix_is_binary(args),
            "matrix.is_diagonal" => self.eval_matrix_is_diagonal(args),
            "matrix.is_antidiagonal" => self.eval_matrix_is_antidiagonal(args),
            "matrix.is_triangular" => self.eval_matrix_is_triangular(args),
            "matrix.is_identity" => self.eval_matrix_is_identity(args),
            "matrix.is_symmetric" => self.eval_matrix_is_symmetric(args),
            "matrix.is_antisymmetric" => self.eval_matrix_is_antisymmetric(args),
            "matrix.is_stochastic" => self.eval_matrix_is_stochastic(args),
            "matrix.is_zero" => self.eval_matrix_is_zero(args),
            "matrix.sum" => self.eval_matrix_sum(args),
            "matrix.avg" => self.eval_matrix_avg(args),
            "matrix.min" => self.eval_matrix_min(args),
            "matrix.max" => self.eval_matrix_max(args),
            "matrix.median" => self.eval_matrix_median(args),
            "matrix.mode" => self.eval_matrix_mode(args),
            "matrix.trace" => self.eval_matrix_trace(args),
            "matrix.det" => self.eval_matrix_det(args),
            "matrix.eigenvalues" => self.eval_matrix_eigenvalues(args),
            "matrix.eigenvectors" => self.eval_matrix_eigenvectors(args),
            "matrix.inv" => self.eval_matrix_inv(args),
            "matrix.pinv" => self.eval_matrix_pinv(args),
            "matrix.rank" => self.eval_matrix_rank(args),
            "matrix.row" => self.eval_matrix_row(args),
            "matrix.col" => self.eval_matrix_col(args),
            _ => return None,
        })
    }

    pub(crate) fn eval_matrix_new_float(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let rows = matrix_dimension_value("row", self.eval_expr(&args[0].value)?)?;
        let columns = matrix_dimension_value("column", self.eval_expr(&args[1].value)?)?;
        let initial_value =
            if let Some(initial_value) = crate::builtins::args::positional_arg(args, 2) {
                eval_matrix_float_value(self.eval_expr(&initial_value.value)?)
            } else {
                PineValue::Na
            };
        self.new_matrix(MatrixElementKind::Float, rows, columns, initial_value)
    }

    pub(crate) fn eval_matrix_new_int(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let rows = matrix_dimension_value("row", self.eval_expr(&args[0].value)?)?;
        let columns = matrix_dimension_value("column", self.eval_expr(&args[1].value)?)?;
        let initial_value =
            if let Some(initial_value) = crate::builtins::args::positional_arg(args, 2) {
                eval_matrix_int_value(self.eval_expr(&initial_value.value)?)
            } else {
                PineValue::Na
            };
        self.new_matrix(MatrixElementKind::Int, rows, columns, initial_value)
    }

    pub(crate) fn eval_matrix_new_bool(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let rows = matrix_dimension_value("row", self.eval_expr(&args[0].value)?)?;
        let columns = matrix_dimension_value("column", self.eval_expr(&args[1].value)?)?;
        let initial_value =
            if let Some(initial_value) = crate::builtins::args::positional_arg(args, 2) {
                eval_matrix_bool_value(self.eval_expr(&initial_value.value)?)
            } else {
                PineValue::Na
            };
        self.new_matrix(MatrixElementKind::Bool, rows, columns, initial_value)
    }

    pub(crate) fn eval_matrix_new_string(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let rows = matrix_dimension_value("row", self.eval_expr(&args[0].value)?)?;
        let columns = matrix_dimension_value("column", self.eval_expr(&args[1].value)?)?;
        let initial_value =
            if let Some(initial_value) = crate::builtins::args::positional_arg(args, 2) {
                eval_matrix_string_value(self.eval_expr(&initial_value.value)?)
            } else {
                PineValue::Na
            };
        self.new_matrix(MatrixElementKind::String, rows, columns, initial_value)
    }

    pub(crate) fn eval_matrix_new_color(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let rows = matrix_dimension_value("row", self.eval_expr(&args[0].value)?)?;
        let columns = matrix_dimension_value("column", self.eval_expr(&args[1].value)?)?;
        let initial_value =
            if let Some(initial_value) = crate::builtins::args::positional_arg(args, 2) {
                eval_matrix_color_value(self.eval_expr(&initial_value.value)?)
            } else {
                PineValue::Na
            };
        self.new_matrix(MatrixElementKind::Color, rows, columns, initial_value)
    }

    pub(crate) fn eval_matrix_get(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let row = self.eval_expr(&args[1].value)?;
        let column = self.eval_expr(&args[2].value)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Na);
        };
        let row = matrix_index_value("row", row)?;
        let column = matrix_index_value("column", column)?;
        Ok(self
            .matrix_get_cloned(id, row, column)?
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_set(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let row = self.eval_expr(&args[1].value)?;
        let column = self.eval_expr(&args[2].value)?;
        let PineValue::Matrix(id) = id else {
            let _ = self.eval_expr(&args[3].value)?;
            return Ok(PineValue::Void);
        };
        let row = matrix_index_value("row", row)?;
        let column = matrix_index_value("column", column)?;
        let value = self.eval_expr(&args[3].value)?;
        let Some(kind) = self.matrix_store.get(&id).map(|matrix| matrix.kind) else {
            return Ok(PineValue::Void);
        };
        let value = eval_matrix_value_for_kind(kind, value);
        self.matrix_set_value(id, row, column, value)?;
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_fill(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let value = self.eval_expr(&args[1].value)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        let Some(kind) = self.matrix_store.get(&id).map(|matrix| matrix.kind) else {
            return Ok(PineValue::Void);
        };
        let value = eval_matrix_value_for_kind(kind, value);
        self.matrix_fill_value(id, value);
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_copy(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.copy_matrix(id))
    }

    pub(crate) fn eval_matrix_transpose(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_transpose(id))
    }

    pub(crate) fn eval_matrix_reverse(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Void);
        };
        self.matrix_reverse(id);
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_rows(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_shape(id)
            .map(|(rows, _)| PineValue::Int(rows as i64))
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_columns(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_shape(id)
            .map(|(_, columns)| PineValue::Int(columns as i64))
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_elements_count(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_elements_count(id)
            .map(|count| PineValue::Int(count as i64))
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_square(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_square(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_zero(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_zero(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_binary(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_binary(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_diagonal(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_diagonal(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_antidiagonal(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_antidiagonal(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_triangular(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_triangular(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_identity(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_identity(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_symmetric(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_symmetric(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_antisymmetric(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_antisymmetric(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_is_stochastic(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_is_stochastic(id)
            .map(PineValue::Bool)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_sum(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_sum(id).unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_avg(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_avg(id).unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_min(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_min(id).unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_max(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_max(id).unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_median(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_median(id).unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_mode(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_mode(id).unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_trace(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::Matrix(id) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        Ok(self.matrix_trace(id).unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_row(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let row = self.eval_expr(&args[1].value)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Na);
        };
        let row = matrix_index_value("row", row)?;
        let Some(kind) = self.matrix_store.get(&id).map(|matrix| matrix.kind) else {
            return Ok(PineValue::Na);
        };
        let Some(values) = self.matrix_row_values(id, row)? else {
            return Ok(PineValue::Na);
        };
        self.new_matrix_result_array(kind, values)
    }

    pub(crate) fn eval_matrix_col(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let column = self.eval_expr(&args[1].value)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Na);
        };
        let column = matrix_index_value("column", column)?;
        let Some(kind) = self.matrix_store.get(&id).map(|matrix| matrix.kind) else {
            return Ok(PineValue::Na);
        };
        let Some(values) = self.matrix_col_values(id, column)? else {
            return Ok(PineValue::Na);
        };
        self.new_matrix_result_array(kind, values)
    }

    pub(crate) fn new_matrix(
        &mut self,
        kind: MatrixElementKind,
        rows: i64,
        columns: i64,
        initial_value: PineValue,
    ) -> Result<PineValue, RuntimeError> {
        let rows = matrix_dimension("row", rows)?;
        let columns = matrix_dimension("column", columns)?;
        let cells = rows.checked_mul(columns).ok_or_else(|| RuntimeError {
            message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
        })?;
        if cells > MAX_MATRIX_CELLS {
            return Err(RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            });
        }

        if !self.record_collection_repeated_value(&initial_value, cells) {
            return Err(self.resource_budget.collection_error());
        }
        Ok(self.insert_matrix_payload(kind, rows, columns, vec![initial_value; cells].into()))
    }

    fn insert_matrix_storage(
        &mut self,
        kind: MatrixElementKind,
        rows: usize,
        columns: usize,
        values: Vec<PineValue>,
    ) -> PineValue {
        if !self.record_collection_values(&values) {
            return PineValue::Na;
        }
        self.insert_matrix_payload(kind, rows, columns, values.into())
    }

    fn insert_matrix_payload(
        &mut self,
        kind: MatrixElementKind,
        rows: usize,
        columns: usize,
        values: ArrayValues<PineValue>,
    ) -> PineValue {
        let id = self.next_matrix_id;
        self.next_matrix_id += 1;
        self.matrix_store.insert(
            id,
            MatrixStorage {
                kind,
                rows,
                columns,
                values,
            },
        );
        PineValue::Matrix(id)
    }

    pub(crate) fn matrix_shape(&self, id: u32) -> Option<(usize, usize)> {
        self.matrix_store
            .get(&id)
            .map(|matrix| (matrix.rows, matrix.columns))
    }

    pub(crate) fn matrix_elements_count(&self, id: u32) -> Option<usize> {
        self.matrix_store.get(&id).map(|matrix| matrix.values.len())
    }

    pub(crate) fn matrix_is_square(&self, id: u32) -> Option<bool> {
        self.matrix_store
            .get(&id)
            .map(|matrix| matrix.rows == matrix.columns)
    }

    pub(crate) fn matrix_is_zero(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            matrix
                .values
                .iter()
                .all(|value| matches!(value.as_f64(), Some(number) if number == 0.0))
        })
    }

    pub(crate) fn matrix_is_binary(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            matrix.values.iter().all(
                |value| matches!(value.as_f64(), Some(number) if number == 0.0 || number == 1.0),
            )
        })
    }

    pub(crate) fn matrix_is_diagonal(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            for row in 0..matrix.rows {
                for column in 0..matrix.columns {
                    if row == column {
                        continue;
                    }
                    let offset = row * matrix.columns + column;
                    if !matches!(matrix.values[offset].as_f64(), Some(number) if number == 0.0) {
                        return false;
                    }
                }
            }
            true
        })
    }

    pub(crate) fn matrix_is_antidiagonal(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            if matrix.rows != matrix.columns {
                return false;
            }
            for row in 0..matrix.rows {
                for column in 0..matrix.columns {
                    if row + column + 1 == matrix.columns {
                        continue;
                    }
                    let offset = row * matrix.columns + column;
                    if !matches!(matrix.values[offset].as_f64(), Some(number) if number == 0.0) {
                        return false;
                    }
                }
            }
            true
        })
    }

    pub(crate) fn matrix_is_triangular(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            if matrix.rows != matrix.columns {
                return false;
            }
            let mut above_is_zero = true;
            let mut below_is_zero = true;
            for row in 0..matrix.rows {
                for column in 0..matrix.columns {
                    if row == column {
                        continue;
                    }
                    let offset = row * matrix.columns + column;
                    let is_zero = matches!(
                        matrix.values[offset].as_f64(),
                        Some(number) if number == 0.0
                    );
                    if row < column {
                        above_is_zero &= is_zero;
                    } else {
                        below_is_zero &= is_zero;
                    }
                    if !above_is_zero && !below_is_zero {
                        return false;
                    }
                }
            }
            true
        })
    }

    pub(crate) fn matrix_is_identity(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            if matrix.rows != matrix.columns {
                return false;
            }
            for row in 0..matrix.rows {
                for column in 0..matrix.columns {
                    let offset = row * matrix.columns + column;
                    let expected = if row == column { 1.0 } else { 0.0 };
                    if !matches!(matrix.values[offset].as_f64(), Some(number) if number == expected)
                    {
                        return false;
                    }
                }
            }
            true
        })
    }

    pub(crate) fn matrix_is_symmetric(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            if matrix.rows != matrix.columns {
                return false;
            }
            for row in 0..matrix.rows {
                for column in 0..matrix.columns {
                    if matrix.values[row * matrix.columns + column]
                        .as_f64()
                        .is_none()
                    {
                        return false;
                    }
                    if row < column {
                        let Some(value) = matrix.values[row * matrix.columns + column].as_f64()
                        else {
                            return false;
                        };
                        let Some(mirror) = matrix.values[column * matrix.columns + row].as_f64()
                        else {
                            return false;
                        };
                        if value != mirror {
                            return false;
                        }
                    }
                }
            }
            true
        })
    }

    pub(crate) fn matrix_is_antisymmetric(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            if matrix.rows != matrix.columns {
                return false;
            }
            for row in 0..matrix.rows {
                for column in row..matrix.columns {
                    let Some(value) = matrix.values[row * matrix.columns + column].as_f64() else {
                        return false;
                    };
                    if row == column {
                        if value != 0.0 {
                            return false;
                        }
                        continue;
                    }
                    let Some(mirror) = matrix.values[column * matrix.columns + row].as_f64() else {
                        return false;
                    };
                    if value != -mirror {
                        return false;
                    }
                }
            }
            true
        })
    }

    pub(crate) fn matrix_is_stochastic(&self, id: u32) -> Option<bool> {
        self.matrix_store.get(&id).map(|matrix| {
            if matrix.values.is_empty() {
                return false;
            }
            // Match f64 Sum's seed and each row's original left-to-right order.
            // A failed row stops row sums, but every remaining cell is validated.
            let mut rows_sum_to_one = true;
            let mut row_sum = -0.0;
            let mut first_column_sum = -0.0;
            let mut column = 0;
            for value in matrix.values.slices().flatten() {
                let Some(number) = value.as_f64() else {
                    return false;
                };
                if !number.is_finite() || number < 0.0 {
                    return false;
                }
                if column == 0 {
                    first_column_sum += number;
                }
                if rows_sum_to_one {
                    row_sum += number;
                }
                column += 1;
                if column == matrix.columns {
                    if rows_sum_to_one {
                        rows_sum_to_one = row_sum == 1.0;
                        row_sum = -0.0;
                    }
                    column = 0;
                }
            }
            if rows_sum_to_one {
                return true;
            }
            if first_column_sum != 1.0 {
                return false;
            }
            if matrix.columns == 1 {
                return true;
            }
            if matrix.rows == 1 {
                return matrix
                    .values
                    .slices()
                    .flatten()
                    .skip(1)
                    .all(|value| value.as_f64().expect("validated numeric matrix cell") == 1.0);
            }

            // The first column is already known to sum to one. Accumulate the
            // remaining columns in row-major storage order, preserving each
            // column's original top-to-bottom addition order.
            let mut column_sums = vec![-0.0; matrix.columns - 1];
            let mut values = matrix.values.slices().flatten();
            for _ in 0..matrix.rows {
                let _ = values.next().expect("valid matrix shape");
                for sum in &mut column_sums {
                    *sum += values
                        .next()
                        .expect("valid matrix shape")
                        .as_f64()
                        .expect("validated numeric matrix cell");
                }
            }
            column_sums.into_iter().all(|sum| sum == 1.0)
        })
    }

    pub(crate) fn matrix_get_cloned(
        &self,
        id: u32,
        row: i64,
        column: i64,
    ) -> Result<Option<PineValue>, RuntimeError> {
        let Some((matrix, offset)) = self.matrix_cell_offset(id, row, column)? else {
            return Ok(None);
        };
        Ok(matrix.values.get(offset).cloned())
    }

    pub(crate) fn matrix_set_value(
        &mut self,
        id: u32,
        row: i64,
        column: i64,
        value: PineValue,
    ) -> Result<(), RuntimeError> {
        let Some(offset) = self
            .matrix_cell_offset(id, row, column)?
            .map(|(_, offset)| offset)
        else {
            return Ok(());
        };
        let copied_cells = self.matrix_store.get(&id).map_or(0, |matrix| {
            collection_values_allocation_bytes(
                matrix
                    .values
                    .write_allocation_values(offset, self.matrix_store.get_mut_clones_value(&id)),
            )
        });
        if !self
            .record_collection_bytes(copied_cells.saturating_add(value_allocation_bytes(&value)))
        {
            return Err(self.resource_budget.collection_error());
        }
        if let Some(slot) = self
            .matrix_store
            .get_mut(&id)
            .and_then(|matrix| matrix.values.get_mut(offset))
        {
            *slot = value;
        }
        Ok(())
    }

    pub(crate) fn matrix_fill_value(&mut self, id: u32, value: PineValue) {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return;
        };
        let cells = matrix.values.len();
        let copied_cells =
            matrix.cloned_entry_allocation_bytes(self.matrix_store.get_mut_clones_value(&id));
        if !self.record_collection_bytes(copied_cells) {
            return;
        }
        if !self.record_collection_repeated_value(&value, cells) {
            return;
        }
        if let Some(matrix) = self.matrix_store.get_mut(&id) {
            matrix.values.fill(value);
        }
    }

    pub(crate) fn copy_matrix(&mut self, source_id: u32) -> PineValue {
        let Some(source) = self.matrix_store.get(&source_id) else {
            return PineValue::Na;
        };
        let bytes = collection_values_allocation_bytes(source.values.clone_allocation_values());
        if !self.record_collection_bytes(bytes) {
            return PineValue::Na;
        }
        let source = self
            .matrix_store
            .get(&source_id)
            .expect("validated matrix")
            .clone();
        let id = self.next_matrix_id;
        self.next_matrix_id += 1;
        self.matrix_store.insert(id, source);
        PineValue::Matrix(id)
    }

    pub(crate) fn matrix_transpose(&mut self, source_id: u32) -> PineValue {
        let Some(source) = self.matrix_store.get(&source_id) else {
            return PineValue::Na;
        };
        let (kind, rows, columns) = (source.kind, source.columns, source.rows);
        let bytes = collection_values_allocation_bytes(source.values.iter());
        if !self.record_collection_bytes(bytes) {
            return PineValue::Na;
        }
        let source = self.matrix_store.get(&source_id).expect("validated matrix");
        let mut values = Vec::with_capacity(source.values.len());
        for row in 0..source.columns {
            for column in 0..source.rows {
                values.push(source.values[column * source.columns + row].clone());
            }
        }

        self.insert_matrix_payload(kind, rows, columns, values.into())
    }

    pub(crate) fn matrix_reverse(&mut self, id: u32) {
        let cloned_matrix = self.matrix_store.get_mut_clones_value(&id);
        let Some(matrix) = self.matrix_store.get(&id) else {
            return;
        };
        if matrix.values.len() <= 1 {
            return;
        }
        let bytes = matrix.values.all_write_allocation_values(cloned_matrix);
        let bytes = collection_values_allocation_bytes(bytes);
        if !self.record_collection_bytes(bytes) {
            return;
        }
        self.matrix_store
            .get_mut(&id)
            .expect("validated matrix")
            .values
            .reverse();
    }

    pub(crate) fn matrix_row_values(
        &self,
        id: u32,
        row: i64,
    ) -> Result<Option<Vec<PineValue>>, RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(None);
        };
        let row = matrix_index("row", row, matrix.rows)?;
        let start = row * matrix.columns;
        Ok(Some(
            matrix
                .values
                .view(start, matrix.columns)
                .iter()
                .cloned()
                .collect(),
        ))
    }

    pub(crate) fn matrix_col_values(
        &self,
        id: u32,
        column: i64,
    ) -> Result<Option<Vec<PineValue>>, RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(None);
        };
        let column = matrix_index("column", column, matrix.columns)?;
        Ok(Some(
            (0..matrix.rows)
                .map(|row| matrix.values[row * matrix.columns + column].clone())
                .collect(),
        ))
    }

    pub(crate) fn matrix_sum(&self, id: u32) -> Option<PineValue> {
        let matrix = self.matrix_store.get(&id)?;
        let mut total = 0.0;
        let mut has_value = false;
        for value in &matrix.values {
            if let Some(number) = value.as_f64() {
                total += number;
                has_value = true;
            }
        }
        has_value.then(|| finite_float_or_na(total))
    }

    pub(crate) fn matrix_avg(&self, id: u32) -> Option<PineValue> {
        let matrix = self.matrix_store.get(&id)?;
        let mut total = 0.0;
        let mut count = 0_usize;
        for value in &matrix.values {
            if let Some(number) = value.as_f64() {
                total += number;
                count += 1;
            }
        }
        (count > 0).then(|| finite_float_or_na(total / count as f64))
    }

    pub(crate) fn matrix_min(&self, id: u32) -> Option<PineValue> {
        let matrix = self.matrix_store.get(&id)?;
        matrix
            .values
            .iter()
            .filter_map(PineValue::as_f64)
            .reduce(f64::min)
            .map(finite_float_or_na)
    }

    pub(crate) fn matrix_max(&self, id: u32) -> Option<PineValue> {
        let matrix = self.matrix_store.get(&id)?;
        matrix
            .values
            .iter()
            .filter_map(PineValue::as_f64)
            .reduce(f64::max)
            .map(finite_float_or_na)
    }

    pub(crate) fn matrix_median(&self, id: u32) -> Option<PineValue> {
        let matrix = self.matrix_store.get(&id)?;
        let mut values: Vec<_> = matrix.values.iter().filter_map(PineValue::as_f64).collect();
        if values.is_empty() {
            return None;
        }
        values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
        let middle = values.len() / 2;
        let median = if values.len() % 2 == 0 {
            (values[middle - 1] + values[middle]) / 2.0
        } else {
            values[middle]
        };
        match matrix.kind {
            MatrixElementKind::Int => Some(PineValue::Int(median as i64)),
            MatrixElementKind::Float => Some(finite_float_or_na(median)),
            _ => None,
        }
    }

    pub(crate) fn matrix_mode(&self, id: u32) -> Option<PineValue> {
        let matrix = self.matrix_store.get(&id)?;
        let mut values: Vec<_> = matrix.values.iter().filter_map(PineValue::as_f64).collect();
        if values.is_empty() {
            return None;
        }
        values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));

        let mut best_value = values[0];
        let mut best_count = 0_usize;
        let mut current_value = values[0];
        let mut current_count = 0_usize;
        for value in values {
            if (value - current_value).abs() < f64::EPSILON {
                current_count += 1;
            } else {
                if current_count > best_count {
                    best_value = current_value;
                    best_count = current_count;
                }
                current_value = value;
                current_count = 1;
            }
        }
        if current_count > best_count {
            best_value = current_value;
            best_count = current_count;
        }
        (best_count >= 2).then(|| finite_float_or_na(best_value))
    }

    pub(crate) fn matrix_trace(&self, id: u32) -> Option<PineValue> {
        let matrix = self.matrix_store.get(&id)?;
        let diagonal_len = matrix.rows.min(matrix.columns);
        let mut total = 0.0;
        let mut has_value = false;
        for index in 0..diagonal_len {
            let offset = index * matrix.columns + index;
            if let Some(number) = matrix.values[offset].as_f64() {
                total += number;
                has_value = true;
            }
        }
        has_value.then(|| finite_float_or_na(total))
    }

    pub(crate) fn matrix_store_profile(&self) -> MatrixStoreProfile {
        MatrixStoreProfile {
            slots: self.matrix_store.len(),
            capacity: self.matrix_store.capacity(),
            cells: self
                .matrix_store
                .values()
                .map(|matrix| matrix.values.len())
                .sum(),
            cell_capacity: self
                .matrix_store
                .values()
                .map(|matrix| matrix.values.capacity())
                .sum(),
        }
    }

    fn matrix_cell_offset(
        &self,
        id: u32,
        row: i64,
        column: i64,
    ) -> Result<Option<(&MatrixStorage, usize)>, RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(None);
        };
        let row = matrix_index("row", row, matrix.rows)?;
        let column = matrix_index("column", column, matrix.columns)?;
        Ok(Some((matrix, row * matrix.columns + column)))
    }
}

fn eval_matrix_float_value(value: PineValue) -> PineValue {
    match value {
        PineValue::Int(value) => PineValue::Float(value as f64),
        PineValue::Float(_) | PineValue::Na => value,
        _ => PineValue::Na,
    }
}

fn eval_matrix_int_value(value: PineValue) -> PineValue {
    match value {
        PineValue::Int(_) | PineValue::Na => value,
        _ => PineValue::Na,
    }
}

fn eval_matrix_bool_value(value: PineValue) -> PineValue {
    match value {
        PineValue::Bool(_) | PineValue::Na => value,
        _ => PineValue::Na,
    }
}

fn eval_matrix_string_value(value: PineValue) -> PineValue {
    match value {
        PineValue::String(_) | PineValue::Na => value,
        _ => PineValue::Na,
    }
}

fn eval_matrix_color_value(value: PineValue) -> PineValue {
    match value {
        PineValue::Color(_) | PineValue::Na => value,
        _ => PineValue::Na,
    }
}

fn eval_matrix_value_for_kind(kind: MatrixElementKind, value: PineValue) -> PineValue {
    match kind {
        MatrixElementKind::Float => eval_matrix_float_value(value),
        MatrixElementKind::Int => eval_matrix_int_value(value),
        MatrixElementKind::Bool => eval_matrix_bool_value(value),
        MatrixElementKind::String => eval_matrix_string_value(value),
        MatrixElementKind::Color => eval_matrix_color_value(value),
        MatrixElementKind::UserType(_) => match value {
            PineValue::UserTypeRef(_) | PineValue::Na => value,
            _ => PineValue::Na,
        },
    }
}

pub(crate) fn matrix_array_element_kind(kind: MatrixElementKind) -> ArrayElementKind {
    match kind {
        MatrixElementKind::Float => ArrayElementKind::Float,
        MatrixElementKind::Int => ArrayElementKind::Int,
        MatrixElementKind::Bool => ArrayElementKind::Bool,
        MatrixElementKind::String => ArrayElementKind::String,
        MatrixElementKind::Color => ArrayElementKind::Color,
        MatrixElementKind::UserType(_) => ArrayElementKind::UserType,
    }
}

fn matrix_dimension(name: &str, value: i64) -> Result<usize, RuntimeError> {
    if value < 0 {
        return Err(RuntimeError {
            message: format!("matrix {name} count cannot be negative"),
        });
    }
    Ok(value as usize)
}

fn matrix_dimension_value(name: &str, value: PineValue) -> Result<i64, RuntimeError> {
    value.as_i64().ok_or_else(|| RuntimeError {
        message: format!("matrix {name} count cannot be na"),
    })
}

fn matrix_index_value(name: &str, value: PineValue) -> Result<i64, RuntimeError> {
    value.as_i64().ok_or_else(|| RuntimeError {
        message: format!("matrix {name} index cannot be na"),
    })
}

fn matrix_insert_index_value(name: &str, value: PineValue) -> Result<i64, RuntimeError> {
    value.as_i64().ok_or_else(|| RuntimeError {
        message: format!("matrix {name} index cannot be na"),
    })
}

fn matrix_index(name: &str, value: i64, len: usize) -> Result<usize, RuntimeError> {
    if value < 0 || value as usize >= len {
        return Err(RuntimeError {
            message: format!("matrix {name} index {value} is out of bounds for size {len}"),
        });
    }
    Ok(value as usize)
}

fn matrix_insert_index(name: &str, value: i64, len: usize) -> Result<usize, RuntimeError> {
    if value < 0 || value as usize > len {
        return Err(RuntimeError {
            message: format!(
                "matrix {name} index {value} is out of bounds for size {}",
                len + 1
            ),
        });
    }
    Ok(value as usize)
}
