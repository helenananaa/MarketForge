use pine_runtime::{Bar, BarUpdate, RealtimeRuntime};
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
fn new_bar_appends_and_replacements_keep_one_independent_equity_row() {
    let analysis = analyze_source(&SourceFile::new(
        "equity.pine",
        "//@version=6\nstrategy(\"equity\",initial_capital=1000)\nif bar_index==0\n    strategy.entry(\"L\",strategy.long,qty=1)\n",
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    let seed = runtime
        .seed_historical(&[bar(0, 100.0), bar(60000, 100.0)])
        .unwrap()
        .strategy
        .unwrap()
        .equity;
    let first = runtime
        .update(BarUpdate::forming(bar(120000, 120.0)))
        .unwrap()
        .strategy
        .unwrap()
        .equity;
    let replacement = runtime
        .update(BarUpdate::forming(bar(120000, 80.0)))
        .unwrap()
        .strategy
        .unwrap()
        .equity;
    let confirmed = runtime
        .update(BarUpdate::confirmed(bar(120000, 110.0)))
        .unwrap()
        .strategy
        .unwrap()
        .equity;
    assert_eq!(seed.len(), 2);
    for rows in [&first, &replacement, &confirmed] {
        assert_eq!(rows.len(), 3);
        assert_eq!(&rows[..2], seed.as_slice());
        assert_eq!(rows[2].bar_index, 2);
    }
    assert_eq!(first[2].equity, 1020.0);
    assert_eq!(replacement[2].equity, 980.0);
    assert_eq!(confirmed[2].equity, 1010.0);
    assert_eq!(
        runtime.confirmed_result().strategy.unwrap().equity,
        confirmed
    );
}
