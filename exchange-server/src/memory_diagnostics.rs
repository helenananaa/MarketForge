//! Test-only requested-heap attribution. Allocation origins are separate from
//! retention ownership; the diagnostic also releases owners to measure the latter.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Origin {
    Other,
    Delivery,
    Bot,
    Journal,
}
thread_local! { static ORIGIN: Cell<usize> = const { Cell::new(Origin::Other as usize) }; }

// Reference mode is accessible only to ignored, isolated test diagnostics. Both
// representations are tight; this compares raw JSON against production LZ4 and
// cannot reuse the historical original-spare-capacity versus shrink comparison.
static RAW_MUTATION_STORAGE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub(crate) fn configure_reference() {
    assert!(
        std::env::var_os("MF_BOT_JOURNAL_CAPACITY").is_none(),
        "MF_BOT_JOURNAL_CAPACITY is obsolete; use MF_BOT_JOURNAL_STORAGE=raw or compressed"
    );
    let mode = std::env::var("MF_BOT_JOURNAL_STORAGE").unwrap_or_else(|_| "compressed".into());
    assert!(
        matches!(mode.as_str(), "raw" | "compressed"),
        "invalid journal storage mode"
    );
    RAW_MUTATION_STORAGE.store(mode == "raw", Ordering::Relaxed);
}

pub(crate) fn uses_raw_storage() -> bool {
    RAW_MUTATION_STORAGE.load(Ordering::Relaxed)
}

pub(crate) fn storage_mode() -> &'static str {
    if uses_raw_storage() {
        "raw"
    } else {
        "compressed"
    }
}

pub(crate) struct Scope(usize);
impl Scope {
    pub(crate) fn enter(origin: Origin) -> Self {
        Self(ORIGIN.with(|tag| tag.replace(origin as usize)))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let _ = ORIGIN.try_with(|tag| tag.set(self.0));
    }
}

struct Counter {
    current: AtomicU64,
    peak: AtomicU64,
}
impl Counter {
    const fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        }
    }
    fn add(&self, n: usize) {
        let current = self.current.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
        self.peak.fetch_max(current, Ordering::Relaxed);
    }
    fn remove(&self, n: usize) {
        self.current.fetch_sub(n as u64, Ordering::Relaxed);
    }
}
static COUNTERS: [Counter; 4] = [const { Counter::new() }; 4];
static TOTAL: Counter = Counter::new();
static HEADERS: Counter = Counter::new();
struct Heap;
#[global_allocator]
static HEAP: Heap = Heap;

fn extended(layout: Layout) -> (Layout, usize) {
    let offset = layout.align().max(std::mem::size_of::<usize>());
    (
        Layout::from_size_align(layout.size().checked_add(offset).unwrap(), offset).unwrap(),
        offset,
    )
}

// SAFETY: Each pointer is aligned by extended(), carries its allocation origin
// immediately before its payload, and is returned to System with the same layout.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let (full, offset) = extended(layout);
        let base = unsafe { System.alloc(full) };
        if base.is_null() {
            return base;
        }
        let tag = ORIGIN.try_with(Cell::get).unwrap_or(0);
        let ptr = unsafe { base.add(offset) };
        unsafe {
            ptr.sub(std::mem::size_of::<usize>())
                .cast::<usize>()
                .write(tag);
        }
        COUNTERS[tag].add(layout.size());
        TOTAL.add(layout.size());
        HEADERS.add(offset);
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { self.alloc(layout) };
        if !ptr.is_null() {
            unsafe {
                ptr.write_bytes(0, layout.size());
            }
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let (full, offset) = extended(layout);
        let tag = unsafe { ptr.sub(std::mem::size_of::<usize>()).cast::<usize>().read() };
        COUNTERS[tag].remove(layout.size());
        TOTAL.remove(layout.size());
        HEADERS.remove(offset);
        unsafe {
            System.dealloc(ptr.sub(offset), full);
        }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let (full, offset) = extended(layout);
        let tag = unsafe { ptr.sub(std::mem::size_of::<usize>()).cast::<usize>().read() };
        let base =
            unsafe { System.realloc(ptr.sub(offset), full, size.checked_add(offset).unwrap()) };
        if base.is_null() {
            return base;
        }
        if size >= layout.size() {
            COUNTERS[tag].add(size - layout.size());
            TOTAL.add(size - layout.size());
        } else {
            COUNTERS[tag].remove(layout.size() - size);
            TOTAL.remove(layout.size() - size);
        }
        unsafe { base.add(offset) }
    }
}

