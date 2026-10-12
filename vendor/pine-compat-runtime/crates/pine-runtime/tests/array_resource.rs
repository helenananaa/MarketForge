//! Allocation regression checks use per-thread counters, without timing gates.
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
        let _ = MAX_ALLOCATION.try_with(|largest| largest.set(largest.get().max(size)));
        let _ = TOTAL_ALLOCATION.try_with(|total| total.set(total.get().saturating_add(size)));
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

fn join_error_allocation(source: &str) -> (usize, usize) {
    let analysis = analyze_source(&SourceFile::new("bounded_join.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&program);
    let bar = Bar {
        time: 0,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    };
    runtime.append_bar(bar).unwrap();
    MAX_ALLOCATION.set(0);
    TOTAL_ALLOCATION.set(0);
    TRACK.set(true);
    let result = runtime.append_bar(Bar { time: 60000, ..bar });
    TRACK.set(false);
    assert_eq!(
        result.unwrap_err().message,
        "array.join result cannot exceed 40960 characters"
    );
    (MAX_ALLOCATION.get(), TOTAL_ALLOCATION.get())
}

#[test]
fn oversized_join_stops_before_allocating_the_expanded_separator_result() {
    let (largest, total) = join_error_allocation(
        r#"//@version=6
indicator("bounded join", max_bars_back=0)
var values = array.new_string(1000, "x")
var separator = str.repeat("x", 40960)
plot(bar_index > 0 ? str.length(array.join(values, separator)) : 0)
"#,
    );
    // The old result contains 40,920,040 ASCII bytes. These bounds include
    // unrelated per-bar bookkeeping and the evaluated separator string.
    assert!(largest < 256 * 1024, "largest allocation: {largest}");
    assert!(total < 2 * 1024 * 1024, "cumulative allocation: {total}");
}

#[test]
fn oversized_udt_join_does_not_materialize_large_fields_or_an_element_string() {
    let mut source =
        String::from("//@version=6\nindicator(\"bounded UDT join\", max_bars_back=0)\ntype Huge\n");
    for index in 0..64 {
        source.push_str(&format!("    string field{index}\n"));
    }
    source.push_str("var values = array.new<Huge>()\nif barstate.isfirst\n    payload = str.repeat(\"x\", 40960)\n    array.push(values, Huge.new(");
    source.push_str(&vec!["payload"; 64].join(", "));
    source.push_str("))\nplot(bar_index > 0 ? str.length(array.join(values)) : 0)\n");
    let (largest, total) = join_error_allocation(&source);
    // A single old element expands to more than 2.6 MB before the join limit
    // is checked. Borrowing each field keeps both counters bounded instead.
    assert!(largest < 256 * 1024, "largest allocation: {largest}");
    assert!(total < 2 * 1024 * 1024, "cumulative allocation: {total}");
}

#[test]
fn join_accepts_exact_character_limit_with_multibyte_elements_and_separator() {
    let analysis = analyze_source(&SourceFile::new(
        "unicode_join.pine",
        "//@version=6\nindicator(\"unicode join\")\nvalues = array.from(str.repeat(\"界\", 40958), \"💥\")\nplot(str.length(array.join(values, \"é\")))\n",
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&program);
    runtime
        .append_bar(Bar {
            time: 0,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        })
        .unwrap();
    assert_eq!(runtime.result().plots[0].values, [PineValue::Int(40960)]);
}

#[test]
fn binary_search_borrows_large_arrays_and_cross_page_slices_without_materializing_them() {
    let analysis = analyze_source(&SourceFile::new(
        "array_search.pine",
        r#"//@version=6
indicator("borrowed search")
var a = array.new_float(100000, 1)
var window = array.slice(a, 127, 99999)
plot(array.binary_search(a, 1))
plot(array.binary_search_rightmost(a, 1))
plot(array.binary_search_leftmost(window, 1))
plot(array.binary_search_rightmost(window, 1))
plot(array.includes(a, 1) ? 1 : 0)
"#,
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    let mut runtime = HistoricalRuntime::new(&program);
    let bar = Bar {
        time: 0,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    };
    runtime.append_bar(bar).unwrap();
    MAX_ALLOCATION.set(0);
    TRACK.set(true);
    let result = runtime.append_bar(bar);
    TRACK.set(false);
    result.unwrap();
    // A cloned 100,000-element PineValue buffer is several MB. The generous
    // bound permits unrelated runtime bookkeeping while rejecting that copy.
    assert!(
        MAX_ALLOCATION.get() < 64 * 1024,
        "largest allocation: {}",
        MAX_ALLOCATION.get()
    );
    let result = runtime.result();
    for (plot, expected) in result.plots.iter().zip([0, 99999, 0, 99871, 1]) {
        assert_eq!(plot.values.last(), Some(&PineValue::Int(expected)));
    }
}

#[test]
fn varip_sparse_updates_do_not_materialize_the_whole_array_during_restore() {
    let analysis = analyze_source(&SourceFile::new(
        "array_varip.pine",
        r#"//@version=6
indicator("paged varip")
varip a = array.new_float(100000, 0)
array.set(a, 50000, array.get(a, 50000) + close)
plot(array.get(a, 50000))
"#,
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    let mut runtime = RealtimeRuntime::new(&program);
    let bar = Bar {
        time: 0,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    };
    runtime.apply_update(BarUpdate::historical(bar)).unwrap();
    let bar = Bar {
        time: 60_000,
        ..bar
    };
    runtime.apply_update(BarUpdate::forming(bar)).unwrap();
    MAX_ALLOCATION.set(0);
    TRACK.set(true);
    let result = runtime.apply_update(BarUpdate::forming(bar));
    TRACK.set(false);
    result.unwrap();
    assert!(
        MAX_ALLOCATION.get() < 64 * 1024,
        "largest allocation: {}",
        MAX_ALLOCATION.get()
    );
    assert_eq!(
        runtime.result().plots[0].values.last(),
        Some(&PineValue::Float(3.0))
    );
}
