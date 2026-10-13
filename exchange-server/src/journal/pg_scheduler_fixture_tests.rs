// Deliberately ignored: imports a large synthetic fixture into a task-only DB.
// Retains its fresh schema for inspection. Does not launch an app or claim an
// actor/gateway restart, exchange equivalence, or PostgreSQL throughput result.
mod pg_scheduler_fixture {
    use super::*;
    use exchange_core::{AgentTemplate, SchedulerState};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::{
        fs::File,
        io::{BufRead, BufReader, Read, Write},
        path::{Path, PathBuf},
    };

    const PREFIX: &str = "mf_bot_throughput_";
    const ROOM: &str = "cadence-recovery-bench";

    fn sha_file(path: &Path) -> (u64, String) {
        let mut file = File::open(path).unwrap();
        let mut hash = Sha256::new();
        let mut total = 0_u64;
        let mut bytes = [0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut bytes).unwrap();
            if count == 0 {
                break;
            }
            total += count as u64;
            hash.update(&bytes[..count]);
        }
        (total, format!("{:x}", hash.finalize()))
    }

    fn sha_json(value: &impl Serialize) -> String {
        struct Writer(Sha256);
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer(Sha256::new());
        serde_json::to_writer(&mut writer, value).unwrap();
        format!("{:x}", writer.0.finalize())
    }

    fn verify_fixture(directory: &Path) -> Value {
        let hash_line = std::fs::read_to_string(directory.join("manifest.sha256")).unwrap();
        let fields = hash_line.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1], "manifest.json");
        let (_, hash) = sha_file(&directory.join("manifest.json"));
        assert_eq!(fields[0], hash);
        let manifest: Value =
            serde_json::from_reader(File::open(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["fixture_version"], 1);
        assert_eq!(manifest["room_id"], ROOM);
        assert_eq!(manifest["command_cursor"], 0);
        assert_eq!(manifest["bounded_tail"], crate::MAX_SCHEDULER_DELTA_TAIL);
        assert_eq!(manifest["agents"], 1000);
        assert_eq!(
            manifest["files"]["mutations.jsonl"]["records"],
            crate::MAX_SCHEDULER_DELTA_TAIL + 1
        );
        for name in [
            "mutations.jsonl",
            "expected-scheduler-99.json",
            "expected-scheduler-bounded-tail.json",
        ] {
            let (bytes, hash) = sha_file(&directory.join(name));
            assert_eq!(
                manifest["files"][name]["bytes"].as_u64(),
                Some(bytes),
                "{name}"
            );
            assert_eq!(
                manifest["files"][name]["sha256"].as_str(),
                Some(hash.as_str()),
                "{name}"
            );
        }
        println!(
            "{}",
            json!({"experiment":"pg_scheduler_fixture_verified", "manifest_sha256":hash,
            "fixture_sha256":manifest["fixture_sha256"], "pid":std::process::id(),
            "resource_sampling":"external process/PG sampling required; this test does not measure peak RSS"})
        );
        manifest
    }

    // Mirrors the existing isolated-schema URL handling. Never inherit a
    // production/default URL and never allow public objects in search_path.
    fn fresh_schema() -> (String, String, postgres::Client) {
        let url = env::var("MARKETFORGE_RECOVERY_FIXTURE_DATABASE_URL")
            .expect("explicit task-only fixture database URL required");
        let expected = env::var("MARKETFORGE_RECOVERY_FIXTURE_EXPECTED_DATABASE")
            .expect("explicit expected task database name required");
        assert!(expected.starts_with(PREFIX) && expected.len() > PREFIX.len());
        let config: postgres::Config = url.parse().unwrap();
        assert_eq!(config.get_dbname(), Some(expected.as_str()));
        let mut admin = postgres::Client::connect(&url, NoTls).unwrap();
        let actual: String = admin
            .query_one("SELECT current_database()", &[])
            .unwrap()
            .get(0);
        assert_eq!(actual, expected);
        assert!(actual.starts_with(PREFIX));
        let suffix: String = admin
            .query_one(
                "SELECT md5(random()::text || clock_timestamp()::text || pg_backend_pid()::text)",
                &[],
            )
            .unwrap()
            .get(0);
        assert!(suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()));
        let schema = format!("mf_scheduler_fixture_{suffix}");
        admin
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .unwrap();
        let options = format!(
            "{} -csearch_path={schema}",
            config.get_options().unwrap_or_default()
        );
        let scoped = if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            let (base, query) = url.split_once('?').unwrap_or((&url, ""));
            let mut params = query
                .split('&')
                .filter(|part| !part.is_empty() && part.split('=').next() != Some("options"))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let encoded: String = options.bytes().map(|byte| format!("%{byte:02X}")).collect();
            params.push(format!("options={encoded}"));
            format!("{base}?{}", params.join("&"))
        } else {
            let escaped = options.replace('\\', "\\\\").replace('\'', "\\'");
            format!("{url} options='{escaped}'")
        };
        let scoped_config: postgres::Config = scoped.parse().unwrap();
        assert_eq!(scoped_config.get_dbname(), Some(expected.as_str()));
        assert_eq!(scoped_config.get_options(), Some(options.as_str()));
        let mut client = postgres::Client::connect(&scoped, NoTls).unwrap();
        let row = client
            .query_one(
                "SELECT current_database(), current_schema(), version()",
                &[],
            )
            .unwrap();
        assert_eq!(row.get::<_, String>(0), expected);
        assert_eq!(row.get::<_, String>(1), schema);
        println!(
            "{}",
            json!({"experiment":"pg_scheduler_fixture_scope", "database":expected,
            "schema":schema, "postgres_version":row.get::<_,String>(2),
            "retained_for_audit":true,"pid":std::process::id(),
            "scope":"journal append/JSONB/state projection and replay only; no app/actor/gateway restart or PG throughput qualification"})
        );
        (scoped, schema, client)
    }

    fn number_changes(
        a: &Value,
        b: &Value,
        path: &str,
        count: &mut usize,
        examples: &mut Vec<Value>,
    ) {
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => {
                if a.to_string() != b.to_string() {
                    *count += 1;
                    if examples.len() < 8 {
                        examples.push(
                            json!({"path":path,"before":a.to_string(),"after":b.to_string()}),
                        );
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "JSONB changed array shape at {path}");
                for (index, (a, b)) in a.iter().zip(b).enumerate() {
                    number_changes(a, b, &format!("{path}/{index}"), count, examples);
                }
            }
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.len(), b.len(), "JSONB changed object shape at {path}");
                for (key, a) in a {
                    let b = b
                        .get(key)
                        .unwrap_or_else(|| panic!("JSONB removed key at {path}/{key}"));
                    number_changes(a, b, &format!("{path}/{key}"), count, examples);
                }
            }
            _ => assert!(a == b, "JSONB changed a nonnumeric value at {path}"),
        }
    }

    struct Expected {
        normalized: SchedulerState,
        raw_sha256: String,
        normalized_sha256: String,
    }

    fn expected_through_jsonb(client: &mut postgres::Client, raw: &SchedulerState) -> Expected {
        // Normalize the expected state through the same PostgreSQL JSONB type,
        // without changing any journal checksum or any recovery validator.
        let encoded = RecoveryJson::encode(raw).unwrap();
        let value: Value = client
            .query_one("SELECT $1::jsonb", &[&encoded])
            .unwrap()
            .get(0);
        let normalized: SchedulerState = serde_json::from_value(value).unwrap();
        let raw_sha256 = sha_json(raw);
        let normalized_sha256 = sha_json(&normalized);
        let mut count = 0;
        let mut examples = Vec::new();
        if raw_sha256 != normalized_sha256 {
            number_changes(
                &serde_json::to_value(raw).unwrap(),
                &serde_json::to_value(&normalized).unwrap(),
                "",
                &mut count,
                &mut examples,
            );
            assert!(count > 0, "unexplained nonnumeric serialization difference");
        }
        println!(
            "{}",
            json!({"experiment":"pg_scheduler_expected_jsonb", "revision":raw.revision,
            "raw_sha256":raw_sha256,"normalized_sha256":normalized_sha256,
            "numeric_representation_changes":count,"examples":examples,
            "checksums":"unchanged production base/patch checksum validation"})
        );
        Expected {
            normalized,
            raw_sha256,
            normalized_sha256,
        }
    }

    fn sample(
        store: &mut PostgresJournalStore,
        expected: &Expected,
        stage: &str,
        mutations: usize,
        runtime_checkpoints: usize,
    ) {
        let started = Instant::now();
        let recovery = store.load_recovery().unwrap();
        let loaded = Instant::now();
        assert_eq!(recovery.rooms.len(), 1);
        assert_eq!(recovery.rooms[0].room_id, ROOM);
        assert!(recovery.executions.is_empty());
        assert_eq!(recovery.mutations.len(), mutations, "{stage}");
        assert_eq!(
            recovery.runtime_checkpoints.len(),
            runtime_checkpoints,
            "{stage}"
        );
        let mut states = crate::scheduler_states_from_recovery(&recovery).unwrap();
        let replayed = Instant::now();
        assert_eq!(states.len(), 1);
        let state = states.remove(ROOM).unwrap();
        let actual_sha256 = sha_json(&state);
        assert!(
            state == expected.normalized,
            "full normalized scheduler mismatch at {stage}; actual {actual_sha256}, expected {}",
            expected.normalized_sha256
        );
        assert_eq!(actual_sha256, expected.normalized_sha256);
        println!(
            "{}",
            json!({"experiment":"pg_scheduler_recovery_sample","stage":stage,
            "load_query_decode_ms":loaded.duration_since(started).as_secs_f64()*1000.0,
            "scheduler_replay_ms":replayed.duration_since(loaded).as_secs_f64()*1000.0,
            "load_and_replay_ms":replayed.duration_since(started).as_secs_f64()*1000.0,
            "comparison_excluded_from_timing":true,"loaded_mutations":recovery.mutations.len(),
            "runtime_checkpoints":recovery.runtime_checkpoints.len(),"snapshots":recovery.snapshots.len(),
            "min_mutation_seq":recovery.mutations.first().map(|record|record.mutation_seq),
            "max_mutation_seq":recovery.mutations.last().map(|record|record.mutation_seq),
            "exact_normalized_state_equal":true,"raw_json_equal":actual_sha256==expected.raw_sha256,
            "actual_sha256":actual_sha256,"raw_expected_sha256":expected.raw_sha256,
            "scheduler_revision":state.revision,"agents":state.agents.len()})
        );
    }

    fn reject_internal_gap(client: &mut postgres::Client, gap: i64, missing_snapshot: bool) {
        let mut tx = client.transaction().unwrap();
        assert_eq!(
            tx.execute(
                "DELETE FROM marketforge_room_mutations WHERE room_id=$1 AND mutation_seq=$2",
                &[&ROOM, &gap]
            )
            .unwrap(),
            1
        );
        if missing_snapshot {
            assert_eq!(
                tx.execute(
                    "DELETE FROM marketforge_room_snapshots WHERE room_id=$1",
                    &[&ROOM]
                )
                .unwrap(),
                1
            );
        }
        // Use the production transaction-level loader so the temporary deletion
        // is visible. A second connection's load_recovery could not see it.
        let recovery = load_postgres_recovery_snapshot(&mut tx, None, true).unwrap();
        assert_eq!(
            recovery.runtime_checkpoints.len(),
            usize::from(!missing_snapshot)
        );
        let error = match crate::scheduler_states_from_recovery(&recovery) {
            Ok(_) => panic!("missing scheduler revision was accepted"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("missing or incompatible predecessor"),
            "{error}"
        );
        tx.rollback().unwrap();
        println!(
            "{}",
            json!({"experiment":"pg_scheduler_gap_rejected","mutation_seq":gap,
            "missing_actor_snapshot":missing_snapshot,"error":error,"rolled_back":true,
            "load_path":"production transaction-level recovery query; fault visible only inside test transaction"})
        );
    }

    #[test]
    #[ignore = "exclusive PG fixture recovery: explicit task DB URL/name and exported bounded-tail fixture required"]
    fn postgres_scheduler_bounded_fixture_recovery() {
        let directory = PathBuf::from(
            env::var_os("MARKETFORGE_RECOVERY_FIXTURE_DIR")
                .expect("exported scheduler fixture directory required"),
        );
        let manifest = verify_fixture(&directory);
        let raw: SchedulerState = serde_json::from_reader(
            File::open(directory.join("expected-scheduler-bounded-tail.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(raw.room_id, ROOM);
        assert_eq!(raw.agents.len(), 1000);
        assert_eq!(raw.revision, crate::MAX_SCHEDULER_DELTA_TAIL);
        let (url, schema, mut client) = fresh_schema();
        let mut store = PostgresJournalStore::connect_migrated(&url).unwrap();
        let row = store
            .client
            .query_one("SELECT current_database(),current_schema()", &[])
            .unwrap();
        assert_eq!(
            row.get::<_, String>(0),
            env::var("MARKETFORGE_RECOVERY_FIXTURE_EXPECTED_DATABASE").unwrap()
        );
        assert_eq!(row.get::<_, String>(1), schema);
        let expected = expected_through_jsonb(&mut client, &raw);
        let account_ids: Vec<_> = raw
            .agents
            .iter()
            .map(|agent| match &agent.template {
                AgentTemplate::Plugin(config) => config.participant.account_id,
                _ => panic!("fixture must contain only Pine plugin agents"),
            })
            .collect();
        let mut scenario = spot_scenario(ROOM);
        scenario.accounts = account_ids
            .iter()
            .map(|id| ScenarioAccount::Spot {
                account_id: *id,
                cash_balance: 10_000,
                position_qty: 0,
            })
            .collect();
        let actor = SimulationRoom::from_scenario(scenario.clone())
            .unwrap()
            .room;
        assert_eq!(actor.next_command_seq(), 0);
        store
            .create_room(
                "fixture-owner",
                &scenario,
                &RoomBootstrap {
                    room_id: ROOM.into(),
                    seed_executions: vec![],
                },
                &account_ids,
                &[],
                None,
            )
            .unwrap();
        let mut reader = BufReader::new(File::open(directory.join("mutations.jsonl")).unwrap());
        let mut line = Vec::new();
        let mut sequence = 0_u64;
        let mut fixture_hash = Sha256::new();
        let import_started = Instant::now();
        let mut append_ms = 0.0;
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line).unwrap() == 0 {
                break;
            }
            assert_eq!(line.pop(), Some(b'\n'));
            fixture_hash.update(&line);
            sequence += 1;
            let record: JournalMutation = serde_json::from_slice(&line).unwrap();
            assert_eq!(record.room_id, ROOM);
            assert_eq!(record.mutation_seq, sequence);
            assert_eq!(record.command_cursor, 0);
            assert_eq!(record.schema_version, ROOM_MUTATION_SCHEMA_VERSION);
            assert!(if sequence == 1 {
                matches!(record.mutation, RoomMutation::SchedulerProgress { .. })
            } else {
                matches!(record.mutation, RoomMutation::SchedulerDelta { .. })
            });
            let pending =
                PendingJournalMutation::new(record.room_id, record.command_cursor, record.mutation);
            let started = Instant::now();
            store
                .append_room_mutation(&pending, &[], &[], None)
                .unwrap();
            append_ms += started.elapsed().as_secs_f64() * 1000.0;
        }
        assert_eq!(sequence, crate::MAX_SCHEDULER_DELTA_TAIL + 1);
        assert_eq!(
            format!("{:x}", fixture_hash.finalize()),
            manifest["fixture_sha256"].as_str().unwrap()
        );
        let row = client.query_one("SELECT COUNT(*),MIN(mutation_seq),MAX(mutation_seq),MIN(pg_typeof(payload_json)::text) FROM marketforge_room_mutations WHERE room_id=$1", &[&ROOM]).unwrap();
        assert_eq!(row.get::<_, i64>(0), sequence as i64);
        assert_eq!(row.get::<_, i64>(1), 1);
        assert_eq!(row.get::<_, i64>(2), sequence as i64);
        assert_eq!(row.get::<_, String>(3), "jsonb");
        println!(
            "{}",
            json!({"experiment":"pg_scheduler_fixture_import","records":sequence,
            "fixture_read_decode_append_ms":import_started.elapsed().as_secs_f64()*1000.0,
            "journal_append_ms":append_ms,"postgres_payload_type":"jsonb","schema":schema})
        );
        drop(reader);
        drop(line);
        // Reconnect to ensure no live Prepared hints can authorize recovery.
        drop(store);
        let mut store = PostgresJournalStore::connect_migrated(&url).unwrap();
        sample(
            &mut store,
            &expected,
            "no_actor_runtime_view",
            sequence as usize,
            0,
        );
        let snapshot = JournalSnapshot {
            room_id: ROOM.into(),
            command_seq: 0,
            actor,
        };
        store.append_snapshot(&snapshot).unwrap();
        sample(
            &mut store,
            &expected,
            "actor_head_runtime_view",
            sequence as usize,
            1,
        );
        let gap = (sequence / 2) as i64;
        reject_internal_gap(&mut client, gap, false);
        reject_internal_gap(&mut client, gap, true);
        // This committed, task-local deletion exercises the public loader's
        // actual missing-snapshot fallback. Restore through the journal API.
        assert_eq!(
            client
                .execute(
                    "DELETE FROM marketforge_room_snapshots WHERE room_id=$1",
                    &[&ROOM]
                )
                .unwrap(),
            1
        );
        sample(
            &mut store,
            &expected,
            "missing_actor_full_journal_fallback",
            sequence as usize,
            0,
        );
        store.append_snapshot(&snapshot).unwrap();
        let mut newer_raw = raw;
        newer_raw.revision += 1;
        let newer_expected = expected_through_jsonb(&mut client, &newer_raw);
        store
            .append_room_mutation(
                &PendingJournalMutation::new(
                    ROOM,
                    0,
                    RoomMutation::SchedulerProgress {
                        clock_steps: 0,
                        state: newer_raw,
                        training: None,
                    },
                ),
                &[],
                &[],
                Some(&snapshot),
            )
            .unwrap();
        sample(
            &mut store,
            &newer_expected,
            "new_full_actor_head_runtime_view",
            1,
            1,
        );
        assert_eq!(
            client
                .execute(
                    "DELETE FROM marketforge_room_snapshots WHERE room_id=$1",
                    &[&ROOM]
                )
                .unwrap(),
            1
        );
        sample(
            &mut store,
            &newer_expected,
            "new_full_missing_actor_fallback",
            sequence as usize + 1,
            0,
        );
        store.append_snapshot(&snapshot).unwrap();
        let row = client.query_one("SELECT pg_total_relation_size('marketforge_room_mutations'),(SELECT COUNT(*) FROM marketforge_room_mutations WHERE room_id=$1),(SELECT COUNT(*) FROM marketforge_runtime_mutations WHERE room_id=$1)", &[&ROOM]).unwrap();
        println!(
            "{}",
            json!({"experiment":"pg_scheduler_fixture_complete","database":env::var("MARKETFORGE_RECOVERY_FIXTURE_EXPECTED_DATABASE").unwrap(),
            "schema":schema,"canonical_rows":row.get::<_,i64>(1),"runtime_view_rows":row.get::<_,i64>(2),
            "mutation_relation_bytes":row.get::<_,i64>(0),"retained_for_audit":true,
            "scope":"PG journal/JSONB/state projection and replay only; no actor/gateway app restart or PG throughput qualification"})
        );
    }
}
