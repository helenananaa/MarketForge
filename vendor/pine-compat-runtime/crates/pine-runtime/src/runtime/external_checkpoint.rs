//! Versioned calculation checkpoints for independently qualified closed-bar
//! external strategies. This is VM state, never a truncated input replay.
//! Unsupported state fails closed. Rendering history is intentionally outside
//! this protocol; hosts must not use it for chart-output restoration.
use super::append_history::AppendHistory;
use crate::algorithms::rolling_extreme::RollingExtremeState;
use crate::algorithms::rolling_window::{RollingWindowKey, RollingWindowState};
use crate::builtins::ta::RsiState;
use crate::external::{ExternalExecution, external_error};
use crate::{HistoricalRuntime, PineValue, RuntimeError, SeriesStore};
use pine_ir::{CallSiteId, SeriesId, SymbolId, VarSlotId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

const VERSION: &str = "marketforge.external-vm/1";
const MAX_VALUES: usize = 1_000_000;

fn payload_digest(value: &Value) -> Result<String, RuntimeError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| external_error("invalid checkpoint payload"))?;
    let mut hash = Sha256::new();
    hash.update(b"marketforge.external-vm.payload.v1\0");
    hash.update(bytes);
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn float(value: f64) -> Value {
    Value::String(format!("{:016x}", value.to_bits()))
}
pub(crate) fn optional_float(value: Option<f64>) -> Value {
    value.map_or(Value::Null, float)
}
pub(crate) fn read_float(value: &Value) -> Result<f64, RuntimeError> {
    let text = value
        .as_str()
        .filter(|v| v.len() == 16)
        .ok_or_else(|| external_error("invalid checkpoint float"))?;
    u64::from_str_radix(text, 16)
        .map(f64::from_bits)
        .map_err(|_| external_error("invalid checkpoint float bits"))
}
pub(crate) fn read_optional_float(value: &Value) -> Result<Option<f64>, RuntimeError> {
    if value.is_null() {
        Ok(None)
    } else {
        read_float(value).map(Some)
    }
}
pub(crate) fn array(value: &Value) -> Result<&Vec<Value>, RuntimeError> {
    value
        .as_array()
        .filter(|v| v.len() <= MAX_VALUES)
        .ok_or_else(|| external_error("invalid checkpoint array"))
}
pub(crate) fn usize_field(value: &Value) -> Result<usize, RuntimeError> {
    value
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| external_error("invalid checkpoint integer"))
}
pub(crate) fn u32_field(value: &Value) -> Result<u32, RuntimeError> {
    value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| external_error("invalid checkpoint id"))
}
pub(crate) fn optional_usize(value: &Value) -> Result<Option<usize>, RuntimeError> {
    if value.is_null() {
        Ok(None)
    } else {
        usize_field(value).map(Some)
    }
}
fn optional_i64(value: &Value) -> Result<Option<i64>, RuntimeError> {
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_i64()
            .map(Some)
            .ok_or_else(|| external_error("invalid checkpoint time"))
    }
}
pub(crate) fn boolean(value: &Value) -> Result<bool, RuntimeError> {
    value
        .as_bool()
        .ok_or_else(|| external_error("invalid checkpoint boolean"))
}
fn scalar(value: &PineValue) -> Result<Value, RuntimeError> {
    Ok(match value {
        PineValue::Int(v) => json!(["i", v.to_string()]),
        PineValue::Float(v) => json!(["f", float(*v)]),
        PineValue::Bool(v) => json!(["b", v]),
        PineValue::String(v) => json!(["s", v]),
        PineValue::Color(v) => json!(["c", v.to_string()]),
        PineValue::Tuple(v) => json!(["t", v.iter().map(scalar).collect::<Result<Vec<_>, _>>()?]),
        PineValue::Na => json!(["na"]),
        PineValue::Void => json!(["void"]),
        _ => {
            return Err(external_error(
                "checkpoint does not support collection/drawing handles",
            ));
        }
    })
}
fn read_scalar(value: &Value) -> Result<PineValue, RuntimeError> {
    Ok(match value[0].as_str() {
        Some("i") => PineValue::Int(
            value[1]
                .as_str()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| external_error("invalid scalar integer"))?,
        ),
        Some("f") => PineValue::Float(read_float(&value[1])?),
        Some("b") => PineValue::Bool(boolean(&value[1])?),
        Some("s") => PineValue::String(
            value[1]
                .as_str()
                .ok_or_else(|| external_error("invalid scalar string"))?
                .into(),
        ),
        Some("c") => PineValue::Color(
            value[1]
                .as_str()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| external_error("invalid scalar color"))?,
        ),
        Some("t") => PineValue::Tuple(
            array(&value[1])?
                .iter()
                .map(read_scalar)
                .collect::<Result<_, _>>()?,
        ),
        Some("na") => PineValue::Na,
        Some("void") => PineValue::Void,
        _ => return Err(external_error("unknown checkpoint scalar")),
    })
}
fn scalar_entries<'a>(
    values: impl Iterator<Item = (u32, &'a PineValue)>,
) -> Result<Value, RuntimeError> {
    let values = values
        .map(|(id, v)| Ok((id.to_string(), scalar(v)?)))
        .collect::<Result<BTreeMap<_, _>, RuntimeError>>()?;
    Ok(json!(values))
}
fn read_entries(value: &Value) -> Result<Vec<(u32, PineValue)>, RuntimeError> {
    value
        .as_object()
        .ok_or_else(|| external_error("invalid checkpoint map"))?
        .iter()
        .map(|(id, v)| {
            Ok((
                id.parse()
                    .map_err(|_| external_error("invalid checkpoint key"))?,
                read_scalar(v)?,
            ))
        })
        .collect()
}
fn rolling_key(key: &RollingWindowKey) -> Result<Value, RuntimeError> {
    Ok(match key {
        RollingWindowKey::Single(id) => json!(["single", id.0]),
        RollingWindowKey::Rma { call_site, channel } => json!(["rma", call_site.0, channel]),
        _ => {
            return Err(external_error(
                "unqualified rolling-window checkpoint state",
            ));
        }
    })
}
fn read_rolling_key(value: &Value) -> Result<RollingWindowKey, RuntimeError> {
    match value[0].as_str() {
        Some("single") => Ok(RollingWindowKey::Single(CallSiteId(u32_field(&value[1])?))),
        Some("rma") => Ok(RollingWindowKey::Rma {
            call_site: CallSiteId(u32_field(&value[1])?),
            channel: u8::try_from(u32_field(&value[2])?)
                .map_err(|_| external_error("invalid RMA channel"))?,
        }),
        _ => Err(external_error("unknown rolling-window checkpoint key")),
    }
}

