//! Isolated recovery-cost experiment, not a throughput or restart qualification.
//! Generate actual schema-3 states through Host, then discard all live Prepared
//! identities by JSON serialization. Setup/source compilation is outside timing.
//! Run only in an exclusive window; generation runs AGENTS initial calls plus
//! MAX_SCHEDULER_DELTA_TAIL * CHANGED_PER_DELTA updates. The optional export path
//! must name a new directory; no files are written when it is unset.
use super::*;
use crate::scheduler_delta::{AgentDelta, SchedulerDelta};
use exchange_core::{BotConfig, PersistedAgentKindState, SchedulerMode, SchedulerState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const AGENTS: usize = 1_000;
const CHANGED_PER_DELTA: usize = 64;
const INITIAL_BARS: usize = 32;

fn request(config: &BotConfig, state: &Value, bars: usize) -> Value {
    let candles: Vec<_> = (0..bars)
        .map(|index| {
            let price = 100 + (index * 73 + index * index * 13) % 101;
            json!({"schema_version":1,"open_time_ms":index*1000,"close_time_ms":(index+1)*1000,
            "open_tick":price,"high_tick":price,"low_tick":price,"close_tick":price,
            "volume":1,"quote_volume":price,"trades":1,"is_final":true})
        })
        .collect();
    let last = candles.last().unwrap()["close_tick"].as_u64().unwrap();
    json!({
        "protocol_version":"bot.v1","plugin_id":"pine.strategy","plugin_version":"2.0.0",
        "state_version":1,"seed":config.seed,"config":config.config,"participant":config.participant,
        "state":state,
        "observation":{
            "version":1,"room_id":config.participant.room_id,"instrument_id":"V-BTC-SPOT",
            "instrument":{"tick_size":1},"status":"Running","step":bars,"market_time_ms":bars*1000,
            "book":{"bids":[{"price_tick":last-1,"qty":100}],"asks":[{"price_tick":last+1,"qty":100}]},
            "own_orders":[],"own_account":{"Spot":{"account_id":config.participant.account_id,
                "cash_balance":10000,"position_qty":0,"fees_paid":0,"available_cash":10000,"available_position":0}},
            "bot_market_data":{"interval_ms":1000,"candles":candles,"own_fills":[],"fill_details":[],
                "account_events":[],"private_history_complete":true,"truncated":false}
        }
    })
}

fn next_kind(
    host: &pine_inprocess::Host,
    config: &BotConfig,
    prior: &Value,
    bars: usize,
) -> PersistedAgentKindState {
    let result = host
        .decide_value(&request(config, prior, bars), 30_000)
        .unwrap();
    let state = result["state"].clone();
    assert_eq!(state["broker_schema"], 3);
    assert!(!state["vm_checkpoint"].is_null());
    assert_eq!(state["evaluated_bars"], bars);
    assert!(state["vm_tail"].as_array().unwrap().len() < 32);
    PersistedAgentKindState::Plugin {
        plugin_id: "pine.strategy".into(),
        plugin_version: "2.0.0".into(),
        state_version: 1,
        data: Arc::new(state),
    }
}

fn payload(kind: &PersistedAgentKindState) -> &Value {
    let PersistedAgentKindState::Plugin { data, .. } = kind else {
        unreachable!()
    };
    data
}

fn encoded_record(seq: u64, mutation: RoomMutation) -> Vec<u8> {
    serde_json::to_vec(&JournalMutation {
        room_id: "cadence-recovery-bench".into(),
        mutation_seq: seq,
        command_cursor: 0,
        schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
        mutation,
    })
    .unwrap()
}

// Export only after exact recovery and cold-session continuation checks pass.
// Each file uses create_new; the manifest and its hash are completion markers.
fn export_recovery_fixture(
    directory: &std::path::Path,
    wire: &[Vec<u8>],
    expected_99: &[u8],
    expected_final: &[u8],
    fixture_sha256: &str,
) {
    use std::io::Write;

    fn write_new(directory: &std::path::Path, name: &str, bytes: &[u8]) -> Value {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))})
    }

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("mutations.jsonl"))
        .unwrap();
    let mut writer = std::io::BufWriter::new(file);
    let mut hash = Sha256::new();
    let mut bytes = 0usize;
    for record in wire {
        writer.write_all(record).unwrap();
        writer.write_all(b"\n").unwrap();
        hash.update(record);
        hash.update(b"\n");
        bytes += record.len() + 1;
    }
    writer.flush().unwrap();
    writer.get_ref().sync_all().unwrap();
    let mutations = json!({"bytes":bytes,"sha256":format!("{:x}",hash.finalize()),
        "records":wire.len(),"line_ending":"LF"});
    let short = write_new(directory, "expected-scheduler-99.json", expected_99);
    let bounded = write_new(
        directory,
        "expected-scheduler-bounded-tail.json",
        expected_final,
    );
    let manifest = json!({
        "fixture_version":1,
        "experiment":"scheduler_recovery_fixture",
        "room_id":"cadence-recovery-bench",
        "agents":AGENTS,"changed_per_delta":CHANGED_PER_DELTA,
        "short_tail":99,"bounded_tail":MAX_SCHEDULER_DELTA_TAIL,
        "base_mutation_seq":1,"final_mutation_seq":wire.len(),"command_cursor":0,
        "fixture_sha256":fixture_sha256,
        "fixture_sha256_encoding":"concatenated compact JSON records without separators",
        "files":{
            "mutations.jsonl":mutations,
            "expected-scheduler-99.json":short,
            "expected-scheduler-bounded-tail.json":bounded,
        },
        "validation":{"exact_json_equal":true,"cold_host_followup_agents":[0,1,2,997,998,999]},
        "scope":"actual Host schema3 payloads; synthetic scheduler sequence; no database or gateway validation",
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let manifest_hash = write_new(directory, "manifest.json", &manifest_bytes);
    let sha = manifest_hash["sha256"].as_str().unwrap();
    write_new(
        directory,
        "manifest.sha256",
        format!("{sha}  manifest.json\n").as_bytes(),
    );
    println!(
        "{}",
        json!({"experiment":"scheduler_recovery_fixture_export",
        "directory":directory.display().to_string(),"manifest_sha256":sha,
        "tail":MAX_SCHEDULER_DELTA_TAIL,"records":wire.len()})
    );
}

#[test]
#[ignore = "exclusive recovery experiment: 1000 real schema3 agents, MAX_SCHEDULER_DELTA_TAIL x 64 changed states"]
fn scheduler_cadence_recovery_1000_pine_bounded_tail() {
    let repeats = std::env::var("MARKETFORGE_RECOVERY_BENCH_REPEATS")
        .map_or(3, |value| value.parse::<usize>().unwrap())
        .clamp(1, 10);
    const { assert!(MAX_SCHEDULER_DELTA_TAIL >= 99) };
    let export_directory = std::env::var_os("MARKETFORGE_RECOVERY_BENCH_EXPORT_DIR").map(|path| {
        let path = std::path::PathBuf::from(path);
        assert!(
            !path.as_os_str().is_empty(),
            "fixture export path must not be empty"
        );
        std::fs::create_dir(&path).expect(
            "fixture export requires a new directory in an existing parent; never overwrite",
        );
        path
    });
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../bot-plugins/pine-strategy");
    let host = pine_inprocess::Host::load(&directory)
        .unwrap()
        .with_checkpoint_mode(true);
    let manifest: Value =
        serde_json::from_str(include_str!("../../bot-plugins/pine-strategy/bot.json")).unwrap();
    let defaults: serde_json::Map<_, _> = manifest["bot"]["parameters"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value["default"].clone()))
        .collect();
    let configs: Vec<BotConfig> = (0..AGENTS).map(|index| {
        let mut config = Value::Object(defaults.clone());
        config["bar_interval_ms"] = 1000.into();
        config["script"] = ["ma.pine", "rsi.pine", "breakout.pine"][index % 3].into();
        serde_json::from_value(json!({
            "participant":{"participant_id":format!("pine-{index}"),"room_id":"cadence-recovery-bench",
                "account_id":10000+index,"instrument_id":"V-BTC-SPOT","kind":"RuleAgent"},
            "plugin_id":"pine.strategy","plugin_version":"2.0.0","config_version":1,"state_version":1,
            "seed":7,"config":config,
        })).unwrap()
    }).collect();
    let mut current = SchedulerState::new(
        "cadence-recovery-bench",
        configs.iter().cloned().map(AgentTemplate::Plugin).collect(),
        SchedulerMode::Auto { interval_ms: 1_000 },
    );
    let setup_started = Instant::now();
    for (agent, config) in current.agents.iter_mut().zip(&configs) {
        agent.kind_state = next_kind(&host, config, &Value::Null, INITIAL_BARS);
    }
    let mut wire = vec![encoded_record(
        1,
        RoomMutation::SchedulerProgress {
            clock_steps: 0,
            state: current.clone(),
            training: None,
        },
    )];
    let mut evaluated = vec![INITIAL_BARS; AGENTS];
    let mut expected_99 = Vec::new();
    let mut append_changes = 0usize;
    let mut full_agent_changes = 0usize;
    for turn in 0..MAX_SCHEDULER_DELTA_TAIL as usize {
        let mut changes = Vec::with_capacity(CHANGED_PER_DELTA);
        for offset in 0..CHANGED_PER_DELTA {
            let index = (turn * CHANGED_PER_DELTA + offset) % AGENTS;
            evaluated[index] += 1;
            changes.push(AgentDelta {
                index,
                kind_state: next_kind(
                    &host,
                    &configs[index],
                    payload(&current.agents[index].kind_state),
                    evaluated[index],
                ),
                unfinished_actions: Vec::new(),
            });
        }
        changes.sort_by_key(|change| change.index);
        let mut metadata = SchedulerDelta::metadata(&current);
        metadata.revision += 1;
        let delta = SchedulerDelta {
            version: 1,
            base_revision: current.revision,
            agent_count: AGENTS,
            state: metadata,
            changes,
            pine_changes: Vec::new(),
        }
        .into_durable(&current)
        .unwrap();
        assert_eq!(
            delta.changes.len() + delta.pine_changes.len(),
            CHANGED_PER_DELTA
        );
        append_changes += delta.pine_changes.len();
        full_agent_changes += delta.changes.len(); // Real VM rollover retains a complete new checkpoint.
        wire.push(encoded_record(
            turn as u64 + 2,
            RoomMutation::SchedulerDelta {
                clock_steps: 0,
                delta: delta.clone(),
                training: None,
            },
        ));
        delta.apply_owned(&mut current).unwrap();
        if turn == 98 {
            expected_99 = serde_json::to_vec(&current).unwrap();
        }
    }
    let expected_final = serde_json::to_vec(&current).unwrap();
    let setup_ms = setup_started.elapsed().as_secs_f64() * 1000.0;
    assert!(append_changes > 0 && full_agent_changes > 0);
    let mut fixture_hash = Sha256::new();
    for record in &wire {
        fixture_hash.update(record);
    }
    let fixture_sha256 = format!("{:x}", fixture_hash.finalize());
    println!(
        "{}",
        json!({"experiment":"scheduler_recovery_fixture","agents":AGENTS,
        "tail":MAX_SCHEDULER_DELTA_TAIL,"changed_per_delta":CHANGED_PER_DELTA,
        "append_agent_changes":append_changes,"full_agent_changes":full_agent_changes,
        "wire_bytes":wire.iter().map(Vec::len).sum::<usize>(),"full_base_bytes":wire[0].len(),
        "fixture_sha256":fixture_sha256,"setup_ms":setup_ms,
        "scope":"actual Host schema3 payloads; synthetic scheduler sequence; no database or gateway timing"})
    );
    let mut final_recovered = None;
    for tail in [99, MAX_SCHEDULER_DELTA_TAIL as usize] {
        for repetition in 0..repeats {
            let started = Instant::now();
            let recovery = JournalRecovery {
                mutations: wire[..=tail]
                    .iter()
                    .map(|bytes| serde_json::from_slice(bytes).unwrap())
                    .collect(),
                ..JournalRecovery::default()
            };
            let decoded_at = Instant::now();
            let mut recovered = scheduler_states_from_recovery(&recovery).unwrap();
            let restored_at = Instant::now();
            let state = recovered.remove("cadence-recovery-bench").unwrap();
            assert_eq!(
                serde_json::to_vec(&state).unwrap(),
                if tail == 99 {
                    &expected_99
                } else {
                    &expected_final
                }
                .as_slice()
            );
            println!(
                "{}",
                json!({"experiment":"scheduler_recovery_sample","tail":tail,"repetition":repetition,
                "wire_bytes":wire[..=tail].iter().map(Vec::len).sum::<usize>(),
                "decode_ms":decoded_at.duration_since(started).as_secs_f64()*1000.0,
                "apply_ms":restored_at.duration_since(decoded_at).as_secs_f64()*1000.0,
                "total_ms":restored_at.duration_since(started).as_secs_f64()*1000.0,
                "exact_json_equal":true})
            );
            if tail == MAX_SCHEDULER_DELTA_TAIL as usize {
                final_recovered = Some(state);
            }
        }
    }
    // VM seal and cursor must really remain executable after journal recovery.
    // Check one member of each source profile, using cold sessions both times.
    let restored = final_recovered.unwrap();
    for index in [0, 1, 2, 997, 998, 999] {
        host.clear_sessions();
        let expected = host
            .decide_value(
                &request(
                    &configs[index],
                    payload(&current.agents[index].kind_state),
                    evaluated[index] + 1,
                ),
                30_000,
            )
            .unwrap();
        host.clear_sessions();
        let actual = host
            .decide_value(
                &request(
                    &configs[index],
                    payload(&restored.agents[index].kind_state),
                    evaluated[index] + 1,
                ),
                30_000,
            )
            .unwrap();
        assert_eq!(actual, expected);
    }
    if let Some(directory) = export_directory {
        export_recovery_fixture(
            &directory,
            &wire,
            &expected_99,
            &expected_final,
            &fixture_sha256,
        );
    }
}
