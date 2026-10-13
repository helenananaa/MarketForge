//! Opt-in elapsed timing for the shared observation history *cell*, excluding
//! the separate history-directory/public-view/event-stamp locks. A scope is
//! recorded only after unlock; intervals can cross measurement snapshots.
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) enum Site {
    Prefilter,
    CheckpointWindow,
    MarketData,
    AccountEvents,
    WaveClone,
}

const NAMES: [&str; 5] = [
    "prefilter",
    "checkpoint_window",
    "market_data",
    "account_events",
    "wave_clone",
];
static COUNTERS: [Counter; 5] = [const { Counter::new() }; 5];

struct Counter {
    count: AtomicU64,
    wait_nanos: AtomicU64,
    held_nanos: AtomicU64,
}

impl Counter {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            wait_nanos: AtomicU64::new(0),
            held_nanos: AtomicU64::new(0),
        }
    }

    fn record(&self, wait_nanos: u64, held_nanos: u64) {
        self.wait_nanos.fetch_add(wait_nanos, Ordering::Relaxed);
        self.held_nanos.fetch_add(held_nanos, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.count.load(Ordering::Relaxed),
            self.wait_nanos.load(Ordering::Relaxed),
            self.held_nanos.load(Ordering::Relaxed),
        )
    }
}

/// (site, completed guards, cumulative wait nanoseconds, held nanoseconds).
/// Reads are deliberately nonblocking, not a coherent cross-counter snapshot.
pub fn snapshot() -> [(&'static str, u64, u64, u64); 5] {
    std::array::from_fn(|i| {
        let (count, wait, held) = COUNTERS[i].snapshot();
        (NAMES[i], count, wait, held)
    })
}

pub(crate) fn lock<T>(mutex: &Mutex<T>, site: Site) -> Guard<'_, 'static, T> {
    lock_counted(mutex, &COUNTERS[site as usize])
}

fn lock_counted<'mutex, 'counter, T>(
    mutex: &'mutex Mutex<T>,
    counter: &'counter Counter,
) -> Guard<'mutex, 'counter, T> {
    let waiting = Instant::now();
    // Preserve the original poison/panic contract.
    let guard = mutex.lock().unwrap();
    let acquired = Instant::now();
    Guard {
        guard: Some(guard),
        wait_nanos: nanos(acquired.duration_since(waiting).as_nanos()),
        acquired,
        counter,
    }
}

fn nanos(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

pub(crate) struct Guard<'mutex, 'counter, T> {
    guard: Option<MutexGuard<'mutex, T>>,
    wait_nanos: u64,
    acquired: Instant,
    counter: &'counter Counter,
}

impl<T> Deref for Guard<'_, '_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_deref().unwrap()
    }
}

impl<T> DerefMut for Guard<'_, '_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_deref_mut().unwrap()
    }
}

impl<T> Drop for Guard<'_, '_, T> {
    fn drop(&mut self) {
        let held_nanos = nanos(self.acquired.elapsed().as_nanos());
        drop(self.guard.take());
        // No metric atomics extend this critical section.
        self.counter.record(self.wait_nanos, held_nanos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_timing_counts_completed_guards_and_keeps_mutation_semantics() {
        let counter = Counter::new();
        let mutex = Mutex::new(7);
        {
            let mut guard = lock_counted(&mutex, &counter);
            *guard += 1;
            assert!(mutex.try_lock().is_err());
            assert_eq!(counter.snapshot(), (0, 0, 0));
        }
        assert_eq!(*mutex.try_lock().unwrap(), 8);
        assert_eq!(counter.snapshot().0, 1);
        fn early_error(mutex: &Mutex<i32>, counter: &Counter) -> Result<(), ()> {
            let _guard = lock_counted(mutex, counter);
            Err(())
        }
        assert!(early_error(&mutex, &counter).is_err());
        assert!(mutex.try_lock().is_ok());
        assert_eq!(counter.snapshot().0, 2);
    }

    #[test]
    fn observation_timing_preserves_poison_and_records_unwound_acquired_guard() {
        let counter = Counter::new();
        let mutex = Mutex::new(());
        assert!(
            std::panic::catch_unwind(|| {
                let _guard = lock_counted(&mutex, &counter);
                panic!("contained fixture");
            })
            .is_err()
        );
        assert_eq!(counter.snapshot().0, 1);
        assert!(mutex.is_poisoned());
        assert!(std::panic::catch_unwind(|| lock_counted(&mutex, &counter)).is_err());
        assert_eq!(counter.snapshot().0, 1);
    }
}
