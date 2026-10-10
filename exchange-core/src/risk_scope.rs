//! Transaction-local invalidation. Derived state is deliberately not persisted:
//! restored/unknown inputs require a successful full pass before sparse reuse.
use crate::model::AccountId;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub(crate) struct RiskChanges(Option<im::OrdSet<AccountId>>);

impl RiskChanges {
    pub fn mark(&mut self, id: AccountId) {
        if let Some(ids) = &mut self.0
            && !ids.contains(&id)
        {
            ids.insert(id);
        }
    }

    pub fn invalidate(&mut self) {
        self.0 = None;
    }

    pub fn accounts(&self) -> Option<&im::OrdSet<AccountId>> {
        self.0.as_ref()
    }

    pub fn clear(&mut self, id: AccountId) {
        if let Some(ids) = &mut self.0
            && ids.contains(&id)
        {
            ids.remove(&id);
        }
    }

    pub fn reconciled(&mut self, scope: Option<&BTreeSet<AccountId>>) {
        match scope {
            None => self.0 = Some(im::OrdSet::new()),
            Some(scope) => {
                if let Some(ids) = &mut self.0 {
                    for id in scope {
                        ids.remove(id);
                    }
                }
            }
        }
    }
}

pub(crate) fn sparse(changed: usize, population: usize) -> bool {
    changed == 0 || changed < population / 4
}

pub(crate) fn full_scan_reference() -> bool {
    #[cfg(test)]
    return FULL_SCAN.with(|value| value.get());
    #[cfg(not(test))]
    false
}

#[cfg(test)]
thread_local! {
    pub(crate) static FULL_SCAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn reference<T>(operation: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            FULL_SCAN.set(self.0);
        }
    }
    let _reset = Reset(FULL_SCAN.replace(true));
    operation()
}
