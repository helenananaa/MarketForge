//! Diagnostic-only server: identical public API plus requested-heap counters.
//! Never install these endpoints/allocator in the production server binary.
use axum::{
    Json,
    routing::{get, post},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountedSystem;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

fn allocated(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(size, Ordering::Relaxed);
}

// SAFETY: each operation forwards the pointer/layout unchanged to System;
// counters use no allocation and only account successful allocation requests.
unsafe impl GlobalAlloc for CountedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(pointer, layout);
        }
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            if size >= layout.size() {
                allocated(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: CountedSystem = CountedSystem;

async fn heap() -> Json<serde_json::Value> {
    let live = LIVE.load(Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed);
    let calls = ALLOCS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);
    Json(
        serde_json::json!({"live_requested_bytes": live, "peak_requested_bytes": peak,
        "allocation_calls": calls, "allocated_growth_bytes": bytes}),
    )
}

async fn reset_peak() -> Json<serde_json::Value> {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    heap().await
}

#[tokio::main]
async fn main() {
    let bind = std::env::args()
        .nth(1)
        .expect("loopback bind address required");
    assert!(bind.starts_with("127.0.0.1:"));
    let app = exchange_server::risk_benchmark::app(format!("http://{bind}"))
        .route("/__benchmark/heap", get(heap))
        .route("/__benchmark/reset-peak", post(reset_peak));
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
