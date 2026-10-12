//! Native reference: live-tick-reference/capture-v1.pine and native-chart-v1.csv.
//! Orders survive rollback and become eligible on the next observed update.
//! Prices below are synthetic observed inputs, not reconstructed native fills.
use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, RuntimeResult};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn runtime(source: &str) -> RealtimeRuntime<'static> {
    let analysis = analyze_source(&SourceFile::new("ticks.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    RealtimeRuntime::from_program(analysis.hir.unwrap())
}

fn bar(time: i64, close: f64) -> Bar {
    Bar {
        time,
        open: 100.0,
        high: 110.0,
        low: 90.0,
        close,
        volume: 1.0,
    }
}

fn plotted(result: &RuntimeResult, index: usize) -> f64 {
    result.plots[index].values.last().unwrap().as_f64().unwrap()
}

#[test]
fn market_entry_and_close_survive_user_rollback_and_fill_next_update() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("ticks", calc_on_every_tick=true)
varip int updates = 0
var int ordinary = 0
if barstate.isnew
    updates := 0
updates += 1
ordinary += 1
if barstate.isrealtime and updates == 1
    strategy.entry("L", strategy.long, qty=1)
if barstate.isrealtime and updates == 3
    strategy.close("L")
plot(strategy.position_size)
plot(strategy.closedtrades)
plot(ordinary)
plot(strategy.opentrades.entry_price(0))
plot(strategy.closedtrades.exit_price(0))
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    let first = runtime
        .update(BarUpdate::forming(bar(60000, 101.0)))
        .unwrap();
    assert_eq!(plotted(&first, 0), 0.0, "creation is not a fill tick");
    let second = runtime
        .update(BarUpdate::forming(bar(60000, 102.0)))
        .unwrap();
    assert_eq!(plotted(&second, 0), 1.0);
    assert_eq!(
        plotted(&second, 3),
        102.0,
        "use observed price, not candle open"
    );
    assert_eq!(plotted(&second, 2), 2.0, "ordinary var still rolls back");
    let third = runtime
        .update(BarUpdate::forming(bar(60000, 103.0)))
        .unwrap();
    assert_eq!(plotted(&third, 0), 1.0, "close waits for next tick");
    let fourth = runtime
        .update(BarUpdate::forming(bar(60000, 104.0)))
        .unwrap();
    assert_eq!(plotted(&fourth, 0), 0.0);
    assert_eq!(plotted(&fourth, 1), 1.0);
    assert_eq!(plotted(&fourth, 4), 104.0);
    let confirmed = runtime
        .update(BarUpdate::confirmed(bar(60000, 105.0)))
        .unwrap();
    assert_eq!(
        plotted(&confirmed, 1),
        1.0,
        "confirmation must not erase or duplicate fill"
    );
    assert_eq!(plotted(&confirmed, 4), 104.0);
}

#[test]
fn default_strategy_pending_order_executes_between_script_calculations() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("close calculation")
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1)
plot(strategy.position_size)
plot(strategy.opentrades.entry_price(0))
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    let forming = runtime
        .update(BarUpdate::forming(bar(60000, 102.0)))
        .unwrap();
    assert_eq!(
        forming
            .strategy
            .as_ref()
            .unwrap()
            .position
            .last()
            .map(|p| p.size),
        Some(1.0)
    );
    assert_eq!(
        forming.plots[0].values.len(),
        1,
        "no normal script pass on forming"
    );
    let confirmed = runtime
        .update(BarUpdate::confirmed(bar(60000, 103.0)))
        .unwrap();
    assert_eq!(plotted(&confirmed, 0), 1.0);
    assert_eq!(plotted(&confirmed, 1), 102.0);
}

#[test]
fn limit_order_cannot_fill_from_extreme_before_its_creation() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("limit ticks", calc_on_every_tick=true)
if barstate.isrealtime and barstate.isnew
    strategy.entry("L", strategy.long, qty=1, limit=95)
