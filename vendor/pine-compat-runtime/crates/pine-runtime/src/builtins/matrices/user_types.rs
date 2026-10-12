use super::*;

impl HistoricalRuntime<'_> {
    pub(crate) fn eval_udt_matrix_new(
        &mut self,
        index: usize,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let rows = if let Some(arg) = args.first() {
            matrix_dimension_value("row", self.eval_expr(&arg.value)?)?
        } else {
            0
        };
        let columns = if let Some(arg) = args.get(1) {
            matrix_dimension_value("column", self.eval_expr(&arg.value)?)?
        } else {
            0
        };
        let value = if let Some(arg) = args.get(2) {
            self.eval_expr(&arg.value)?
        } else {
            PineValue::Na
        };
        self.new_matrix(MatrixElementKind::UserType(index), rows, columns, value)
    }
    pub(crate) fn new_matrix_result_array(
        &mut self,
        kind: MatrixElementKind,
        values: Vec<PineValue>,
    ) -> Result<PineValue, RuntimeError> {
        if let MatrixElementKind::UserType(index) = kind {
            let type_name = self
                .program
                .user_types
                .get(index)
                .ok_or_else(|| RuntimeError {
                    message: "invalid matrix UDT element identity".to_owned(),
                })?
                .identity
                .type_name
                .clone();
            return Ok(self.new_user_type_array_from_values(&type_name, values));
        }
        Ok(self.new_array_from_values(matrix_array_element_kind(kind), values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_syntax::SourceFile;

    #[test]
    fn source_level_udt_matrix_constructors_reach_typed_storage() {
        let analysis = pine_sema::analyze_source(&SourceFile::new(
            "matrix.pine",
            "//@version=6\nindicator(\"matrix\")\ntype Item\n    int value\nvar matrix<Item> empty=matrix.new<Item>()\nvalues=matrix.new<Item>(columns=2,initial_value=Item.new(7),rows=1)\nplot(close)\n",
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let program = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        runtime
            .append_bar(Bar {
                time: 0,
                open: 1.,
                high: 1.,
                low: 1.,
                close: 1.,
                volume: 1.,
            })
            .unwrap();
        assert_eq!(runtime.matrix_store.len(), 2);
        assert_eq!(runtime.matrix_shape(0), Some((0, 0)));
        assert_eq!(runtime.matrix_shape(1), Some((1, 2)));
        assert!(matches!(
            runtime.matrix_store[&1].kind,
            MatrixElementKind::UserType(_)
        ));
        let first = runtime.matrix_get_cloned(1, 0, 0).unwrap().unwrap();
        assert_eq!(
            runtime.matrix_get_cloned(1, 0, 1).unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            runtime.materialize_object(&first).unwrap(),
            PineValue::UserType(vec![PineValue::Int(7)])
        );
    }

    #[test]
    fn udt_matrix_constructor_rejects_wrong_dimensions_and_element_identity() {
        for expression in [
            "matrix.new<Item>(rows=1,rows=2)",
            "matrix.new<Item>(1.5,2)",
            "matrix.new<Item>(1,1,Other.new(1))",
        ] {
            let source = format!(
                "//@version=6\nindicator(\"invalid\")\ntype Item\n    int value\ntype Other\n    int value\nx={expression}\nplot(close)\n"
            );
            let analysis = pine_sema::analyze_source(&SourceFile::new("invalid.pine", source));
            assert!(analysis.hir.is_none());
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "E_UDT_MATRIX_ARG"),
                "{:?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn udt_matrix_is_not_admitted_as_a_numeric_matrix() {
        for operation in [
            "matrix.sum(values)",
            "matrix.det(values)",
            "matrix.avg(values)",
        ] {
            let source = format!(
                "//@version=6\nindicator(\"numeric boundary\")\ntype Item\n    int value\nvalues=matrix.new<Item>(1,1,Item.new(7))\nplot({operation})\n"
            );
            let analysis = pine_sema::analyze_source(&SourceFile::new("numeric.pine", source));
            assert!(analysis.hir.is_none());
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "E_CALL_ARG_TYPE"),
                "{:?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn udt_matrix_copy_and_rows_preserve_objects_but_not_cell_slots() {
        let analysis = pine_sema::analyze_source(&SourceFile::new(
            "types.pine",
            "//@version=6\nindicator(\"types\")\ntype Item\n    int value\nplot(close)\n",
        ));
        let program = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        let identity = program.user_types[0].identity.clone();
        let object = runtime
            .allocate_object(vec![PineValue::Int(1)], &identity)
            .unwrap();
        let PineValue::UserTypeRef(object_id) = object else {
            panic!("object")
        };
        let kind = MatrixElementKind::UserType(0);
        let PineValue::Matrix(matrix) = runtime.new_matrix(kind, 1, 2, object.clone()).unwrap()
        else {
            panic!("matrix")
        };
        let PineValue::Matrix(copy) = runtime.copy_matrix(matrix) else {
            panic!("copy")
        };
        assert_eq!(runtime.matrix_store[&copy].kind, kind);
        let values = runtime.matrix_row_values(matrix, 0).unwrap().unwrap();
        let PineValue::Array(row) = runtime.new_matrix_result_array(kind, values).unwrap() else {
            panic!("row")
        };
        assert_eq!(runtime.array_user_type_name(row), Some("Item"));
        runtime
            .set_object_field(object_id, 0, PineValue::Int(7))
            .unwrap();
        assert_eq!(
            runtime.matrix_get_cloned(copy, 0, 1).unwrap(),
            Some(object.clone())
        );
        assert_eq!(
            runtime
                .materialize_object(&runtime.array_get_cloned(row, 1).unwrap().unwrap())
                .unwrap(),
            PineValue::UserType(vec![PineValue::Int(7)])
        );
        let replacement = runtime
            .allocate_object(vec![PineValue::Int(9)], &identity)
            .unwrap();
        runtime
            .array_set_value(row, 0, replacement.clone())
            .unwrap();
        assert_eq!(
            runtime.matrix_get_cloned(matrix, 0, 0).unwrap(),
            Some(object.clone())
        );
        runtime.matrix_set_value(copy, 0, 0, replacement).unwrap();
        assert_eq!(
            runtime.matrix_get_cloned(matrix, 0, 0).unwrap(),
            Some(object)
        );
        let mut fork = runtime.clone();
        fork.set_object_field(object_id, 0, PineValue::Int(11))
            .unwrap();
        assert_eq!(
            runtime.object_field(object_id, 0).unwrap(),
            PineValue::Int(7)
        );
    }

    #[test]
    fn empty_matrices_infer_dimensions_without_partial_updates_on_error() {
        let analysis = pine_sema::analyze_source(&SourceFile::new(
            "types.pine",
            "//@version=6\nindicator(\"types\")\ntype Item\n    int value\nplot(close)\n",
        ));
        let program = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        let object = runtime
            .allocate_object(vec![PineValue::Int(1)], &program.user_types[0].identity)
            .unwrap();
        for (kind, value) in [
            (MatrixElementKind::Int, PineValue::Int(7)),
            (MatrixElementKind::UserType(0), object),
        ] {
            let PineValue::Matrix(rows) = runtime.new_matrix(kind, 0, 0, PineValue::Na).unwrap()
            else {
                panic!("matrix")
            };
            assert!(
                runtime
                    .matrix_add_row(rows, 1, vec![value.clone()])
                    .is_err()
            );
            assert_eq!(runtime.matrix_shape(rows), Some((0, 0)));
            runtime
                .matrix_add_row(rows, 0, vec![value.clone(), value.clone()])
                .unwrap();
            assert_eq!(runtime.matrix_shape(rows), Some((1, 2)));
            let before = runtime.matrix_store[&rows].clone();
            assert!(
                runtime
                    .matrix_add_row(rows, 1, vec![value.clone()])
                    .is_err()
            );
            assert_eq!(runtime.matrix_store[&rows], before);
            let PineValue::Matrix(columns) = runtime.new_matrix(kind, 0, 0, PineValue::Na).unwrap()
            else {
                panic!("matrix")
            };
            runtime
                .matrix_add_col(columns, 0, vec![value.clone(), value.clone()])
                .unwrap();
            assert_eq!(runtime.matrix_shape(columns), Some((2, 1)));
            assert_eq!(
                runtime.matrix_get_cloned(columns, 1, 0).unwrap(),
                Some(value)
            );
        }
    }
}
