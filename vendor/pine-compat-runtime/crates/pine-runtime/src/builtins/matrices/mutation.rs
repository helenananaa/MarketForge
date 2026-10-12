use std::cmp::Ordering;

use pine_ir::HirCallArg;

use super::*;

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_matrix_concat(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let target = self.eval_expr(&args[0].value)?;
        let source = self.eval_expr(&args[1].value)?;
        let (PineValue::Matrix(target_id), PineValue::Matrix(source_id)) = (target, source) else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .matrix_concat(target_id, source_id)?
            .map(PineValue::Matrix)
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn eval_matrix_reshape(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let rows = matrix_dimension_value("row", self.eval_expr(&args[1].value)?)?;
        let columns = matrix_dimension_value("column", self.eval_expr(&args[2].value)?)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        self.matrix_reshape(id, rows, columns)?;
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_add_row(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let row = crate::builtins::args::call_arg_expr(args, 1, "row")
            .map(|expr| {
                self.eval_expr(expr)
                    .and_then(|value| matrix_insert_index_value("row", value))
            })
            .transpose()?;
        let array_id = self.eval_expr(&args[2].value)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        let row = row.unwrap_or_else(|| self.matrix_shape(id).map_or(0, |(rows, _)| rows as i64));
        let PineValue::Array(array_id) = array_id else {
            return Ok(PineValue::Void);
        };
        let Some(values) = self.array_values(array_id)? else {
            return Ok(PineValue::Void);
        };
        let bytes = collection_values_allocation_bytes(values);
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let values = self.array_values_clone(array_id)?.expect("validated array");
        self.matrix_add_row_inner(id, row, values, true)?;
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_add_col(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let column = crate::builtins::args::call_arg_expr(args, 1, "column")
            .map(|expr| {
                self.eval_expr(expr)
                    .and_then(|value| matrix_insert_index_value("column", value))
            })
            .transpose()?;
        let array_id = self.eval_expr(&args[2].value)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        let PineValue::Array(array_id) = array_id else {
            return Ok(PineValue::Void);
        };
        let Some(values) = self.array_values(array_id)? else {
            return Ok(PineValue::Void);
        };
        let bytes = collection_values_allocation_bytes(values);
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let values = self.array_values_clone(array_id)?.expect("validated array");
        let column = column.unwrap_or_else(|| {
            self.matrix_shape(id)
                .map_or(0, |(_, columns)| columns as i64)
        });
        self.matrix_add_col_inner(id, column, values, true)?;
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_remove_row(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let row = matrix_index_value("row", self.eval_expr(&args[1].value)?)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        let kind = matrix.kind;
        let index = matrix_index("row", row, matrix.rows)?;
        let bytes = collection_values_allocation_bytes(
            matrix.values.view(index * matrix.columns, matrix.columns),
        );
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let Some(values) = self.matrix_row_values(id, row)? else {
            return Ok(PineValue::Na);
        };
        self.matrix_remove_row(id, row)?;
        self.insert_precharged_matrix_result_array(kind, values)
    }

    pub(crate) fn eval_matrix_remove_col(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let column = matrix_index_value("column", self.eval_expr(&args[1].value)?)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        let kind = matrix.kind;
        let index = matrix_index("column", column, matrix.columns)?;
        let bytes = collection_values_allocation_bytes(
            (0..matrix.rows).map(|row| &matrix.values[row * matrix.columns + index]),
        );
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let Some(values) = self.matrix_col_values(id, column)? else {
            return Ok(PineValue::Na);
        };
        self.matrix_remove_col(id, column)?;
        self.insert_precharged_matrix_result_array(kind, values)
    }

    pub(crate) fn eval_matrix_swap_rows(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let row1 = matrix_index_value("row", self.eval_expr(&args[1].value)?)?;
        let row2 = matrix_index_value("row", self.eval_expr(&args[2].value)?)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        self.matrix_swap_rows(id, row1, row2)?;
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_swap_columns(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let column1 = matrix_index_value("column", self.eval_expr(&args[1].value)?)?;
        let column2 = matrix_index_value("column", self.eval_expr(&args[2].value)?)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        self.matrix_swap_columns(id, column1, column2)?;
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_sort(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let column = match crate::builtins::args::positional_arg(args, 1) {
            Some(column) => matrix_index_value("column", self.eval_expr(&column.value)?)?,
            None => 0,
        };
        let descending = self.eval_matrix_sort_descending(args)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Void);
        };
        self.matrix_sort(id, column, descending)?;
        Ok(PineValue::Void)
    }

    pub(crate) fn eval_matrix_submatrix(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let id = self.eval_expr(&args[0].value)?;
        let PineValue::Matrix(id) = id else {
            return Ok(PineValue::Na);
        };
        let Some((rows, columns)) = self.matrix_shape(id) else {
            return Ok(PineValue::Na);
        };
        let from_row = self.eval_optional_matrix_slice_index(args, 1, "row", 0)?;
        let to_row = self.eval_optional_matrix_slice_index(args, 2, "row", rows as i64)?;
        let from_column = self.eval_optional_matrix_slice_index(args, 3, "column", 0)?;
        let to_column = self.eval_optional_matrix_slice_index(args, 4, "column", columns as i64)?;
        self.matrix_submatrix(id, from_row, to_row, from_column, to_column)
    }

    pub(crate) fn matrix_reshape(
        &mut self,
        id: u32,
        rows: i64,
        columns: i64,
    ) -> Result<(), RuntimeError> {
        let rows = matrix_dimension("row", rows)?;
        let columns = matrix_dimension("column", columns)?;
        let cells = rows.checked_mul(columns).ok_or_else(|| RuntimeError {
            message: "matrix reshape dimensions must preserve element count".to_owned(),
        })?;
        let cloned_matrix = self.matrix_store.get_mut_clones_value(&id);
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        if cells != matrix.values.len() {
            return Err(RuntimeError {
                message: "matrix reshape dimensions must preserve element count".to_owned(),
            });
        }
        let bytes = matrix.cloned_entry_allocation_bytes(cloned_matrix);
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get_mut(&id).expect("validated matrix");
        matrix.rows = rows;
        matrix.columns = columns;
        Ok(())
    }

    pub(crate) fn matrix_concat(
        &mut self,
        target_id: u32,
        source_id: u32,
    ) -> Result<Option<u32>, RuntimeError> {
        let Some(source) = self.matrix_store.get(&source_id) else {
            return Ok(None);
        };
        let cloned_target = self.matrix_store.get_mut_clones_value(&target_id);
        let Some(target) = self.matrix_store.get(&target_id) else {
            return Ok(None);
        };
        if target.kind != source.kind {
            return Ok(None);
        }
        if target.columns != source.columns {
            return Err(RuntimeError {
                message: format!(
                    "matrix.concat column count {} must match source column count {}",
                    target.columns, source.columns
                ),
            });
        }
        let new_rows = target
            .rows
            .checked_add(source.rows)
            .ok_or_else(|| RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            })?;
        let new_cells = target
            .values
            .len()
            .checked_add(source.values.len())
            .ok_or_else(|| RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            })?;
        if new_cells > MAX_MATRIX_CELLS {
            return Err(RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            });
        }
        let bytes = collection_values_allocation_bytes(source.values.clone_allocation_values())
            .saturating_add(collection_values_allocation_bytes(source.values.iter()))
            .saturating_add(if source.values.is_empty() {
                target.cloned_entry_allocation_bytes(cloned_target)
            } else {
                collection_values_allocation_bytes(target.values.append_allocation_values(
                    cloned_target
                        || (target_id == source_id
                            && target.values.clone_allocation_values().is_empty()),
                ))
            });
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let source = self
            .matrix_store
            .get(&source_id)
            .expect("validated matrix")
            .clone();
        let target = self
            .matrix_store
            .get_mut(&target_id)
            .expect("validated matrix");
        target.values.extend(source.values.iter().cloned());
        target.rows = new_rows;
        Ok(Some(target_id))
    }

    pub(crate) fn matrix_add_row(
        &mut self,
        id: u32,
        row: i64,
        values: Vec<PineValue>,
    ) -> Result<(), RuntimeError> {
        self.matrix_add_row_inner(id, row, values, false)
    }

    fn matrix_add_row_inner(
        &mut self,
        id: u32,
        row: i64,
        values: Vec<PineValue>,
        incoming_precharged: bool,
    ) -> Result<(), RuntimeError> {
        let cloned_matrix = self.matrix_store.get_mut_clones_value(&id);
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        let row = matrix_insert_index("row", row, matrix.rows)?;
        let columns = if matrix.rows == 0 && matrix.columns == 0 {
            values.len()
        } else {
            matrix.columns
        };
        if values.len() != columns {
            return Err(RuntimeError {
                message: format!(
                    "matrix add_row array size {} must match column count {}",
                    values.len(),
                    matrix.columns
                ),
            });
        }
        let new_cells = (matrix.rows + 1)
            .checked_mul(columns)
            .ok_or_else(|| RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            })?;
        if new_cells > MAX_MATRIX_CELLS {
            return Err(RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            });
        }
        let offset = row * columns;
        let kind = matrix.kind;
        let rows = matrix.rows + 1;
        let copied_values = if offset < matrix.values.len() {
            collection_values_allocation_bytes(matrix.values.iter())
        } else {
            collection_values_allocation_bytes(
                matrix.values.append_allocation_values(cloned_matrix),
            )
        };
        let bytes = copied_values.saturating_add(if incoming_precharged {
            0
        } else {
            collection_values_allocation_bytes(&values)
        });
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let inserted = values
            .into_iter()
            .map(|value| eval_matrix_value_for_kind(kind, value));
        let matrix = self.matrix_store.get(&id).expect("validated matrix");
        if offset < matrix.values.len() {
            let mut next = Vec::with_capacity(new_cells);
            next.extend(matrix.values.view(0, offset).iter().cloned());
            next.extend(inserted);
            next.extend(
                matrix
                    .values
                    .view(offset, matrix.values.len() - offset)
                    .iter()
                    .cloned(),
            );
            self.matrix_store.insert(
                id,
                MatrixStorage {
                    kind,
                    rows,
                    columns,
                    values: next.into(),
                },
            );
        } else {
            let matrix = self.matrix_store.get_mut(&id).expect("validated matrix");
            matrix.values.extend(inserted);
            matrix.rows = rows;
            matrix.columns = columns;
        }
        Ok(())
    }

    pub(crate) fn matrix_add_col(
        &mut self,
        id: u32,
        column: i64,
        values: Vec<PineValue>,
    ) -> Result<(), RuntimeError> {
        self.matrix_add_col_inner(id, column, values, false)
    }

    fn matrix_add_col_inner(
        &mut self,
        id: u32,
        column: i64,
        mut values: Vec<PineValue>,
        incoming_precharged: bool,
    ) -> Result<(), RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        let column = matrix_insert_index("column", column, matrix.columns)?;
        let rows = if matrix.rows == 0 && matrix.columns == 0 {
            values.len()
        } else {
            matrix.rows
        };
        if values.len() != rows {
            return Err(RuntimeError {
                message: format!(
                    "matrix add_col array size {} must match row count {}",
                    values.len(),
                    matrix.rows
                ),
            });
        }
        let new_columns = matrix.columns + 1;
        let new_cells = rows.checked_mul(new_columns).ok_or_else(|| RuntimeError {
            message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
        })?;
        if new_cells > MAX_MATRIX_CELLS {
            return Err(RuntimeError {
                message: format!("matrix cell count cannot exceed {MAX_MATRIX_CELLS}"),
            });
        }

        let kind = matrix.kind;
        let old_columns = matrix.columns;
        for value in &mut values {
            *value = eval_matrix_value_for_kind(kind, std::mem::replace(value, PineValue::Na));
        }
        let bytes = collection_values_allocation_bytes(matrix.values.iter()).saturating_add(
            if incoming_precharged {
                0
            } else {
                collection_values_allocation_bytes(&values)
            },
        );
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get(&id).expect("validated matrix");
        let mut inserted_values = values.into_iter();
        let mut next_values = Vec::with_capacity(new_cells);
        for row in 0..rows {
            let start = row * old_columns;
            let insert_offset = start + column;
            next_values.extend(matrix.values.view(start, column).iter().cloned());
            next_values.push(inserted_values.next().unwrap_or(PineValue::Na));
            next_values.extend(
                matrix
                    .values
                    .view(insert_offset, old_columns - column)
                    .iter()
                    .cloned(),
            );
        }
        self.matrix_store.insert(
            id,
            MatrixStorage {
                kind,
                rows,
                columns: new_columns,
                values: next_values.into(),
            },
        );
        Ok(())
    }

    pub(crate) fn matrix_remove_row(&mut self, id: u32, row: i64) -> Result<(), RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        let row = matrix_index("row", row, matrix.rows)?;
        let start = row * matrix.columns;
        let end = start + matrix.columns;
        let bytes = collection_values_allocation_bytes(matrix.values.view(0, start))
            .saturating_add(collection_values_allocation_bytes(
                matrix.values.view(end, matrix.values.len() - end),
            ));
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get(&id).expect("validated matrix");
        let (kind, rows, columns) = (matrix.kind, matrix.rows - 1, matrix.columns);
        let mut values = Vec::with_capacity(rows * columns);
        values.extend(matrix.values.view(0, start).iter().cloned());
        values.extend(
            matrix
                .values
                .view(end, matrix.values.len() - end)
                .iter()
                .cloned(),
        );
        self.matrix_store.insert(
            id,
            MatrixStorage {
                kind,
                rows,
                columns,
                values: values.into(),
            },
        );
        Ok(())
    }

    pub(crate) fn matrix_remove_col(&mut self, id: u32, column: i64) -> Result<(), RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        let column = matrix_index("column", column, matrix.columns)?;
        let new_columns = matrix.columns - 1;
        let (kind, rows) = (matrix.kind, matrix.rows);
        let bytes = collection_values_allocation_bytes(
            matrix
                .values
                .iter()
                .enumerate()
                .filter(|(index, _)| index % matrix.columns != column)
                .map(|(_, value)| value),
        );
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get(&id).expect("validated matrix");
        let mut next_values = Vec::with_capacity(matrix.rows * new_columns);
        for row in 0..matrix.rows {
            let start = row * matrix.columns;
            let remove_offset = start + column;
            next_values.extend(matrix.values.view(start, column).iter().cloned());
            next_values.extend(
                matrix
                    .values
                    .view(remove_offset + 1, matrix.columns - column - 1)
                    .iter()
                    .cloned(),
            );
        }
        self.matrix_store.insert(
            id,
            MatrixStorage {
                kind,
                rows,
                columns: new_columns,
                values: next_values.into(),
            },
        );
        Ok(())
    }

    pub(crate) fn matrix_swap_rows(
        &mut self,
        id: u32,
        row1: i64,
        row2: i64,
    ) -> Result<(), RuntimeError> {
        let cloned_matrix = self.matrix_store.get_mut_clones_value(&id);
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        let row1 = matrix_index("row", row1, matrix.rows)?;
        let row2 = matrix_index("row", row2, matrix.rows)?;
        if row1 == row2 || matrix.columns == 0 {
            return Ok(());
        }
        let bytes = matrix.swap_allocation_bytes(
            (0..matrix.columns).map(|column| {
                (
                    row1 * matrix.columns + column,
                    row2 * matrix.columns + column,
                )
            }),
            cloned_matrix,
        );
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get_mut(&id).expect("validated matrix");
        for column in 0..matrix.columns {
            matrix.values.swap(
                row1 * matrix.columns + column,
                row2 * matrix.columns + column,
            );
        }
        Ok(())
    }

    pub(crate) fn matrix_swap_columns(
        &mut self,
        id: u32,
        column1: i64,
        column2: i64,
    ) -> Result<(), RuntimeError> {
        let cloned_matrix = self.matrix_store.get_mut_clones_value(&id);
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        let column1 = matrix_index("column", column1, matrix.columns)?;
        let column2 = matrix_index("column", column2, matrix.columns)?;
        if column1 == column2 || matrix.rows == 0 {
            return Ok(());
        }
        let bytes = matrix.swap_allocation_bytes(
            (0..matrix.rows).map(|row| {
                (
                    row * matrix.columns + column1,
                    row * matrix.columns + column2,
                )
            }),
            cloned_matrix,
        );
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get_mut(&id).expect("validated matrix");
        for row in 0..matrix.rows {
            matrix.values.swap(
                row * matrix.columns + column1,
                row * matrix.columns + column2,
            );
        }
        Ok(())
    }

    pub(crate) fn matrix_sort(
        &mut self,
        id: u32,
        column: i64,
        descending: bool,
    ) -> Result<(), RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(());
        };
        let column = matrix_index("column", column, matrix.columns)?;
        if matrix.rows <= 1 || matrix.columns == 0 {
            return Ok(());
        }
        let (kind, rows, columns) = (matrix.kind, matrix.rows, matrix.columns);
        let bytes = collection_values_allocation_bytes(matrix.values.iter());
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get(&id).expect("validated matrix");
        let mut row_indexes = (0..matrix.rows).collect::<Vec<_>>();
        row_indexes.sort_by(|left, right| {
            let left_value = &matrix.values[left * matrix.columns + column];
            let right_value = &matrix.values[right * matrix.columns + column];
            compare_matrix_sort_values(left_value, right_value, descending)
                .then_with(|| left.cmp(right))
        });

        let mut next_values = Vec::with_capacity(matrix.values.len());
        for row in row_indexes {
            let start = row * matrix.columns;
            next_values.extend(matrix.values.view(start, matrix.columns).iter().cloned());
        }
        self.matrix_store.insert(
            id,
            MatrixStorage {
                kind,
                rows,
                columns,
                values: next_values.into(),
            },
        );
        Ok(())
    }

    pub(crate) fn matrix_submatrix(
        &mut self,
        id: u32,
        from_row: i64,
        to_row: i64,
        from_column: i64,
        to_column: i64,
    ) -> Result<PineValue, RuntimeError> {
        let Some(matrix) = self.matrix_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        let from_row = matrix_slice_index("row", from_row, matrix.rows)?;
        let to_row = matrix_slice_index("row", to_row, matrix.rows)?;
        let from_column = matrix_slice_index("column", from_column, matrix.columns)?;
        let to_column = matrix_slice_index("column", to_column, matrix.columns)?;
        if from_row > to_row {
            return Err(RuntimeError {
                message: "matrix row range start cannot be greater than end".to_owned(),
            });
        }
        if from_column > to_column {
            return Err(RuntimeError {
                message: "matrix column range start cannot be greater than end".to_owned(),
            });
        }

        let rows = to_row - from_row;
        let columns = to_column - from_column;
        let kind = matrix.kind;
        let bytes = collection_values_allocation_bytes((from_row..to_row).flat_map(|row| {
            matrix
                .values
                .view(row * matrix.columns + from_column, columns)
                .iter()
        }));
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let matrix = self.matrix_store.get(&id).expect("validated matrix");
        let mut values = Vec::with_capacity(rows * columns);
        for row in from_row..to_row {
            let start = row * matrix.columns + from_column;
            values.extend(matrix.values.view(start, columns).iter().cloned());
        }
        Ok(self.insert_matrix_payload(kind, rows, columns, values.into()))
    }

    fn insert_precharged_matrix_result_array(
        &mut self,
        kind: MatrixElementKind,
        values: Vec<PineValue>,
    ) -> Result<PineValue, RuntimeError> {
        let type_name = if let MatrixElementKind::UserType(index) = kind {
            Some(
                self.program
                    .user_types
                    .get(index)
                    .ok_or_else(|| RuntimeError {
                        message: "invalid matrix UDT element identity".to_owned(),
                    })?
                    .identity
                    .type_name
                    .clone(),
            )
        } else {
            None
        };
        let value = self.insert_precharged_array_values(matrix_array_element_kind(kind), values);
        if let (PineValue::Array(id), Some(type_name)) = (&value, type_name) {
            self.array_user_types.insert(*id, type_name);
        }
        Ok(value)
    }

    fn eval_matrix_sort_descending(&mut self, args: &[HirCallArg]) -> Result<bool, RuntimeError> {
        match crate::builtins::args::positional_arg(args, 2) {
            Some(order) => match self.eval_expr(&order.value)? {
                PineValue::String(order) if order == "order.descending" => Ok(true),
                PineValue::String(order) if order == "order.ascending" => Ok(false),
                PineValue::String(order) => Err(RuntimeError {
                    message: format!("unsupported matrix.sort order `{order}`"),
                }),
                _ => Ok(false),
            },
            None => Ok(false),
        }
    }

    fn eval_optional_matrix_slice_index(
        &mut self,
        args: &[HirCallArg],
        index: usize,
        name: &str,
        default: i64,
    ) -> Result<i64, RuntimeError> {
        match crate::builtins::args::positional_arg(args, index) {
            Some(arg) => matrix_index_value(name, self.eval_expr(&arg.value)?),
            None => Ok(default),
        }
    }
}

fn compare_matrix_sort_values(left: &PineValue, right: &PineValue, descending: bool) -> Ordering {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => {
            let ordering = left.partial_cmp(&right).unwrap_or(Ordering::Equal);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        }
        (Some(_), None) => {
            if descending {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (None, Some(_)) => {
            if descending {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (None, None) => Ordering::Equal,
    }
}

fn matrix_slice_index(name: &str, value: i64, len: usize) -> Result<usize, RuntimeError> {
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