plot(strategy.position_size)
plot(strategy.opentrades.entry_price(0))
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    runtime
        .update(BarUpdate::forming(bar(60000, 101.0)))
        .unwrap();
    let second = runtime
        .update(BarUpdate::forming(bar(60000, 102.0)))
        .unwrap();
    assert_eq!(plotted(&second, 0), 0.0, "cumulative low=90 predates order");
    let third = runtime
        .update(BarUpdate::forming(bar(60000, 94.0)))
        .unwrap();
    assert_eq!(plotted(&third, 0), 1.0);
    assert_eq!(
        plotted(&third, 1),
        94.0,
        "marketable limit gets observed better price"
    );
}

#[test]
fn failed_execution_does_not_consume_a_pending_entry() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("atomic tick", calc_on_every_tick=true)
if barstate.isrealtime and barstate.isnew
    strategy.entry("L", strategy.long, qty=1)
if barstate.isrealtime and close == 102
    runtime.error("reject update")
plot(strategy.position_size)
plot(strategy.opentrades.entry_price(0))
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    runtime
        .update(BarUpdate::forming(bar(60000, 101.0)))
        .unwrap();
    let before = pine_runtime::public_runtime_result_json(&runtime.result());
    assert!(
        runtime
            .update(BarUpdate::forming(bar(60000, 102.0)))
            .is_err()
    );
    assert_eq!(
        pine_runtime::public_runtime_result_json(&runtime.result()),
        before
    );
    let accepted = runtime
        .update(BarUpdate::forming(bar(60000, 103.0)))
        .unwrap();
    assert_eq!(plotted(&accepted, 0), 1.0);
    assert_eq!(plotted(&accepted, 1), 103.0);
}

#[test]
fn intrabar_cancellation_persists_until_confirmation() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("cancel ticks", calc_on_every_tick=true)
varip int updates = 0
if barstate.isnew
    updates := 0
updates += 1
if barstate.isrealtime and updates == 1
    strategy.entry("L", strategy.long, qty=1, limit=95)
if barstate.isrealtime and updates == 2
    strategy.cancel("L")
plot(strategy.position_size)
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    runtime
        .update(BarUpdate::forming(bar(60000, 101.0)))
        .unwrap();
    runtime
        .update(BarUpdate::forming(bar(60000, 102.0)))
        .unwrap();
    let result = runtime
        .update(BarUpdate::confirmed(bar(60000, 94.0)))
        .unwrap();
    assert_eq!(plotted(&result, 0), 0.0);
    assert!(result.strategy.unwrap().orders.is_empty());
}

#[test]
fn process_orders_on_close_does_not_turn_forming_into_bar_close() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("close processing", calc_on_every_tick=true, process_orders_on_close=true)
if barstate.isrealtime and barstate.isnew
    strategy.entry("L", strategy.long, qty=1)
plot(strategy.position_size)
plot(strategy.opentrades.entry_price(0))
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    let first = runtime
        .update(BarUpdate::forming(bar(60000, 101.0)))
        .unwrap();
    assert!(first.strategy.unwrap().orders.is_empty());
    let second = runtime
        .update(BarUpdate::forming(bar(60000, 102.0)))
        .unwrap();
    assert_eq!(plotted(&second, 0), 1.0);
    assert_eq!(plotted(&second, 1), 102.0);
}

#[test]
fn short_stop_limit_activation_can_fill_on_later_tick_in_same_bar() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("short stop limit", calc_on_every_tick=true)
if barstate.isrealtime and barstate.isnew
    strategy.entry("S", strategy.short, qty=1, stop=95, limit=96)
plot(strategy.position_size)
plot(strategy.opentrades.entry_price(0))
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    runtime
        .update(BarUpdate::forming(bar(60000, 101.0)))
        .unwrap();
    let activated = runtime
        .update(BarUpdate::forming(bar(60000, 94.0)))
        .unwrap();
    assert_eq!(plotted(&activated, 0), 0.0);
    let filled = runtime
        .update(BarUpdate::forming(bar(60000, 97.0)))
        .unwrap();
    assert_eq!(plotted(&filled, 0), -1.0);
    assert_eq!(plotted(&filled, 1), 97.0);
}

