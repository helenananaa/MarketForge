//! Heap allocation bounds verify sparse matrix updates without timing gates.
use pine_runtime::{Bar, BarUpdate, PineValue, RealtimeRuntime};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

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

fn assert_sparse_update(source: &str, expected: f64) {
    let analysis = analyze_source(&SourceFile::new("matrix_resource.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    let mut runtime = RealtimeRuntime::new(&program);
    runtime
        .apply_update(BarUpdate::historical(bar(0, 1.0)))
        .unwrap();
    runtime
        .apply_update(BarUpdate::forming(bar(1, 2.0)))
        .unwrap();
    MAX_ALLOCATION.set(0);
    TOTAL_ALLOCATION.set(0);
    TRACK.set(true);
    let update = runtime.apply_update(BarUpdate::forming(bar(1, 3.0)));
    TRACK.set(false);
    update.unwrap();
    assert!(
        MAX_ALLOCATION.get() < 64 * 1024,
        "largest allocation: {}",
        MAX_ALLOCATION.get()
    );
    // The old contiguous payload allocated roughly 2 MiB per checkpoint write.
    // This also prevents replacing it with many equally expensive small copies.
    assert!(
        TOTAL_ALLOCATION.get() < 256 * 1024,
        "total allocation: {}",
        TOTAL_ALLOCATION.get()
    );
    assert_eq!(
        runtime.result().plots[0].values.last(),
        Some(&PineValue::Float(expected))
    );
}

#[test]
fn forming_matrix_set_copies_one_page_and_rolls_back_the_previous_tick() {
    assert_sparse_update(
        "//@version=6\nindicator(\"paged matrix\")\nvar m = matrix.new<float>(256, 256, 0)\nmatrix.set(m, 127, 128, matrix.get(m, 127, 128) + close)\nplot(matrix.get(m, 127, 128))\n",
        4.0,
    );
}

#[test]
fn forming_matrix_reshape_shares_cells_and_preserves_committed_shape() {
    assert_sparse_update(
        "//@version=6\nindicator(\"reshape matrix\")\nvar m = matrix.new<float>(256, 256, 0)\nif barstate.isrealtime\n    matrix.reshape(m, 128, 512)\nplot(matrix.columns(m) * 1.0)\n",
        512.0,
    );
}
