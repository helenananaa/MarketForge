use crate::prelude::*;

mod declaration;

const STRATEGY_STATE_VARIABLES: &[&str] = &[
    "strategy.account_currency",
    "strategy.position_size",
    "strategy.position_avg_price",
    "strategy.position_entry_name",
    "strategy.initial_capital",
    "strategy.openprofit",
    "strategy.openprofit_percent",
    "strategy.netprofit",
    "strategy.netprofit_percent",
    "strategy.grossprofit",
    "strategy.grossprofit_percent",
    "strategy.grossloss",
    "strategy.grossloss_percent",
    "strategy.buy_and_hold_return_percent",
    "strategy.avg_trade",
    "strategy.avg_trade_percent",
    "strategy.avg_winning_trade",
    "strategy.avg_winning_trade_percent",
    "strategy.avg_losing_trade",
    "strategy.avg_losing_trade_percent",
    "strategy.max_runup",
    "strategy.max_runup_percent",
    "strategy.max_drawdown",
    "strategy.max_drawdown_percent",
    "strategy.max_contracts_held_all",
    "strategy.max_contracts_held_long",
    "strategy.max_contracts_held_short",
    "strategy.equity",
    "strategy.closedtrades",
    "strategy.closedtrades.first_index",
    "strategy.wintrades",
    "strategy.losstrades",
    "strategy.eventrades",
    "strategy.opentrades",
    "strategy.opentrades.capital_held",
    "strategy.margin_liquidation_price",
];

const STRATEGY_CLOSED_TRADE_FIELD_FUNCTIONS: &[&str] = &[
    "strategy.closedtrades.entry_price",
    "strategy.closedtrades.entry_comment",
    "strategy.closedtrades.entry_id",
    "strategy.closedtrades.exit_price",
    "strategy.closedtrades.exit_comment",
    "strategy.closedtrades.exit_id",
    "strategy.closedtrades.entry_bar_index",
    "strategy.closedtrades.exit_bar_index",
    "strategy.closedtrades.entry_time",
    "strategy.closedtrades.exit_time",
    "strategy.closedtrades.commission",
    "strategy.closedtrades.size",
    "strategy.closedtrades.profit",
    "strategy.closedtrades.profit_percent",
    "strategy.closedtrades.max_runup",
    "strategy.closedtrades.max_runup_percent",
    "strategy.closedtrades.max_drawdown",
    "strategy.closedtrades.max_drawdown_percent",
];

