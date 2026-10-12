use pine_ir::{CallSiteId, HirCallArg, StrategyDefaultQuantity};

use crate::builtins::args::call_arg_expr;
use crate::strategy::{
    LossLimitBracketSpec, LossProfitBracketSpec, StopProfitBracketSpec, StrategyExitMetadata,
    TrailPointsExitSpec, TrailPriceExitSpec,
};
use crate::*;

mod exit_placements;
mod fields;
mod metadata;

#[derive(Clone, Copy)]
enum StrategyExitQuantityArg {
    Full,
    Fixed(f64),
    Percent(f64),
}

struct RuntimeExitTicksPlacement {
    id: String,
    from_entry: String,
    ticks: f64,
    mintick: f64,
    quantity: StrategyExitQuantityArg,
    bar_index: usize,
    metadata: StrategyExitMetadata,
}

struct RuntimeExitBracketPlacement {
    id: String,
    from_entry: String,
    downside_price: f64,
    upside_price: f64,
    quantity: StrategyExitQuantityArg,
    bar_index: usize,
    metadata: StrategyExitMetadata,
}

impl<'a> HistoricalRuntime<'a> {
    fn default_order_qty(
        &self,
        equity: f64,
        close: f64,
        is_long: bool,
        market: bool,
    ) -> Option<f64> {
        // A cash market order is sized from the expected fill price, including
        // its configured slippage. The actual next-bar open is not known yet.
        let price = if market
            && matches!(
                self.program.strategy_settings.default_qty,
                Some(StrategyDefaultQuantity::Cash(_))
            ) {
            let slippage = self.program.strategy_settings.slippage_ticks
                * self.request_environment.chart().min_tick();
            close + if is_long { slippage } else { -slippage }
        } else {
            close
        };
        match self.program.strategy_settings.default_qty {
            Some(StrategyDefaultQuantity::PercentOfEquity(percent))
                if equity.is_finite() && equity > 0.0 && price.is_finite() && price > 0.0 =>
            {
                Some(crate::strategy::percent_of_equity_order_qty(
                    equity,
                    percent,
                    price,
                    self.program.strategy_settings.commission,
                    self.request_environment.chart().configured_quantity_scale(),
                ))
            }
            _ => self
                .program
                .strategy_settings
                .default_entry_qty(equity, price)
                .map(|qty| {
                    crate::strategy::quantity_on_chart_grid(
                        qty,
                        self.request_environment.chart().configured_quantity_scale(),
                    )
                }),
        }
    }

    pub(crate) fn eval_strategy_call(
        &mut self,
        callee: &str,
        _call_site_id: CallSiteId,
        args: &[HirCallArg],
    ) -> Option<Result<PineValue, RuntimeError>> {
        if self.external_execution.is_some() && callee.starts_with("strategy.") {
            return Some(self.eval_external_call(callee, args));
        }
        Some(match callee {
            "strategy.convert_to_account" | "strategy.convert_to_symbol" => {
                self.eval_strategy_currency_conversion(args)
            }
            "strategy.default_entry_qty" => self.eval_strategy_default_entry_qty(args),
            "strategy.entry" => self.eval_strategy_entry(args),
            "strategy.order" => self.eval_strategy_order(args),
            "strategy.close" => self.eval_strategy_close(args),
            "strategy.close_all" => self.eval_strategy_close_all(args),
            "strategy.cancel" => self.eval_strategy_cancel(args),
            "strategy.cancel_all" => self.eval_strategy_cancel_all(),
            "strategy.exit" => {
                let result = self.eval_strategy_exit(args);
                self.strategy_broker.set_next_exit_oca_name(None);
                result
            }
            "strategy.risk.allow_entry_in" => self.eval_strategy_risk_allow_entry_in(args),
            "strategy.risk.max_position_size" => self.eval_strategy_risk_max_position_size(args),
            "strategy.risk.max_drawdown" => self.eval_strategy_risk_max_drawdown(args),
            "strategy.risk.max_intraday_loss" => self.eval_strategy_risk_max_intraday_loss(args),
            "strategy.risk.max_intraday_filled_orders" => {
                self.eval_strategy_risk_max_intraday_filled_orders(args)
            }
            "strategy.risk.max_cons_loss_days" => self.eval_strategy_risk_max_cons_loss_days(args),
            "strategy.closedtrades.entry_price"
            | "strategy.closedtrades.entry_comment"
            | "strategy.closedtrades.entry_id"
            | "strategy.closedtrades.exit_price"
            | "strategy.closedtrades.exit_comment"
            | "strategy.closedtrades.exit_id"
            | "strategy.closedtrades.entry_bar_index"
            | "strategy.closedtrades.exit_bar_index"
            | "strategy.closedtrades.entry_time"
            | "strategy.closedtrades.exit_time"
            | "strategy.closedtrades.commission"
            | "strategy.closedtrades.size"
            | "strategy.closedtrades.profit"
            | "strategy.closedtrades.profit_percent"
            | "strategy.closedtrades.max_runup"
            | "strategy.closedtrades.max_runup_percent"
            | "strategy.closedtrades.max_drawdown"
            | "strategy.closedtrades.max_drawdown_percent" => {
                self.eval_strategy_closed_trade_field(callee, args)
            }
            "strategy.opentrades.entry_price"
            | "strategy.opentrades.entry_comment"
            | "strategy.opentrades.entry_id"
            | "strategy.opentrades.entry_bar_index"
            | "strategy.opentrades.entry_time"
            | "strategy.opentrades.size"
            | "strategy.opentrades.profit"
            | "strategy.opentrades.profit_percent"
            | "strategy.opentrades.commission"
            | "strategy.opentrades.max_runup"
            | "strategy.opentrades.max_runup_percent"
            | "strategy.opentrades.max_drawdown"
            | "strategy.opentrades.max_drawdown_percent" => {
                self.eval_strategy_open_trade_field(callee, args)
            }
            _ => return None,
        })
    }

