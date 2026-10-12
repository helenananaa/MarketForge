use pine_runtime::{
    Bar, BarUpdate, RealtimeRuntime, RuntimeReplica, public_runtime_changes_json,
    public_runtime_result_json, runtime_changes_from_json, runtime_result_from_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn json_snapshots_and_streamed_drawings_preserve_exact_f64_values() {
    let source = SourceFile::new(
        "precision.pine",
        "//@version=6\nindicator(\"precision\")\nplot(close)\nlabel.new(bar_index,close)\nline.new(bar_index,close,bar_index+1,close)\n",
    );
    let hir = analyze_source(&source).hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    let make_bar = |i, close| Bar {
        time: i * 60000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.,
    };
    runtime
        .seed_historical(&[make_bar(0, 96118.61666666665)])
        .unwrap();
    let result = runtime.result();
    let parsed = runtime_result_from_json(&public_runtime_result_json(&result)).unwrap();
    assert_eq!(
        public_runtime_result_json(&parsed),
        public_runtime_result_json(&result),
        "snapshot JSON changed a representable float"
    );
    let mut replica = RuntimeReplica::new(parsed, runtime.revision());
    for (i, value) in [
        117729.23333333331,
        14345.973333333299,
        191031.47999999998,
        -15735.040000000037,
        212642.09666666662,
    ]
    .into_iter()
    .enumerate()
    {
        let bar = make_bar(i as i64 + 1, value);
        for update in [
            BarUpdate::forming(bar),
            BarUpdate::forming(bar),
            BarUpdate::confirmed(bar),
        ] {
            let changes = runtime.apply_update(update).unwrap();
            let decoded =
                runtime_changes_from_json(&public_runtime_changes_json(&changes)).unwrap();
            replica.apply(&decoded).unwrap();
            assert_eq!(
                public_runtime_result_json(replica.result()),
                public_runtime_result_json(&runtime.result()),
                "change JSON altered plotted or drawing coordinates"
            );
        }
    }
}
