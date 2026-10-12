use pine_ir::HirProgram;
use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime, RuntimeResult,
    public_runtime_result_json, run_historical,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

// The old predicate first copied every convertible numeric cell into a flat
// buffer, rejecting invalid cells before checking either orientation.
fn original_stochastic(rows: usize, columns: usize, cells: &[Option<f64>]) -> bool {
    if cells.is_empty() {
        return false;
    }
    assert_eq!(cells.len(), rows * columns);
    let mut numbers = Vec::with_capacity(cells.len());
    for cell in cells {
        let Some(value) = *cell else {
            return false;
        };
        if !value.is_finite() || value < 0.0 {
            return false;
        }
        numbers.push(value);
    }
    let row_ok = (0..rows).all(|row| {
        numbers[row * columns..(row + 1) * columns]
            .iter()
            .sum::<f64>()
            == 1.0
    });
    let column_ok = (0..columns).all(|column| {
        (0..rows)
            .map(|row| numbers[row * columns + column])
            .sum::<f64>()
            == 1.0
    });
    row_ok || column_ok
}

fn program(source: &str) -> HirProgram {
    let analysis = analyze_source(&SourceFile::new("matrix-stochastic.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("matrix stochastic HIR")
}

fn bar(index: usize, close: f64, volume: f64) -> Bar {
    Bar {
        time: index as i64 * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume,
    }
}

fn bool_number(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

fn assert_outputs(result: &RuntimeResult, expected: &[Vec<f64>]) {
    assert_eq!(result.plots.len(), expected.len());
    for (plot_index, (plot, values)) in result.plots.iter().zip(expected).enumerate() {
        assert_eq!(plot.values.len(), values.len(), "plot {plot_index} length");
        for (index, (actual, expected)) in plot.values.iter().zip(values).enumerate() {
            let PineValue::Float(actual) = actual else {
                panic!("plot {plot_index} at {index}: expected Float, got {actual:?}");
            };
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "plot {plot_index} at {index}"
            );
        }
    }
}

struct Fixture {
    rows: usize,
    columns: usize,
    integer: bool,
    cells: Vec<Option<f64>>,
}

#[test]
fn stochastic_public_rectangles_types_and_invalid_cells_match_old_oracle() {
    let fixtures = [
        Fixture {
            rows: 2,
            columns: 3,
            integer: false,
            cells: vec![
                Some(0.25),
                Some(0.75),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(1.0),
            ],
        },
        Fixture {
            rows: 3,
            columns: 2,
            integer: false,
            cells: vec![
                Some(0.5),
                Some(0.0),
                Some(0.5),
                Some(0.25),
                Some(0.0),
                Some(0.75),
            ],
        },
        Fixture {
            rows: 2,
            columns: 2,
            integer: false,
            cells: vec![Some(1.0), Some(0.0), Some(0.0), Some(1.0)],
        },
        Fixture {
            rows: 2,
            columns: 2,
            integer: true,
            cells: vec![Some(1.0), Some(0.0), Some(0.0), Some(1.0)],
        },
        Fixture {
            rows: 2,
            columns: 2,
            integer: false,
            cells: vec![Some(0.25); 4],
        },
        Fixture {
            rows: 1,
            columns: 2,
            integer: false,
            cells: vec![Some(1.25), Some(-0.25)],
        },
        Fixture {
            rows: 1,
            columns: 1,
            integer: false,
            cells: vec![None],
        },
        Fixture {
            rows: 1,
            columns: 2,
            integer: false,
            cells: vec![Some(-0.0), Some(1.0)],
        },
        Fixture {
            rows: 0,
            columns: 3,
            integer: false,
            cells: vec![],
        },
        Fixture {
            rows: 3,
            columns: 0,
            integer: false,
            cells: vec![],
        },
    ];
    let input: Vec<_> = [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -0.0, 0.0, 1.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| bar(index, close, 1.0))
        .collect();
    let mut expected: Vec<_> = fixtures
        .iter()
        .map(|fixture| {
            vec![
                bool_number(original_stochastic(
                    fixture.rows,
                    fixture.columns,
                    &fixture.cells
                ));
                input.len()
            ]
        })
        .collect();
    expected.push(
        input
            .iter()
            .map(|sample| bool_number(original_stochastic(1, 1, &[Some(sample.close)])))
            .collect(),
    );
    assert_eq!(
        expected.iter().map(|values| values[0]).collect::<Vec<_>>(),
        [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]
    );
    for version in [5, 6] {
        let mut source = format!("//@version={version}\nindicator(\"stochastic cases\")\n");
        for (index, fixture) in fixtures.iter().enumerate() {
            let kind = if fixture.integer { "int" } else { "float" };
            source.push_str(&format!(
                "m{index}=matrix.new<{kind}>({},{},0)\n",
                fixture.rows, fixture.columns
            ));
            for (offset, value) in fixture.cells.iter().enumerate() {
                let value = match value {
                    None => "float(na)".to_owned(),
                    Some(value) if fixture.integer => format!("{}", *value as i64),
                    Some(value) => format!("{value:?}"),
                };
                source.push_str(&format!(
                    "matrix.set(m{index},{},{},{value})\n",
                    offset / fixture.columns,
                    offset % fixture.columns
                ));
            }
            source.push_str(&format!(
                "plot(matrix.is_stochastic(m{index})?1.0:0.0,\"case{index}\")\n"
            ));
        }
        // Builtin close is an unmodified Float; no arithmetic normalizes the
        // nonfinite public input before it reaches the float matrix cell.
        source.push_str("invalid=matrix.new<float>(1,1,close)\nplot(matrix.is_stochastic(invalid)?1.0:0.0,\"invalid\")\n");
        assert_outputs(
            &run_historical(&program(&source), &input).unwrap(),
            &expected,
        );
    }
}

#[test]
fn stochastic_mutations_copies_and_var_history_match_flat_cells() {
    let hir = program(
        "//@version=6\nindicator(\"stochastic copies\")\nvar m=matrix.new<float>(20,17,0.0)\nmatrix.fill(m,0.0)\ncol=16\nfor row=0 to 19\n    matrix.set(m,row,col,1.0)\nbefore=matrix.is_stochastic(m)\nsaved=matrix.copy(m)\nmatrix.set(m,0,col,close)\nafter=matrix.is_stochastic(m)\nplot(before?1.0:0.0)\nplot(after?1.0:0.0)\nplot(matrix.is_stochastic(saved)?1.0:0.0)\nplot(matrix.get(m,0,col))\nplot(matrix.get(saved,0,col))\nplot(after[1]?1.0:0.0)\n",
    );
    let input: Vec<_> = [1.0, 0.0, 0.5, -0.0, 1.0, 2.0, 0.25, 1.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| bar(index, close, 1.0))
        .collect();
    let mut expected: [Vec<f64>; 6] = std::array::from_fn(|_| Vec::new());
    let mut previous = false;
    for sample in &input {
        // 340 cells cross the private payload's 128-cell page boundary.
        let mut cells = vec![Some(0.0); 20 * 17];
        let column = 16;
        for row in 0..20 {
            cells[row * 17 + column] = Some(1.0);
        }
        let saved = cells.clone();
        let before = original_stochastic(20, 17, &cells);
        cells[column] = Some(sample.close);
        let after = original_stochastic(20, 17, &cells);
        let values = [
            bool_number(before),
            bool_number(after),
            bool_number(original_stochastic(20, 17, &saved)),
            sample.close,
            saved[column].unwrap(),
            bool_number(previous),
        ];
        for (plot, value) in expected.iter_mut().zip(values) {
            plot.push(value);
        }
        previous = after;
    }
    let result = run_historical(&hir, &input).unwrap();
    assert_outputs(&result, &expected);
    let mut incremental = HistoricalRuntime::new(&hir);
    incremental.append_bars(&input[..3]).unwrap();
    incremental.append_bars(&input[3..]).unwrap();
    assert_outputs(&incremental.result(), &expected);
    assert_eq!(
        public_runtime_result_json(&result),
        public_runtime_result_json(&incremental.result())
    );
}

#[test]
fn stochastic_same_site_calls_observe_argument_mutations_in_v5_and_v6() {
    let input: Vec<_> = [1.0, 0.0, 0.25, 1.0, 0.75, -0.0]
        .into_iter()
        .enumerate()
        .map(|(index, close)| bar(index, close, 1.0))
        .collect();
    let mut expected: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    for sample in &input {
        let mut cells = vec![
            Some(0.25),
            Some(0.75),
            Some(0.0),
            Some(0.0),
            Some(0.0),
            Some(1.0),
        ];
        let mut count = 0.0;
        for call in 0..4 {
            cells[5] = Some(if call % 2 == 0 { 1.0 } else { sample.close });
            count += bool_number(original_stochastic(2, 3, &cells));
        }
        for (plot, value) in expected.iter_mut().zip([
            count,
            bool_number(original_stochastic(2, 3, &cells)),
            cells[5].unwrap(),
        ]) {
            plot.push(value);
        }
    }
    for version in [5, 6] {
        let hir = program(&format!(
            "//@version={version}\nindicator(\"stochastic argument\")\nchanged(m,x)=>\n    matrix.set(m,1,2,x)\n    m\nvar m=matrix.new<float>(2,3,0.0)\nmatrix.set(m,0,0,0.25)\nmatrix.set(m,0,1,0.75)\ncount=0\nfor i=0 to 3\n    count+=matrix.is_stochastic(changed(m,i%2==0?1.0:close))?1:0\nplot(count*1.0)\nplot(matrix.is_stochastic(m)?1.0:0.0)\nplot(matrix.get(m,1,2))\n"
        ));
        assert_outputs(&run_historical(&hir, &input).unwrap(), &expected);
    }
}

fn advance_cells(cells: &mut [Option<f64>], close: f64) -> [f64; 2] {
    cells[0] = Some(0.25);
    cells[1] = Some(0.75);
    cells[5] = Some(cells[5].unwrap() + close);
    [
        bool_number(original_stochastic(2, 3, cells)),
        cells[5].unwrap(),
    ]
}

fn append_expected(expected: &mut [Vec<f64>], values: [f64; 2]) {
    for (plot, value) in expected.iter_mut().zip(values) {
        plot.push(value);
    }
}

#[test]
fn stochastic_forming_failed_predicate_and_future_commits_are_atomic() {
    let hir = program(
        "//@version=6\nindicator(\"stochastic rollback\")\nvar m=matrix.new<float>(2,3,0.0)\nmatrix.set(m,0,0,0.25)\nmatrix.set(m,0,1,0.75)\nmatrix.set(m,1,2,matrix.get(m,1,2)+close)\nok=matrix.is_stochastic(m)\nif volume<0\n    runtime.error(\"stochastic rollback\")\nplot(ok?1.0:0.0)\nplot(matrix.get(m,1,2))\n",
    );
    let mut input = vec![bar(0, 0.25, 1.0), bar(1, 0.25, 1.0)];
    let mut cells = vec![Some(0.0); 6];
    let mut expected = [Vec::new(), Vec::new()];
    for sample in &input {
        append_expected(&mut expected, advance_cells(&mut cells, sample.close));
    }
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.seed_historical(&input).unwrap();
    assert_outputs(&runtime.result(), &expected);
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    for close in [0.5, 1.5, 0.25] {
        let mut branch = cells.clone();
        let mut visible = expected.clone();
        append_expected(&mut visible, advance_cells(&mut branch, close));
        runtime
            .update_without_output(BarUpdate::forming(bar(2, close, 1.0)))
            .unwrap();
        assert_outputs(&runtime.result(), &visible);
        assert_eq!(runtime.confirmed_bar_count(), 2);
        assert_eq!(
            public_runtime_result_json(&runtime.confirmed_result()),
            confirmed
        );
    }
    let before = public_runtime_result_json(&runtime.result());
    let profile = runtime.profile();
    let confirmed_profile = runtime.confirmed_profile();
    let revision = runtime.revision();
    let changes = runtime.last_changes().cloned();
    let error = runtime
        .apply_update(BarUpdate::forming(bar(2, 0.5, -1.0)))
        .unwrap_err();
    assert_eq!(error.message, "stochastic rollback");
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.confirmed_bar_count(), 2);
    assert_eq!(runtime.profile(), profile);
    assert_eq!(runtime.confirmed_profile(), confirmed_profile);
    assert_eq!(runtime.last_changes(), changes.as_ref());
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        confirmed
    );
    let sample = bar(2, 0.5, 1.0);
    let mut branch = cells.clone();
    let mut visible = expected.clone();
    append_expected(&mut visible, advance_cells(&mut branch, sample.close));
    runtime.apply_update(BarUpdate::forming(sample)).unwrap();
    assert_outputs(&runtime.result(), &visible);
    append_expected(&mut expected, advance_cells(&mut cells, sample.close));
    input.push(sample);
    assert_outputs(
        &runtime.update(BarUpdate::confirmed(sample)).unwrap(),
        &expected,
    );
    for (index, close) in [0.0, -0.5, 0.5].into_iter().enumerate() {
        let sample = bar(index + 3, close, 1.0);
        append_expected(&mut expected, advance_cells(&mut cells, close));
        input.push(sample);
        runtime
            .update_without_output(BarUpdate::confirmed(sample))
            .unwrap();
        assert_outputs(&runtime.result(), &expected);
    }
    assert_eq!(runtime.confirmed_bar_count(), input.len());
    assert_outputs(&runtime.confirmed_result(), &expected);
    assert_eq!(
        public_runtime_result_json(&runtime.result()),
        public_runtime_result_json(&run_historical(&hir, &input).unwrap())
    );
}
