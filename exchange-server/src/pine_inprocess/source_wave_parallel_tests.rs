use super::*;
use std::sync::{Condvar, mpsc};
use std::time::Duration;

#[test]
fn unrelated_scripts_can_read_concurrently_but_keep_one_snapshot_per_key() {
    let checks = Arc::new(Checks::default());
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered, reads) = mpsc::channel();
    std::thread::scope(|scope| {
        for name in ["first", "second"] {
            let checks = checks.clone();
            let release = release.clone();
            let entered = entered.clone();
            scope.spawn(move || {
                let _scope = enter_checks(checks);
                assert!(verified(1, name, || {
                    entered.send(name).unwrap();
                    let (lock, cv) = &*release;
                    let (ready, _) = cv
                        .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(3), |r| !*r)
                        .unwrap();
                    assert!(*ready, "another source read was serialized behind this one");
                    true
                }));
                assert!(verified(1, name, || panic!("read the same snapshot twice")));
            });
        }
        let first = reads.recv_timeout(Duration::from_secs(2));
        let second = reads.recv_timeout(Duration::from_secs(2));
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
        assert!(
            first.is_ok() && second.is_ok(),
            "unrelated reads must both enter before either finishes"
        );
        assert_ne!(first.unwrap(), second.unwrap());
    });
}
