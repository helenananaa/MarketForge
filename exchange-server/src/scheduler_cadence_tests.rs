//! Candidate cadence tests use durable checkpoints, including a simulated old
//! database revision, without spending 1,000 background clock ticks per case.
use super::*;
use crate::journal::{JournalStore, ROOM_MUTATION_SCHEMA_VERSION};
use crate::scheduler_delta::SchedulerDelta;

pub(super) async fn seed_durable_revision(shared: &SharedState, room: &str, revision: u64) {
    let mut app = shared.app.lock_room(room).await;
    let mut state = app.schedulers[room].clone();
    state.revision = revision;
    let cursor = next_persisted_command_cursor(&app, room).unwrap();
    let pending = PendingJournalMutation::new(
        room,
        cursor,
        RoomMutation::SchedulerProgress {
            clock_steps: 0,
            state: state.clone(),
            training: None,
        },
    );
    app.append_room_mutation_owned(pending, &[], &[], None)
        .await
        .unwrap();
    app.schedulers.insert(room.to_owned(), state);
}

#[tokio::test]
async fn scheduler_cadence_both_commit_paths_bound_tail_and_keep_actor_checkpoint() {
    for state_only in [false, true] {
        let room = if state_only {
            "cadence-empty"
        } else {
            "cadence-clock"
        };
        let (shared, _, mut store) = setup(room).await;
        let (control, _) = prepare_wave(&shared, room).await;
        seed_durable_revision(&shared, room, MAX_SCHEDULER_DELTA_TAIL - 1).await;
        for expected in [MAX_SCHEDULER_DELTA_TAIL, MAX_SCHEDULER_DELTA_TAIL + 1] {
            let pending = if state_only {
                let prior = shared.app.lock_room(room).await.schedulers[room].agents[0].clone();
                work(
                    &control,
                    &prior,
                    control.epoch.load(Ordering::Acquire),
                    vec![],
                )
            } else {
                realtime::Work::Clock(control.clone())
            };
            let committed = commit(&shared, room, pending).await.unwrap();
            assert_eq!(committed.revision, expected);
            let recovery = store.load_recovery().unwrap();
            assert_eq!(
                scheduler_states_from_recovery(&recovery).unwrap()[room],
                committed
            );
            let last = &recovery.mutations.last().unwrap().mutation;
            assert_eq!(
                matches!(last, RoomMutation::SchedulerProgress { .. }),
                expected == MAX_SCHEDULER_DELTA_TAIL + 1
            );
        }
        if !state_only {
            // The 100th market clock step must still force both actor and full
            // scheduler snapshots, although scheduler revision is not a boundary.
            let initial = shared.lifecycle.metrics_snapshot().checkpoint_writes_total;
            while shared
                .app
                .lock_room(room)
                .await
                .rooms
                .clock(room)
                .unwrap()
                .step()
                < LIVE_SNAPSHOT_INTERVAL_STEPS
            {
                commit(&shared, room, realtime::Work::Clock(control.clone()))
                    .await
                    .unwrap();
            }
            let recovery = store.load_recovery().unwrap();
            assert!(matches!(
                recovery.mutations.last().unwrap().mutation,
                RoomMutation::SchedulerProgress { .. }
            ));
            assert!(shared.lifecycle.metrics_snapshot().checkpoint_writes_total > initial);
            assert_eq!(
                recover_rooms(&recovery)
                    .unwrap()
                    .clock(room)
                    .unwrap()
                    .step(),
                LIVE_SNAPSHOT_INTERVAL_STEPS
            );
        }
        shutdown(&shared).await;
    }
}

fn record(state: &SchedulerState, mutation: RoomMutation) -> JournalMutation {
    JournalMutation {
        room_id: state.room_id.clone(),
        mutation_seq: state.revision + 1,
        command_cursor: 0,
        schema_version: ROOM_MUTATION_SCHEMA_VERSION,
        mutation,
    }
}

#[test]
fn scheduler_cadence_old_hundred_checkpoint_extends_to_new_bound_and_corruption_fails() {
    let room = "legacy-cadence";
    let mut current = SchedulerState::new(
        room,
        vec![template(room, "fast")],
        exchange_core::SchedulerMode::Auto { interval_ms: 1_000 },
    );
    current.revision = 100; // Existing binary's actual full-checkpoint revision.
    let mut recovery = JournalRecovery {
        mutations: vec![record(
            &current,
            RoomMutation::SchedulerProgress {
                clock_steps: 0,
                state: current.clone(),
                training: None,
            },
        )],
        ..JournalRecovery::default()
    };
    for revision in 101..=MAX_SCHEDULER_DELTA_TAIL {
        let mut next = current.clone();
        next.revision = revision;
        next.agents[0].kind_state = PersistedAgentKindState::Plugin {
            plugin_id: "test.live".into(),
            plugin_version: "1".into(),
            state_version: 1,
            data: serde_json::json!({"turn": revision, "observed_price": 90 + revision % 11})
                .into(),
        };
        let delta = SchedulerDelta::between(&current, &next).unwrap();
        recovery.mutations.push(record(
            &next,
            RoomMutation::SchedulerDelta {
                clock_steps: 0,
                delta,
                training: None,
            },
        ));
        current = next;
    }
    let frozen = serde_json::to_vec(&recovery).unwrap();
    let decoded: JournalRecovery = serde_json::from_slice(&frozen).unwrap();
    assert_eq!(
        scheduler_states_from_recovery(&decoded).unwrap()[room],
        current
    );
    let mut reordered = decoded.clone();
    reordered.mutations.reverse();
    assert_eq!(
        scheduler_states_from_recovery(&reordered).unwrap()[room],
        current,
        "unordered storage reads are sorted by durable sequence"
    );
    for index in [1, decoded.mutations.len() / 2, decoded.mutations.len() - 2] {
        let mut missing = decoded.clone();
        missing.mutations.remove(index);
        assert!(scheduler_states_from_recovery(&missing).is_err());
        let mut corrupt = decoded.clone();
        let RoomMutation::SchedulerDelta { delta, .. } = &mut corrupt.mutations[index].mutation
        else {
            unreachable!()
        };
        delta.base_revision += 1;
        assert!(scheduler_states_from_recovery(&corrupt).is_err());
        let mut sequence_swap = decoded.clone();
        let first = sequence_swap.mutations[index].mutation_seq;
        sequence_swap.mutations[index].mutation_seq =
            sequence_swap.mutations[index + 1].mutation_seq;
        sequence_swap.mutations[index + 1].mutation_seq = first;
        assert!(
            scheduler_states_from_recovery(&sequence_swap).is_err(),
            "reordered durable revisions cannot silently install"
        );
    }
    current.revision += 1;
    assert!(scheduler_checkpoint_due(current.revision));
    recovery.mutations.push(record(
        &current,
        RoomMutation::SchedulerProgress {
            clock_steps: 0,
            state: current.clone(),
            training: None,
        },
    ));
    // A committed full checkpoint is a recovery base even if an older scheduler
    // patch is absent. This does not remove any transaction/audit history.
    recovery.mutations.remove(1);
    assert_eq!(
        scheduler_states_from_recovery(&recovery).unwrap()[room],
        current
    );
}
