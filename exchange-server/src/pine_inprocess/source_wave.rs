//! Private source admission snapshots. Public Host entry points never consult
//! this cache. The opt-in in-process realtime route shares checks while its exact
//! immutable observation wave is alive, including across blocking tasks.
use exchange_core::room::NativeObservationWave;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[derive(Default)]
pub(super) struct Checks(Mutex<BTreeMap<(u64, String), bool>>);

type Waves = Vec<(Weak<NativeObservationWave>, Arc<Checks>)>;
static WAVES: OnceLock<Mutex<Waves>> = OnceLock::new();
thread_local! {
    static CURRENT: RefCell<Option<Arc<Checks>>> = const { RefCell::new(None) };
}

// A thread-local scope must be restored on the thread that entered it.
// Prevent future callers from moving the guard into a different worker/task.
pub(crate) struct Guard {
    previous: Option<Arc<Checks>>,
    same_thread: PhantomData<Rc<()>>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.previous.take());
    }
}

pub(crate) fn enter(wave: &Arc<NativeObservationWave>) -> Guard {
    let mut waves = WAVES.get_or_init(Mutex::default).lock().unwrap();
    waves.retain(|(owner, _)| owner.strong_count() != 0);
    let checks = waves
        .iter()
        .find(|(owner, _)| owner.ptr_eq(&Arc::downgrade(wave)))
        .map(|(_, checks)| checks.clone())
        .unwrap_or_else(|| {
            let checks = Arc::new(Checks::default());
            waves.push((Arc::downgrade(wave), checks.clone()));
            checks
        });
    drop(waves);
    enter_checks(checks)
}

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MARKETFORGE_PINE_SOURCE_WAVE").map_or(true, |value| value == "1")
    })
}

fn enter_checks(checks: Arc<Checks>) -> Guard {
    Guard {
        previous: CURRENT.with(|current| current.replace(Some(checks))),
        same_thread: PhantomData,
    }
}

pub(super) fn verified(host: u64, name: &str, read: impl FnOnce() -> bool) -> bool {
    let current = CURRENT.with(|current| current.borrow().clone());
    let Some(checks) = current else {
        return read();
    };
    // Hold the lock through the first read: concurrent callers must share the
    // same admission snapshot even when the file is edited during this wave.
    let mut checks = checks.0.lock().unwrap();
    *checks.entry((host, name.to_owned())).or_insert_with(read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn snapshots_are_scoped_to_wave_host_and_script_and_cache_rejection() {
        let reads = AtomicUsize::new(0);
        let current = AtomicBool::new(true);
        let read = || {
            reads.fetch_add(1, Ordering::Relaxed);
            current.load(Ordering::Relaxed)
        };
        {
            let _scope = enter_checks(Arc::default());
            assert!(verified(1, "ma", read));
            current.store(false, Ordering::Relaxed);
            assert!(verified(1, "ma", read));
            assert!(!verified(2, "ma", read));
            assert!(!verified(1, "rsi", read));
            current.store(true, Ordering::Relaxed);
            assert!(!verified(1, "rsi", read));
            assert_eq!(reads.load(Ordering::Relaxed), 3);
        }
        assert!(verified(1, "ma", read));
        {
            let _scope = enter_checks(Arc::default());
            current.store(false, Ordering::Relaxed);
            assert!(!verified(1, "ma", read));
        }
        assert_eq!(reads.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn concurrent_tasks_share_one_read_and_unwind_restores_scope() {
        let checks = Arc::new(Checks::default());
        let reads = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..32 {
                let checks = checks.clone();
                let reads = &reads;
                scope.spawn(move || {
                    let _scope = enter_checks(checks);
                    assert!(verified(1, "ma", || {
                        reads.fetch_add(1, Ordering::Relaxed);
                        true
                    }));
                });
            }
        });
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        let _outer = enter_checks(checks);
        let _ = std::panic::catch_unwind(|| {
            let _inner = enter_checks(Arc::default());
            panic!("isolated bot failure");
        });
        assert!(verified(1, "ma", || panic!("outer scope was lost")));
    }

    #[test]
    fn host_wave_snapshot_never_authorizes_a_public_call_or_another_host() {
        let root = std::env::temp_dir().join(format!(
            "mf-source-wave-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let scripts = root.join("scripts");
        std::fs::create_dir(&scripts).unwrap();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("bot-plugins/pine-strategy/scripts/ma.pine");
        let target = scripts.join("ma.pine");
        std::fs::copy(source, &target).unwrap();
        let mut host = super::super::Host::load(&root).unwrap();
        host.wave_source_checks = true;
        let other = super::super::Host::load(&root).unwrap();
        {
            let _scope = enter_checks(Arc::default());
            assert!(host.wave_source("ma.pine").is_some());
            std::fs::write(&target, "edited source").unwrap();
            assert!(host.wave_source("ma.pine").is_some());
            assert!(host.current_source("ma.pine").is_none());
            let config: exchange_core::BotConfig = serde_json::from_value(serde_json::json!({
                "plugin_id":"pine.strategy","plugin_version":"2.0.0","state_version":1,
                "participant":{"participant_id":"id","room_id":"room","account_id":300,"instrument_id":"V-BTC-SPOT","kind":"RuleAgent"},
                "config":{"execution_mode":"broker","script":"ma.pine","hedge_instrument_id":"","inputs":{}}
            })).unwrap();
            let view: exchange_core::ParticipantObservation = serde_json::from_value(serde_json::json!({
                "version":1,"room_id":"room","venue_id":"venue","instrument_id":"V-BTC-SPOT","status":"Running",
                "step":0,"market_time_ms":0,"book":{"bids":[],"asks":[]},"public_trades":[],"own_orders":[],"own_account":null
            })).unwrap();
            let before = host.source_read_count();
            assert_eq!(
                host.decide_borrowed(&config, &serde_json::Value::Null, &view, 5000)
                    .unwrap_err()
                    .0,
                "request is outside the qualified in-process Pine profile"
            );
            assert_eq!(host.source_read_count(), before + 1);
            assert!(other.wave_source("ma.pine").is_none());
        }
        {
            let _scope = enter_checks(Arc::default());
            assert!(host.wave_source("ma.pine").is_none());
        }
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(scripts).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
