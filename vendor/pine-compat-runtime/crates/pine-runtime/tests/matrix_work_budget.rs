use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime, ResourceLimits,
    public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

const QR_SOURCE: &str = r#"//@version=6
indicator("QR work")
m=matrix.new<float>(8,8,0)
if close>0
    for i=0 to 7
        matrix.set(m,i,i,i+1)
matrix.set(m,0,1,1)
roots=matrix.eigenvalues(m)
plot(array.get(roots,7))
"#;

fn program(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("matrix-work.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(time: i64, close: f64) -> Bar {
    Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

fn matrix_limit(work: u64) -> ResourceLimits {
    ResourceLimits {
        max_matrix_work_per_bar: Some(work),
        ..ResourceLimits::default()
    }
}

#[test]
fn ordinary_qr_succeeds_while_dependent_column_search_exhausts_the_same_allowance() {
    let hir = program(QR_SOURCE);
    // Enough for the input scan and one ordinary cubic QR phase. The full-rank
    // upper-triangular matrix needs no basis search, even on subsequent bars.
    let limit = matrix_limit(8 * 8 + 4 * 8 * 8 * 8);
    let mut ordinary = HistoricalRuntime::new(&hir).with_resource_limits(limit);
    ordinary
        .append_bars(&[bar(0, 1.0), bar(60_000, 2.0)])
        .unwrap();
    assert_eq!(
        ordinary.result().plots[0].values,
        vec![PineValue::Float(8.0); 2]
    );

    // Removing the diagonal leaves a valid rank-one nilpotent matrix. QR now
    // has to complete dependent columns; this extra work must be bounded.
    let mut dependent = HistoricalRuntime::new(&hir).with_resource_limits(limit);
    let error = dependent.append_bar(bar(0, 0.0)).unwrap_err();
    assert!(
        error.message.contains("per-bar matrix work budget"),
        "{}",
        error.message
    );

    let mut control = HistoricalRuntime::new(&hir);
    control.append_bar(bar(0, 0.0)).unwrap();
    assert_eq!(
        control.result().plots[0].values,
        vec![PineValue::Float(0.0)]
    );
}

#[test]
fn jacobi_exhaustion_after_a_rotation_is_an_error_for_all_callers() {
    for (operation, work) in [("eigenvalues", 30), ("eigenvectors", 30), ("pinv", 57)] {
        let hir = program(&format!(
            "//@version=6\nindicator(\"Jacobi work\")\nm=matrix.new<float>(3,3,0)\nmatrix.set(m,0,1,1)\nmatrix.set(m,1,0,1)\nmatrix.set(m,1,2,1)\nmatrix.set(m,2,1,1)\nx=matrix.{operation}(m)\nplot(na(x)?1:0)\n"
        ));
        let mut runtime = HistoricalRuntime::new(&hir).with_resource_limits(matrix_limit(work));
        let error = runtime.append_bar(bar(0, 1.0)).unwrap_err();
        assert!(
            error.message.contains("per-bar matrix work budget"),
            "{operation}: {}",
            error.message
        );
    }
}

#[test]
fn failed_qr_basis_search_preserves_the_realtime_view_and_cached_delta() {
    let hir = program(QR_SOURCE);
    let mut runtime =
        RealtimeRuntime::new(&hir).with_resource_limits(matrix_limit(8 * 8 + 4 * 8 * 8 * 8));
    runtime
        .seed_historical_without_output(&[bar(0, 1.0)])
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(60_000, 1.0)))
        .unwrap();
    let before = public_runtime_result_json(&runtime.result());
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    let delta = runtime.last_changes().cloned();
    let revision = runtime.revision();

    let error = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 0.0)))
        .unwrap_err();
    assert!(
        error.message.contains("per-bar matrix work budget"),
        "{}",
        error.message
    );
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        confirmed
    );
    assert_eq!(runtime.last_changes().cloned(), delta);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.forming_bar_time(), Some(60_000));
    assert_eq!(runtime.last_confirmed_bar_time(), Some(0));
    runtime
        .apply_update(BarUpdate::confirmed(bar(60_000, 2.0)))
        .unwrap();
}
