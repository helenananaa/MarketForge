#![cfg(feature = "pine-in-process")]

use pine_runtime::HistoricalRuntime;
use pine_runtime::{
    Bar, ChartContext, ExternalAccountFrame, ExternalPass, ExternalRequestStream, InputOverrides,
    NoRequestDataProvider, PreparedProgram, RequestEnvironment, RequestTimeframe,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn prepare(source: &str) -> PreparedProgram {
    let declaration = source.find("strategy(").unwrap();
    let end = declaration + source[declaration..].find(')').unwrap();
    let lowered = format!(
        "{}, calc_on_order_fills=true{}",
        &source[..end],
        &source[end..]
    );
    let analysis =
        pine_sema::analyze_source(&pine_syntax::SourceFile::new("checkpoint.pine", lowered));
    assert!(analysis.hir.is_some(), "{:?}", analysis.diagnostics);
    PreparedProgram::new(analysis.hir.unwrap())
}
fn fresh(program: &PreparedProgram) -> HistoricalRuntime<'static> {
    HistoricalRuntime::from_prepared_with_request_environment_and_input_overrides(
        program,
        RequestEnvironment::new(
            ChartContext::new("V-BTC-SPOT", RequestTimeframe::parse("10S").unwrap()),
            Arc::new(NoRequestDataProvider),
        ),
        InputOverrides::new(),
    )
}
fn bounded(program: &PreparedProgram) -> PreparedProgram {
    let values = pine_runtime::input_calls(program)
        .into_iter()
        .map(|call| {
            let Some(pine_runtime::PineValue::Int(value)) = call.default_value else {
                panic!("integer default");
            };
            (call.call_site_id, value)
        })
        .collect::<Vec<_>>();
    PreparedProgram::new(pine_sema::specialize_integer_input_history(program, &values).unwrap())
}
fn next(runtime: &mut HistoricalRuntime<'_>, index: usize) -> Value {
    let close = 100.0 + ((index * 73 + index * index * 13) % 101) as f64;
    let bar = Bar {
        time: index as i64 * 10_000,
        open: close,
        high: close + 3.,
        low: close - 4.,
        close,
        volume: (index % 9 + 1) as f64,
    };
    let position = if index % 11 < 5 { 0. } else { 2. };
    let account = ExternalAccountFrame {
        time: bar.time,
        position_size: position,
        position_avg_price: (position != 0.).then_some(120.),
        equity: 10_000.,
        initial_capital: 10_000.,
        netprofit: index as f64,
        openprofit: 0.,
    };
    let stream = ExternalRequestStream {
        symbol: "V-BTC-PERP".into(),
        timeframe: "10S".into(),
        bars: vec![Bar {
            close: close + if index % 7 < 3 { 8. } else { -2. },
            ..bar
        }],
    };
    let passes = Some(vec![vec![ExternalPass {
        account: account.clone(),
        bar,
        event_time_ms: bar.time + 10_000,
        confirmed: true,
        request_data: Some(vec![stream]),
    }]]);
    if runtime.external_intents().is_err() {
        *runtime = runtime
            .clone()
            .with_external_accounts_and_passes(vec![account], passes)
            .unwrap();
    } else {
        runtime
            .extend_external_accounts_and_passes(vec![account], passes)
            .unwrap();
    }
    runtime.append_bar(bar).unwrap();
    serde_json::to_value(runtime.take_external_intents().unwrap()).unwrap()
}

