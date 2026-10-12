use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, RuntimeReplica};
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
fn replica_rejects_stale_gap_conflict_and_schema_without_mutating() {
    let hir = analyze_source(&SourceFile::new(
        "replica.pine",
        "//@version=6\nindicator(\"replica\")\nplot(close)\n",
    ))
    .hir
    .unwrap();
    let mut runtime = RealtimeRuntime::from_program(hir);
    runtime.seed_historical(&[bar(0, 10.0)]).unwrap();
    let mut replica = runtime.replica();
    let a = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 20.0)))
        .unwrap();
    let b = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 30.0)))
        .unwrap();
    let before = replica.result().clone();
    assert!(
        replica
            .apply(&b)
            .unwrap_err()
            .message
            .contains("E_STREAM_GAP")
    );
    assert_eq!(replica.result(), &before);
    assert!(replica.apply(&a).unwrap());
    assert!(!replica.apply(&a).unwrap());
    assert!(replica.apply(&b).unwrap());
    let before = replica.result().clone();
    assert!(
        replica
            .apply(&a)
            .unwrap_err()
            .message
            .contains("E_STREAM_STALE")
    );
    let mut conflict = b.clone();
    conflict.series.clear();
    assert!(
        replica
            .apply(&conflict)
            .unwrap_err()
            .message
            .contains("E_STREAM_CONFLICT")
    );
    let mut wrong_schema = b.clone();
    wrong_schema.schema_version += 1;
    assert!(
        replica
            .apply(&wrong_schema)
            .unwrap_err()
            .message
            .contains("E_STREAM_SCHEMA")
    );
    let mut wrong_base = b.clone();
    wrong_base.base_revision = 0;
    assert!(
        replica
            .apply(&wrong_base)
            .unwrap_err()
            .message
            .contains("E_STREAM_REVISION")
    );
    assert_eq!(replica.result(), &before);
    assert_eq!(replica.revision(), b.revision);
    assert_eq!(replica.result(), &runtime.result());
}

#[test]
fn snapshot_reset_recovers_a_gap_then_resumes_without_replaying_lost_updates() {
    let hir = analyze_source(&SourceFile::new(
        "recover.pine",
        "//@version=6\nindicator(\"recover\")\nvarip int count = 0\ncount += 1\nplot(count)\n",
    ))
    .hir
    .unwrap();
    let mut runtime = RealtimeRuntime::from_program(hir);
    let seed = runtime.seed_historical(&[bar(0, 10.0)]).unwrap();
    let mut replica = RuntimeReplica::new(seed, runtime.revision());
    runtime
        .apply_update(BarUpdate::forming(bar(60_000, 20.0)))
        .unwrap();
    let skipped = runtime
        .apply_update(BarUpdate::forming(bar(60_000, 30.0)))
        .unwrap();
    assert!(replica.apply(&skipped).is_err());
    replica.reset(runtime.result(), runtime.revision());
    let next = runtime
        .apply_update(BarUpdate::confirmed(bar(60_000, 30.0)))
        .unwrap();
    assert!(replica.apply(&next).unwrap());
    assert_eq!(replica.result(), &runtime.confirmed_result());
    assert!(!replica.apply(&next).unwrap());
}