pub(crate) fn snapshot() -> serde_json::Value {
    let values: Vec<_> = COUNTERS
        .iter()
        .map(|c| c.current.load(Ordering::Relaxed))
        .collect();
    serde_json::json!({"requested_live_bytes":TOTAL.current.load(Ordering::Relaxed),
        "requested_peak_bytes":TOTAL.peak.load(Ordering::Relaxed),
        "tracker_header_live_bytes":HEADERS.current.load(Ordering::Relaxed),
        "origin_live_bytes":{"other":values[0],"delivery":values[1],"bot":values[2],"journal":values[3]}})
}

#[cfg(test)]
mod probes {
    use super::snapshot;
    use crate::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "isolated Release allocation ownership probe; destructive test store only"]
    async fn native_bot_memory_ownership_probe() {
        super::configure_reference();
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_slice(
            &std::fs::read(std::env::var("MF_FIXED_RECIPE").unwrap()).unwrap(),
        )
        .unwrap();
        let seconds: u64 = std::env::var("MF_MEMORY_SECONDS")
            .unwrap_or("30".into())
            .parse()
            .unwrap();
        assert!((5..=60).contains(&seconds));
        let room = spec.scenario.room_id.clone();
        let bots = spec.agents.len();
        let store = journal::SharedInMemoryJournalStore::new();
        let shared = Arc::new(ServerState::new(AppState::new_with_journal(
            "http://localhost",
            Box::new(store.clone()),
        )));
        let _ = create_room(
            State(shared.clone()),
            HeaderMap::new(),
            Json(serde_json::to_value(spec.scenario).unwrap()),
        )
        .await
        .unwrap();
        let _ = start_agents(
            State(shared.clone()),
            HeaderMap::new(),
            Path(room.clone()),
            Json(StartAgentsRequest {
                agents: spec.agents,
                interval_ms: Some(25),
            }),
        )
        .await
        .unwrap();
        let mut samples = Vec::new();
        let mut elapsed = 0;
        while elapsed < seconds {
            let interval = (seconds - elapsed).min(5);
            tokio::time::sleep(Duration::from_secs(interval)).await;
            elapsed += interval;
            let step = shared
                .app
                .lock_room(&room)
                .await
                .rooms
                .clock(&room)
                .unwrap()
                .step();
            let heap = snapshot();
            let journal = store.memory_diagnostics();
            println!("memory sample elapsed={elapsed} step={step} heap={heap} journal={journal}");
            samples.push(serde_json::json!({"elapsed_seconds":elapsed,"step":step,"heap":heap,"journal":journal}));
        }
        let _ = pause_room(State(shared.clone()), HeaderMap::new(), Path(room.clone()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let status = agent_status_for_room(&*shared.app.lock_room(&room).await, &room);
        assert!(status.bot_errors.is_empty());
        let paused = snapshot();
        let journal = store.memory_diagnostics();
        let workers = std::mem::take(&mut shared.app.lock().await.agent_workers);
        for worker in workers.values() {
            worker.request_stop();
        }
        tokio::task::spawn_blocking(move || {
            for (_, worker) in workers {
                worker.shutdown_and_join();
            }
        })
        .await
        .unwrap();
        let stopped = snapshot();
        // Stop all writers before the destructive ownership experiment. Only
        // this test store is cleared; app room and committed bot state stay alive.
        store.release_mutations_for_memory_probe();
        let without_mutations = snapshot();
        store.release_other_journal_for_memory_probe();
        let without_journal = snapshot();
        drop(shared);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let without_app = snapshot();
        let report = serde_json::json!({"bots":bots,"seconds":seconds,"samples":samples,
            "storage_mode":super::storage_mode(),
            "journal":journal,"paused":paused,"stopped":stopped,"without_mutations":without_mutations,
            "without_journal":without_journal,"without_app":without_app,
            "definition":"requested live heap by allocation origin, excludes allocator slack/stacks; includes test-only kind labels; header overhead separately counted; compares tight raw JSON to production compression; owner release only in an isolated stopped test store"});
        std::fs::write(
            std::env::var("MF_MEMORY_OUTPUT").unwrap(),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        println!("ownership report {report}");
    }
}
