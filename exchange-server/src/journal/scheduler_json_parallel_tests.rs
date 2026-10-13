use super::*;
use std::cell::Cell;

thread_local! {
    static START_FAILURE: Cell<Option<usize>> = const { Cell::new(None) };
    static PANIC_WORKER: Cell<Option<usize>> = const { Cell::new(None) };
    static STARTED: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn fail_start(worker: usize) -> bool {
    START_FAILURE.get() == Some(worker)
}

pub(super) fn panic_worker(worker: usize) -> bool {
    PANIC_WORKER.get() == Some(worker)
}

pub(super) fn started() {
    STARTED.set(STARTED.get() + 1);
}

struct FaultReset;
impl Drop for FaultReset {
    fn drop(&mut self) {
        START_FAILURE.set(None);
        PANIC_WORKER.set(None);
    }
}

fn full(count: usize) -> RoomMutation {
    let mut state = tests::state();
    let agent = state.agents[2].clone();
    state.agents = (0..count)
        .map(|index| {
            let mut agent = agent.clone();
            let PersistedAgentKindState::Plugin { data, .. } = &mut agent.kind_state else {
                unreachable!()
            };
            *data = Arc::new(serde_json::json!({
                "broker_schema": 3, "evaluated_bars": 35, "instance": index,
                "vm_checkpoint": {"frozen": {"$f64": "3ff0000000000000"}},
                "vm_tail": [{"time": 33000}, {"time": 34000}, {"time": 35000}],
                "vm_tail_intents": [[], [], []],
                "u64": u64::MAX, "i64": i64::MIN, "float": -0.0,
                "quoted": "\\\"雪\n", "null": null, "flag": true,
                "body": "checkpoint-state-".repeat(128)
            }));
            agent
        })
        .collect();
    RoomMutation::SchedulerProgress {
        clock_steps: 1,
        state,
        training: None,
    }
}

fn data_mut(mutation: &mut RoomMutation, index: usize) -> &mut Arc<Value> {
    let RoomMutation::SchedulerProgress { state, .. } = mutation else {
        unreachable!()
    };
    let PersistedAgentKindState::Plugin { data, .. } = &mut state.agents[index].kind_state else {
        unreachable!()
    };
    data
}

fn assert_cache_equal(actual: &Cache, expected: &Cache) {
    assert_eq!(actual.bytes, expected.bytes);
    assert_eq!(actual.clock, expected.clock);
    assert_eq!(actual.recency, expected.recency);
    assert_eq!(actual.entries.len(), expected.entries.len());
    assert!(actual.bytes <= MAX_BYTES);
    assert!(actual.entries.len() <= MAX_ENTRIES);
    for (key, entry) in &actual.entries {
        let old = &expected.entries[key];
        assert_eq!(entry.touched, old.touched);
        assert_eq!(entry.json.get(), old.json.get());
    }
}

fn compare(mutation: &RoomMutation, parallel: &mut Cache, serial: &mut Cache) {
    let expected = encode_with_workers(mutation, serial, 1).unwrap().0;
    let actual = encode_with_workers(mutation, parallel, 4).unwrap().0;
    assert_eq!(actual, expected);
    assert_eq!(actual, RecoveryJson::encode(mutation).unwrap().0);
    let recovered: RoomMutation = serde_json::from_slice(&actual).unwrap();
    assert_eq!(
        serde_json::to_value(recovered).unwrap(),
        serde_json::to_value(mutation).unwrap()
    );
    assert_cache_equal(parallel, serial);
}

#[test]
fn parallel_full_cold_hot_mixed_and_cow_keep_exact_wire_and_lru() {
    let mut mutation = full(96);
    let (mut parallel, mut serial) = (Cache::default(), Cache::default());
    STARTED.set(0);
    compare(&mutation, &mut parallel, &mut serial);
    assert_eq!(STARTED.get(), 4);
    STARTED.set(0);
    compare(&mutation, &mut parallel, &mut serial);
    assert_eq!(STARTED.get(), 0, "hot snapshots do not start workers");

    let old = data_mut(&mut mutation, 0).clone();
    for index in (0..96).step_by(2) {
        Arc::make_mut(data_mut(&mut mutation, index))["edited"] = (index + 1).into();
    }
    assert!(old.get("edited").is_none());
    STARTED.set(0);
    compare(&mutation, &mut parallel, &mut serial);
    assert_eq!(STARTED.get(), 4);
    assert_eq!(
        parallel.get(&old).unwrap().get(),
        serial.get(&old).unwrap().get()
    );
}

#[test]
fn parallel_full_deduplicates_arcs_and_small_misses_stay_serial() {
    let mut mutation = full(96);
    let repeated = data_mut(&mut mutation, 0).clone();
    for index in 1..64 {
        *data_mut(&mut mutation, index) = repeated.clone();
    }
    assert_eq!(missing_sources(&mutation, &Cache::default()).len(), 33);
    STARTED.set(0);
    compare(&mutation, &mut Cache::default(), &mut Cache::default());
    assert_eq!(STARTED.get(), 4);
    *data_mut(&mut mutation, 64) = repeated.clone();
    *data_mut(&mut mutation, 65) = repeated;
    STARTED.set(0);
    compare(&mutation, &mut Cache::default(), &mut Cache::default());
    assert_eq!(STARTED.get(), 0);
}

#[test]
fn parallel_full_eviction_matches_serial_including_unconsumed_hot_entries() {
    let mut mutation = full(MAX_ENTRIES + 64);
    let retained: Vec<_> = (0..MAX_ENTRIES)
        .map(|index| Arc::new(serde_json::json!({"cached": index})))
        .collect();
    let (mut parallel, mut serial) = (Cache::default(), Cache::default());
    for value in &retained {
        parallel.get(value).unwrap();
        serial.get(value).unwrap();
    }
    // This initially hot value is legitimately evicted by earlier new values.
    // Its later visit must use the same serial fallback and original LRU order.
    *data_mut(&mut mutation, 100) = retained[0].clone();
    let duplicate = data_mut(&mut mutation, 0).clone();
    *data_mut(&mut mutation, 200) = duplicate;
    parallel.clock = u64::MAX;
    serial.clock = u64::MAX;
    assert_eq!(missing_sources(&mutation, &parallel).len(), MAX_ENTRIES);
    STARTED.set(0);
    compare(&mutation, &mut parallel, &mut serial);
    assert_eq!(STARTED.get(), 4);
    assert_eq!(parallel.entries.len(), MAX_ENTRIES);
}

#[test]
fn parallel_full_oversized_fragment_uses_serial_without_truncation() {
    let mut writer = BoundedJson {
        bytes: Vec::with_capacity(PREFETCH_FRAGMENT_BYTES),
    };
    writer
        .write_all(&vec![b'x'; PREFETCH_FRAGMENT_BYTES])
        .unwrap();
    assert!(writer.write_all(b"x").is_err());
    assert_eq!(writer.bytes.len(), PREFETCH_FRAGMENT_BYTES);
    let mut mutation = full(40);
    let value = Arc::new(Value::String("雪\\\"".repeat(PREFETCH_FRAGMENT_BYTES / 4)));
    assert!(bounded_fragment(&value).is_none());
    *data_mut(&mut mutation, 3) = value;
    compare(&mutation, &mut Cache::default(), &mut Cache::default());
}

#[test]
fn parallel_full_byte_capacity_evicts_in_serial_order() {
    let mutation = full(40);
    let large = Arc::new(Value::String("x".repeat(MAX_BYTES - 512)));
    let mut serial = Cache::default();
    serial.get(&large).unwrap();
    assert!(serial.bytes > MAX_BYTES - PREFETCH_FRAGMENT_BYTES);
    let mut parallel = serial.clone();
    compare(&mutation, &mut parallel, &mut serial);
    assert!(!parallel.contains(&large));
}

#[test]
fn parallel_full_start_failure_or_panicked_worker_falls_back_and_joins() {
    let _reset = FaultReset;
    let mutation = full(128);
    for failure in [0, 2, 3] {
        START_FAILURE.set(Some(failure));
        STARTED.set(0);
        compare(&mutation, &mut Cache::default(), &mut Cache::default());
        assert_eq!(STARTED.get(), failure);
    }
    START_FAILURE.set(None);
    for worker in [0, 1, 3] {
        PANIC_WORKER.set(Some(worker));
        compare(&mutation, &mut Cache::default(), &mut Cache::default());
    }
    PANIC_WORKER.set(None);
    compare(&mutation, &mut Cache::default(), &mut Cache::default());
}

#[test]
fn parallel_full_invalid_typed_integer_is_rejected_before_journal_append() {
    use crate::journal::{InMemoryJournalStore, JournalStore, PendingJournalMutation};
    let recipe: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/microstructure_market.json"
    ))
    .unwrap();
    let mut training = exchange_core::TrainingRun::new(
        exchange_core::TrainingSpec::low_slippage_buy(
            "cache-training",
            recipe.scenario,
            recipe.agents,
            20,
            1,
            7,
            100,
        )
        .unwrap(),
    );
    training.fees_paid = i128::from(u64::MAX) + 1;
    let mut mutation = full(64);
    let RoomMutation::SchedulerProgress {
        state,
        training: value,
        ..
    } = &mut mutation
    else {
        unreachable!()
    };
    let room = state.room_id.clone();
    *value = Some(Box::new(training));
    let mut cache = Cache::default();
    STARTED.set(0);
    assert!(encode_with_workers(&mutation, &mut cache, 4).is_err());
    assert_eq!(STARTED.get(), 4);
    assert!(RecoveryJson::encode(&mutation).is_err());
    let mut store = InMemoryJournalStore::new();
    let pending = PendingJournalMutation::new(room.clone(), 0, mutation);
    assert!(
        store
            .append_room_mutation(&pending, &[], &[], None)
            .is_err()
    );
    assert!(
        store
            .load_room_recovery(&room)
            .unwrap()
            .mutations
            .is_empty()
    );
    // The same store can still accept and recover the next valid mutation.
    let valid = PendingJournalMutation::new(room.clone(), 0, full(64));
    store.append_room_mutation(&valid, &[], &[], None).unwrap();
    assert_eq!(store.load_room_recovery(&room).unwrap().mutations.len(), 1);
}
