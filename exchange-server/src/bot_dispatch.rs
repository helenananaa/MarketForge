//! Durable event cursors are an optimization only; returned state remains authoritative.
use exchange_core::{BotConfig, BotError, ParticipantObservation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write;

pub(super) const CURSOR: &str = "_host_dispatch";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    #[default]
    EveryTick,
    Events {
        #[serde(default)]
        closed_bars: bool,
        #[serde(default)]
        book: bool,
        #[serde(default)]
        account: bool,
        #[serde(default)]
        interval_ms: Option<u64>,
    },
}

#[derive(Serialize)]
struct Inputs<'a> {
    instrument: &'a str,
    status: &'a exchange_core::MarketStatus,
    book: Option<&'a exchange_core::BookSnapshot>,
    trades: Option<&'a Vec<exchange_core::Trade>>,
    account: Option<&'a Option<exchange_core::AccountSnapshot>>,
    orders: Option<&'a Vec<exchange_core::Order>>,
    protection: Option<&'a Vec<exchange_core::PositionProtection>>,
    risk: Option<&'a Option<exchange_core::position_protection::PositionRisk>>,
    price: Option<&'a Option<exchange_core::PerpPriceSnapshot>>,
    data: Option<DataInputs<'a>>,
    bucket: Option<u64>,
    related: Vec<Inputs<'a>>,
}

