use super::*;
use pine_syntax::SourceFile;

fn program() -> pine_ir::HirProgram {
    let analysis = pine_sema::analyze_source(&SourceFile::new(
        "matrix_stochastic.pine",
        "//@version=6\nindicator(\"matrix stochastic oracle\")\n",
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

// Keep the original flat conversion buffer and both reductions independent of
// the runtime's paged storage and borrowed row/column traversal.
fn copy_buffer_oracle(rows: usize, columns: usize, cells: &[PineValue]) -> bool {
    assert_eq!(rows * columns, cells.len());
    if cells.is_empty() {
        return false;
    }
    let mut values = Vec::with_capacity(cells.len());
    for value in cells {
        let Some(number) = value.as_f64() else {
            return false;
        };
        if !number.is_finite() || number < 0.0 {
            return false;
        }
        values.push(number);
    }

    let rows_sum_to_one = (0..rows).all(|row| {
        let start = row * columns;
        let end = start + columns;
        values[start..end].iter().sum::<f64>() == 1.0
    });
    let columns_sum_to_one = (0..columns).all(|column| {
        (0..rows)
            .map(|row| values[row * columns + column])
            .sum::<f64>()
            == 1.0
    });
    rows_sum_to_one || columns_sum_to_one
}

fn insert_case(
    runtime: &mut HistoricalRuntime<'_>,
    kind: MatrixElementKind,
    rows: usize,
    columns: usize,
    cells: &[PineValue],
) -> u32 {
    assert_eq!(rows * columns, cells.len());
    let PineValue::Matrix(id) = runtime.insert_matrix_storage(kind, rows, columns, cells.to_vec())
    else {
        panic!("matrix id");
    };
    id
}

fn assert_value_bits(actual: &PineValue, expected: &PineValue) {
    assert_eq!(
        std::mem::discriminant(actual),
        std::mem::discriminant(expected)
    );
    match (actual, expected) {
        (PineValue::Float(actual), PineValue::Float(expected)) => {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        (PineValue::Tuple(actual), PineValue::Tuple(expected))
        | (PineValue::UserType(actual), PineValue::UserType(expected)) => {
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_value_bits(actual, expected);
            }
        }
        (PineValue::ChartPoint(actual), PineValue::ChartPoint(expected)) => {
            assert_value_bits(&actual.time, &expected.time);
            assert_value_bits(&actual.index, &expected.index);
            assert_value_bits(&actual.price, &expected.price);
        }
        _ => assert_eq!(actual, expected),
    }
}

fn assert_logical_cells(values: &ArrayValues, cells: &[PineValue]) {
    assert_eq!(values.len(), cells.len());
    let mut flattened = values.slices().flatten();
    for (index, expected) in cells.iter().enumerate() {
        let actual = flattened.next().expect("logical cell");
        assert_value_bits(actual, expected);
        assert!(std::ptr::eq(
            actual,
            values.get(index).expect("logical cell")
        ));
    }
    assert!(flattened.next().is_none());
    assert!(flattened.next().is_none());
}

fn assert_case(
    runtime: &HistoricalRuntime<'_>,
    id: u32,
    rows: usize,
    columns: usize,
    cells: &[PineValue],
    expected: bool,
) {
    assert_logical_cells(&runtime.matrix_store.get(&id).unwrap().values, cells);
    let oracle = copy_buffer_oracle(rows, columns, cells);
    assert_eq!(oracle, expected, "oracle: {rows}x{columns}, {cells:?}");
    assert_eq!(
        runtime.matrix_is_stochastic(id),
        Some(oracle),
        "runtime: {rows}x{columns}, {cells:?}"
    );
}

#[test]
fn stochastic_numeric_rectangles_and_zero_dimensions_match_copy_oracle() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let float = PineValue::Float;
    let int = PineValue::Int;
    let cases = [
        (MatrixElementKind::Float, 0, 0, vec![], false),
        (MatrixElementKind::Float, 0, 129, vec![], false),
        (MatrixElementKind::Int, 129, 0, vec![], false),
        (MatrixElementKind::Float, 1, 1, vec![float(1.0)], true),
        (MatrixElementKind::Int, 1, 1, vec![int(i64::MAX)], false),
        (
            MatrixElementKind::Float,
            1,
            129,
            vec![float(1.0); 129],
            true,
        ),
        (
            MatrixElementKind::Float,
            1,
            129,
            {
                let mut cells = vec![float(1.0); 129];
                cells[128] = float(f64::from_bits(1.0_f64.to_bits() - 1));
                cells
            },
            false,
        ),
        (
            MatrixElementKind::Float,
            2,
            3,
            vec![
                float(0.25),
                float(0.75),
                float(-0.0),
                int(0),
                int(0),
                int(1),
            ],
            true,
        ),
        (
            MatrixElementKind::Float,
            3,
            2,
            vec![
                float(0.5),
                float(-0.0),
                float(0.5),
                float(0.25),
                int(0),
                float(0.75),
            ],
            true,
        ),
        (
            MatrixElementKind::Int,
            2,
            3,
            vec![int(0), int(1), int(0), int(0), int(0), int(1)],
            true,
        ),
        (
            MatrixElementKind::Int,
            3,
            2,
            vec![int(0), int(1), int(1), int(0), int(0), int(0)],
            true,
        ),
        (MatrixElementKind::Int, 2, 2, vec![int(1); 4], false),
        (
            MatrixElementKind::Float,
            1,
            2,
            vec![float(-0.0), float(0.0)],
            false,
        ),
    ];
    for (kind, rows, columns, cells, expected) in cases {
        let id = insert_case(&mut runtime, kind, rows, columns, &cells);
        assert_case(&runtime, id, rows, columns, &cells, expected);
    }
    assert_eq!(runtime.matrix_is_stochastic(u32::MAX), None);
}

#[test]
fn stochastic_invalid_cells_and_non_numeric_values_match_copy_oracle() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let invalid = [
        PineValue::Int(-1),
        PineValue::Int(i64::MIN),
        PineValue::Float(-f64::from_bits(1)),
        PineValue::Float(f64::INFINITY),
        PineValue::Float(f64::NEG_INFINITY),
        PineValue::Float(f64::NAN),
        PineValue::Bool(true),
        PineValue::String("1".into()),
        PineValue::Color(1),
        PineValue::Plot(0),
        PineValue::HLine(0),
        PineValue::Label(0),
        PineValue::Line(0),
        PineValue::LineFill(0),
        PineValue::Polyline(0),
        PineValue::Box(0),
        PineValue::Table(0),
        PineValue::ChartPoint(ChartPointValue::new(
            PineValue::Int(0),
            PineValue::Int(0),
            PineValue::Float(1.0),
        )),
        PineValue::Array(0),
        PineValue::Matrix(0),
        PineValue::Map(0),
        PineValue::UserType(vec![PineValue::Float(1.0)]),
        PineValue::UserTypeRef(0),
        PineValue::Tuple(vec![PineValue::Float(1.0)]),
        PineValue::Na,
        PineValue::Void,
    ];
    for value in invalid {
        for index in [0, 2, 5] {
            let mut cells = vec![
                PineValue::Float(1.0),
                PineValue::Float(0.0),
                PineValue::Float(-0.0),
                PineValue::Float(0.0),
                PineValue::Float(0.0),
                PineValue::Float(1.0),
            ];
            // Bypass element-kind coercion so unsupported raw PineValue kinds
            // must be rejected by the predicate's own complete validation.
            cells[index] = value.clone();
            let id = insert_case(&mut runtime, MatrixElementKind::Float, 2, 3, &cells);
            assert_case(&runtime, id, 2, 3, &cells, false);
        }
    }
}

#[test]
fn stochastic_rounding_sensitive_order_matches_copy_oracle() {
    let program = program();
    let mut runtime = HistoricalRuntime::new(&program);
    let quarter_ulp = f64::from_bits(0x3c90_0000_0000_0000);
    let previous_one = f64::from_bits(1.0_f64.to_bits() - 1);
    let cases = [
        // Each quarter-ulp rounds away after 1; accumulating them first raises
        // the total by one ulp. These cases detect reassociation in either axis.
        (
            vec![1.0, quarter_ulp, quarter_ulp, quarter_ulp, quarter_ulp],
            true,
        ),
        (
            vec![quarter_ulp, quarter_ulp, quarter_ulp, quarter_ulp, 1.0],
            false,
        ),
        (vec![previous_one, f64::EPSILON / 2.0, -0.0], true),
        (vec![f64::from_bits(1.0_f64.to_bits() + 1), 0.0], false),
        (vec![f64::MAX, f64::MAX, -0.0], false),
        (vec![f64::from_bits(1), previous_one], false),
    ];
    for (numbers, expected) in cases {
        let cells: Vec<_> = numbers.into_iter().map(PineValue::Float).collect();
        for (rows, columns) in [(1, cells.len()), (cells.len(), 1)] {
            let id = insert_case(
                &mut runtime,
                MatrixElementKind::Float,
                rows,
                columns,
                &cells,
            );
            assert_case(&runtime, id, rows, columns, &cells, expected);
        }
    }

    // Three columns cross page boundaries and require a reset between rows.
    // The leading one discards each small addition; moving a middle column's
    // one to the bottom makes its preceding small additions survive.
    let mut cells: Vec<_> = (0..129)
        .flat_map(|row| [if row == 0 { 1.0 } else { quarter_ulp }; 3].map(PineValue::Float))
        .collect();
    let id = insert_case(&mut runtime, MatrixElementKind::Float, 129, 3, &cells);
    assert_case(&runtime, id, 129, 3, &cells, true);
    cells[1] = PineValue::Float(quarter_ulp);
    cells[128 * 3 + 1] = PineValue::Float(1.0);
    let id = insert_case(&mut runtime, MatrixElementKind::Float, 129, 3, &cells);
    assert_case(&runtime, id, 129, 3, &cells, false);
}

#[test]
fn stochastic_paged_boundaries_and_snapshots_match_copy_oracle() {
    let program = program();
    let original: Vec<_> = (0..257)
        .map(|index| match index % 4 {
            0 => PineValue::Float(-0.0),
            1 => PineValue::Int(index),
            2 => PineValue::Float(f64::from_bits(0x7ff8_0000_0000_0001)),
            _ => PineValue::String(index.to_string()),
        })
        .collect();
    let mut cells = original.clone();
    let mut values: ArrayValues = original.clone().into();
    let snapshot = values.clone();
    for length in [257, 129, 128, 127, 1, 0] {
        while cells.len() > length {
            assert_value_bits(&values.remove(values.len() - 1), &cells.pop().unwrap());
        }
        assert_logical_cells(&values, &cells);
        assert_logical_cells(&snapshot, &original);
    }
    for value in &original[..129] {
        values.insert(values.len(), value.clone());
        cells.push(value.clone());
    }
    assert_logical_cells(&values, &cells);
    assert_logical_cells(&snapshot, &original);

    for (rows, columns, by_column) in [
        (1, 127, false),
        (1, 128, false),
        (1, 129, false),
        (2, 65, false),
        (3, 129, false),
        (129, 3, true),
        (257, 1, true),
    ] {
        let mut runtime = HistoricalRuntime::new(&program);
        let mut cells: Vec<_> = (0..rows * columns)
            .map(|index| PineValue::Float(if index % 2 == 0 { -0.0 } else { 0.0 }))
            .collect();
        if by_column {
            for column in 0..columns {
                cells[((127 + column) % rows) * columns + column] = PineValue::Float(1.0);
            }
        } else {
            for row in 0..rows {
                cells[row * columns + (127 + row) % columns] = PineValue::Float(1.0);
            }
        }
        let id = insert_case(
            &mut runtime,
            MatrixElementKind::Float,
            rows,
            columns,
            &cells,
        );
        assert_case(&runtime, id, rows, columns, &cells, true);
        let checkpoint = runtime.clone();
        let original = cells.clone();

        for index in [0, 127, 128, cells.len() - 1] {
            if index >= cells.len() {
                continue;
            }
            cells[index] = PineValue::Na;
            runtime.matrix_store.get_mut(&id).unwrap().values[index] = PineValue::Na;
            assert_case(&runtime, id, rows, columns, &cells, false);
            assert_case(&checkpoint, id, rows, columns, &original, true);
            cells[index] = original[index].clone();
            runtime.matrix_store.get_mut(&id).unwrap().values[index] = original[index].clone();
            assert_case(&runtime, id, rows, columns, &cells, true);
        }

        if rows == 3 && columns == 129 {
            // Earlier complete rows must not let a fused validation/sum pass
            // accept before visiting the last cell of the last row.
            let index = cells.len() - 1;
            for invalid in [f64::NAN, f64::INFINITY, -f64::from_bits(1)] {
                cells[index] = PineValue::Float(invalid);
                runtime.matrix_store.get_mut(&id).unwrap().values[index] =
                    PineValue::Float(invalid);
                assert_case(&runtime, id, rows, columns, &cells, false);
                assert_case(&checkpoint, id, rows, columns, &original, true);
            }
            cells[index] = original[index].clone();
            runtime.matrix_store.get_mut(&id).unwrap().values[index] = original[index].clone();
            assert_case(&runtime, id, rows, columns, &cells, true);
        }

        let mut branch = checkpoint.clone();
        let mut branch_cells = original.clone();
        branch_cells[original.len() - 1] = PineValue::Float(-1.0);
        branch.matrix_store.get_mut(&id).unwrap().values[original.len() - 1] =
            PineValue::Float(-1.0);
        assert_case(&branch, id, rows, columns, &branch_cells, false);
        assert_case(&runtime, id, rows, columns, &original, true);
        assert_case(&checkpoint, id, rows, columns, &original, true);
    }
}
