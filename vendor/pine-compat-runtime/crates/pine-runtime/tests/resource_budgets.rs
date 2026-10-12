use pine_runtime::{
    Bar, BarUpdate, ChartContext, HistoricalRuntime, InMemoryRequestDataProvider, PineValue,
    RealtimeRuntime, RequestEnvironment, RequestKey, RequestTimeframe, ResourceLimits,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;
use std::{mem::size_of, sync::Arc};

fn program(body: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new(
        "resources.pine",
        format!("//@version=6\nindicator(\"resources\")\n{body}\n"),
    ));
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
fn collection_limit(elements: usize) -> ResourceLimits {
    ResourceLimits {
        max_collection_bytes_per_bar: Some(elements * size_of::<PineValue>()),
        ..ResourceLimits::default()
    }
}

#[test]
fn aggregate_allocations_fail_before_another_large_array_is_inserted() {
    let hir = program(
        "float n=0\nfor i=0 to 2\n    a=array.new_float(5000,0)\n    n+=array.size(a)\nplot(n)",
    );
    let mut runtime = HistoricalRuntime::new(&hir).with_resource_limits(collection_limit(10_000));
    let error = runtime.append_bar(bar(0, 1.0)).unwrap_err();
    assert!(
        error.message.contains("E_RESOURCE_BUDGET"),
        "{}",
        error.message
    );
    assert_eq!(runtime.profile().array_values, 10_000);
    assert_eq!(runtime.profile().array_slots, 2);
    assert!(runtime.append_bar(bar(60_000, 1.0)).is_err());
}

#[test]
fn exact_boundary_and_each_new_bar_receive_one_allowance() {
    let hir = program(
        "float n=0\nfor i=0 to 1\n    a=array.new_float(5000,0)\n    n+=array.size(a)\nplot(n)",
    );
    let mut runtime = HistoricalRuntime::new(&hir).with_resource_limits(collection_limit(10_000));
    runtime
        .append_bars(&[bar(0, 1.0), bar(60_000, 2.0)])
        .unwrap();
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Int(10_000); 2]
    );
}

#[test]
fn resource_failure_preserves_realtime_state_and_revision() {
    let hir = program("a=array.new_float(int(close),0)\nplot(array.size(a))");
    let mut live = RealtimeRuntime::new(&hir).with_resource_limits(collection_limit(2));
    live.seed_historical_without_output(&[bar(0, 1.0)]).unwrap();
    live.update_without_output(BarUpdate::forming(bar(60_000, 2.0)))
        .unwrap();
    let before = live.result();
    let revision = live.revision();
    assert!(
        live.update_without_output(BarUpdate::forming(bar(60_000, 3.0)))
            .unwrap_err()
            .message
            .contains("E_RESOURCE_BUDGET")
    );
    assert_eq!(live.revision(), revision);
    assert_eq!(
        pine_runtime::public_runtime_result_json(&live.result()),
        pine_runtime::public_runtime_result_json(&before)
    );
    live.update_without_output(BarUpdate::confirmed(bar(60_000, 2.0)))
        .unwrap();
    assert_eq!(live.resource_limits(), collection_limit(2));
}

#[test]
fn explicit_unlimited_allows_existing_large_collection_workloads() {
    let hir = program("for i=0 to 2\n    a=array.new_float(5000,0)\nplot(close)");
    HistoricalRuntime::new(&hir)
        .with_resource_limits(ResourceLimits {
            max_collection_bytes_per_bar: None,
            max_matrix_work_per_bar: None,
        })
        .append_bar(bar(0, 1.0))
        .unwrap();
}

#[test]
fn allocation_budget_is_shared_with_requested_evaluations() {
    let hir = program(
        "a=array.new_float(2,0)\nvalues=request.security(\"OTHER\",\"1\",array.from(close,close))\nplot(array.size(values))",
    );
    let tf = RequestTimeframe::parse("1").unwrap();
    let mut provider = InMemoryRequestDataProvider::default();
    provider
        .insert(RequestKey::new("OTHER", tf.clone()), vec![bar(0, 1.0)])
        .unwrap();
    let env = RequestEnvironment::new(ChartContext::new("ROOT", tf), Arc::new(provider));
    let mut runtime = HistoricalRuntime::with_request_environment(&hir, env.clone())
        .with_resource_limits(collection_limit(3));
    assert!(
        runtime
            .append_bar(bar(0, 1.0))
            .unwrap_err()
            .message
            .contains("E_RESOURCE_BUDGET")
    );
    // Root allocation, child result, owned request sample and parent import
    // each contain two values and share the same chart execution allowance.
    let mut too_small = HistoricalRuntime::with_request_environment(&hir, env.clone())
        .with_resource_limits(collection_limit(7));
    assert!(
        too_small
            .append_bar(bar(0, 1.0))
            .unwrap_err()
            .message
            .contains("E_RESOURCE_BUDGET")
    );
    let mut exact = HistoricalRuntime::with_request_environment(&hir, env)
        .with_resource_limits(collection_limit(8));
    exact.append_bar(bar(0, 1.0)).unwrap();
    assert_eq!(exact.result().plots[0].values, vec![PineValue::Int(2)]);
}

#[test]
fn matrix_kernels_fail_with_a_work_error_instead_of_returning_na() {
    for operation in ["eigenvalues", "eigenvectors", "pinv", "inv", "det", "rank"] {
        let hir = program(&format!(
            "m=matrix.new<float>(3,3,0)\nmatrix.set(m,0,1,1)\nmatrix.set(m,1,0,1)\nmatrix.set(m,1,2,1)\nmatrix.set(m,2,1,1)\nx=matrix.{operation}(m)\nplot(close)"
        ));
        let mut runtime = HistoricalRuntime::new(&hir).with_resource_limits(ResourceLimits {
            max_matrix_work_per_bar: Some(1),
            ..ResourceLimits::default()
        });
        let error = runtime.append_bar(bar(0, 1.0)).unwrap_err();
        assert!(
            error.message.contains("per-bar matrix work budget"),
            "{operation}: {}",
            error.message
        );
    }
}

#[test]
fn qr_and_jacobi_spend_work_during_iterations() {
    for symmetric in [true, false] {
        let body = format!(
            "m=matrix.new<float>(3,3,0)\nmatrix.set(m,0,1,1)\nmatrix.set(m,1,0,{})\nmatrix.set(m,1,2,1)\nmatrix.set(m,2,1,1)\na=matrix.eigenvalues(m)\nplot(array.get(a,0))",
            if symmetric { 1 } else { 2 }
        );
        let hir = program(&body);
        let mut runtime = HistoricalRuntime::new(&hir).with_resource_limits(ResourceLimits {
            max_matrix_work_per_bar: Some(20),
            ..ResourceLimits::default()
        });
        assert!(
            runtime
                .append_bar(bar(0, 1.0))
                .unwrap_err()
                .message
                .contains("per-bar matrix work budget")
        );
    }
}

#[test]
fn matrix_multiply_and_power_share_the_work_allowance() {
    for operation in ["matrix.mult(m,m)", "matrix.pow(m,8)"] {
        let hir = program(&format!(
            "m=matrix.new<float>(3,3,1)\nx={operation}\nplot(close)"
        ));
        let mut runtime = HistoricalRuntime::new(&hir).with_resource_limits(ResourceLimits {
            max_matrix_work_per_bar: Some(26),
            ..ResourceLimits::default()
        });
        assert!(
            runtime
                .append_bar(bar(0, 1.0))
                .unwrap_err()
                .message
                .contains("per-bar matrix work budget")
        );
    }
}