#[derive(Serialize)]
struct DataInputs<'a> {
    interval_ms: u64,
    truncated: bool,
    candles: Option<&'a Vec<exchange_core::Candle>>,
    streams: Option<&'a Vec<exchange_core::observation::BotDataStream>>,
    volume: Option<&'a str>,
    fills: Option<&'a Vec<exchange_core::Trade>>,
    fill_details: Option<&'a Vec<exchange_core::observation::BotFillDetail>>,
    accounts: Option<&'a Vec<exchange_core::observation::BotAccountEvent>>,
    conditions: Option<&'a Vec<exchange_core::conditional_orders::ConditionalOrder>>,
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Trigger {
    pub fn validate(&self, has_history: bool) -> Result<(), BotError> {
        if let Self::Events {
            closed_bars,
            book,
            account,
            interval_ms,
        } = self
            && ((*closed_bars && !has_history)
                || *interval_ms == Some(0)
                || !(*closed_bars || *book || *account || interval_ms.is_some()))
        {
            return Err(BotError("invalid bot event trigger".into()));
        }
        Ok(())
    }
    fn inputs<'a>(&self, view: &'a ParticipantObservation) -> Inputs<'a> {
        let Self::Events {
            closed_bars,
            book,
            account,
            interval_ms,
        } = self
        else {
            unreachable!()
        };
        Inputs {
            instrument: &view.instrument_id,
            status: &view.status,
            book: book.then_some(&view.book),
            trades: book.then_some(&view.public_trades),
            account: account.then_some(&view.own_account),
            orders: account.then_some(&view.own_orders),
            protection: account.then_some(&view.position_protections),
            risk: account.then_some(&view.risk),
            price: account.then_some(&view.perp_price),
            // Include complete supplied history: a corrected/truncated prefix must wake
            // the consumer and be rejected there, never hidden by a tail-only cursor.
            data: view
                .bot_market_data
                .as_ref()
                .filter(|_| *closed_bars || *account || *book)
                .map(|data| DataInputs {
                    interval_ms: data.interval_ms,
                    truncated: data.truncated,
                    candles: closed_bars.then_some(&data.candles),
                    streams: closed_bars.then_some(&data.streams),
                    volume: book.then_some(data.external_volume_qty.as_str()),
                    fills: account.then_some(&data.own_fills),
                    fill_details: account.then_some(&data.fill_details),
                    accounts: account.then_some(&data.account_events),
                    conditions: account.then_some(&data.conditional_orders),
                }),
            bucket: interval_ms.map(|interval| view.market_time_ms / interval),
            related: view
                .related_markets
                .iter()
                .map(|v| self.inputs(v))
                .collect(),
        }
    }
    pub fn fingerprint(
        &self,
        config: &BotConfig,
        state: &Value,
        view: &ParticipantObservation,
    ) -> Result<Option<String>, BotError> {
        if matches!(self, Self::EveryTick) {
            return Ok(None);
        }
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(
            &mut writer,
            &(self, config, self.inputs(view), state.get("_host_receipts")),
        )
        .map_err(|e| BotError(e.to_string()))?;
        Ok(Some(format!("{:x}", writer.0.finalize())))
    }
    pub fn due(
        &self,
        config: &BotConfig,
        state: &Value,
        view: &ParticipantObservation,
    ) -> Result<bool, BotError> {
        if state
            .get("_next_wake_ms")
            .and_then(Value::as_u64)
            .is_some_and(|t| t <= view.market_time_ms)
        {
            return Ok(true);
        }
        Ok(self
            .fingerprint(config, state, view)?
            .is_none_or(|key| state.get(CURSOR).and_then(Value::as_str) != Some(&key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_cursor_ignores_clock_only_changes_but_wakes_for_private_feedback_and_deadlines() {
        let value: Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json"))
                .unwrap();
        let mut rooms = exchange_core::RoomManager::new();
        rooms
            .create_room(serde_json::from_value(value["scenario"].clone()).unwrap())
            .unwrap();
        let mut view = rooms
            .participant_observation("f6-batch", "V-BTC-SPOT", 20)
            .unwrap();
        let config = BotConfig {
            participant: exchange_core::ParticipantConfig {
                participant_id: "a".into(),
                room_id: "f6-batch".into(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".into()),
                kind: exchange_core::ParticipantKind::RuleAgent,
            },
            plugin_id: "x".into(),
            plugin_version: "1".into(),
            state_version: 1,
            config_version: 1,
            seed: 7,
            config: serde_json::json!({}),
        };
        let trigger = Trigger::Events {
            closed_bars: false,
            book: true,
            account: true,
            interval_ms: None,
        };
        let mut state = serde_json::json!({});
        assert!(trigger.due(&config, &state, &view).unwrap());
        state[CURSOR] = Value::String(
            trigger
                .fingerprint(&config, &state, &view)
                .unwrap()
                .unwrap(),
        );
        // Serialization and recreation preserve the cursor; no speculative local state.
        let mut restored: Value =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        view.market_time_ms += 1000;
        view.step += 1;
        assert!(!trigger.due(&config, &restored, &view).unwrap());
        restored["_next_wake_ms"] = Value::from(view.market_time_ms + 1000);
        assert!(!trigger.due(&config, &restored, &view).unwrap());
        view.market_time_ms += 1000;
        assert!(trigger.due(&config, &restored, &view).unwrap());
        restored.as_object_mut().unwrap().remove("_next_wake_ms");
        restored["_host_receipts"] = serde_json::json!([{"accepted":false}]);
        assert!(trigger.due(&config, &restored, &view).unwrap());
        restored.as_object_mut().unwrap().remove("_host_receipts");
        view.book.asks.clear();
        assert!(trigger.due(&config, &restored, &view).unwrap());
        let timer = Trigger::Events {
            closed_bars: false,
            book: false,
            account: false,
            interval_ms: Some(10_000),
        };
        state[CURSOR] = Value::String(timer.fingerprint(&config, &state, &view).unwrap().unwrap());
        view.market_time_ms = 10_000;
        assert!(timer.due(&config, &state, &view).unwrap());

        let bars = Trigger::Events {
            closed_bars: true,
            book: false,
            account: true,
            interval_ms: None,
        };
        view.bot_market_data = Some(
            serde_json::from_value(serde_json::json!({
                "interval_ms":1000,"external_volume_qty":"0","candles":[{
                    "schema_version":1,"open_time_ms":0,"close_time_ms":1000,
                    "open_tick":1,"high_tick":1,"low_tick":1,"close_tick":1,
                    "volume":1,"quote_volume":1,"trades":1,"is_final":true
                }],"own_fills":[],"fill_details":[],"truncated":false
            }))
            .unwrap(),
        );
        state[CURSOR] = Value::String(bars.fingerprint(&config, &state, &view).unwrap().unwrap());
        view.bot_market_data.as_mut().unwrap().external_volume_qty = "5".into();
        assert!(
            !bars.due(&config, &state, &view).unwrap(),
            "forming public volume must not wake a bar/account-only strategy"
        );
        view.bot_market_data.as_mut().unwrap().candles[0].close_tick = 2;
        assert!(
            bars.due(&config, &state, &view).unwrap(),
            "history corrections must wake the consumer"
        );
        view.bot_market_data.as_mut().unwrap().candles[0].close_tick = 1;
        view.bot_market_data.as_mut().unwrap().truncated = true;
        assert!(
            bars.due(&config, &state, &view).unwrap(),
            "truncated history must never be hidden"
        );
    }
}
