//! Exact full-state asynchronous snapshot prototype and current-protocol audit.
//! It deliberately does not introduce a new durable record kind. The audit
//! demonstrates why late SchedulerProgress cannot safely be used as an anchor.
use super::*;
use crate::scheduler_delta::SchedulerDelta;

fn record(
    state: &exchange_core::SchedulerState,
    seq: u64,
    mutation: RoomMutation,
) -> JournalMutation {
    JournalMutation {
        room_id: state.room_id.clone(),
        mutation_seq: seq,
        command_cursor: 0,
        schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
        mutation,
    }
}

#[test]
#[ignore = "explicit 1000-state async full checkpoint/recovery experiment"]
fn full_snapshot_background_encoding_and_late_record_recovery_audit() {
    let fixture = std::env::var("MARKETFORGE_PINE_STATE_FIXTURE").unwrap();
    let bytes = std::fs::read(&fixture).unwrap();
    let mut state: exchange_core::SchedulerState = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(state.agents.len(), 1038);
    state.revision = 0;
    let mut history = vec![record(
        &state,
        0,
        RoomMutation::SchedulerProgress {
            clock_steps: 0,
            state: state.clone(),
            training: None,
        },
    )];
    let index = state
        .agents
        .iter()
        .position(|agent| agent.template.bot_id() == "pine.strategy")
        .unwrap();
    let advance = |state: &mut exchange_core::SchedulerState,
                   history: &mut Vec<JournalMutation>| {
        let mut next = state.clone();
        next.revision += 1;
        let exchange_core::PersistedAgentKindState::Plugin { data, .. } =
            &mut next.agents[index].kind_state
        else {
            unreachable!()
        };
        Arc::make_mut(data)["_checkpoint_trial"] = serde_json::json!(next.revision);
        let delta = SchedulerDelta::between(state, &next).unwrap();
        history.push(record(
            &next,
            next.revision,
            RoomMutation::SchedulerDelta {
                clock_steps: 0,
                delta,
                training: None,
            },
        ));
        *state = next;
    };
    for _ in 0..8 {
        advance(&mut state, &mut history);
    }
    let clone_started = Instant::now();
    let snapshot = state.clone();
    let clone_ms = clone_started.elapsed().as_secs_f64() * 1000.0;
    let snapshot_revision = snapshot.revision;
    let snapshot_room = snapshot.room_id.clone();
    let (started, received) = std::sync::mpsc::sync_channel(1);
    let encoder = std::thread::spawn(move || {
        started.send(()).unwrap();
        let began = Instant::now();
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        (encoded, began.elapsed().as_secs_f64() * 1000.0)
    });
    received.recv().unwrap();
    let foreground_started = Instant::now();
    for _ in 0..32 {
        advance(&mut state, &mut history);
    }
    let foreground_ms = foreground_started.elapsed().as_secs_f64() * 1000.0;
    let (encoded, encoding_ms) = encoder.join().unwrap();
    let restored: exchange_core::SchedulerState = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored.revision, snapshot_revision);
    assert_eq!(restored.room_id, snapshot_room);
    let exchange_core::PersistedAgentKindState::Plugin { data: frozen, .. } =
        &restored.agents[index].kind_state
    else {
        unreachable!()
    };
    assert_eq!(frozen["_checkpoint_trial"], 8);
    let recovery = JournalRecovery {
        mutations: history.clone(),
        ..Default::default()
    };
    assert_eq!(
        scheduler_states_from_recovery(&recovery).unwrap()[&state.room_id],
        state
    );

    // An independent covering-N anchor works when replay starts at the covered
    // sequence and follows its ordered tail. This is a prototype, not support
    // for such records in the current storage/recovery protocol.
    let mut anchored = restored.clone();
    for record in history
        .iter()
        .filter(|record| record.mutation_seq > snapshot_revision)
    {
        if let RoomMutation::SchedulerDelta { delta, .. } = &record.mutation {
            delta.apply(&mut anchored).unwrap();
        }
    }
    assert_eq!(anchored, state);

    // The proposed "write it later as a full record" shortcut selects the old
    // snapshot as the newest full state and loses all subsequent revisions.
    history.push(record(
        &state,
        state.revision + 1,
        RoomMutation::SchedulerProgress {
            clock_steps: 0,
            state: restored,
            training: None,
        },
    ));
    let unsafe_recovery = JournalRecovery {
        mutations: history,
        ..Default::default()
    };
    let rolled_back = scheduler_states_from_recovery(&unsafe_recovery).unwrap();
    assert_eq!(rolled_back[&state.room_id].revision, snapshot_revision);
    assert_ne!(rolled_back[&state.room_id], state);
    let result = serde_json::json!({"fixture":fixture,"agents":state.agents.len(),"snapshot_revision":snapshot_revision,
        "final_revision":state.revision,"copy_on_write_snapshot_ms":clone_ms,"background_full_encode_ms":encoding_ms,
        "foreground_32_deltas_ms":foreground_ms,"encoded_bytes":encoded.len(),"frozen_snapshot_isolated":true,
        "covering_revision_plus_ordered_tail_equal":true,"late_existing_record_rolls_back_to":snapshot_revision,
        "decision":"reject late SchedulerProgress insertion; requires independently versioned anchor/recovery protocol",
        "scope":"Real saved 1000-Pine state, background full encode and current recovery code; synthetic opaque metadata updates, no trading or capacity claim."});
    if let Ok(path) = std::env::var("MARKETFORGE_ASYNC_CHECKPOINT_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    eprintln!("{result}");
}
