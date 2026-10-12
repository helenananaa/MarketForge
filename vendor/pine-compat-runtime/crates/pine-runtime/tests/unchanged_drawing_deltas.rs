use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, RuntimeReplica};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn unchanged_drawings_are_omitted_but_preview_retractions_are_preserved() {
    let source = SourceFile::new(
        "drawing deltas.pine",
        "//@version=6\nindicator(\"draw deltas\")\nvar tag=label.new(0,10)\nvar edge=line.new(0,10,1,10)\nif close>20\n    tag.set_y(close)\n    edge.set_y2(close)\nplot(close)\n",
    );
    let hir = analyze_source(&source).hir.unwrap();
    let bar = |time, close| Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.,
    };
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .seed_historical(&[bar(0, 10.), bar(60000, 10.), bar(120000, 10.)])
        .unwrap();
    let mut replica = RuntimeReplica::new(runtime.result(), runtime.revision());
    let unchanged = runtime
        .apply_update(BarUpdate::forming(bar(180000, 10.)))
        .unwrap();
    assert!(unchanged.drawings.is_empty());
    replica.apply(&unchanged).unwrap();
    let changed = runtime
        .apply_update(BarUpdate::forming(bar(180000, 30.)))
        .unwrap();
    assert_eq!(changed.drawings.len(), 2);
    replica.apply(&changed).unwrap();
    let reverted = runtime
        .apply_update(BarUpdate::forming(bar(180000, 10.)))
        .unwrap();
    assert_eq!(
        reverted.drawings.len(),
        2,
        "must retract the previous preview tails"
    );
    replica.apply(&reverted).unwrap();
    assert_eq!(replica.result(), &runtime.result());
    let confirmed = runtime
        .apply_update(BarUpdate::confirmed(bar(180000, 10.)))
        .unwrap();
    assert!(confirmed.drawings.is_empty());
    replica.apply(&confirmed).unwrap();
    assert_eq!(replica.result(), &runtime.result());
}
