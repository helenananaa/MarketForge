//! Uses the real live-work commits and observation/cache code, with a fixed
//! completion order. This is compiled only for the diagnostic example.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Default)]
pub(crate) struct Session {
    instances: BTreeMap<String, CachedBot>,
    control: Control,
    history_cursor: usize,
    history_digest: Sha256,
    decision_cursor: usize,
}

impl Session {
    pub(crate) async fn step(
        &mut self,
        shared: SharedState,
        room: &str,
    ) -> Result<serde_json::Value, String> {
        commit_realtime_work(
            shared.clone(),
            room.into(),
            Work::Clock(self.control.clone()),
        )
        .await
        .map_err(|(_, body)| body.0.error)?;
        let (rooms, registry, agents) = {
            let app = shared.app.lock_room(room).await;
            let scheduler = app.schedulers.get(room).ok_or("missing scheduler")?;
            let count = MAX_READY_DECISIONS.min(scheduler.agents.len());
            let agents: Vec<_> = scheduler
                .agents
                .iter()
                .cycle()
                .skip(self.decision_cursor)
                .take(count)
                .map(|agent| {
                    let cached = self.instances.remove(agent.template.participant_id());
                    let valid = cached
                        .as_ref()
                        .is_some_and(|c| c.matches(0, agent, &app.bot_registry));
                    let prior = if valid {
                        cached.as_ref().unwrap().expected.clone()
                    } else {
                        agent.clone().into()
                    };
                    (prior, cached.filter(|_| valid))
                })
                .collect();
            if !scheduler.agents.is_empty() {
                self.decision_cursor = (self.decision_cursor + count) % scheduler.agents.len();
            }
            (app.rooms.clone(), app.bot_registry.clone(), agents)
        };
        let mut observations = rooms.observation_batch(room);
        let mut inputs = Vec::with_capacity(agents.len());
        for (prior, cached) in agents {
            let request = match &cached {
                Some(cached) => cached.request.clone(),
                None => Arc::new(ObservationRequest {
                    history: registry
                        .market_data_request(&prior.template)
                        .map_err(|e| e.to_string())?,
                    related: registry
                        .related_instruments(&prior.template)
                        .map_err(|e| e.to_string())?,
                    intervals: registry
                        .additional_intervals(&prior.template)
                        .map_err(|e| e.to_string())?,
                    account_events: registry.account_events(&prior.template),
                    event_filter: registry.event_filter(&prior.template),
                }),
            };
            let mut view = observations
                .bot_observation(
                    prior.requires_instrument().map_err(|e| format!("{e:?}"))?,
                    prior.config().account_id,
                    request.history,
                    &request.related,
                )
                .map_err(|e| format!("{e:?}"))?;
            observations
                .enrich_data(
                    &mut view,
                    request.history,
                    &request.related,
                    &request.intervals,
                    request.account_events,
                    prior.config().account_id,
                )
                .map_err(|e| format!("{e:?}"))?;
            inputs.push((prior, view, cached, request));
        }
        // Keep all observations from the same frozen market version, as run().
        drop(observations);
        drop(rooms);
        let mut wave = Vec::with_capacity(MAX_READY_DECISIONS);
        let mut commits = 0;
        for (prior, view, cached, request) in inputs {
            let id = prior.template.participant_id().to_string();
            let (decision, cached) =
                compute_decision(prior, view, registry.clone(), cached, request, 0)?;
            if let Some(cached) = cached {
                self.instances.insert(id, cached);
            }
            wave.push(decision);
            if wave.len() == MAX_READY_DECISIONS {
                self.commit(&shared, room, std::mem::take(&mut wave))
                    .await?;
                commits += 1;
            }
        }
        if !wave.is_empty() {
            self.commit(&shared, room, wave).await?;
            commits += 1;
        }
        let app = shared.app.lock_room(room).await;
        for execution in app
            .rooms
            .execution_history_from(room, self.history_cursor)
            .map_err(|e| format!("{e:?}"))?
        {
            self.history_digest.update(format!("{execution:?}\n"));
        }
        self.history_cursor = app
            .rooms
            .execution_history_len(room)
            .map_err(|e| format!("{e:?}"))?;
        Ok(
            serde_json::json!({"clock":app.rooms.clock(room).map_err(|e|format!("{e:?}"))?,
            "revision":app.schedulers[room].revision, "next_order_id":app.next_order_id,
            "history_len":self.history_cursor, "history_digest": format!("{:x}", self.history_digest.clone().finalize()), "waves":commits}),
        )
    }

    async fn commit(
        &mut self,
        shared: &SharedState,
        room: &str,
        decisions: Vec<Decision>,
    ) -> Result<(), String> {
        let result = commit_realtime_work(
            shared.clone(),
            room.into(),
            Work::Bots {
                control: self.control.clone(),
                epoch: 0,
                decisions: decisions.clone(),
            },
        )
        .await;
        match result {
            Ok(()) => Ok(()),
            Err((StatusCode::BAD_REQUEST, _)) => {
                for decision in decisions {
                    let result = commit_realtime_work(
                        shared.clone(),
                        room.into(),
                        Work::Bot {
                            control: self.control.clone(),
                            epoch: 0,
                            decision: Box::new(decision),
                        },
                    )
                    .await;
                    if let Err((status, body)) = result {
                        return Err(format!("{status}: {}", body.0.error));
                    }
                }
                Ok(())
            }
            Err((status, body)) => Err(format!("{status}: {}", body.0.error)),
        }
    }
}
