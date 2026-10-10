//! Explicit opt-in diagnostic support; never part of the default server.
use super::*;

static VIEW_PHASES: Mutex<Vec<[u64; 2]>> = Mutex::new(Vec::new());

pub(crate) fn record_view(auth: Duration, build: Duration) {
    VIEW_PHASES
        .lock()
        .unwrap()
        .push([auth.as_micros() as u64, build.as_micros() as u64]);
}

pub fn app(base_url: String) -> Router {
    let shared = shared_state(AppState::new(base_url));
    let session = Arc::new(AsyncMutex::new(realtime::risk_benchmark::Session::default()));
    let start_state = shared.clone();
    let step_state = shared.clone();
    let store_state = shared.clone();
    let snapshot_state = shared.clone();
    super::app(shared)
        .route(
            "/__benchmark/start",
            post(move || {
                let shared = start_state.clone();
                async move {
                    let mut app = shared.app.lock_room("equal-progress").await;
                    app.rooms.resume_room("equal-progress").unwrap();
                    app.schedulers
                        .get_mut("equal-progress")
                        .unwrap()
                        .bots_enabled = true;
                    Json(serde_json::json!({"started":true}))
                }
            }),
        )
        .route(
            "/__benchmark/step",
            post(move || {
                let shared = step_state.clone();
                let session = session.clone();
                async move {
                    session
                        .lock()
                        .await
                        .step(shared, "equal-progress")
                        .await
                        .map(Json)
                        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))
                }
            }),
        )
        .route(
            "/__benchmark/storage",
            get(move || {
                let shared = store_state.clone();
                async move {
                    let journal = shared.app.metadata().await.journal.clone();
                    journal
                        .benchmark_storage()
                        .await
                        .map(Json)
                        .map_err(api_error_from_journal)
                }
            }),
        )
        .route(
            "/__benchmark/state",
            get(move || {
                let shared = snapshot_state.clone();
                async move {
                    let state = shared.app.read_room("equal-progress").await;
                    Json(
                        serde_json::to_value(
                            state.rooms.simulation_room("equal-progress").unwrap(),
                        )
                        .unwrap(),
                    )
                }
            }),
        )
        .route(
            "/__benchmark/view-phases",
            get(|| async { Json(std::mem::take(&mut *VIEW_PHASES.lock().unwrap())) }),
        )
}
