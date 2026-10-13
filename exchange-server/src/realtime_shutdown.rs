//! Stop-only drain. No new jobs or writes are admitted by this function.
use super::*;

pub(super) async fn drain(
    writer: &mut tokio::task::JoinSet<Result<CommitCompletion, String>>,
    decisions: &mut tokio::task::JoinSet<Vec<DecisionResult>>,
    completed_results: &mut VecDeque<DecisionResult>,
    ready_decisions: &mut VecDeque<(u64, Decision)>,
    mut discard: impl FnMut(Option<u64>, &str),
) -> Result<(), String> {
    let mut errors = Vec::new();
    // Work already handed to the ordered writer keeps its true durable or
    // failed disposition. It must never receive a shutdown discard too.
    while let Some(result) = writer.join_next().await {
        match result {
            Ok(Ok(completed)) => errors.extend(
                completed
                    .errors
                    .into_iter()
                    .map(|(id, error)| format!("bot {id}: {error}")),
            ),
            Ok(Err(error)) => errors.push(error),
            Err(error) => errors.push(error.to_string()),
        }
    }
    for (_, decision) in ready_decisions.drain(..) {
        discard(decision.trace_id, "worker_shutdown");
    }
    // JoinSet::drop does not stop a started spawn_blocking call. Explicitly
    // await it, then give every emitted decision its terminal disposition.
    while let Some(result) = decisions.join_next().await {
        match result {
            Ok(results) => completed_results.extend(results),
            Err(error) => errors.push(error.to_string()),
        }
    }
    for (id, _, result, _, _) in completed_results.drain(..) {
        match result {
            Ok((decision, _)) => discard(decision.trace_id, "worker_shutdown"),
            Err(error) => errors.push(format!("bot {id}: {error}")),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "trace shutdown drain failed: {}",
            errors.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decision(id: u64) -> Decision {
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let state = exchange_core::SchedulerState::new(
            "trace-drain",
            spec.agents,
            exchange_core::SchedulerMode::Auto { interval_ms: 1_000 },
        );
        let prior: SharedAgent = state.agents[0].clone().into();
        Decision {
            next: prior.kind_state.clone(),
            prior,
            actions: Arc::default(),
            state_hint: None,
            trace_id: Some(id),
        }
    }
    fn completed(id: u64) -> DecisionResult {
        (
            format!("bot-{id}"),
            0,
            Ok((decision(id), None)),
            false,
            None,
        )
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn trace_shutdown_waits_for_writer_and_started_blocking_job_before_discarding() {
        let mut writer = tokio::task::JoinSet::new();
        let (commit_send, commit_wait) = tokio::sync::oneshot::channel();
        writer.spawn(async move {
            commit_wait.await.unwrap();
            Ok(CommitCompletion {
                clock: false,
                ids: vec!["already-admitted".into()],
                errors: Vec::new(),
            })
        });
        let mut decisions = tokio::task::JoinSet::new();
        let (job_send, job_wait) = std::sync::mpsc::channel();
        let (entered_send, entered_wait) = tokio::sync::oneshot::channel();
        decisions.spawn_blocking(move || {
            entered_send.send(()).unwrap();
            job_wait.recv().unwrap();
            vec![completed(3)]
        });
        entered_wait.await.unwrap();
        let mut completed_results = VecDeque::from([completed(2)]);
        let mut ready = VecDeque::from([(0, decision(1))]);
        let task = tokio::spawn(async move {
            let mut ids = Vec::new();
            drain(
                &mut writer,
                &mut decisions,
                &mut completed_results,
                &mut ready,
                |id, reason| {
                    assert_eq!(reason, "worker_shutdown");
                    ids.extend(id);
                },
            )
            .await
            .unwrap();
            assert!(
                writer.is_empty()
                    && decisions.is_empty()
                    && completed_results.is_empty()
                    && ready.is_empty()
            );
            ids
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        commit_send.send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "started blocking work must still be joined"
        );
        job_send.send(()).unwrap();
        let mut ids = task.await.unwrap();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![1, 2, 3],
            "already-admitted writer work must not receive a discard"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn trace_shutdown_collects_contained_errors_and_caught_panic_after_joining_all_jobs() {
        let mut writer = tokio::task::JoinSet::new();
        writer.spawn(async {
            Ok(CommitCompletion {
                clock: false,
                ids: vec![],
                errors: vec![("rejected".into(), "contained commit error".into())],
            })
        });
        let mut decisions = tokio::task::JoinSet::new();
        let (release, wait) = std::sync::mpsc::channel();
        let (entered, started) = tokio::sync::oneshot::channel();
        decisions.spawn_blocking(move || {
            entered.send(()).unwrap();
            wait.recv().unwrap();
            // Same caught-panic -> ComputedDecision::Err shape as production.
            let result =
                std::panic::catch_unwind(|| -> ComputedDecision { panic!("controlled panic") })
                    .unwrap_or_else(|_| Err("bot panicked".into()));
            vec![("panicked".into(), 0, result, false, None), completed(7)]
        });
        started.await.unwrap();
        let mut pending = VecDeque::from([(
            "failed".into(),
            0,
            Err("contained compute error".into()),
            false,
            None,
        )]);
        let mut ready = VecDeque::new();
        let closeout = tokio::spawn(async move {
            let mut ids = Vec::new();
            let result = drain(
                &mut writer,
                &mut decisions,
                &mut pending,
                &mut ready,
                |id, _| ids.extend(id),
            )
            .await;
            assert!(writer.is_empty() && decisions.is_empty() && pending.is_empty());
            (result.unwrap_err(), ids)
        });
        tokio::task::yield_now().await;
        assert!(
            !closeout.is_finished(),
            "errors must not abandon a started job"
        );
        release.send(()).unwrap();
        let (error, ids) = closeout.await.unwrap();
        for message in [
            "contained commit error",
            "contained compute error",
            "bot panicked",
        ] {
            assert!(error.contains(message), "{error}");
        }
        assert_eq!(ids, vec![7]);
    }
}
