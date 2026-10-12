//! Allocation bounds cover transaction admission and avoid copying old history.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use pine_runtime::{Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

struct TrackingAllocator;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static MAX_ALLOCATION: Cell<usize> = const { Cell::new(0) };
    static TOTAL_ALLOCATION: Cell<usize> = const { Cell::new(0) };
}
fn record(size: usize) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = MAX_ALLOCATION.try_with(|value| value.set(value.get().max(size)));
        let _ = TOTAL_ALLOCATION.try_with(|value| value.set(value.get().saturating_add(size)));
    }
}
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn track<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    MAX_ALLOCATION.set(0);
    TOTAL_ALLOCATION.set(0);
    TRACK.set(true);
    let result = operation();
    TRACK.set(false);
    (result, MAX_ALLOCATION.get(), TOTAL_ALLOCATION.get())
}

fn bar(index: i64) -> Bar {
    Bar {
        time: index * 60_000,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    }
}

fn program(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("replay-resource.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn long_runtime() -> RealtimeRuntime<'static> {
    let mut runtime = RealtimeRuntime::from_program(program(
        "//@version=6\nindicator(\"clock\")\nplot(timenow)\n",
    ));
    let bars: Vec<_> = (0..20_000).map(bar).collect();
    let times: Vec<_> = (0..20_000).collect();
    runtime
        .seed_historical_with_execution_times_without_output(&bars, &times)
        .unwrap();
    runtime
        .apply_update_with_execution_time(BarUpdate::forming(bar(20_000)), 20_000)
        .unwrap();
    runtime
}

#[test]
fn invalid_replay_and_correction_clocks_do_not_copy_existing_history() {
    let mut runtime = long_runtime();
    let revision = runtime.revision();
    let before = runtime.result();
    let (error, largest, total) =
        track(|| runtime.replay_historical_with_execution_times(&[bar(0)], &[1, 2]));
    assert!(
        error
            .unwrap_err()
            .message
            .contains("execution timestamp count")
    );
    assert!(
        largest < 16 * 1024 && total < 32 * 1024,
        "largest={largest}, total={total}"
    );
    let (error, largest, total) = track(|| {
        runtime.correct_historical_with_execution_times(19_999 * 60_000, &[bar(19_999)], &[])
    });
    assert!(
        error
            .unwrap_err()
            .message
            .contains("execution timestamp count")
    );
    assert!(
        largest < 16 * 1024 && total < 32 * 1024,
        "largest={largest}, total={total}"
    );
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.result(), before);
}

#[test]
fn short_replay_does_not_make_a_rollback_copy_of_the_old_session() {
    let mut runtime = long_runtime();
    let (replayed, largest, total) =
        track(|| runtime.replay_historical_with_execution_times(&[bar(0)], &[1234]));
    let replayed = replayed.unwrap();
    // The old chart vector alone was nearly 1 MiB, regardless of the new history.
    assert!(
        largest < 128 * 1024 && total < 256 * 1024,
        "largest={largest}, total={total}"
    );
    assert_eq!(replayed.plots[0].values, vec![PineValue::Int(1234)]);
    assert_eq!(runtime.confirmed_bar_count(), 1);
}

#[test]
fn historical_error_contract_does_not_clone_the_evaluator_before_each_bar() {
    let mut source = "//@version=6\nindicator(\"poison\")\n".to_owned();
    // A persistent collection alone shares its backing pages during a clone.
    // Many live scalar slots make an accidental whole-evaluator copy measurable.
    for index in 0..2000 {
        source.push_str(&format!("var value{index}={index}\n"));
    }
    source.push_str("var values=array.new<float>(65536,0)\nvar n=0\nn+=1\nif close<0\n    runtime.error(\"bad close\")\nplot(n)\nplot(array.size(values))\n");
    let program = program(&source);
    let mut runtime = HistoricalRuntime::new(&program);
    runtime.append_bar(bar(0)).unwrap();
    let (result, largest, total) = track(|| runtime.append_bar(bar(1)));
    result.unwrap();
    assert!(
        largest < 64 * 1024 && total < 128 * 1024,
        "largest={largest}, total={total}"
    );
    let mut invalid = bar(2);
    invalid.close = -1.0;
    let (error, largest, total) = track(|| runtime.append_bar(invalid));
    assert_eq!(error.unwrap_err().message, "bad close");
    assert!(
        largest < 64 * 1024 && total < 128 * 1024,
        "largest={largest}, total={total}"
    );
    assert!(
        runtime
            .append_bar(bar(2))
            .unwrap_err()
            .message
            .starts_with("E_RUNTIME_POISONED:")
    );
}