impl HistoricalRuntime<'_> {
    fn checkpoint_settings(&self) -> Result<Value, RuntimeError> {
        Ok(json!({
            // The protocol version pins this canonical IR representation and
            // its semantics. Caller binding independently binds exact source.
            "program_sha256": format!("{:x}", Sha256::digest(format!("{VERSION}\0{:?}", &*self.program).as_bytes())),
            "inputs": scalar_entries(self.input_overrides.checkpoint_entries())?,
            "chart": format!("{:?}", self.request_environment.chart()),
            "execution": format!("{:?}", self.execution_limits),
            "resource": format!("{:?}", self.resource_budget.limits),
        }))
    }

    /// Export only at a confirmed boundary. The caller binds the snapshot to
    /// its exact source/config/account identity and commits it with the orders.
    /// Complete original render output is not part of this execution protocol.
    pub fn export_external_checkpoint(&self, binding: &str) -> Result<Value, RuntimeError> {
        if self.program.series_history.iter().any(|requirement| {
            self.series_retention
                .max_depth_for(requirement.series_id)
                .is_none()
        }) || self.strategy_position_size_history_depth.is_none()
        {
            return Err(external_error(
                "checkpoint requires statically bounded calculation history",
            ));
        }
        let external = self
            .external_execution
            .as_ref()
            .ok_or_else(|| external_error("checkpoint requires external execution"))?;
        if self.execution_failed
            || self.current_bar.is_some()
            || external.active_pass.is_some()
            || external.error.is_some()
            || external.base.checked_add(external.frames.len()) != Some(self.bars)
            || external.passes.as_ref().is_some_and(|groups| {
                groups
                    .last()
                    .is_some_and(|g| g.last().is_none_or(|p| !p.confirmed))
            })
        {
            return Err(external_error(
                "checkpoint requires a completed confirmed boundary",
            ));
        }
        if self.array_store.len() != 0
            || self.object_store.len() != 0
            || self.matrix_store.len() != 0
            || self.map_store.len() != 0
            || self.labels.len() != 0
            || self.lines.len() != 0
            || self.boxes.len() != 0
            || self.tables.len() != 0
            || self.line_fills.len() != 0
            || self.polylines.len() != 0
            || !self.alerts.is_empty()
            || !self.plot_chars.is_empty()
            || !self.plot_shapes.is_empty()
            || !self.plot_arrows.is_empty()
            || !self.plot_bars.is_empty()
            || !self.plot_candles.is_empty()
            || !self.bg_colors.is_empty()
            || !self.bar_colors.is_empty()
            || !self.hlines.is_empty()
            || !self.fills.is_empty()
            || !self.session_windows.is_empty()
            || !self.magnifier_input.is_empty()
            || !self.bounded_same_context_evaluations.is_empty()
            || !self.bounded_same_context_captures.is_empty()
            || !self.historical_dynamic_request_contexts.is_empty()
            || !self.request_feed.checkpoint_is_empty()
            || !self.ta_state.macd_state.is_empty()
            || !self.ta_state.cross_state.is_empty()
            || !self.ta_state.valuewhen_state.is_empty()
            || !self.ta_state.vwap_call_state.is_empty()
            || !self.ta_state.pivot_point_state.is_empty()
            || !self.ta_state.random_state.is_empty()
        {
            return Err(external_error(
                "runtime state is outside the qualified checkpoint profile",
            ));
        }
        let series: BTreeMap<_, _> = self
            .series_store
            .buffers
            .iter()
            .map(|(id, values)| {
                Ok((
                    id.0.to_string(),
                    values.iter().map(scalar).collect::<Result<Vec<_>, _>>()?,
                ))
            })
            .collect::<Result<_, RuntimeError>>()?;
        let mut active_series: Vec<_> = self.active_series.iter().map(|id| id.0).collect();
        active_series.sort_unstable();
        let mut rolling = self
            .ta_state
            .rolling_windows
            .iter()
            .map(|(key, value)| Ok(json!([rolling_key(key)?, value.external_checkpoint()])))
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        rolling.sort_by_key(Value::to_string);
        let extremes: BTreeMap<_, _> = self
            .ta_state
            .extreme_windows
            .iter()
            .map(|(id, state)| (id.0.to_string(), state.external_checkpoint()))
            .collect();
        let rsi: BTreeMap<_, _> = self
            .ta_state
            .rsi_state
            .iter()
            .map(|(id, state)| (id.0.to_string(), state.external_checkpoint()))
            .collect();
        let flow = [
            &self.accdist_state,
            &self.accdist_current,
            &self.iii_current,
            &self.nvi_state,
            &self.nvi_current,
            &self.obv_state,
            &self.obv_current,
            &self.pvi_state,
            &self.pvi_current,
            &self.pvt_state,
            &self.pvt_current,
            &self.vwap_current,
            &self.wad_state,
            &self.wad_current,
            &self.wvad_current,
        ]
        .into_iter()
        .map(scalar)
        .collect::<Result<Vec<_>, _>>()?;
        let mut checkpoint = json!({"version": VERSION, "binding": binding, "settings": self.checkpoint_settings()?,
            "bars": self.bars, "previous_time": external.frames.last().map(|f| f.time).or(external.previous_time),
        "passes": external.passes.is_some(), "series": series, "series_bar": self.series_store.current_bar(), "active_series":active_series,
            "symbols": scalar_entries(self.current_symbols.iter().map(|(id, v)| (id.0, v)))?,
            "current_series": scalar_entries(self.current_series.iter().map(|(id, v)| (id.0, v)))?,
            "var": scalar_entries(self.var_store.iter().map(|(id, v)| (id.0, v)))?,
            "rolling": rolling, "extremes": extremes, "rsi": rsi,
            "calls": scalar_entries(self.ta_state.call_state.iter().map(|(id, v)| (id.0, v)))?,
            "first_close": optional_float(self.first_bar_close), "last_index": self.last_bar_index,
            "last_time": self.last_bar_time, "left_time": self.chart_visible_left_time,
            "right_time": self.chart_visible_right_time, "previous_bar_time": self.previous_bar_time,
            "previous_close": optional_float(self.price_flow_previous_close),
            "previous_volume": optional_float(self.price_flow_previous_volume), "flow": flow,
            "vwap_sum": float(self.vwap_weighted_sum), "vwap_volume": float(self.vwap_volume_sum),
            "position_history": self.strategy_position_size_at_script_pass.iter().copied().map(float).collect::<Vec<_>>(),
            "position_origin": self.strategy_position_size_history_origin,
            "history_misses": self.history_dynamic_retention_misses,
            "history_max": self.history_dynamic_retention_max_bars_back,
            "history_offset": self.history_dynamic_retention_max_missed_offset,
        });
        checkpoint["payload_sha256"] = json!(payload_digest(&checkpoint)?);
        Ok(checkpoint)
    }

    /// Restore into a fresh runtime prepared from the same exact program,
    /// overrides, chart and budgets. Errors leave this receiver untouched.
    pub fn restore_external_checkpoint(
        &mut self,
        checkpoint: &Value,
        binding: &str,
    ) -> Result<(), RuntimeError> {
        if self.bars != 0 || self.external_execution.is_some() || self.execution_failed {
            return Err(external_error(
                "checkpoint restore requires a fresh runtime",
            ));
        }
        let mut payload = checkpoint.clone();
        let checksum = payload
            .as_object_mut()
            .and_then(|o| o.remove("payload_sha256"))
            .ok_or_else(|| external_error("checkpoint payload checksum missing"))?;
        if checksum.as_str() != Some(payload_digest(&payload)?.as_str()) {
            return Err(external_error("checkpoint payload checksum mismatch"));
        }
        if checkpoint["version"] != VERSION
            || checkpoint["binding"] != binding
            || checkpoint["settings"] != self.checkpoint_settings()?
        {
            return Err(external_error(
                "checkpoint version/program/settings/binding mismatch",
            ));
        }
        let mut restored = self.clone();
        restored.bars = usize_field(&checkpoint["bars"])?;
        let previous_time = optional_i64(&checkpoint["previous_time"])?;
        if restored.bars == 0 || previous_time.is_none() {
            return Err(external_error("checkpoint has no confirmed cursor"));
        }
        let passes = boolean(&checkpoint["passes"])?;
        if passes != restored.program.strategy_settings.calc_on_order_fills {
            return Err(external_error("checkpoint pass mode mismatch"));
        }
        restored.external_execution = Some(ExternalExecution {
            base: restored.bars,
            previous_time,
            frames: vec![],
            passes: passes.then(Vec::new),
            active_pass: None,
            intents: vec![],
            error: None,
        });
        restored.series_store = SeriesStore::new();
        for (id, samples) in checkpoint["series"]
            .as_object()
            .ok_or_else(|| external_error("invalid checkpoint series"))?
        {
            let id = SeriesId(
                id.parse()
                    .map_err(|_| external_error("invalid series id"))?,
            );
            if id.0 >= restored.program.next_series_id {
                return Err(external_error("unknown series id"));
            }
            let samples = array(samples)?;
            if samples.len() > restored.bars
                || restored
                    .series_retention
                    .max_depth_for(id)
                    .is_some_and(|max| samples.len() > max)
            {
                return Err(external_error(
                    "checkpoint series exceeds declared retention",
                ));
            }
            for value in samples {
                restored.series_store.commit(id, read_scalar(value)?, None);
            }
        }
        restored
            .series_store
            .set_current_bar(usize_field(&checkpoint["series_bar"])?);
        for value in array(&checkpoint["active_series"])? {
            let id = SeriesId(u32_field(value)?);
            if id.0 >= restored.program.next_series_id || !restored.active_series.insert(id) {
                return Err(external_error("invalid active series id"));
            }
        }
        restored.current_symbols = read_entries(&checkpoint["symbols"])?
            .into_iter()
            .map(|(id, v)| (SymbolId(id), v))
            .collect();
        restored.current_series = read_entries(&checkpoint["current_series"])?
            .into_iter()
            .map(|(id, v)| (SeriesId(id), v))
            .collect();
        restored.var_store = read_entries(&checkpoint["var"])?
            .into_iter()
            .map(|(id, v)| (VarSlotId(id), v))
            .collect();
        if restored.current_symbols.keys().any(|id| {
            !restored
                .program
                .symbols
                .iter()
                .any(|symbol| symbol.id == *id)
        }) || restored
            .current_series
            .keys()
            .any(|id| id.0 >= restored.program.next_series_id)
            || restored
                .var_store
                .keys()
                .any(|id| id.0 >= restored.program.next_var_slot_id)
        {
            return Err(external_error(
                "unknown checkpoint symbol/series/variable id",
            ));
        }
        for item in array(&checkpoint["rolling"])? {
            if u32_field(&item[0][1])? >= restored.program.next_call_site_id {
                return Err(external_error("unknown rolling checkpoint call id"));
            }
            if restored
                .ta_state
                .rolling_windows
                .insert(
                    read_rolling_key(&item[0])?,
                    RollingWindowState::restore_external_checkpoint(&item[1])?,
                )
                .is_some()
            {
                return Err(external_error("duplicate rolling checkpoint"));
            }
        }
        restored.ta_state.extreme_windows = restore_states(
            &checkpoint["extremes"],
            RollingExtremeState::restore_external_checkpoint,
        )?;
        restored.ta_state.rsi_state =
            restore_states(&checkpoint["rsi"], RsiState::restore_external_checkpoint)?;
        restored.ta_state.call_state = read_entries(&checkpoint["calls"])?
            .into_iter()
            .map(|(id, v)| (CallSiteId(id), v))
            .collect();
        if restored
            .ta_state
            .call_state
            .keys()
            .chain(restored.ta_state.rsi_state.keys())
            .chain(restored.ta_state.extreme_windows.keys())
            .any(|id| id.0 >= restored.program.next_call_site_id)
        {
            return Err(external_error("unknown TA checkpoint call id"));
        }
        restored.first_bar_close = read_optional_float(&checkpoint["first_close"])?;
        restored.last_bar_index = optional_usize(&checkpoint["last_index"])?;
        restored.last_bar_time = optional_i64(&checkpoint["last_time"])?;
        restored.chart_visible_left_time = optional_i64(&checkpoint["left_time"])?;
        restored.chart_visible_right_time = optional_i64(&checkpoint["right_time"])?;
        restored.previous_bar_time = optional_i64(&checkpoint["previous_bar_time"])?;
        if restored.series_store.current_bar().checked_add(1) != Some(restored.bars)
            || restored.last_bar_index != Some(restored.bars - 1)
            || restored.last_bar_time != previous_time
            || restored.previous_bar_time != previous_time
            || restored.chart_visible_right_time != previous_time
            || restored
                .chart_visible_left_time
                .is_none_or(|left| Some(left) > previous_time)
            || restored.first_bar_close.is_none()
        {
            return Err(external_error("checkpoint calculation cursor mismatch"));
        }
        restored.price_flow_previous_close = read_optional_float(&checkpoint["previous_close"])?;
        restored.price_flow_previous_volume = read_optional_float(&checkpoint["previous_volume"])?;
        let flow = array(&checkpoint["flow"])?;
        let targets = [
            &mut restored.accdist_state,
            &mut restored.accdist_current,
            &mut restored.iii_current,
            &mut restored.nvi_state,
            &mut restored.nvi_current,
            &mut restored.obv_state,
            &mut restored.obv_current,
            &mut restored.pvi_state,
            &mut restored.pvi_current,
            &mut restored.pvt_state,
            &mut restored.pvt_current,
            &mut restored.vwap_current,
            &mut restored.wad_state,
            &mut restored.wad_current,
            &mut restored.wvad_current,
        ];
        if flow.len() != targets.len() {
            return Err(external_error("invalid flow checkpoint"));
        }
        for (target, value) in targets.into_iter().zip(flow) {
            *target = read_scalar(value)?;
        }
        restored.vwap_weighted_sum = read_float(&checkpoint["vwap_sum"])?;
        restored.vwap_volume_sum = read_float(&checkpoint["vwap_volume"])?;
        restored.strategy_position_size_at_script_pass = AppendHistory::from_values(
            array(&checkpoint["position_history"])?
                .iter()
                .map(read_float)
                .collect::<Result<Vec<_>, _>>()?,
        );
        restored.strategy_position_size_history_origin =
            usize_field(&checkpoint["position_origin"])?;
        let position_len = restored.strategy_position_size_at_script_pass.len();
        if restored.strategy_position_size_history_depth == Some(0) {
            if position_len != 0 || restored.strategy_position_size_history_origin != 0 {
                return Err(external_error("unexpected checkpoint position history"));
            }
        } else if restored
            .strategy_position_size_history_origin
            .checked_add(position_len)
            != Some(restored.bars)
            || restored
                .strategy_position_size_history_depth
                .is_some_and(|depth| position_len > depth.saturating_add(1))
        {
            return Err(external_error(
                "checkpoint position history cursor mismatch",
            ));
        }
        restored.history_dynamic_retention_misses = usize_field(&checkpoint["history_misses"])?;
        restored.history_dynamic_retention_max_bars_back =
            optional_usize(&checkpoint["history_max"])?;
        restored.history_dynamic_retention_max_missed_offset =
            optional_usize(&checkpoint["history_offset"])?;
        // The next bar starts a new output interval. This does not discard any
        // calculation series or TA state and is not a lookback reconstruction.
        restored.display_origin = restored.bars;
        restored.stored_origin = restored.bars;
        *self = restored;
        Ok(())
    }

    /// Drop render-only accumulated output after a host has consumed a turn.
    /// Valid only for this explicitly qualified intent-only checkpoint profile.
    pub fn compact_external_outputs(&mut self) {
        self.apply_display_origin(self.bars);
    }
}

fn restore_states<T>(
    value: &Value,
    restore: impl Fn(&Value) -> Result<T, RuntimeError>,
) -> Result<HashMap<CallSiteId, T>, RuntimeError> {
    value
        .as_object()
        .ok_or_else(|| external_error("invalid checkpoint state map"))?
        .iter()
        .map(|(id, v)| {
            Ok((
                CallSiteId(
                    id.parse()
                        .map_err(|_| external_error("invalid checkpoint call id"))?,
                ),
                restore(v)?,
            ))
        })
        .collect()
}