    fn eval_strategy_currency_conversion(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(value_expr) = call_arg_expr(args, 0, "value") else {
            return Ok(PineValue::Na);
        };
        Ok(self
            .eval_expr(value_expr)?
            .as_f64()
            .map_or(PineValue::Na, PineValue::Float))
    }

    fn eval_strategy_default_entry_qty(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(fill_price_expr) = call_arg_expr(args, 0, "fill_price") else {
            return Ok(PineValue::Na);
        };
        let fill_price = self
            .eval_expr(fill_price_expr)?
            .as_f64()
            .unwrap_or(f64::NAN);
        let Some(bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.default_entry_qty` requires an active bar".to_owned(),
            });
        };
        let equity = self.strategy_broker.equity_value(bar.close);
        let qty = match self.program.strategy_settings.default_qty {
            Some(StrategyDefaultQuantity::PercentOfEquity(percent))
                if equity.is_finite()
                    && equity > 0.0
                    && fill_price.is_finite()
                    && fill_price > 0.0 =>
            {
                Some(crate::strategy::percent_of_equity_order_qty(
                    equity,
                    percent,
                    fill_price,
                    self.program.strategy_settings.commission,
                    self.request_environment.chart().configured_quantity_scale(),
                ))
            }
            _ => self
                .program
                .strategy_settings
                .default_entry_qty(equity, fill_price)
                .map(|qty| {
                    crate::strategy::quantity_on_chart_grid(
                        qty,
                        self.request_environment.chart().configured_quantity_scale(),
                    )
                }),
        };
        Ok(qty.map_or(PineValue::Na, PineValue::Float))
    }

    fn eval_strategy_risk_allow_entry_in(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(value_expr) = call_arg_expr(args, 0, "value") else {
            return Ok(PineValue::Void);
        };
        let PineValue::String(value) = self.eval_expr(value_expr)? else {
            return Ok(PineValue::Void);
        };
        self.strategy_broker.set_allow_entry_in(&value);
        Ok(PineValue::Void)
    }

    fn eval_strategy_risk_max_drawdown(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(value_expr) = call_arg_expr(args, 0, "value") else {
            return Ok(PineValue::Void);
        };
        let Some(type_expr) = call_arg_expr(args, 1, "type") else {
            return Ok(PineValue::Void);
        };
        let Some(value) = self.eval_expr(value_expr)?.as_f64() else {
            return Ok(PineValue::Void);
        };
        let PineValue::String(type_name) = self.eval_expr(type_expr)? else {
            return Ok(PineValue::Void);
        };
        let alert_message = match call_arg_expr(args, 2, "alert_message") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(message) => Some(message),
                _ => None,
            },
            None => None,
        };
        self.strategy_broker
            .set_max_drawdown(value, &type_name, alert_message);
        Ok(PineValue::Void)
    }

    fn eval_strategy_risk_max_intraday_loss(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(value_expr) = call_arg_expr(args, 0, "value") else {
            return Ok(PineValue::Void);
        };
        let Some(type_expr) = call_arg_expr(args, 1, "type") else {
            return Ok(PineValue::Void);
        };
        let Some(value) = self.eval_expr(value_expr)?.as_f64() else {
            return Ok(PineValue::Void);
        };
        let PineValue::String(type_name) = self.eval_expr(type_expr)? else {
            return Ok(PineValue::Void);
        };
        let alert_message = match call_arg_expr(args, 2, "alert_message") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(message) => Some(message),
                _ => None,
            },
            None => None,
        };
        self.strategy_broker
            .set_max_intraday_loss(value, &type_name, alert_message);
        Ok(PineValue::Void)
    }

    fn eval_strategy_risk_max_intraday_filled_orders(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(count_expr) = call_arg_expr(args, 0, "count") else {
            return Ok(PineValue::Void);
        };
        let Some(count) = self.eval_expr(count_expr)?.as_f64() else {
            return Ok(PineValue::Void);
        };
        let alert_message = match call_arg_expr(args, 1, "alert_message") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(message) => Some(message),
                _ => None,
            },
            None => None,
        };
        self.strategy_broker
            .set_max_intraday_filled_orders(count, alert_message);
        Ok(PineValue::Void)
    }

    fn eval_strategy_risk_max_cons_loss_days(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(count_expr) = call_arg_expr(args, 0, "count") else {
            return Ok(PineValue::Void);
        };
        let Some(count) = self.eval_expr(count_expr)?.as_f64() else {
            return Ok(PineValue::Void);
        };
        let alert_message = match call_arg_expr(args, 1, "alert_message") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(message) => Some(message),
                _ => None,
            },
            None => None,
        };
        self.strategy_broker
            .set_max_cons_loss_days(count, alert_message);
        Ok(PineValue::Void)
    }

    fn eval_strategy_risk_max_position_size(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let Some(contracts_expr) = call_arg_expr(args, 0, "contracts") else {
            return Ok(PineValue::Void);
        };
        let Some(contracts) = self.eval_expr(contracts_expr)?.as_f64() else {
            return Ok(PineValue::Void);
        };
        self.strategy_broker.set_max_position_size(contracts);
        Ok(PineValue::Void)
    }

    fn eval_strategy_entry(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.entry` requires an active bar".to_owned(),
            });
        };
        if let Some(when_expr) = call_arg_expr(args, 10, "when") {
            match self.eval_expr(when_expr)? {
                PineValue::Bool(true) => {}
                _ => return Ok(PineValue::Void),
            }
        }
        let Some(id_expr) = call_arg_expr(args, 0, "id") else {
            return Ok(PineValue::Void);
        };
        let Some(direction_expr) = call_arg_expr(args, 1, "direction") else {
            return Ok(PineValue::Void);
        };

        let id = match self.eval_expr(id_expr)? {
            PineValue::String(value) => value,
            _ => return Ok(PineValue::Void),
        };
        let direction = self.eval_expr(direction_expr)?;
        let legacy_boolean_direction = self.program.language_version.is_some_and(|v| v <= 4);
        let is_long = matches!(&direction, PineValue::String(value) if value == "strategy.long")
            || (legacy_boolean_direction && direction == PineValue::Bool(true));
        let is_short = matches!(&direction, PineValue::String(value) if value == "strategy.short")
            || (legacy_boolean_direction && direction == PineValue::Bool(false));
        if !is_long && !is_short {
            return Ok(PineValue::Void);
        }
        let qty_expr = args
            .iter()
            .find(|arg| arg.name.as_deref() == Some("qty"))
            .or_else(|| args.get(2).filter(|arg| arg.name.is_none()))
            .map(|arg| &arg.value);
        let limit_expr = args
            .iter()
            .find(|arg| arg.name.as_deref() == Some("limit"))
            .or_else(|| args.get(3).filter(|arg| arg.name.is_none()))
            .map(|arg| &arg.value);
        let stop_expr = args
            .iter()
            .find(|arg| arg.name.as_deref() == Some("stop"))
            .or_else(|| args.get(4).filter(|arg| arg.name.is_none()))
            .map(|arg| &arg.value);
        let metadata = self.eval_strategy_entry_metadata(args)?;
        let optional_entry_arg_expr = |index: usize, name: &str| {
            args.iter()
                .find(|arg| arg.name.as_deref() == Some(name))
                .or_else(|| args.get(index).filter(|arg| arg.name.is_none()))
                .map(|arg| &arg.value)
        };
        let oca_name = match optional_entry_arg_expr(5, "oca_name") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(value) => Some(value),
                _ => None,
            },
            None => None,
        };
        let oca_type = match optional_entry_arg_expr(6, "oca_type") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(value) => Some(value),
                _ => None,
            },
            None => None,
        };

        let explicit_qty = match qty_expr {
            Some(expr) => self.eval_expr(expr)?.as_f64(),
            None => None,
        };
        let qty = if let Some(qty) = explicit_qty {
            qty
        } else {
            let equity = self.strategy_broker.equity_value(bar.close);
            self.default_order_qty(
                equity,
                bar.close,
                is_long,
                limit_expr.is_none() && stop_expr.is_none(),
            )
            .unwrap_or(f64::NAN)
        };
        if qty == 0.0 {
            return Ok(PineValue::Void);
        }
        if !qty.is_finite() || !(0.0..=1_000_000_000_000.0).contains(&qty) {
            return Err(RuntimeError {
                message: format!(
                    "`strategy.entry` invalid `qty` value ({qty}) on bar {}: expected a positive number no greater than 1000000000000",
                    self.bars
                ),
            });
        }
        // Validate the caller's value before rounding. Explicit quantities use
        // the same host-provided contract grid as default sizing; do not round
        // default quantities twice at floating-point grid boundaries.
        let qty = if explicit_qty.is_some() {
            crate::strategy::explicit_quantity_on_chart_grid(
                qty,
                self.request_environment.chart().configured_quantity_scale(),
            )
        } else {
            qty
        };
        // A positive explicit request can truncate to zero opening contracts.
        // Keep it pending: an opposite position still contributes the closing
        // transaction when this entry becomes eligible to fill.
        let oca_id = id.clone();
        if is_short {
            if let (Some(limit_expr), Some(stop_expr)) = (limit_expr, stop_expr) {
                let limit = self.eval_expr(limit_expr)?.as_f64().unwrap_or(f64::NAN);
                let stop = self.eval_expr(stop_expr)?.as_f64().unwrap_or(f64::NAN);
                self.strategy_broker
                    .place_pending_stop_limit_short_entry_with_metadata(
                        id, qty, stop, limit, self.bars, metadata,
                    );
            } else if let Some(stop_expr) = stop_expr {
                let stop = self.eval_expr(stop_expr)?.as_f64().unwrap_or(f64::NAN);
                self.strategy_broker
                    .place_pending_stop_short_entry_with_metadata(
                        id, qty, stop, self.bars, metadata,
                    );
            } else if let Some(limit_expr) = limit_expr {
                let limit = self.eval_expr(limit_expr)?.as_f64().unwrap_or(f64::NAN);
                self.strategy_broker
                    .place_pending_limit_short_entry_with_metadata(
                        id, qty, limit, self.bars, metadata,
                    );
            } else {
                self.strategy_broker
                    .place_pending_market_short_entry_with_metadata(id, qty, self.bars, metadata);
            }
        } else if let (Some(limit_expr), Some(stop_expr)) = (limit_expr, stop_expr) {
            let limit = self.eval_expr(limit_expr)?.as_f64().unwrap_or(f64::NAN);
            let stop = self.eval_expr(stop_expr)?.as_f64().unwrap_or(f64::NAN);
            self.strategy_broker
                .place_pending_stop_limit_long_entry_with_metadata(
                    id, qty, stop, limit, self.bars, metadata,
                );
        } else if let Some(limit_expr) = limit_expr {
            let limit = self.eval_expr(limit_expr)?.as_f64().unwrap_or(f64::NAN);
            self.strategy_broker
                .place_pending_limit_long_entry_with_metadata(id, qty, limit, self.bars, metadata);
        } else if let Some(stop_expr) = stop_expr {
            let stop = self.eval_expr(stop_expr)?.as_f64().unwrap_or(f64::NAN);
            self.strategy_broker
                .place_pending_stop_long_entry_with_metadata(id, qty, stop, self.bars, metadata);
        } else {
            self.strategy_broker
                .place_pending_market_long_entry_with_metadata(id, qty, self.bars, metadata);
        }
        if qty_expr.is_none()
            && limit_expr.is_none()
            && stop_expr.is_none()
            && let Some(StrategyDefaultQuantity::PercentOfEquity(percent)) =
                self.program.strategy_settings.default_qty
        {
            self.strategy_broker
                .set_pending_market_same_bar_percent_of_equity(&oca_id, percent);
        }
        if let Some(name) = oca_name {
            self.strategy_broker
                .assign_pending_order_oca_named(&oca_id, name, oca_type.as_deref());
        }
        Ok(PineValue::Void)
    }

    fn eval_strategy_order(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.order` requires an active bar".to_owned(),
            });
        };
        let Some(id_expr) = call_arg_expr(args, 0, "id") else {
            return Ok(PineValue::Void);
        };
        let Some(direction_expr) = call_arg_expr(args, 1, "direction") else {
            return Ok(PineValue::Void);
        };
        let optional_order_arg_expr = |index: usize, name: &str| {
            args.iter()
                .find(|arg| arg.name.as_deref() == Some(name))
                .or_else(|| args.get(index).filter(|arg| arg.name.is_none()))
                .map(|arg| &arg.value)
        };
        let qty_expr = optional_order_arg_expr(2, "qty");
        let limit_expr = optional_order_arg_expr(3, "limit");
        let stop_expr = optional_order_arg_expr(4, "stop");

        let id = match self.eval_expr(id_expr)? {
            PineValue::String(value) => value,
            _ => return Ok(PineValue::Void),
        };
        let direction = self.eval_expr(direction_expr)?;
        let explicit_qty = match qty_expr {
            Some(expr) => self.eval_expr(expr)?.as_f64(),
            None => None,
        };
        let qty = if let Some(qty) = explicit_qty {
            qty
        } else if matches!(
            &direction,
            PineValue::String(value)
                if value == "strategy.long" || value == "strategy.short"
        ) {
            let equity = self.strategy_broker.equity_value(bar.close);
            self.default_order_qty(
                equity,
                bar.close,
                matches!(&direction, PineValue::String(value) if value == "strategy.long"),
                limit_expr.is_none() && stop_expr.is_none(),
            )
            .unwrap_or(f64::NAN)
        } else {
            return Ok(PineValue::Void);
        };
        if qty == 0.0 {
            return Ok(PineValue::Void);
        }
        if !qty.is_finite() || !(0.0..=1_000_000_000_000.0).contains(&qty) {
            return Err(RuntimeError {
                message: format!(
                    "`strategy.order` invalid `qty` value ({qty}) on bar {}: expected a positive number no greater than 1000000000000",
                    self.bars
                ),
            });
        }
        let qty = if explicit_qty.is_some() {
            crate::strategy::explicit_quantity_on_chart_grid(
                qty,
                self.request_environment.chart().configured_quantity_scale(),
            )
        } else {
            qty
        };
        if qty == 0.0 {
            return Ok(PineValue::Void);
        }
        let limit = match limit_expr {
            Some(expr) => Some(self.eval_expr(expr)?.as_f64().unwrap_or(f64::NAN)),
            None => None,
        };
        let stop = match stop_expr {
            Some(expr) => Some(self.eval_expr(expr)?.as_f64().unwrap_or(f64::NAN)),
            None => None,
        };
        let metadata = self.eval_strategy_order_metadata(args)?;
        let oca_name = match optional_order_arg_expr(5, "oca_name") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(value) => Some(value),
                _ => None,
            },
            None => None,
        };
        let oca_type = match optional_order_arg_expr(6, "oca_type") {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(value) => Some(value),
                _ => None,
            },
            None => None,
        };
        let oca_id = id.clone();
        match direction {
            PineValue::String(value) if value == "strategy.long" => match (limit, stop) {
                (Some(limit), Some(stop)) => self
                    .strategy_broker
                    .place_pending_stop_limit_long_order_with_metadata(
                        id, qty, stop, limit, self.bars, metadata,
                    ),
                (Some(limit), None) => self
                    .strategy_broker
                    .place_pending_limit_long_order_with_metadata(
                        id, qty, limit, self.bars, metadata,
                    ),
                (None, Some(stop)) => self
                    .strategy_broker
                    .place_pending_stop_long_order_with_metadata(
                        id, qty, stop, self.bars, metadata,
                    ),
                (None, None) => self
                    .strategy_broker
                    .place_pending_market_long_order_with_metadata(id, qty, self.bars, metadata),
            },
            PineValue::String(value) if value == "strategy.short" => match (limit, stop) {
                (Some(limit), Some(stop)) => self
                    .strategy_broker
                    .place_pending_stop_limit_short_order_with_metadata(
                        id, qty, stop, limit, self.bars, metadata,
                    ),
                (Some(limit), None) => self
                    .strategy_broker
                    .place_pending_limit_short_order_with_metadata(
                        id, qty, limit, self.bars, metadata,
                    ),
                (None, Some(stop)) => self
                    .strategy_broker
                    .place_pending_stop_short_order_with_metadata(
                        id, qty, stop, self.bars, metadata,
                    ),
                (None, None) => self
                    .strategy_broker
                    .place_pending_market_short_order_with_metadata(id, qty, self.bars, metadata),
            },
            _ => {}
        }
        if qty_expr.is_none()
            && limit.is_none()
            && stop.is_none()
            && let Some(StrategyDefaultQuantity::PercentOfEquity(percent)) =
                self.program.strategy_settings.default_qty
        {
            self.strategy_broker
                .set_pending_market_same_bar_percent_of_equity(&oca_id, percent);
        }
        if let Some(name) = oca_name {
            self.strategy_broker
                .assign_pending_order_oca_named(&oca_id, name, oca_type.as_deref());
        }
        Ok(PineValue::Void)
    }

    fn eval_strategy_close(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.close` requires an active bar".to_owned(),
            });
        };
        if let Some(when_expr) = call_arg_expr(args, 7, "when") {
            match self.eval_expr(when_expr)? {
                PineValue::Bool(true) => {}
                _ => return Ok(PineValue::Void),
            }
        }
        let Some(id_expr) = call_arg_expr(args, 0, "id") else {
            return Ok(PineValue::Void);
        };
        let id = match self.eval_expr(id_expr)? {
            PineValue::String(value) => value,
            _ => return Ok(PineValue::Void),
        };
        let qty_expr = call_arg_expr(args, 1, "qty");
        let qty_percent_expr = call_arg_expr(args, 2, "qty_percent");
        let metadata = self.eval_strategy_close_metadata(args, 3)?;

        let quantity = if let Some(qty_expr) = qty_expr {
            let qty = self.eval_expr(qty_expr)?.as_f64().unwrap_or(f64::NAN);
            if !qty.is_finite() || qty <= 0.0 {
                self.strategy_broker
                    .with_next_close_metadata(metadata, |broker| {
                        broker.close_long_qty(id, self.bars, bar.time, bar.close, qty)
                    });
                return Ok(PineValue::Void);
            }
            crate::strategy::PendingCloseQuantity::Qty(qty)
        } else if let Some(qty_percent_expr) = qty_percent_expr {
            let qty_percent = self
                .eval_expr(qty_percent_expr)?
                .as_f64()
                .unwrap_or(f64::NAN);
            if !qty_percent.is_finite() || qty_percent <= 0.0 {
                self.strategy_broker
                    .with_next_close_metadata(metadata, |broker| {
                        broker.close_long_qty_percent(
                            id,
                            self.bars,
                            bar.time,
                            bar.close,
                            qty_percent,
                        )
                    });
                return Ok(PineValue::Void);
            }
            crate::strategy::PendingCloseQuantity::QtyPercent(qty_percent)
        } else {
            crate::strategy::PendingCloseQuantity::Full
        };
        let immediately = self.eval_strategy_immediately_arg(args, 6)?;
        if immediately {
            self.strategy_broker
                .place_pending_close_with_immediately(id, quantity, self.bars, metadata, true);
        } else {
            self.strategy_broker
                .place_pending_close(id, quantity, self.bars, metadata);
        }
        Ok(PineValue::Void)
    }

    fn eval_strategy_close_all(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(_bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.close_all` requires an active bar".to_owned(),
            });
        };
        if let Some(when_expr) = call_arg_expr(args, 4, "when")
            && !matches!(self.eval_expr(when_expr)?, PineValue::Bool(true))
        {
            return Ok(PineValue::Void);
        }
        let metadata = self.eval_strategy_close_metadata(args, 0)?;
        let immediately = self.eval_strategy_immediately_arg(args, 3)?;

        if immediately {
            self.strategy_broker
                .place_pending_close_all_with_immediately(
                    crate::strategy::PendingCloseQuantity::Full,
                    self.bars,
                    metadata,
                    true,
                );
        } else {
            self.strategy_broker.place_pending_close_all(
                crate::strategy::PendingCloseQuantity::Full,
                self.bars,
                metadata,
            );
        }
        Ok(PineValue::Void)
    }

    fn eval_strategy_cancel(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(_bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.cancel` requires an active bar".to_owned(),
            });
        };
        let Some(id_expr) = call_arg_expr(args, 0, "id") else {
            return Ok(PineValue::Void);
        };
        let id = match self.eval_expr(id_expr)? {
            PineValue::String(value) => value,
            _ => return Ok(PineValue::Void),
        };

        self.strategy_broker.cancel_pending_order(&id);
        Ok(PineValue::Void)
    }

    fn eval_strategy_cancel_all(&mut self) -> Result<PineValue, RuntimeError> {
        let Some(_bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.cancel_all` requires an active bar".to_owned(),
            });
        };

        self.strategy_broker.cancel_all_pending_orders();
        Ok(PineValue::Void)
    }

    fn eval_strategy_profit_ticks(
        &mut self,
        profit_expr: &pine_ir::HirExpr,
        extra_profit_expr: Option<&pine_ir::HirExpr>,
    ) -> Result<f64, RuntimeError> {
        let profit = self.eval_expr(profit_expr)?.as_f64().unwrap_or(f64::NAN);
        let Some(extra_expr) = extra_profit_expr else {
            return Ok(profit);
        };
        let extra = self.eval_expr(extra_expr)?.as_f64().unwrap_or(f64::NAN);
        Ok(profit.min(extra))
    }

    fn eval_strategy_exit(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(_bar) = self.current_bar else {
            return Err(RuntimeError {
                message: "`strategy.exit` requires an active bar".to_owned(),
            });
        };
        let Some(id_expr) = call_arg_expr(args, 0, "id") else {
            return Ok(PineValue::Void);
        };
        let from_entry_expr = call_arg_expr(args, 1, "from_entry");
        let stop_expr = call_arg_expr(args, 2, "stop");
        let limit_expr = call_arg_expr(args, 3, "limit");
        let explicit_profit_expr = call_arg_expr(args, 4, "profit");
        let loss_expr = call_arg_expr(args, 5, "loss");
        let trail_price_expr = call_arg_expr(args, 6, "trail_price");
        let trail_points_expr = call_arg_expr(args, 7, "trail_points");
        let trail_offset_expr = call_arg_expr(args, 8, "trail_offset");
        let legacy_trail_profit_expr = if self.program.language_version.is_some_and(|v| v <= 4)
            && explicit_profit_expr.is_some()
            && trail_points_expr.is_some()
            && trail_price_expr.is_none()
            && trail_offset_expr.is_none()
        {
            trail_points_expr
        } else {
            None
        };
        let profit_expr = explicit_profit_expr.or(legacy_trail_profit_expr);
        let extra_profit_expr = explicit_profit_expr.and(legacy_trail_profit_expr);
        let qty_expr = call_arg_expr(args, 9, "qty");
        let qty_percent_expr = call_arg_expr(args, 10, "qty_percent");
        let metadata = self.eval_strategy_exit_metadata(args)?;
        let oca_name = call_arg_expr(args, 11, "oca_name");
        let oca_name = match oca_name {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(value) if !value.is_empty() => Some(value),
                _ => None,
            },
            None => None,
        };
        self.strategy_broker.set_next_exit_oca_name(oca_name);

        let id = match self.eval_expr(id_expr)? {
            PineValue::String(value) => value,
            _ => return Ok(PineValue::Void),
        };
        let from_entry = match from_entry_expr {
            Some(expr) => match self.eval_expr(expr)? {
                PineValue::String(value) => value,
                _ => return Ok(PineValue::Void),
            },
            None => String::new(),
        };
        let qty = if let Some(qty_expr) = qty_expr {
            Some(self.eval_expr(qty_expr)?.as_f64().unwrap_or(f64::NAN))
        } else {
            None
        };
        let qty_percent = if let Some(qty_percent_expr) = qty_percent_expr {
            Some(
                self.eval_expr(qty_percent_expr)?
                    .as_f64()
                    .unwrap_or(f64::NAN),
            )
        } else {
            None
        };
        let quantity = match (qty, qty_percent) {
            (Some(qty), Some(_)) => StrategyExitQuantityArg::Fixed(qty),
            (Some(qty), None) => StrategyExitQuantityArg::Fixed(qty),
            (None, Some(qty_percent)) => StrategyExitQuantityArg::Percent(qty_percent),
            (None, None) => StrategyExitQuantityArg::Full,
        };
        let has_downside = stop_expr.is_some() || loss_expr.is_some();
        let has_upside = limit_expr.is_some() || profit_expr.is_some();
        let has_fixed_exit = has_downside || has_upside;
        let has_trailing_activation = trail_price_expr.is_some() || trail_points_expr.is_some();
        // A lone activation argument has no effect when a fixed exit exists.
        let has_trailing = legacy_trail_profit_expr.is_none()
            && (trail_offset_expr.is_some() || (has_trailing_activation && !has_fixed_exit));
        let has_single_trailing_activation =
            trail_price_expr.is_some() != trail_points_expr.is_some();
        let is_trailing_only =
            !has_fixed_exit && has_single_trailing_activation && trail_offset_expr.is_some();
        let is_stop_profit_bracket = stop_expr.is_some()
            && profit_expr.is_some()
            && loss_expr.is_none()
            && limit_expr.is_none();
        let is_loss_limit_bracket = loss_expr.is_some()
            && limit_expr.is_some()
            && stop_expr.is_none()
            && profit_expr.is_none();
        let is_loss_profit_bracket = loss_expr.is_some()
            && profit_expr.is_some()
            && stop_expr.is_none()
            && limit_expr.is_none();
        let is_omitted_absolute_single = (stop_expr.is_some() != limit_expr.is_some())
            && profit_expr.is_none()
            && loss_expr.is_none()
            && !has_trailing;
        let is_omitted_absolute_bracket = stop_expr.is_some()
            && limit_expr.is_some()
            && profit_expr.is_none()
            && loss_expr.is_none()
            && !has_trailing
            && matches!(quantity, StrategyExitQuantityArg::Full);
        let is_omitted_relative_single = (profit_expr.is_some() != loss_expr.is_some())
            && stop_expr.is_none()
            && limit_expr.is_none()
            && !has_trailing
            && matches!(quantity, StrategyExitQuantityArg::Full);
        let is_omitted_loss_profit_bracket = is_loss_profit_bracket
            && !has_trailing
            && matches!(quantity, StrategyExitQuantityArg::Full);
        let is_omitted_stop_profit_bracket = is_stop_profit_bracket
            && !has_trailing
            && matches!(quantity, StrategyExitQuantityArg::Full);
        let is_omitted_loss_limit_bracket = is_loss_limit_bracket
            && !has_trailing
            && matches!(quantity, StrategyExitQuantityArg::Full);
        let is_omitted_trail_price = is_trailing_only
            && trail_price_expr.is_some()
            && trail_points_expr.is_none()
            && matches!(quantity, StrategyExitQuantityArg::Full);
        let is_omitted_trail_points = is_trailing_only
            && trail_points_expr.is_some()
            && trail_price_expr.is_none()
            && matches!(quantity, StrategyExitQuantityArg::Full);
        if from_entry.is_empty()
            && !(is_omitted_absolute_single
                || is_omitted_absolute_bracket
                || is_omitted_relative_single
                || is_omitted_loss_profit_bracket
                || is_omitted_stop_profit_bracket
                || is_omitted_loss_limit_bracket
                || is_omitted_trail_price
                || is_omitted_trail_points)
        {
            return Ok(PineValue::Void);
        }
        let has_unsupported_entry_relative_active_entry_exit =
            (has_trailing && trail_points_expr.is_some() && !is_trailing_only)
                || ((profit_expr.is_some() || loss_expr.is_some())
                    && has_downside
                    && has_upside
                    && !is_stop_profit_bracket
                    && !is_loss_limit_bracket
                    && !is_loss_profit_bracket);
        if has_unsupported_entry_relative_active_entry_exit
            && self
                .strategy_broker
                .reject_entry_relative_exit_for_pending_entry(&from_entry)
        {
            return Ok(PineValue::Void);
        }

        if has_trailing {
            if !is_trailing_only {
                return Ok(PineValue::Void);
            }

            let mintick = self.request_environment.chart().min_tick();
            if let Some(trail_price_expr) = trail_price_expr {
                let activation_price = self
                    .eval_expr(trail_price_expr)?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                let trail_offset_expr =
                    trail_offset_expr.expect("checked trailing offset presence");
                let trail_offset_ticks = self
                    .eval_expr(trail_offset_expr)?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                self.place_exit_trail_price_quantity(
                    id,
                    from_entry,
                    TrailPriceExitSpec {
                        activation_price,
                        offset_ticks: trail_offset_ticks,
                        mintick,
                    },
                    quantity,
                    self.bars,
                    metadata.clone(),
                );
                return Ok(PineValue::Void);
            }

            if let Some(trail_points_expr) = trail_points_expr {
                let activation_ticks = self
                    .eval_expr(trail_points_expr)?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                let trail_offset_expr =
                    trail_offset_expr.expect("checked trailing offset presence");
                let trail_offset_ticks = self
                    .eval_expr(trail_offset_expr)?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                if from_entry.is_empty() {
                    self.strategy_broker
                        .with_next_exit_metadata(metadata.clone(), |broker| {
                            broker.place_all_entry_exit_trail_points(
                                id,
                                activation_ticks,
                                trail_offset_ticks,
                                mintick,
                                self.bars,
                            )
                        });
                    return Ok(PineValue::Void);
                }
                self.place_exit_trail_points_quantity(
                    id,
                    from_entry,
                    TrailPointsExitSpec {
                        activation_ticks,
                        offset_ticks: trail_offset_ticks,
                        mintick,
                    },
                    quantity,
                    self.bars,
                    metadata.clone(),
                );
                return Ok(PineValue::Void);
            }

            return Ok(PineValue::Void);
        }

        if has_downside && has_upside {
            if is_stop_profit_bracket {
                let stop_price = self
                    .eval_expr(stop_expr.expect("checked stop presence"))?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                let profit_ticks = self.eval_strategy_profit_ticks(
                    profit_expr.expect("checked profit presence"),
                    extra_profit_expr,
                )?;
                let mintick = self.request_environment.chart().min_tick();
                if from_entry.is_empty() {
                    self.strategy_broker
                        .with_next_exit_metadata(metadata.clone(), |broker| {
                            broker.place_all_entry_exit_stop_profit_bracket(
                                id,
                                StopProfitBracketSpec {
                                    stop_price,
                                    profit_ticks,
                                    mintick,
                                },
                                self.bars,
                            )
                        });
                    return Ok(PineValue::Void);
                }
                self.place_exit_stop_profit_bracket_quantity(
                    id,
                    from_entry,
                    StopProfitBracketSpec {
                        stop_price,
                        profit_ticks,
                        mintick,
                    },
                    quantity,
                    self.bars,
                    metadata.clone(),
                );
                return Ok(PineValue::Void);
            }
            if is_loss_limit_bracket {
                let loss_ticks = self
                    .eval_expr(loss_expr.expect("checked loss presence"))?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                let limit_price = self
                    .eval_expr(limit_expr.expect("checked limit presence"))?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                let mintick = self.request_environment.chart().min_tick();
                if from_entry.is_empty() {
                    self.strategy_broker
                        .with_next_exit_metadata(metadata.clone(), |broker| {
                            broker.place_all_entry_exit_loss_limit_bracket(
                                id,
                                LossLimitBracketSpec {
                                    loss_ticks,
                                    limit_price,
                                    mintick,
                                },
                                self.bars,
                            )
                        });
                    return Ok(PineValue::Void);
                }
                self.place_exit_loss_limit_bracket_quantity(
                    id,
                    from_entry,
                    LossLimitBracketSpec {
                        loss_ticks,
                        limit_price,
                        mintick,
                    },
                    quantity,
                    self.bars,
                    metadata.clone(),
                );
                return Ok(PineValue::Void);
            }
            if is_loss_profit_bracket {
                let loss_ticks = self
                    .eval_expr(loss_expr.expect("checked loss presence"))?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                let profit_ticks = self.eval_strategy_profit_ticks(
                    profit_expr.expect("checked profit presence"),
                    extra_profit_expr,
                )?;
                let mintick = self.request_environment.chart().min_tick();
                if from_entry.is_empty() {
                    self.strategy_broker
                        .with_next_exit_metadata(metadata.clone(), |broker| {
                            broker.place_all_entry_exit_loss_profit_bracket(
                                id,
                                LossProfitBracketSpec {
                                    loss_ticks,
                                    profit_ticks,
                                    mintick,
                                },
                                self.bars,
                            )
                        });
                    return Ok(PineValue::Void);
                }
                self.place_exit_loss_profit_bracket_quantity(
                    id,
                    from_entry,
                    LossProfitBracketSpec {
                        loss_ticks,
                        profit_ticks,
                        mintick,
                    },
                    quantity,
                    self.bars,
                    metadata.clone(),
                );
                return Ok(PineValue::Void);
            }

            let downside_price = if let Some(stop_expr) = stop_expr {
                self.eval_expr(stop_expr)?.as_f64().unwrap_or(f64::NAN)
            } else if let Some(loss_expr) = loss_expr {
                let loss_ticks = self.eval_expr(loss_expr)?.as_f64().unwrap_or(f64::NAN);
                let mintick = self.request_environment.chart().min_tick();
                let Some(loss_price) = self
                    .strategy_broker
                    .exit_loss_price_from_ticks(loss_ticks, mintick)
                else {
                    return Ok(PineValue::Void);
                };
                loss_price
            } else {
                return Ok(PineValue::Void);
            };

            let upside_price = if let Some(limit_expr) = limit_expr {
                self.eval_expr(limit_expr)?.as_f64().unwrap_or(f64::NAN)
            } else if let Some(profit_expr) = profit_expr {
                let profit_ticks =
                    self.eval_strategy_profit_ticks(profit_expr, extra_profit_expr)?;
                let mintick = self.request_environment.chart().min_tick();
                let Some(profit_price) = self
                    .strategy_broker
                    .exit_profit_price_from_ticks(profit_ticks, mintick)
                else {
                    return Ok(PineValue::Void);
                };
                profit_price
            } else {
                return Ok(PineValue::Void);
            };

            match (downside_price.is_nan(), upside_price.is_nan()) {
                (false, false) => self.place_exit_bracket_quantity(RuntimeExitBracketPlacement {
                    id,
                    from_entry,
                    downside_price,
                    upside_price,
                    quantity,
                    bar_index: self.bars,
                    metadata: metadata.clone(),
                }),
                (false, true) => self.place_exit_stop_quantity(
                    id,
                    from_entry,
                    downside_price,
                    quantity,
                    self.bars,
                    metadata.clone(),
                ),
                (true, false) => self.place_exit_limit_quantity(
                    id,
                    from_entry,
                    upside_price,
                    quantity,
                    self.bars,
                    metadata.clone(),
                ),
                (true, true) => {}
            }
        } else if let Some(stop_expr) = stop_expr {
            let stop_price = self.eval_expr(stop_expr)?.as_f64().unwrap_or(f64::NAN);
            self.place_exit_stop_quantity(
                id,
                from_entry,
                stop_price,
                quantity,
                self.bars,
                metadata.clone(),
            );
        } else if let Some(limit_expr) = limit_expr {
            let limit_price = self.eval_expr(limit_expr)?.as_f64().unwrap_or(f64::NAN);
            self.place_exit_limit_quantity(
                id,
                from_entry,
                limit_price,
                quantity,
                self.bars,
                metadata.clone(),
            );
        } else if let Some(profit_expr) = profit_expr {
            let profit_ticks = self.eval_strategy_profit_ticks(profit_expr, extra_profit_expr)?;
            let mintick = self.request_environment.chart().min_tick();
            if from_entry.is_empty() {
                self.strategy_broker
                    .with_next_exit_metadata(metadata.clone(), |broker| {
                        broker.place_all_entry_exit_profit_ticks(
                            id,
                            profit_ticks,
                            mintick,
                            self.bars,
                        )
                    });
                return Ok(PineValue::Void);
            }
            self.place_exit_profit_ticks_quantity(RuntimeExitTicksPlacement {
                id,
                from_entry,
                ticks: profit_ticks,
                mintick,
                quantity,
                bar_index: self.bars,
                metadata: metadata.clone(),
            });
        } else if let Some(loss_expr) = loss_expr {
            let loss_ticks = self.eval_expr(loss_expr)?.as_f64().unwrap_or(f64::NAN);
            let mintick = self.request_environment.chart().min_tick();
            if from_entry.is_empty() {
                self.strategy_broker
                    .with_next_exit_metadata(metadata.clone(), |broker| {
                        broker.place_all_entry_exit_loss_ticks(id, loss_ticks, mintick, self.bars)
                    });
                return Ok(PineValue::Void);
            }
            self.place_exit_loss_ticks_quantity(RuntimeExitTicksPlacement {
                id,
                from_entry,
                ticks: loss_ticks,
                mintick,
                quantity,
                bar_index: self.bars,
                metadata: metadata.clone(),
            });
        }
        Ok(PineValue::Void)
    }
}
