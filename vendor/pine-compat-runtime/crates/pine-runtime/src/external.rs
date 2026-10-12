//! Host-neutral BAR_CLOSE intent evaluation with authoritative account feedback.
//! V1 deliberately excludes native fills and fill-triggered/realtime recalculation.
use crate::builtins::args::call_arg_expr;
use crate::{HistoricalRuntime, PineValue, RuntimeError};
use pine_ir::{HirCallArg, ScriptMode, StrategyDefaultQuantity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalAccountFrame {
    pub time: i64,
    pub position_size: f64,
    pub position_avg_price: Option<f64>,
    pub equity: f64,
    pub initial_capital: f64,
    pub netprofit: f64,
    pub openprofit: f64,
}

/// One scheduled evaluation, with only the market/account state visible then.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalPass {
    pub account: ExternalAccountFrame,
    pub bar: crate::Bar,
    pub event_time_ms: i64,
    pub confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_data: Option<Vec<ExternalRequestStream>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRequestStream {
    pub symbol: String,
    pub timeframe: String,
    pub bars: Vec<crate::Bar>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalOrderIntent {
    pub bar_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pass_index: Option<usize>,
    pub action: String,
    pub id: String,
    pub direction: Option<String>,
    pub qty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qty_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_entry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trail_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trail_points: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trail_offset: Option<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExternalExecution {
    pub frames: Vec<ExternalAccountFrame>,
    pub passes: Option<Vec<Vec<ExternalPass>>>,
    pub active_pass: Option<(usize, ExternalPass)>,
    pub intents: Vec<ExternalOrderIntent>,
    pub error: Option<String>,
}

pub(crate) fn external_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError {
        message: format!("E_EXTERNAL_UNSUPPORTED: {}", message.into()),
    }
}

impl HistoricalRuntime<'_> {
    pub(crate) fn prepare_external_window(&mut self, skip: usize) -> Result<(), RuntimeError> {
        if skip == 0 {
            return Ok(());
        }
        if let Some(state) = &mut self.external_execution {
            if state.frames.len() < skip
                || state
                    .passes
                    .as_ref()
                    .is_some_and(|groups| groups.len() < skip)
            {
                return Err(external_error(
                    "missing feedback for the historical execution window",
                ));
            }
            state.frames.drain(..skip);
            if let Some(groups) = &mut state.passes {
                groups.drain(..skip);
            }
        }
        Ok(())
    }

    pub(crate) fn bind_external_request_data(&mut self) -> Result<(), RuntimeError> {
        let Some(streams) = self
            .external_execution
            .as_ref()
            .and_then(|state| state.active_pass.as_ref())
            .and_then(|(_, pass)| pass.request_data.as_ref())
        else {
            return Ok(());
        };
        let mut provider = crate::InMemoryRequestDataProvider::new();
        for stream in streams {
            if stream.symbol.trim().is_empty() {
                return Err(external_error("empty requested symbol"));
            }
            let timeframe = crate::RequestTimeframe::parse(&stream.timeframe)
                .map_err(|err| external_error(err.to_string()))?;
            provider
                .insert(
                    crate::RequestKey::new(&stream.symbol, timeframe),
                    stream.bars.clone(),
                )
                .map_err(|err| external_error(err.to_string()))?;
        }
        self.request_environment = crate::RequestEnvironment::new(
            self.request_environment.chart().clone(),
            std::sync::Arc::new(provider),
        );
        self.request_cache.clear();
        self.request_evaluations.clear();
        Ok(())
    }

    pub fn with_external_accounts(
        self,
        frames: Vec<ExternalAccountFrame>,
    ) -> Result<Self, RuntimeError> {
        self.with_external_accounts_and_passes(frames, None)
    }

    pub fn with_external_accounts_and_passes(
        mut self,
        frames: Vec<ExternalAccountFrame>,
        passes: Option<Vec<Vec<ExternalPass>>>,
    ) -> Result<Self, RuntimeError> {
        if self.bars != 0 || self.execution_failed || self.external_execution.is_some() {
            return Err(external_error(
                "external feedback must be bound to a fresh runtime",
            ));
        }
        self.validate_external_feedback(&frames, &passes)?;
        self.external_execution = Some(ExternalExecution {
            frames,
            passes,
            active_pass: None,
            intents: vec![],
            error: None,
        });
        Ok(self)
    }

    fn validate_external_feedback(
        &self,
        frames: &[ExternalAccountFrame],
        passes: &Option<Vec<Vec<ExternalPass>>>,
    ) -> Result<(), RuntimeError> {
        let settings = self.program.strategy_settings;
        if self.program.script_mode != ScriptMode::Strategy
            || settings.calc_on_order_fills != passes.is_some()
            || settings.calc_on_every_tick
            || settings.process_orders_on_close
            || settings.use_bar_magnifier
            || settings.slippage_ticks != 0.0
            || settings.commission.is_some()
            || !matches!(
                settings.default_qty,
                Some(StrategyDefaultQuantity::Fixed(_))
            )
        {
            return Err(external_error(
                "V1 requires a bar-close, next-event, fixed-quantity strategy without native execution settings",
            ));
        }
        if let Some(groups) = passes {
            if groups.len() != frames.len()
                || groups
                    .iter()
                    .any(|group| group.is_empty() || group.len() > 65)
            {
                return Err(external_error("invalid execution pass count"));
            }
            for (group_index, (group, frame)) in groups.iter().zip(frames).enumerate() {
                if group_index + 1 < groups.len() && !group.last().unwrap().confirmed {
                    return Err(external_error("only the final bar may be unconfirmed"));
                }
                for (index, pass) in group.iter().enumerate() {
                    let bar = pass.bar;
                    if pass.account.time != frame.time
                        || bar.time != frame.time
                        || pass.event_time_ms < bar.time
                        || (index > 0
                            && (group[index - 1].confirmed
                                || group[index - 1].event_time_ms > pass.event_time_ms))
                        || [bar.open, bar.high, bar.low, bar.close, bar.volume]
                            .iter()
                            .any(|v| !v.is_finite())
                        || bar.high < bar.open.max(bar.close)
                        || bar.low > bar.open.min(bar.close)
                        || bar.volume < 0.0
                    {
                        return Err(external_error("invalid execution pass state"));
                    }
                }
            }
        }
        for frame in frames
            .iter()
            .chain(passes.iter().flatten().flatten().map(|pass| &pass.account))
        {
            if [
                frame.position_size,
                frame.equity,
                frame.initial_capital,
                frame.netprofit,
                frame.openprofit,
            ]
            .iter()
            .any(|value| !value.is_finite())
                || frame
                    .position_avg_price
                    .is_some_and(|value| !value.is_finite() || value <= 0.0)
                || (frame.position_size != 0.0 && frame.position_avg_price.is_none())
            {
                return Err(external_error("invalid account feedback"));
            }
        }
        Ok(())
    }

    /// Add feedback for newly discovered confirmed bars. Existing feedback is immutable.
    pub fn extend_external_accounts_and_passes(
        &mut self,
        frames: Vec<ExternalAccountFrame>,
        passes: Option<Vec<Vec<ExternalPass>>>,
    ) -> Result<(), RuntimeError> {
        self.validate_external_feedback(&frames, &passes)?;
        let state = self
            .external_execution
            .as_mut()
            .ok_or_else(|| external_error("external mode is not active"))?;
        if self.execution_failed
            || state.frames.len() != self.bars
            || state.passes.is_some() != passes.is_some()
            || state
                .passes
                .as_ref()
                .and_then(|g| g.last())
                .is_some_and(|g| !g.last().unwrap().confirmed)
            || frames.windows(2).any(|w| w[0].time >= w[1].time)
            || state
                .frames
                .last()
                .zip(frames.first())
                .is_some_and(|(old, new)| old.time >= new.time)
        {
            return Err(external_error(
                "invalid incremental external feedback boundary",
            ));
        }
        state.frames.extend(frames);
        if let (Some(existing), Some(groups)) = (&mut state.passes, passes) {
            existing.extend(groups);
        }
        Ok(())
    }

    pub fn external_intents(&self) -> Result<&[ExternalOrderIntent], RuntimeError> {
        let state = self
            .external_execution
            .as_ref()
            .ok_or_else(|| external_error("external mode is not active"))?;
        if let Some(error) = &state.error {
            return Err(external_error(error));
        }
        Ok(&state.intents)
    }

    pub(crate) fn external_value(&mut self, name: &str) -> Option<PineValue> {
        let state = self.external_execution.as_mut()?;
        if let Some((index, pass)) = &state.active_pass {
            match name {
                "barstate.isconfirmed" => return Some(PineValue::Bool(pass.confirmed)),
                "barstate.isnew" => return Some(PineValue::Bool(*index == 0)),
                "barstate.islastconfirmedhistory" if !pass.confirmed => {
                    return Some(PineValue::Bool(false));
                }
                _ => {}
            }
        }
        if !name.starts_with("strategy.") {
            return None;
        }
        let constant = crate::builtins::variables::eval_static_builtin_value(name);
        if constant != PineValue::Void {
            return Some(constant);
        }
        let Some(frame) = state
            .active_pass
            .as_ref()
            .map(|(_, pass)| &pass.account)
            .or_else(|| state.frames.get(self.bars))
        else {
            state.error = Some("missing account feedback".into());
            return Some(PineValue::Na);
        };
        Some(match name {
            "strategy.position_size" => PineValue::Float(frame.position_size),
            "strategy.position_avg_price" => frame
                .position_avg_price
                .map_or(PineValue::Na, PineValue::Float),
            "strategy.equity" => PineValue::Float(frame.equity),
            "strategy.initial_capital" => PineValue::Float(frame.initial_capital),
            "strategy.netprofit" => PineValue::Float(frame.netprofit),
            "strategy.openprofit" => PineValue::Float(frame.openprofit),
            _ => {
                state.error = Some(format!("account field {name}"));
                PineValue::Na
            }
        })
    }

    pub(crate) fn eval_external_call(
        &mut self,
        callee: &str,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let names: &[&str] = match callee {
            "strategy.entry" => &["id", "direction", "qty", "limit", "stop"],
            "strategy.close" => &["id", "qty", "qty_percent"],
            "strategy.exit" => &[
                "id",
                "from_entry",
                "stop",
                "limit",
                "profit",
                "loss",
                "trail_price",
                "trail_points",
                "trail_offset",
                "qty",
                "qty_percent",
            ],
            "strategy.cancel" => &["id"],
            "strategy.cancel_all" => &[],
            "strategy.close_all" => &[],
            _ => return Err(external_error(format!("order API {callee}"))),
        };
        for (index, arg) in args.iter().enumerate() {
            if arg.name.as_deref() == Some(pine_ir::OMITTED_BUILTIN_ARG) {
                continue;
            }
            if arg.name.as_deref().map_or(index >= names.len(), |name| {
                !names.contains(&name) && name != "when"
            }) {
                return Err(external_error("unsupported external order argument"));
            }
        }
        if let Some(arg) = args.iter().find(|arg| arg.name.as_deref() == Some("when"))
            && self.eval_expr(&arg.value)? != PineValue::Bool(true)
        {
            return Ok(PineValue::Void);
        }
        let id = if ["strategy.close_all", "strategy.cancel_all"].contains(&callee) {
            String::new()
        } else {
            match self.eval_expr(
                call_arg_expr(args, 0, "id").ok_or_else(|| external_error("id is required"))?,
            )? {
                PineValue::String(id) if !id.is_empty() => id,
                _ => return Err(external_error("id must be a nonempty string")),
            }
        };
        let (direction, qty) = if callee == "strategy.entry" {
            let value = self.eval_expr(
                call_arg_expr(args, 1, "direction")
                    .ok_or_else(|| external_error("direction required"))?,
            )?;
            let direction = match value {
                PineValue::String(value) if value == "strategy.long" => "long",
                PineValue::String(value) if value == "strategy.short" => "short",
                _ => return Err(external_error("invalid direction")),
            };
            let qty = match call_arg_expr(args, 2, "qty") {
                Some(expr) => self
                    .eval_expr(expr)?
                    .as_f64()
                    .ok_or_else(|| external_error("quantity must be numeric"))?,
                None => match self.program.strategy_settings.default_qty {
                    Some(StrategyDefaultQuantity::Fixed(qty)) => qty,
                    _ => unreachable!(),
                },
            };
            if !qty.is_finite() || qty <= 0.0 {
                return Err(external_error("quantity must be positive and finite"));
            }
            (Some(direction.to_string()), Some(qty))
        } else {
            (None, None)
        };
        let mut numbers = [None; 9];
        for (slot, name) in [
            "limit",
            "stop",
            "qty_percent",
            "qty",
            "profit",
            "loss",
            "trail_price",
            "trail_points",
            "trail_offset",
        ]
        .iter()
        .enumerate()
        {
            // Entry quantity has already been evaluated exactly once above.
            if callee == "strategy.entry" && *name == "qty" {
                continue;
            }
            let position = match (callee, *name) {
                ("strategy.entry", "limit") => 3,
                ("strategy.entry", "stop") => 4,
                ("strategy.entry", "qty") => 2,
                ("strategy.close", "qty") => 1,
                ("strategy.close", "qty_percent") => 2,
                ("strategy.exit", "stop") => 2,
                ("strategy.exit", "limit") => 3,
                ("strategy.exit", "qty") => 9,
                ("strategy.exit", "qty_percent") => 10,
                ("strategy.exit", "profit") => 4,
                ("strategy.exit", "loss") => 5,
                ("strategy.exit", "trail_price") => 6,
                ("strategy.exit", "trail_points") => 7,
                ("strategy.exit", "trail_offset") => 8,
                _ => usize::MAX,
            };
            let expr = call_arg_expr(args, position, name);
            if let Some(expr) = expr {
                let value = self
                    .eval_expr(expr)?
                    .as_f64()
                    .ok_or_else(|| external_error("order value must be numeric"))?;
                if !value.is_finite() || value <= 0.0 || (slot == 2 && value > 100.0) {
                    return Err(external_error("invalid price or quantity"));
                }
                numbers[slot] = Some(value);
            }
        }
        if numbers[2].is_some() && (qty.is_some() || numbers[3].is_some()) {
            return Err(external_error("choose qty or qty_percent"));
        }
        let from_entry = if callee == "strategy.exit" {
            let trail_activation = numbers[6].is_some() || numbers[7].is_some();
            if trail_activation != numbers[8].is_some()
                || (numbers[6].is_some() && numbers[7].is_some())
            {
                return Err(external_error(
                    "trailing exit requires one activation and trail_offset",
                ));
            }
            if [numbers[0], numbers[1], numbers[4], numbers[5], numbers[8]]
                .iter()
                .all(Option::is_none)
            {
                return Err(external_error(
                    "exit requires a price, distance or trailing bracket",
                ));
            }
            match call_arg_expr(args, 1, "from_entry") {
                Some(expr) => match self.eval_expr(expr)? {
                    PineValue::String(value) => Some(value),
                    _ => return Err(external_error("from_entry must be a string")),
                },
                None => None,
            }
        } else {
            None
        };
        let state = self.external_execution.as_mut().unwrap();
        state.intents.push(ExternalOrderIntent {
            bar_index: self.bars,
            pass_index: state.active_pass.as_ref().map(|(index, _)| *index),
            action: callee.trim_start_matches("strategy.").to_owned(),
            id,
            direction,
            qty: qty.or(numbers[3]),
            limit: numbers[0],
            stop: numbers[1],
            qty_percent: numbers[2],
            from_entry,
            profit: numbers[4],
            loss: numbers[5],
            trail_price: numbers[6],
            trail_points: numbers[7],
            trail_offset: numbers[8],
        });
        Ok(PineValue::Void)
    }
}