#[test]
fn six_qualified_programs_resume_exact_intents_after_512_and_2048_bars() {
    let sources = [
        include_str!("../../bot-plugins/pine-strategy/scripts/ma.pine"),
        include_str!("../../bot-plugins/pine-strategy/scripts/rsi.pine"),
        include_str!("../../bot-plugins/pine-strategy/scripts/breakout.pine"),
        include_str!("../../bot-plugins/pine-strategy/scripts/limit.pine"),
        include_str!("../../bot-plugins/pine-strategy/scripts/perp.pine"),
        include_str!("../../bot-plugins/pine-strategy/scripts/basis.pine"),
    ];
    for (source_index, source) in sources.into_iter().enumerate() {
        let program = prepare(source);
        let mut reference = fresh(&program);
        let specialized = bounded(&program);
        let mut continued = fresh(&specialized);
        let mut initial_size = 0;
        for index in 0..4097 {
            assert_eq!(
                next(&mut continued, index),
                next(&mut reference, index),
                "source={source_index} bar={index}"
            );
            continued.compact_external_feedback().unwrap();
            continued.compact_external_outputs();
            if [511, 1023, 2047, 3071, 4095].contains(&index) {
                let checkpoint = continued
                    .export_external_checkpoint("test-binding")
                    .unwrap();
                let bytes = serde_json::to_vec(&checkpoint).unwrap();
                if initial_size == 0 {
                    initial_size = bytes.len();
                }
                assert!(
                    bytes.len() <= initial_size + 4096,
                    "source={source_index} checkpoint grew with elapsed history: {initial_size} -> {}",
                    bytes.len()
                );
                let frozen: Value = serde_json::from_slice(&bytes).unwrap();
                let mut restored = fresh(&specialized);
                restored
                    .restore_external_checkpoint(&frozen, "test-binding")
                    .unwrap();
                assert_eq!(
                    restored.export_external_checkpoint("test-binding").unwrap(),
                    frozen
                );
                continued = restored;
            }
        }
    }
}

#[test]
fn mismatched_and_malformed_checkpoints_do_not_mutate_fresh_runtime() {
    let program = bounded(&prepare(include_str!(
        "../../bot-plugins/pine-strategy/scripts/rsi.pine"
    )));
    let mut original = fresh(&program);
    for index in 0..30 {
        next(&mut original, index);
    }
    let checkpoint = original.export_external_checkpoint("correct").unwrap();
    let mut restored = fresh(&program);
    assert!(
        restored
            .restore_external_checkpoint(&checkpoint, "wrong")
            .is_err()
    );
    assert!(restored.external_intents().is_err());
    let mut broken = checkpoint.clone();
    broken["flow"][0] = json!(["unknown"]);
    assert!(
        restored
            .restore_external_checkpoint(&broken, "correct")
            .is_err()
    );
    assert!(restored.external_intents().is_err());
    restored
        .restore_external_checkpoint(&checkpoint, "correct")
        .unwrap();
    assert_eq!(next(&mut restored, 30), next(&mut original, 30));

    for key in [
        "first_close",
        "series_bar",
        "last_index",
        "previous_time",
        "position_origin",
        "series",
        "active_series",
        "payload_sha256",
    ] {
        let mut missing = checkpoint.clone();
        missing.as_object_mut().unwrap().remove(key);
        let mut target = fresh(&program);
        assert!(
            target
                .restore_external_checkpoint(&missing, "correct")
                .is_err(),
            "missing {key}"
        );
        assert!(target.external_intents().is_err());
        let mut tampered = checkpoint.clone();
        tampered[key] = json!(99_999);
        assert!(
            target
                .restore_external_checkpoint(&tampered, "correct")
                .is_err(),
            "tampered {key}"
        );
        assert!(target.external_intents().is_err());
    }
}

#[test]
fn unsupported_collections_fail_closed() {
    let program = prepare(
        "//@version=6\nstrategy(\"arrays\")\nvar a = array.new_float()\narray.push(a, close)\n",
    );
    let mut runtime = fresh(&program);
    next(&mut runtime, 0);
    assert!(runtime.export_external_checkpoint("arrays").is_err());
}

#[test]
fn unbounded_dynamic_history_cannot_be_exported_as_a_compact_checkpoint() {
    let program = prepare(include_str!(
        "../../bot-plugins/pine-strategy/scripts/breakout.pine"
    ));
    assert!(program.series_history.iter().any(|r| r.has_dynamic_offsets));
    let mut original = fresh(&program);
    next(&mut original, 0);
    assert!(original.export_external_checkpoint("unbounded").is_err());
    let specialized = bounded(&program);
    assert!(
        !specialized
            .series_history
            .iter()
            .any(|r| r.has_dynamic_offsets)
    );
    assert_eq!(
        specialized.statements, program.statements,
        "specialization must not change execution"
    );
}

