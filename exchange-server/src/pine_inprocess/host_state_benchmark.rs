//! Isolated typed-state boundary prototype, not an alternate trading runtime.
//! Include clone and durable Value materialization; scalar-only microbenchmarks
//! would hide the dominant cost of adapting back to the current host contract.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc, time::Instant};

#[derive(Clone, Deserialize, Serialize)]
struct TypedState {
    broker_schema: u64,
    identity: String,
    evaluated_bars: u64,
    last_bar_time: u64,
    execution_seal: String,
    vm_checkpoint: Arc<Value>,
    vm_tail: Arc<Vec<Value>>,
    vm_tail_intents: Arc<Vec<Value>>,
    candle_audits: Arc<BTreeMap<String, Value>>,
    #[serde(flatten)]
    other: BTreeMap<String, Arc<Value>>,
}

// Direct typed encoding removes Value materialization, but requires a host
// accepting this representation. Keep canonical root key order for byte parity.
#[derive(Serialize)]
#[serde(untagged)]
enum Field<'a> {
    Number(u64),
    Text(&'a str),
    Value(&'a Value),
    Array(&'a [Value]),
    Object(&'a BTreeMap<String, Value>),
}

struct Canonical<'a>(&'a TypedState);
impl Serialize for Canonical<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let state = self.0;
        let mut fields: BTreeMap<&str, Field<'_>> = state
            .other
            .iter()
            .map(|(name, value)| (name.as_str(), Field::Value(value)))
            .collect();
        fields.insert("broker_schema", Field::Number(state.broker_schema));
        fields.insert("identity", Field::Text(&state.identity));
        fields.insert("evaluated_bars", Field::Number(state.evaluated_bars));
        fields.insert("last_bar_time", Field::Number(state.last_bar_time));
        fields.insert("execution_seal", Field::Text(&state.execution_seal));
        fields.insert("vm_checkpoint", Field::Value(&state.vm_checkpoint));
        fields.insert("vm_tail", Field::Array(&state.vm_tail));
        fields.insert("vm_tail_intents", Field::Array(&state.vm_tail_intents));
        fields.insert("candle_audits", Field::Object(&state.candle_audits));
        fields.serialize(serializer)
    }
}

#[test]
#[ignore = "explicit fixed-work experiment requires a saved real scheduler fixture"]
fn typed_state_clone_and_durable_boundary_abba() {
    let fixture_path =
        std::env::var("MARKETFORGE_PINE_STATE_FIXTURE").expect("saved scheduler JSON path");
    let input = std::fs::read(&fixture_path).unwrap();
    let scheduler: Value = serde_json::from_slice(&input).unwrap();
    let states: Vec<Value> = scheduler["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|agent| agent["template"]["Plugin"]["plugin_id"] == "pine.strategy")
        .map(|agent| agent["kind_state"]["Plugin"]["data"].clone())
        .collect();
    assert_eq!(states.len(), 1000, "freeze a complete 1000-bot fixture");
    let typed: Vec<TypedState> = states
        .iter()
        .map(|state| TypedState::deserialize(state).unwrap())
        .collect();
    for (state, typed) in states.iter().zip(&typed) {
        let value = serde_json::to_value(typed).unwrap();
        assert_eq!(&value, state);
        assert_eq!(
            serde_json::to_vec(&value).unwrap(),
            serde_json::to_vec(state).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&Canonical(typed)).unwrap(),
            serde_json::to_vec(state).unwrap()
        );
    }
    let mut rows = Vec::new();
    for round in 0..3 {
        for mode in [
            "value",
            "typed",
            "typed",
            "value",
            "value",
            "typed_direct",
            "typed_direct",
            "value",
        ] {
            let start = Instant::now();
            let mut outputs = Vec::with_capacity(states.len());
            if mode == "value" {
                for state in &states {
                    let mut working = state.clone();
                    let next = working["evaluated_bars"].as_u64().unwrap() + 1;
                    working["evaluated_bars"] = serde_json::json!(next);
                    outputs.push(serde_json::to_vec(&working).unwrap());
                }
            } else {
                for state in &typed {
                    let mut working = state.clone();
                    working.evaluated_bars += 1;
                    if mode == "typed_direct" {
                        outputs.push(serde_json::to_vec(&Canonical(&working)).unwrap());
                    } else {
                        let value = serde_json::to_value(&working).unwrap();
                        outputs.push(serde_json::to_vec(&value).unwrap());
                    }
                }
            }
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            for ((state, typed), bytes) in states.iter().zip(&typed).zip(&outputs) {
                let mut expected = state.clone();
                expected["evaluated_bars"] = serde_json::json!(typed.evaluated_bars + 1);
                assert_eq!(bytes, &serde_json::to_vec(&expected).unwrap());
            }
            let row = serde_json::json!({"round":round,"mode":mode,"states":states.len(),
                "elapsed_ms":elapsed_ms,"output_bytes":outputs.iter().map(Vec::len).sum::<usize>(),"byte_equal":true});
            eprintln!("{row}");
            rows.push(row);
            std::hint::black_box(outputs);
        }
    }
    if let Ok(output_path) = std::env::var("MARKETFORGE_PINE_STATE_BENCH_OUTPUT") {
        use sha2::{Digest, Sha256};
        let result = serde_json::json!({"fixture":fixture_path,"fixture_sha256":format!("{:x}",Sha256::digest(&input)),
            "cases":rows,"scope":"Three serial ABBA rounds for typed Value-boundary and direct canonical encoding separately; predecoded typed records; clone + scalar edit + exact durable encode. Direct mode requires a different internal host representation. Excludes VM, gateway, state admission and queueing. Not a complete typed broker architecture."});
        std::fs::write(output_path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
}