#[test]
fn calc_on_fills_does_not_run_on_a_tick_without_a_fill() {
    let mut runtime = runtime(
        r#"//@version=6
strategy("fill calculation", calc_on_order_fills=true)
if bar_index == 0
    strategy.entry("L", strategy.long, qty=1, limit=95)
plot(strategy.position_size)
"#,
    );
    runtime.seed_historical(&[bar(0, 100.0)]).unwrap();
    let quiet = runtime
        .update(BarUpdate::forming(bar(60000, 101.0)))
        .unwrap();
    assert_eq!(quiet.plots[0].values.len(), 1);
    let fill = runtime
        .update(BarUpdate::forming(bar(60000, 94.0)))
        .unwrap();
    assert_eq!(plotted(&fill, 0), 1.0);
    let quiet_after_fill = runtime
        .update(BarUpdate::forming(bar(60000, 96.0)))
        .unwrap();
    assert_eq!(
        quiet_after_fill.plots, fill.plots,
        "a non-calculating tick retains the most recent script outputs"
    );
    assert_eq!(
        quiet_after_fill.strategy.as_ref().unwrap().orders,
        fill.strategy.as_ref().unwrap().orders
    );
    let confirmed = runtime
        .update(BarUpdate::confirmed(bar(60000, 97.0)))
        .unwrap();
    assert_eq!(plotted(&confirmed, 0), 1.0);
    assert_eq!(confirmed.plots[0].values.len(), 2);
}

#[test]
fn two_native_fills_and_every_tick_share_one_observed_execution() {
    let samples: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/realtime/strategy_multi_fill_native.json"
    ))
    .unwrap();
    assert_eq!(samples.len(), 7);
    let capture = 1_789_003_980_000;
    let fields = [
        "now",
        "open",
        "high",
        "low",
        "close",
        "volume",
        "position",
        "openTrades",
        "closedTrades",
    ];
    // Native quantity precision is supplied by the host, just as in the wheel
    // replay. The default whole-contract grid would round 0.001 orders away.
    // Use that explicit environment before executing the fixture.
    let analysis = analyze_source(&SourceFile::new(
        "native.pine",
        include_str!("../../../tests/fixtures/realtime/strategy_multi_fill_native.pine"),
    ));
    let hir = analysis.hir.unwrap();
    let environment = pine_runtime::RequestEnvironment::default().for_chart(
        pine_runtime::ChartContext::default()
            .with_price_grid(1, 10)
            .unwrap()
            .with_quantity_precision(6)
            .unwrap(),
    );
    let mut runtime = RealtimeRuntime::with_request_environment(&hir, environment);
    runtime
        .seed_historical(&[bar(capture - 60_000, 100.0)])
        .unwrap();
    let mut compared = 0;
    for (i, sample) in samples.iter().enumerate() {
        let update = Bar {
            time: capture,
            open: sample["open"].as_f64().unwrap(),
            high: sample["high"].as_f64().unwrap(),
            low: sample["low"].as_f64().unwrap(),
            close: sample["close"].as_f64().unwrap(),
            volume: sample["volume"].as_f64().unwrap(),
        };
        let result = runtime
            .update_with_execution_time(
                BarUpdate::forming(update),
                sample["now"].as_f64().unwrap() as i64,
            )
            .unwrap();
        assert_eq!(result.plots.len(), 64);
        for (j, expected_sample) in samples.iter().enumerate() {
            for (k, field) in fields.iter().enumerate() {
                let actual = result.plots[j * 9 + k].values.last().unwrap().as_f64();
                let expected = (j <= i).then(|| expected_sample[field].as_f64().unwrap());
                assert_eq!(actual, expected, "step {i}, MF{j} {field}");
                compared += 1;
            }
        }
        assert_eq!(plotted(&result, 63), (i + 1) as f64);
        compared += 1;
    }
    assert_eq!(compared, 448);
}