const STRATEGY_OPEN_TRADE_FIELD_FUNCTIONS: &[&str] = &[
    "strategy.opentrades.entry_price",
    "strategy.opentrades.entry_comment",
    "strategy.opentrades.entry_id",
    "strategy.opentrades.entry_bar_index",
    "strategy.opentrades.entry_time",
    "strategy.opentrades.size",
    "strategy.opentrades.profit",
    "strategy.opentrades.profit_percent",
    "strategy.opentrades.commission",
    "strategy.opentrades.max_runup",
    "strategy.opentrades.max_runup_percent",
    "strategy.opentrades.max_drawdown",
    "strategy.opentrades.max_drawdown_percent",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StrategyExitArgFamily {
    Identity,
    DownsidePriceTrigger,
    DownsideTickTrigger,
    UpsidePriceTrigger,
    UpsideTickTrigger,
    TrailingActivation,
    TrailingOffset,
    Quantity,
    PercentQuantity,
    Metadata,
    OcaName,
    #[allow(dead_code)]
    UnsupportedOption,
}

fn strategy_exit_arg_family(name: &str) -> Option<StrategyExitArgFamily> {
    match name {
        "id" | "from_entry" => Some(StrategyExitArgFamily::Identity),
        "stop" => Some(StrategyExitArgFamily::DownsidePriceTrigger),
        "loss" => Some(StrategyExitArgFamily::DownsideTickTrigger),
        "limit" => Some(StrategyExitArgFamily::UpsidePriceTrigger),
        "profit" => Some(StrategyExitArgFamily::UpsideTickTrigger),
        "trail_price" | "trail_points" => Some(StrategyExitArgFamily::TrailingActivation),
        "trail_offset" => Some(StrategyExitArgFamily::TrailingOffset),
        "qty" => Some(StrategyExitArgFamily::Quantity),
        "qty_percent" => Some(StrategyExitArgFamily::PercentQuantity),
        "comment" | "comment_profit" | "comment_loss" | "comment_trailing" | "alert_message"
        | "alert_profit" | "alert_loss" | "alert_trailing" | "disable_alert" => {
            Some(StrategyExitArgFamily::Metadata)
        }
        "oca_name" => Some(StrategyExitArgFamily::OcaName),
        _ => None,
    }
}

pub(crate) fn is_strategy_state_variable(name: &str) -> bool {
    STRATEGY_STATE_VARIABLES.contains(&name)
}

pub(crate) fn is_supported_strategy_trade_field_function(name: &str) -> bool {
    STRATEGY_CLOSED_TRADE_FIELD_FUNCTIONS.contains(&name)
        || STRATEGY_OPEN_TRADE_FIELD_FUNCTIONS.contains(&name)
}

pub(crate) fn is_supported_strategy_value_function(name: &str) -> bool {
    matches!(
        name,
        "strategy.convert_to_account" | "strategy.convert_to_symbol" | "strategy.default_entry_qty"
    ) || is_supported_strategy_trade_field_function(name)
}

impl Analyzer {
    pub(crate) fn validate_script_declaration_call(
        &mut self,
        name: &str,
        span: Span,
        args: &[CallArg],
    ) {
        let Some(mode) = (match name {
            "indicator" => Some(ScriptMode::Indicator),
            "strategy" => Some(ScriptMode::Strategy),
            _ => None,
        }) else {
            return;
        };

        if self.block_depth > 0 || self.function_depth > 0 {
            self.diagnostics.push(Diagnostic::error(
                "E_SCRIPT_DECL_LOCATION",
                format!("`{name}` declarations must be top-level"),
                span,
            ));
            return;
        }

        if let Some((existing_mode, _)) = self.script_declaration {
            self.diagnostics.push(Diagnostic::error(
                "E_SCRIPT_DECL_DUPLICATE",
                format!(
                    "script already has a {:?} declaration; only one indicator(...) or strategy(...) declaration is allowed",
                    existing_mode
                ),
                span,
            ));
            return;
        }

        self.script_declaration = Some((mode, span));
        let signature = pine_builtins::get_phase_1_builtin(name).expect("script signature");
        if let Some(value) = args.iter().enumerate().find_map(|(index, arg)| {
            (arg.name.as_deref() == Some("dynamic_requests")
                || (arg.name.is_none()
                    && signature
                        .params
                        .get(index)
                        .is_some_and(|param| param.name == "dynamic_requests")))
            .then_some(arg)
        }) {
            if self.legacy.dialect() < crate::PineDialect::V5 {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_NAME",
                    "`dynamic_requests` requires Pine v5 or v6",
                    value.span,
                ));
            } else if let Some(enabled) = self.known_const_bool_value(&value.value) {
                self.dynamic_requests = enabled;
            }
        }
        if mode == ScriptMode::Strategy {
            self.validate_strategy_declaration_args(args);
        } else {
            self.validate_indicator_timeframe_args(args);
        }
    }

    pub(crate) fn validate_strategy_order_call(
        &mut self,
        name: &str,
        span: Span,
        args: &[CallArg],
    ) {
        if !matches!(
            name,
            "strategy.entry"
                | "strategy.order"
                | "strategy.close"
                | "strategy.close_all"
                | "strategy.cancel"
                | "strategy.cancel_all"
                | "strategy.exit"
                | "strategy.risk.allow_entry_in"
                | "strategy.risk.max_position_size"
                | "strategy.risk.max_drawdown"
                | "strategy.risk.max_intraday_loss"
                | "strategy.risk.max_intraday_filled_orders"
                | "strategy.risk.max_cons_loss_days"
        ) {
            return;
        }

        if !matches!(self.script_declaration, Some((ScriptMode::Strategy, _))) {
            self.diagnostics.push(Diagnostic::error(
                "E_STRATEGY_MODE",
                format!("`{name}` is only supported in scripts declared with strategy(...)"),
                span,
            ));
        }

        if name == "strategy.entry" {
            self.validate_strategy_entry_args(args);
        } else if name == "strategy.order" {
            self.validate_strategy_order_args(args);
        } else if name == "strategy.close" {
            self.validate_strategy_close_args(args);
        } else if name == "strategy.close_all" {
            self.validate_strategy_close_all_args(args);
        } else if name == "strategy.exit" {
            self.validate_strategy_exit_args(args);
        } else if name == "strategy.risk.allow_entry_in" {
            self.validate_strategy_risk_allow_entry_in_args(args);
        } else if name == "strategy.risk.max_position_size" {
            self.validate_strategy_risk_max_position_size_args(args);
        } else if name == "strategy.risk.max_drawdown" {
            self.validate_strategy_risk_max_drawdown_args(args);
        } else if name == "strategy.risk.max_intraday_loss" {
            self.validate_strategy_risk_max_intraday_loss_args(args);
        } else if name == "strategy.risk.max_intraday_filled_orders" {
            self.validate_strategy_risk_max_intraday_filled_orders_args(args);
        } else if name == "strategy.risk.max_cons_loss_days" {
            self.validate_strategy_risk_max_cons_loss_days_args(args);
        }
    }

    pub(crate) fn validate_strategy_value_function_call(&mut self, name: &str, span: Span) {
        if !is_supported_strategy_value_function(name) {
            return;
        }

        if !matches!(self.script_declaration, Some((ScriptMode::Strategy, _))) {
            self.diagnostics.push(Diagnostic::error(
                "E_STRATEGY_MODE",
                format!("`{name}` is only supported in scripts declared with strategy(...)"),
                span,
            ));
        }
    }

    pub(crate) fn validate_strategy_state_variable(&mut self, name: &str, span: Span) -> bool {
        if !is_strategy_state_variable(name) {
            return false;
        }

        if !matches!(self.script_declaration, Some((ScriptMode::Strategy, _))) {
            self.diagnostics.push(Diagnostic::error(
                "E_STRATEGY_MODE",
                format!("`{name}` is only supported in scripts declared with strategy(...)"),
                span,
            ));
            return true;
        }

        false
    }

    pub(crate) fn validate_strategy_entry_args(&mut self, args: &[CallArg]) {
        fn strategy_entry_arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name.as_deref().or_else(|| {
                [
                    "id",
                    "direction",
                    "qty",
                    "limit",
                    "stop",
                    "oca_name",
                    "oca_type",
                    "comment",
                    "alert_message",
                    "disable_alert",
                    "when",
                ]
                .get(index)
                .copied()
            })
        }
        let direction = args.iter().enumerate().find_map(|(index, arg)| {
            (strategy_entry_arg_name(index, arg) == Some("direction"))
                .then(|| self.known_const_string_value(&arg.value))
                .flatten()
        });
        for (index, arg) in args.iter().enumerate() {
            let Some(name) = strategy_entry_arg_name(index, arg) else {
                continue;
            };
            match name {
                "direction" => {
                    let Some(direction) = direction.as_deref() else {
                        continue;
                    };
                    if !matches!(direction, "strategy.long" | "strategy.short") {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.entry` argument `direction` only supports strategy.long or strategy.short",
                            arg.span,
                        ));
                    }
                }
                // Order quantities are checked at execution time. A zero
                // quantity is a no-op and `na` selects the declared default.
                "qty" => {}
                "limit" => {
                    if let Some(limit) = self.known_const_numeric_value(&arg.value)
                        && (!limit.is_finite() || limit <= 0.0)
                    {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.entry` argument `limit` must be positive",
                            arg.span,
                        ));
                    }
                }
                "stop" => {
                    if let Some(stop) = self.known_const_numeric_value(&arg.value)
                        && (!stop.is_finite() || stop <= 0.0)
                    {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.entry` argument `stop` must be positive",
                            arg.span,
                        ));
                    }
                }
                "oca_name" => {}
                "oca_type" => match self.known_const_string_value(&arg.value).as_deref() {
                    Some("strategy.oca.none" | "strategy.oca.cancel" | "strategy.oca.reduce") => {}
                    _ => {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.entry` argument `oca_type` only supports strategy.oca.none, strategy.oca.cancel, or strategy.oca.reduce",
                            arg.span,
                        ));
                    }
                },
                "comment" | "alert_message" | "disable_alert" => {}
                "when" if self.legacy.dialect().version() >= 6 => {
                    self.unsupported(
                        "strategy.entry.when",
                        "`strategy.entry` argument `when` was removed in Pine v6",
                        arg.span,
                    );
                }
                "when" => {}
                _ => {}
            }
        }
    }

    pub(crate) fn validate_strategy_risk_allow_entry_in_args(&mut self, args: &[CallArg]) {
        fn arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name
                .as_deref()
                .or_else(|| ["value"].get(index).copied())
        }
        for (index, arg) in args.iter().enumerate() {
            if arg_name(index, arg) != Some("value") {
                continue;
            }
            let Some(value) = self.known_const_string_value(&arg.value) else {
                continue;
            };
            if !matches!(
                value.as_str(),
                "strategy.direction.all" | "strategy.direction.long" | "strategy.direction.short"
            ) {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_VALUE",
                    "`strategy.risk.allow_entry_in` argument `value` only supports strategy.direction.all, strategy.direction.long, or strategy.direction.short",
                    arg.span,
                ));
            }
        }
    }

    pub(crate) fn validate_strategy_risk_max_drawdown_args(&mut self, args: &[CallArg]) {
        fn arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name
                .as_deref()
                .or_else(|| ["value", "type", "alert_message"].get(index).copied())
        }
        let kind = args.iter().enumerate().find_map(|(index, arg)| {
            (arg_name(index, arg) == Some("type"))
                .then(|| self.known_const_string_value(&arg.value))
                .flatten()
        });
        for (index, arg) in args.iter().enumerate() {
            let Some(name) = arg_name(index, arg) else {
                continue;
            };
            match name {
                "value" => {
                    if let Some(value) = self.known_const_numeric_value(&arg.value) {
                        if !value.is_finite() || value <= 0.0 {
                            self.diagnostics.push(Diagnostic::error(
                                "E_CALL_ARG_VALUE",
                                "`strategy.risk.max_drawdown` argument `value` must be finite and positive",
                                arg.span,
                            ));
                        } else if kind.as_deref() == Some("strategy.percent_of_equity")
                            && value > 100.0
                        {
                            self.diagnostics.push(Diagnostic::error(
                                "E_CALL_ARG_VALUE",
                                "`strategy.risk.max_drawdown` percent `value` must be at most 100",
                                arg.span,
                            ));
                        }
                    }
                }
                "type" => match kind.as_deref() {
                    Some("strategy.cash" | "strategy.percent_of_equity") => {}
                    Some(_) => {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.risk.max_drawdown` argument `type` only supports strategy.cash or strategy.percent_of_equity",
                            arg.span,
                        ));
                    }
                    None => {}
                },
                "alert_message" => {}
                _ => {}
            }
        }
    }

    pub(crate) fn validate_strategy_risk_max_intraday_loss_args(&mut self, args: &[CallArg]) {
        fn arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name
                .as_deref()
                .or_else(|| ["value", "type", "alert_message"].get(index).copied())
        }
        let kind = args.iter().enumerate().find_map(|(index, arg)| {
            (arg_name(index, arg) == Some("type"))
                .then(|| self.known_const_string_value(&arg.value))
                .flatten()
        });
        for (index, arg) in args.iter().enumerate() {
            let Some(name) = arg_name(index, arg) else {
                continue;
            };
            match name {
                "value" => {
                    if let Some(value) = self.known_const_numeric_value(&arg.value) {
                        if !value.is_finite() || value <= 0.0 {
                            self.diagnostics.push(Diagnostic::error(
                                "E_CALL_ARG_VALUE",
                                "`strategy.risk.max_intraday_loss` argument `value` must be finite and positive",
                                arg.span,
                            ));
                        } else if kind.as_deref() == Some("strategy.percent_of_equity")
                            && value > 100.0
                        {
                            self.diagnostics.push(Diagnostic::error(
                                "E_CALL_ARG_VALUE",
                                "`strategy.risk.max_intraday_loss` percent `value` must be at most 100",
                                arg.span,
                            ));
                        }
                    }
                }
                "type" => match kind.as_deref() {
                    Some("strategy.cash" | "strategy.percent_of_equity") => {}
                    Some(_) => {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.risk.max_intraday_loss` argument `type` only supports strategy.cash or strategy.percent_of_equity",
                            arg.span,
                        ));
                    }
                    None => {}
                },
                "alert_message" => {}
                _ => {}
            }
        }
    }

    pub(crate) fn validate_strategy_risk_max_cons_loss_days_args(&mut self, args: &[CallArg]) {
        fn arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name
                .as_deref()
                .or_else(|| ["count", "alert_message"].get(index).copied())
        }
        for (index, arg) in args.iter().enumerate() {
            if arg_name(index, arg) != Some("count") {
                continue;
            }
            if let Some(count) = self.known_const_numeric_value(&arg.value)
                && (!count.is_finite() || count <= 0.0 || count != count.trunc())
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_VALUE",
                    "`strategy.risk.max_cons_loss_days` argument `count` must be a finite positive integer",
                    arg.span,
                ));
            }
        }
    }

    pub(crate) fn validate_strategy_risk_max_intraday_filled_orders_args(
        &mut self,
        args: &[CallArg],
    ) {
        fn arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name
                .as_deref()
                .or_else(|| ["count", "alert_message"].get(index).copied())
        }
        for (index, arg) in args.iter().enumerate() {
            if arg_name(index, arg) != Some("count") {
                continue;
            }
            if let Some(count) = self.known_const_numeric_value(&arg.value)
                && (!count.is_finite() || count <= 0.0 || count != count.trunc())
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_VALUE",
                    "`strategy.risk.max_intraday_filled_orders` argument `count` must be a finite positive integer",
                    arg.span,
                ));
            }
        }
    }

    pub(crate) fn validate_strategy_risk_max_position_size_args(&mut self, args: &[CallArg]) {
        fn arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name
                .as_deref()
                .or_else(|| ["contracts"].get(index).copied())
        }
        for (index, arg) in args.iter().enumerate() {
            if arg_name(index, arg) != Some("contracts") {
                continue;
            }
            if let Some(contracts) = self.known_const_numeric_value(&arg.value)
                && (!contracts.is_finite() || contracts <= 0.0)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_VALUE",
                    "`strategy.risk.max_position_size` argument `contracts` must be finite and positive",
                    arg.span,
                ));
            }
        }
    }

    pub(crate) fn validate_strategy_order_args(&mut self, args: &[CallArg]) {
        fn strategy_order_arg_name(index: usize, arg: &CallArg) -> Option<&str> {
            arg.name.as_deref().or_else(|| {
                [
                    "id",
                    "direction",
                    "qty",
                    "limit",
                    "stop",
                    "oca_name",
                    "oca_type",
                    "comment",
                    "alert_message",
                    "disable_alert",
                ]
                .get(index)
                .copied()
            })
        }
        for (index, arg) in args.iter().enumerate() {
            let Some(name) = strategy_order_arg_name(index, arg) else {
                continue;
            };
            match name {
                "direction" => {
                    let Some(direction) = self.known_const_string_value(&arg.value) else {
                        continue;
                    };
                    if !matches!(direction.as_str(), "strategy.long" | "strategy.short") {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.order` argument `direction` only supports strategy.long or strategy.short",
                            arg.span,
                        ));
                    }
                }
                "qty" => {}
                "limit" => {
                    if let Some(limit) = self.known_const_numeric_value(&arg.value)
                        && (!limit.is_finite() || limit <= 0.0)
                    {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.order` argument `limit` must be positive",
                            arg.span,
                        ));
                    }
                }
                "stop" => {
                    if let Some(stop) = self.known_const_numeric_value(&arg.value)
                        && (!stop.is_finite() || stop <= 0.0)
                    {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.order` argument `stop` must be positive",
                            arg.span,
                        ));
                    }
                }
                "oca_name" => {}
                "oca_type" => match self.known_const_string_value(&arg.value).as_deref() {
                    Some("strategy.oca.none" | "strategy.oca.cancel" | "strategy.oca.reduce") => {}
                    _ => {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.order` argument `oca_type` only supports strategy.oca.none, strategy.oca.cancel, or strategy.oca.reduce",
                            arg.span,
                        ));
                    }
                },
                "comment" | "alert_message" | "disable_alert" => {}
                _ => {}
            }
        }
    }

    pub(crate) fn validate_strategy_close_args(&mut self, args: &[CallArg]) {
        for (index, arg) in args.iter().enumerate() {
            let Some(name) = arg.name.as_deref().or_else(|| ["id"].get(index).copied()) else {
                if arg.name.is_none() {
                    self.diagnostics.push(Diagnostic::error(
                        "E_CALL_ARG_NAME",
                        "`strategy.close` partial quantity arguments must be named arguments",
                        arg.span,
                    ));
                }
                continue;
            };
            match name {
                "when" if self.legacy.dialect() >= crate::PineDialect::V6 => {
                    self.diagnostics.push(Diagnostic::error(
                        "E_CALL_ARG_NAME",
                        "`strategy.close` argument `when` was removed in Pine v6",
                        arg.span,
                    ));
                }
                "qty" => {
                    if let Some(qty) = self.known_const_numeric_value(&arg.value)
                        && (!qty.is_finite() || qty <= 0.0)
                    {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.close` argument `qty` must be finite and positive",
                            arg.span,
                        ));
                    }
                }
                "qty_percent" => {
                    if let Some(qty_percent) = self.known_const_numeric_value(&arg.value)
                        && (!qty_percent.is_finite() || qty_percent <= 0.0)
                    {
                        self.diagnostics.push(Diagnostic::error(
                            "E_CALL_ARG_VALUE",
                            "`strategy.close` argument `qty_percent` must be finite and positive",
                            arg.span,
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn validate_strategy_close_all_args(&mut self, args: &[CallArg]) {
        for (index, arg) in args.iter().enumerate() {
            if (arg.name.as_deref() == Some("when") || (arg.name.is_none() && index == 4))
                && self.legacy.dialect() >= crate::PineDialect::V6
            {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_NAME",
                    "`strategy.close_all` argument `when` was removed in Pine v6",
                    arg.span,
                ));
            }
        }
    }

    pub(crate) fn validate_strategy_exit_args(&mut self, args: &[CallArg]) {
        let mut has_stop = false;
        let mut has_limit = false;
        let mut has_profit = false;
        let mut has_loss = false;
        let mut has_trail_price = false;
        let mut has_trail_points = false;
        let mut has_trail_offset = false;
        let mut has_id = false;
        let mut has_unsupported_arg = false;
        for (index, arg) in args.iter().enumerate() {
            let Some(name) = arg
                .name
                .as_deref()
                .or_else(|| ["id", "from_entry", "stop", "limit"].get(index).copied())
            else {
                if arg.name.is_none() {
                    self.diagnostics.push(Diagnostic::error(
                        "E_CALL_ARG_NAME",
                        "`strategy.exit` profit and loss arguments must be named arguments",
                        arg.span,
                    ));
                }
                continue;
            };
            let Some(family) = strategy_exit_arg_family(name) else {
                has_unsupported_arg = true;
                continue;
            };
            match family {
                StrategyExitArgFamily::Identity => has_id |= name == "id",
                StrategyExitArgFamily::DownsidePriceTrigger => has_stop = true,
                StrategyExitArgFamily::DownsideTickTrigger => has_loss = true,
                StrategyExitArgFamily::UpsidePriceTrigger => has_limit = true,
                StrategyExitArgFamily::UpsideTickTrigger => has_profit = true,
                StrategyExitArgFamily::TrailingActivation => {
                    if name == "trail_price" {
                        has_trail_price = true;
                    } else {
                        has_trail_points = true;
                    }
                }
                StrategyExitArgFamily::TrailingOffset => has_trail_offset = true,
                StrategyExitArgFamily::Quantity | StrategyExitArgFamily::PercentQuantity => {}
                StrategyExitArgFamily::Metadata | StrategyExitArgFamily::OcaName => {}
                StrategyExitArgFamily::UnsupportedOption => {
                    has_unsupported_arg = true;
                    self.diagnostics.push(Diagnostic::error(
                        "E_CALL_ARG_NAME",
                        format!(
                            "`strategy.exit` argument `{name}` is not supported in the current strategy subset"
                        ),
                        arg.span,
                    ))
                }
            }
        }
        let legacy_unpaired_trail = self.legacy.dialect() <= crate::PineDialect::V4
            && has_trail_points
            && !has_trail_offset
            && !has_trail_price;
        let legacy_trail_profit = legacy_unpaired_trail && has_profit;
        let trigger_count = usize::from(has_stop)
            + usize::from(has_limit)
            + usize::from(has_profit || legacy_trail_profit)
            + usize::from(has_loss);
        let trailing_activation_count =
            usize::from(has_trail_price) + usize::from(has_trail_points);
        let has_trailing_args = !legacy_unpaired_trail
            && (has_trail_offset || (trailing_activation_count > 0 && trigger_count == 0));
        if legacy_unpaired_trail && trigger_count == 0 {
            return;
        }
        if has_trailing_args {
            if trigger_count > 0 {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_NAME",
                    "`strategy.exit` cannot combine trailing exits with fixed stop, limit, profit, or loss triggers in the current strategy subset",
                    args.iter()
                        .find(|arg| {
                            matches!(
                                arg.name.as_deref(),
                                Some(
                                    "stop"
                                        | "limit"
                                        | "profit"
                                        | "loss"
                                        | "trail_price"
                                        | "trail_points"
                                        | "trail_offset"
                                )
                            )
                        })
                        .map_or(Span::default(), |arg| arg.span),
                ));
            }
            if trailing_activation_count != 1 || !has_trail_offset {
                self.diagnostics.push(Diagnostic::error(
                    "E_CALL_ARG_NAME",
                    "`strategy.exit` trailing exits require exactly one of `trail_price` or `trail_points` plus `trail_offset`",
                    args.iter()
                        .find(|arg| {
                            matches!(
                                arg.name.as_deref(),
                                Some("trail_price" | "trail_points" | "trail_offset")
                            )
                        })
                        .map_or(Span::default(), |arg| arg.span),
                ));
            }
            return;
        }
        let downside_trigger_count = usize::from(has_stop) + usize::from(has_loss);
        let upside_trigger_count = usize::from(has_limit) + usize::from(has_profit);
        let supported_trigger_shape = trigger_count <= 1
            || (trigger_count == 2 && downside_trigger_count == 1 && upside_trigger_count == 1);
        if !supported_trigger_shape {
            self.diagnostics.push(Diagnostic::error(
                "E_CALL_ARG_NAME",
                "`strategy.exit` combined trigger families are not supported in the current strategy subset",
                args.iter()
                    .find(|arg| matches!(arg.name.as_deref(), Some("limit" | "profit" | "loss")))
                    .or_else(|| args.get(3))
                    .map_or(Span::default(), |arg| arg.span),
            ));
        } else if trigger_count == 0 && has_id && !has_unsupported_arg {
            self.diagnostics.push(Diagnostic::error(
                "E_CALL_ARITY",
                "`strategy.exit` requires one of `stop`, `limit`, `profit`, or `loss`",
                args.first().map_or(Span::default(), |arg| arg.span),
            ));
        }
    }
}