#[test]
fn integer_input_history_overflow_never_becomes_a_small_retention_window() {
    for expression in [
        "ta.mom(close, length)",
        "ta.highest(close, length)",
        "ta.change(close, length)",
        "close[length * 2]",
    ] {
        let source = format!(
            "//@version=6\nstrategy(\"overflow\")\nlength = input.int(2, \"Length\")\nplot({expression})\n"
        );
        let program = prepare(&source);
        let id = pine_runtime::input_calls(&program)[0].call_site_id;
        let value = if expression.contains("* 2") {
            u32::MAX as i64
        } else {
            u32::MAX as i64 + 3
        };
        let specialized =
            pine_sema::specialize_integer_input_history(&program, &[(id, value)]).unwrap();
        assert!(
            specialized.history.has_dynamic_offsets,
            "{expression} must not wrap to a short window"
        );
    }
}

mod host {
    use exchange_core::{Candle, candle_history_append, candle_history_hex, candle_history_seed};
    use exchange_server::pine_inprocess::Host;
    use serde_json::{Value, json};
    use std::path::Path;

    fn host(compact: bool) -> Host {
        Host::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../bot-plugins/pine-strategy"))
            .unwrap()
            .with_checkpoint_mode(compact)
    }
    fn candles(count: usize) -> Vec<Candle> {
        (0..count)
            .map(|i| {
                let close = 100 + ((i * 73 + i * i * 13) % 101) as i64;
                Candle {
                    schema_version: 1,
                    open_time_ms: i as u64 * 1000,
                    close_time_ms: (i as u64 + 1) * 1000,
                    open_tick: close,
                    high_tick: close + 3,
                    low_tick: close - 4,
                    close_tick: close,
                    volume: (i % 9 + 1) as u64,
                    quote_volume: i128::from(close) * (i % 9 + 1) as i128,
                    trades: 1,
                    taker_buy_base: Some(0),
                    taker_buy_quote: Some(0),
                    is_final: true,
                }
            })
            .collect()
    }
    fn prefixes(bars: &[Candle]) -> Vec<[u8; 32]> {
        let mut result = vec![candle_history_seed(1000)];
        for bar in bars {
            result.push(candle_history_append(result.last().unwrap(), bar));
        }
        result
    }
    fn data(bars: &[Candle], prefix: usize, hash: &[u8; 32]) -> Value {
        json!({"interval_ms":1000,"candles":bars,"own_fills":[],"fill_details":[],
            "truncated":prefix>0,"private_history_complete":true,
            "history_proof":{"version":1,"prefix_bars":prefix,"prefix_sha256":candle_history_hex(hash)}})
    }
    fn request(
        script: &str,
        bars: &[Candle],
        prefix: usize,
        hash: &[u8; 32],
        state: Value,
    ) -> Value {
        let manifest: Value =
            serde_json::from_str(include_str!("../../bot-plugins/pine-strategy/bot.json")).unwrap();
        let mut config = Value::Object(
            manifest["bot"]["parameters"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.clone(), value["default"].clone()))
                .collect(),
        );
        config["script"] = json!(script);
        config["bar_interval_ms"] = json!(1000);
        let last = bars.last().unwrap();
        let mut result = json!({"protocol_version":"bot.v1","plugin_id":"pine.strategy","plugin_version":"2.0.0",
            "state_version":1,"seed":7,"config":config,"state":state,
            "participant":{"participant_id":"pine","room_id":"test","account_id":300,"instrument_id":"V-BTC-SPOT","kind":"RuleAgent"},
            "observation":{"version":1,"room_id":"test","instrument_id":"V-BTC-SPOT","status":"Running",
                "step":prefix+bars.len(),"market_time_ms":last.close_time_ms,
                "instrument":{"tick_size":1},"book":{"bids":[{"price_tick":last.close_tick-1,"qty":100}],"asks":[{"price_tick":last.close_tick+1,"qty":100}]},
                "own_orders":[],"own_account":{"Spot":{"account_id":300,"cash_balance":10000,"position_qty":0,"fees_paid":0,"available_cash":10000,"available_position":0}},
                "bot_market_data":data(bars,prefix,hash)}});
        if script == "perp.pine" {
            make_perp(&mut result);
        }
        result
    }
    fn make_perp(request: &mut Value) {
        request["participant"]["instrument_id"] = json!("V-BTC-PERP");
        request["observation"]["instrument_id"] = json!("V-BTC-PERP");
        request["observation"]["own_account"] = json!({"Perp":{"account_id":300,"cash_balance":10000,"available_cash":10000,
            "position_qty":0,"avg_entry_price_tick":0,"equity":10000,"realized_pnl":0,"unrealized_pnl":0,"fees_paid":0,"funding_pnl":0,"margin_status":"Healthy"}});
    }
    fn attach_basis(request: &mut Value, peer: &[Candle], prefix: usize, hash: &[u8; 32]) {
        request["config"]["related_instruments"] = json!(["V-BTC-PERP"]);
        let mut related = request.clone();
        make_perp(&mut related);
        related["observation"]["bot_market_data"] = data(peer, prefix, hash);
        request["observation"]["related_markets"] = json!([related["observation"]]);
    }
    fn durable(state: &Value) -> Value {
        serde_json::from_slice(&serde_json::to_vec(state).unwrap()).unwrap()
    }

    #[test]
    fn compact_host_matches_transcript_host_and_speculative_retries_for_all_qualified_sources() {
        let all = candles(97);
        let hashes = prefixes(&all);
        // Non-contiguous provider bars also exercise gaps_off's prior value.
        let peer: Vec<_> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 7 != 3)
            .map(|(i, c)| {
                let mut c = c.clone();
                let delta = if i % 11 < 5 { 9 } else { -4 };
                c.open_tick += delta;
                c.high_tick += delta;
                c.low_tick += delta;
                c.close_tick += delta;
                c
            })
            .collect();
        let peer_hashes = prefixes(&peer);
        for script in [
            "ma.pine",
            "rsi.pine",
            "breakout.pine",
            "limit.pine",
            "perp.pine",
            "basis.pine",
        ] {
            let reference = host(false);
            let compact = host(true);
            let mut old = Value::Null;
            let mut new = Value::Null;
            for count in 1..=97 {
                let mut baseline = request(script, &all[..count], 0, &hashes[0], old);
                if script == "basis.pine" {
                    let visible =
                        peer.partition_point(|b| b.close_time_ms <= all[count - 1].close_time_ms);
                    attach_basis(&mut baseline, &peer[..visible], 0, &peer_hashes[0]);
                }
                let mut input = baseline.clone();
                input["state"] = new;
                if [34, 66, 97].contains(&count) {
                    compact.clear_sessions();
                }
                let expected = reference.decide_value(&baseline, 5000).unwrap();
                let actual = compact
                    .decide_value(&input, 5000)
                    .unwrap_or_else(|e| panic!("{script} bar {count}: {e}"));
                assert_eq!(
                    actual["actions"], expected["actions"],
                    "{script} bar {count}"
                );
                for key in [
                    "last_intents",
                    "entries",
                    "orders",
                    "ledgers",
                    "observed_position",
                    "observed_cash",
                    "evaluated_bars",
                    "last_fill_id",
                ] {
                    assert_eq!(
                        actual["state"][key], expected["state"][key],
                        "{script} bar {count}, {key}"
                    );
                }
                if [31, 32, 33, 64, 65].contains(&count) {
                    assert_eq!(
                        compact.decide_value(&input, 5000).unwrap(),
                        actual,
                        "speculative retry at {script}/{count}"
                    );
                }
                assert!(actual["state"].get("bars").is_none());
                assert!(actual["state"]["vm_tail"].as_array().unwrap().len() < 32);
                old = expected["state"].clone();
                new = durable(&actual["state"]);
            }
            let mut no_new = request(script, &all, 0, &hashes[0], new.clone());
            if script == "basis.pine" {
                attach_basis(&mut no_new, &peer, 0, &peer_hashes[0]);
            }
            let response = compact.decide_value(&no_new, 5000).unwrap();
            assert_eq!(response["state"]["vm_checkpoint"], new["vm_checkpoint"]);
            assert_eq!(response["state"]["vm_tail"], new["vm_tail"]);
        }
    }

    #[test]
    fn compact_host_continues_past_2048_with_bounded_windows_and_cold_restarts() {
        let all = candles(2081);
        let hashes = prefixes(&all);
        for script in ["rsi.pine", "ma.pine", "breakout.pine"] {
            let compact = host(true);
            let mut state = Value::Null;
            let mut initial_size = 0;
            for count in 1usize..=all.len() {
                let start = count.saturating_sub(64);
                let mut input = request(script, &all[start..count], start, &hashes[start], state);
                if script == "ma.pine" {
                    input["config"]["inputs"] = json!({"Fast":257,"Slow":513});
                }
                if script == "breakout.pine" {
                    input["config"]["inputs"] = json!({"Length":513});
                }
                if [33, 513, 1025, 2049].contains(&count) {
                    compact.clear_sessions();
                }
                let response = compact
                    .decide_value(&input, 5000)
                    .unwrap_or_else(|e| panic!("bar {count}: {e}"));
                assert_eq!(response["state"]["evaluated_bars"], count);
                state = durable(&response["state"]);
                if count == 512 {
                    initial_size = serde_json::to_vec(&state).unwrap().len();
                }
                if count > 512 && count % 32 == 0 {
                    assert!(serde_json::to_vec(&state).unwrap().len() <= initial_size + 4096);
                }
            }
            assert_eq!(state["vm_checkpoint"]["bars"], 2080);
            assert_eq!(state["vm_tail"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn execution_seal_rejects_tail_and_prior_provider_mutation_without_new_bars() {
        let all = candles(34);
        let hashes = prefixes(&all);
        let compact = host(true);
        let initial = request("rsi.pine", &all[..32], 0, &hashes[0], Value::Null);
        let state = compact.decide_value(&initial, 5000).unwrap()["state"].clone();
        let append = request("rsi.pine", &all[..33], 0, &hashes[0], state);
        let state = compact.decide_value(&append, 5000).unwrap()["state"].clone();
        let valid = request("rsi.pine", &all[..33], 0, &hashes[0], state);
        let mut tail = valid.clone();
        tail["state"]["vm_tail"][0]["bar"]["close"] = json!({"$f64":"4059000000000000"});
        let mut prior = valid.clone();
        prior["state"]["candle_audits"]["V-BTC-SPOT:1S"]["last"]["close_tick"] = json!(999);
        for input in [tail, prior] {
            compact.clear_sessions();
            assert!(
                compact
                    .decide_value(&input, 5000)
                    .unwrap_err()
                    .to_string()
                    .contains("execution seal")
            );
        }
        assert!(compact.decide_value(&valid, 5000).is_ok());
    }

    #[test]
    fn compact_host_rejects_old_prefix_corrections_missing_proofs_and_cursor_damage() {
        let all = candles(100);
        let hashes = prefixes(&all);
        let compact = host(true);
        let initial = request("rsi.pine", &all[..96], 0, &hashes[0], Value::Null);
        let state = compact.decide_value(&initial, 5000).unwrap()["state"].clone();
        let valid = request("rsi.pine", &all[64..97], 64, &hashes[64], state.clone());
        let mut corrected = all.clone();
        corrected[0].high_tick += 1;
        let changed = prefixes(&corrected);
        let mut ancient = valid.clone();
        ancient["observation"]["bot_market_data"]["history_proof"]["prefix_sha256"] =
            json!(candle_history_hex(&changed[64]));
        assert!(
            compact
                .decide_value(&ancient, 5000)
                .unwrap_err()
                .to_string()
                .contains("prefix changed")
        );
        let mut visible = valid.clone();
        visible["observation"]["bot_market_data"]["candles"][0]["high_tick"] = json!(999);
        assert!(compact.decide_value(&visible, 5000).is_err());
        let mut no_proof = valid.clone();
        no_proof["observation"]["bot_market_data"]["history_proof"] = Value::Null;
        assert!(compact.decide_value(&no_proof, 5000).is_err());
        let mut unknown = valid.clone();
        unknown["observation"]["bot_market_data"]["history_proof"]["version"] = json!(999);
        assert!(compact.decide_value(&unknown, 5000).is_err());
        let mut private = valid.clone();
        private["observation"]["bot_market_data"]["private_history_complete"] = json!(false);
        assert!(compact.decide_value(&private, 5000).is_err());
        let mut damaged = valid.clone();
        damaged["state"]["evaluated_bars"] = json!(95);
        assert!(compact.decide_value(&damaged, 5000).is_err());
        let mut damaged_tail = valid.clone();
        damaged_tail["state"]["vm_tail"] = json!([{}]);
        assert!(compact.decide_value(&damaged_tail, 5000).is_err());
        compact.clear_sessions();
        assert_eq!(
            compact.decide_value(&valid, 5000).unwrap()["state"]["evaluated_bars"],
            97
        );
        // Restarting a new instance cannot initialize from an omitted prefix.
        let mut cold = valid.clone();
        cold["state"] = Value::Null;
        assert!(compact.decide_value(&cold, 5000).is_err());
    }
}
