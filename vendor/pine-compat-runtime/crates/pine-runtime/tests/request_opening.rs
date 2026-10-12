use std::sync::Arc;

use pine_runtime::{
    Bar, BarUpdate, ChartContext, InMemoryRequestDataProvider, PineValue, RealtimeRuntime,
    RealtimeUpdateContext, RequestEnvironment, RequestKey, RequestTimeframe,
    public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

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

#[test]
fn request_recalculation_after_explicit_open_is_not_another_opening() {
    let source = SourceFile::new(
        "opening.pine",
        "//@version=6\nindicator(\"opening\")\nvarip int calls = 0\ncalls += 1\nplot(barstate.isnew ? 1 : 0)\nplot(calls)\nplot(timenow)\nplot(request.security(\"R\", \"1\", close))\n",
    );
    let hir = analyze_source(&source).hir.unwrap();
    let tf = RequestTimeframe::parse("1").unwrap();
    let key = RequestKey::new("R", tf.clone());
    let provider =
        InMemoryRequestDataProvider::from_streams([(key.clone(), vec![bar(0, 1.0)])]).unwrap();
    let env = RequestEnvironment::new(ChartContext::new("C", tf), Arc::new(provider));
    let mut runtime = RealtimeRuntime::with_request_environment(&hir, env);
    runtime
        .seed_historical_with_execution_times(&[bar(0, 1.0)], &[1_000])
        .unwrap();
    let mut replica = runtime.replica();
    let changes = runtime
        .apply_update_with_context(
            BarUpdate::forming(bar(60_000, 2.0)),
            RealtimeUpdateContext {
                execution_time: Some(61_000),
                opening_update: Some(true),
            },
        )
        .unwrap();
    replica.apply(&changes).unwrap();
    assert_eq!(
        runtime.result().plots[0].values.last(),
        Some(&PineValue::Int(1))
    );
    for update in [
        BarUpdate::forming(bar(60_000, 3.0)),
        BarUpdate::confirmed(bar(60_000, 4.0)),
    ] {
        let changes = runtime
            .apply_request_update(key.clone(), update)
            .unwrap()
            .unwrap();
        replica.apply(&changes).unwrap();
        assert_eq!(runtime.confirmed_bar_count(), 1);
        assert_eq!(
            runtime.result().plots[0].values.last(),
            Some(&PineValue::Int(0))
        );
        assert_eq!(
            runtime.result().plots[2].values.last(),
            Some(&PineValue::Int(61_000))
        );
        assert_eq!(
            public_runtime_result_json(replica.result()),
            public_runtime_result_json(&runtime.result())
        );
    }
    assert_eq!(
        runtime.result().plots[1].values.last(),
        Some(&PineValue::Int(4))
    );
    let before = public_runtime_result_json(&runtime.result());
    assert!(
        runtime
            .apply_update_with_context(
                BarUpdate::forming(bar(60_000, 2.0)),
                RealtimeUpdateContext {
                    execution_time: Some(61_000),
                    opening_update: Some(true)
                }
            )
            .is_err()
    );
    assert_eq!(before, public_runtime_result_json(&runtime.result()));
}
