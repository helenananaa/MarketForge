//! Windows-only paired CPU samples for synchronous BOT_TURN scopes. Cycles
//! must not be converted to time; FILETIME's 100ns unit is not its precision.
use std::ffi::c_void;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
impl FileTime {
    fn ticks(&self) -> u64 {
        (u64::from(self.high) << 32) | u64::from(self.low)
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThread() -> *mut c_void;
    fn QueryThreadCycleTime(thread: *mut c_void, cycles: *mut u64) -> i32;
    fn GetThreadTimes(
        thread: *mut c_void,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Reading {
    cycles: u64,
    user_100ns: u64,
    kernel_100ns: u64,
}

fn read_current_thread() -> Result<Reading, ()> {
    let mut cycles = 0;
    let (mut creation, mut exit, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: The pseudo handle refers to this calling OS thread and needs no
    // closing. All output pointers name initialized, correctly sized local
    // storage and remain live for these synchronous Win32 calls. No values
    // are interpreted unless both BOOL return values report success.
    unsafe {
        let thread = GetCurrentThread();
        if QueryThreadCycleTime(thread, &mut cycles) == 0
            || GetThreadTimes(thread, &mut creation, &mut exit, &mut kernel, &mut user) == 0
        {
            return Err(());
        }
    }
    Ok(Reading {
        cycles,
        user_100ns: user.ticks(),
        kernel_100ns: kernel.ticks(),
    })
}

#[derive(Default)]
struct Counters {
    paired_count: AtomicU64,
    wall_nanos: AtomicU64,
    cycles: AtomicU64,
    user_100ns: AtomicU64,
    kernel_100ns: AtomicU64,
    read_failure_count: AtomicU64,
    nonmonotonic_count: AtomicU64,
    overflow_count: AtomicU64,
}

impl Counters {
    fn record(
        &self,
        before: Result<Reading, ()>,
        after: Result<Reading, ()>,
        wall: Option<Duration>,
    ) {
        let (Ok(before), Ok(after)) = (before, after) else {
            self.read_failure_count.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let (Some(cycles), Some(user), Some(kernel), Some(wall)) = (
            after.cycles.checked_sub(before.cycles),
            after.user_100ns.checked_sub(before.user_100ns),
            after.kernel_100ns.checked_sub(before.kernel_100ns),
            wall,
        ) else {
            self.nonmonotonic_count.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Ok(wall) = u64::try_from(wall.as_nanos()) else {
            self.overflow_count.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.wall_nanos.fetch_add(wall, Ordering::Relaxed);
        self.cycles.fetch_add(cycles, Ordering::Relaxed);
        self.user_100ns.fetch_add(user, Ordering::Relaxed);
        self.kernel_100ns.fetch_add(kernel, Ordering::Relaxed);
        self.paired_count.fetch_add(1, Ordering::Relaxed);
    }
}

static COUNTERS: std::sync::LazyLock<Counters> = std::sync::LazyLock::new(Counters::default);

pub(super) struct TurnCpu {
    before: Result<Reading, ()>,
    started: Instant,
    // A pseudo handle always refers to its *calling* thread. Make accidental
    // movement into a Send future or to another thread a compile-time error.
    _same_thread: PhantomData<Rc<()>>,
}

impl TurnCpu {
    pub(super) fn start() -> Self {
        let started = Instant::now();
        Self {
            before: read_current_thread(),
            started,
            _same_thread: PhantomData,
        }
    }
}

impl Drop for TurnCpu {
    fn drop(&mut self) {
        let after = read_current_thread();
        let ended = Instant::now();
        COUNTERS.record(
            self.before,
            after,
            ended.checked_duration_since(self.started),
        );
    }
}

pub(super) fn append_metrics(body: &mut String) {
    for (suffix, value) in [
        ("paired_samples_total", &COUNTERS.paired_count),
        ("paired_wall_nanos_total", &COUNTERS.wall_nanos),
        ("paired_cycles_total", &COUNTERS.cycles),
        ("paired_user_100ns_total", &COUNTERS.user_100ns),
        ("paired_kernel_100ns_total", &COUNTERS.kernel_100ns),
        ("read_failures_total", &COUNTERS.read_failure_count),
        ("nonmonotonic_samples_total", &COUNTERS.nonmonotonic_count),
        ("overflow_samples_total", &COUNTERS.overflow_count),
    ] {
        crate::append_prometheus_metric(
            body,
            &format!("marketforge_bench_bot_turn_cpu_{suffix}"),
            "Windows synchronous turn paired samples; cycles are not elapsed time.",
            "counter",
            value.load(Ordering::Relaxed),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(counters: &Counters) -> [u64; 8] {
        [
            &counters.paired_count,
            &counters.wall_nanos,
            &counters.cycles,
            &counters.user_100ns,
            &counters.kernel_100ns,
            &counters.read_failure_count,
            &counters.nonmonotonic_count,
            &counters.overflow_count,
        ]
        .map(|counter| counter.load(Ordering::Relaxed))
    }

    fn reading() -> Reading {
        Reading {
            cycles: 100,
            user_100ns: 20,
            kernel_100ns: 10,
        }
    }

    #[test]
    fn thread_cpu_pairs_only_successful_monotonic_samples_including_real_zero() {
        let counters = Counters::default();
        counters.record(
            Ok(reading()),
            Ok(Reading {
                cycles: 137,
                user_100ns: 23,
                kernel_100ns: 12,
            }),
            Some(Duration::from_nanos(900)),
        );
        assert_eq!(totals(&counters), [1, 900, 37, 3, 2, 0, 0, 0]);
        counters.record(Ok(reading()), Ok(reading()), Some(Duration::ZERO));
        assert_eq!(totals(&counters), [2, 900, 37, 3, 2, 0, 0, 0]);
    }

    #[test]
    fn thread_cpu_failed_reads_never_become_successful_zero_samples() {
        let counters = Counters::default();
        for (before, after) in [
            (Err(()), Ok(reading())),
            (Ok(reading()), Err(())),
            (Err(()), Err(())),
        ] {
            counters.record(before, after, Some(Duration::from_nanos(900)));
        }
        assert_eq!(totals(&counters), [0, 0, 0, 0, 0, 3, 0, 0]);
    }

    #[test]
    fn thread_cpu_any_backwards_field_or_wall_rejects_the_whole_pair() {
        let counters = Counters::default();
        for after in [
            Reading {
                cycles: 99,
                ..reading()
            },
            Reading {
                user_100ns: 19,
                ..reading()
            },
            Reading {
                kernel_100ns: 9,
                ..reading()
            },
        ] {
            counters.record(Ok(reading()), Ok(after), Some(Duration::from_nanos(900)));
        }
        counters.record(Ok(reading()), Ok(reading()), None);
        assert_eq!(totals(&counters), [0, 0, 0, 0, 0, 0, 4, 0]);
        counters.record(
            Ok(reading()),
            Ok(reading()),
            Some(Duration::from_secs(u64::MAX)),
        );
        assert_eq!(totals(&counters), [0, 0, 0, 0, 0, 0, 4, 1]);
    }

    #[test]
    fn thread_cpu_guard_cannot_be_sent_between_os_threads() {
        trait AmbiguousIfSend<A> {
            fn probe() {}
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {}
        impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
        // Adding Send to TurnCpu makes inference ambiguous and fails compilation.
        let _ = <TurnCpu as AmbiguousIfSend<_>>::probe;
    }

    #[test]
    fn thread_cpu_windows_current_thread_api_is_readable() {
        assert!(read_current_thread().is_ok(), "actual Win32 query failed");
    }
}
