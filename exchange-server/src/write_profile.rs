//! Opt-in diagnostics; disabled by default and never part of durable state.
use serde::Serialize;
use std::{sync::OnceLock, time::Instant};

fn settings() -> Option<u64> {
    static SETTINGS: OnceLock<Option<u64>> = OnceLock::new();
    *SETTINGS.get_or_init(|| {
        (std::env::var("MARKETFORGE_WRITE_PROFILE").as_deref() == Ok("1")).then(|| {
            std::env::var("MARKETFORGE_WRITE_PROFILE_MIN_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10)
        })
    })
}

pub(crate) struct Profile(Option<Inner>);
struct Inner {
    kind: &'static str,
    started: Instant,
    previous: Instant,
    fields: serde_json::Map<String, serde_json::Value>,
    stages: serde_json::Map<String, serde_json::Value>,
    minimum_ms: u64,
}
impl Profile {
    pub(crate) fn enabled() -> bool {
        settings().is_some()
    }

    pub(crate) fn new(kind: &'static str) -> Self {
        Self(settings().map(|minimum_ms| {
            let started = Instant::now();
            Inner {
                kind,
                started,
                previous: started,
                fields: Default::default(),
                stages: Default::default(),
                minimum_ms,
            }
        }))
    }
    pub(crate) fn field(&mut self, key: &str, value: impl Serialize) {
        if let Some(inner) = &mut self.0
            && let Ok(value) = serde_json::to_value(value)
        {
            inner.fields.insert(key.into(), value);
        }
    }
    pub(crate) fn mark(&mut self, stage: &str) {
        if let Some(inner) = &mut self.0 {
            let now = Instant::now();
            inner.stages.insert(
                stage.into(),
                (now.duration_since(inner.previous).as_secs_f64() * 1000.0).into(),
            );
            inner.previous = now;
        }
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        if let Some(inner) = &self.0 {
            let total_ms = inner.started.elapsed().as_secs_f64() * 1000.0;
            if total_ms >= inner.minimum_ms as f64 {
                eprintln!(
                    "write_profile {}",
                    serde_json::json!({
                        "kind":inner.kind,"thread":std::thread::current().name(),
                        "total_ms":total_ms,"fields":inner.fields,"stages_ms":inner.stages,
                    })
                );
            }
        }
    }
}
