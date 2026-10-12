pub const DEFAULT_STRATEGY_INITIAL_CAPITAL: f64 = 1_000_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StrategyDefaultQuantity {
    Fixed(f64),
    Cash(f64),
    PercentOfEquity(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StrategyCommission {
    CashPerContract(f64),
    CashPerOrder(f64),
    Percent(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategyMarginSetting {
    pub value_percent: f64,
    /// Whether the source supplied the property; inherited v6 margins are active
    /// even though this provenance flag remains false.
    pub explicit: bool,
}

impl StrategyMarginSetting {
    #[must_use]
    pub fn explicit(value_percent: f64) -> Self {
        Self {
            value_percent,
            explicit: true,
        }
    }

    #[must_use]
    pub fn is_active(self) -> bool {
        self.value_percent > 0.0
    }
}

impl Default for StrategyMarginSetting {
    fn default() -> Self {
        Self {
            value_percent: 0.0,
            explicit: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyCloseEntriesRule {
    Fifo,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategySettings {
    /// Explicit account currency; None inherits the chart currency.
    pub account_currency: Option<&'static str>,
    pub initial_capital: f64,
    pub default_qty: Option<StrategyDefaultQuantity>,
    pub commission: Option<StrategyCommission>,
    pub pyramiding_limit: usize,
    pub slippage_ticks: f64,
    pub backtest_fill_limit_ticks: f64,
    pub margin_long: StrategyMarginSetting,
    pub margin_short: StrategyMarginSetting,
    pub close_entries_rule: StrategyCloseEntriesRule,
    pub process_orders_on_close: bool,
    pub calc_on_order_fills: bool,
    pub calc_on_every_tick: bool,
    pub use_bar_magnifier: bool,
}

impl Default for StrategySettings {
    fn default() -> Self {
        Self {
            account_currency: None,
            initial_capital: DEFAULT_STRATEGY_INITIAL_CAPITAL,
            default_qty: Some(StrategyDefaultQuantity::Fixed(1.0)),
            commission: None,
            pyramiding_limit: 1,
            slippage_ticks: 0.0,
            backtest_fill_limit_ticks: 0.0,
            margin_long: StrategyMarginSetting::default(),
            margin_short: StrategyMarginSetting::default(),
            close_entries_rule: StrategyCloseEntriesRule::Fifo,
            process_orders_on_close: false,
            calc_on_order_fills: false,
            calc_on_every_tick: false,
            use_bar_magnifier: false,
        }
    }
}

impl StrategySettings {
    /// Resolve omitted account margins without marking them as source-explicit.
    /// Pine v5 uses zero; v6 uses 100 percent. Explicit zero remains an override.
    #[must_use]
    pub fn with_language_defaults(mut self, language_version: Option<u16>) -> Self {
        let margin = if language_version.is_some_and(|version| version >= 6) {
            100.0
        } else {
            0.0
        };
        if !self.margin_long.explicit {
            self.margin_long.value_percent = margin;
        }
        if !self.margin_short.explicit {
            self.margin_short.value_percent = margin;
        }
        self
    }

    #[must_use]
    pub fn default_entry_qty(self, equity: f64, price: f64) -> Option<f64> {
        self.default_qty.and_then(|default_qty| match default_qty {
            StrategyDefaultQuantity::Fixed(qty) => Some(qty),
            StrategyDefaultQuantity::Cash(cash) => {
                if !price.is_finite() || price <= 0.0 {
                    return None;
                }
                Some(cash / price)
            }
            StrategyDefaultQuantity::PercentOfEquity(percent) => {
                if !equity.is_finite() || !price.is_finite() || equity <= 0.0 || price <= 0.0 {
                    return None;
                }
                Some(equity * percent / 100.0 / price)
            }
        })
    }

    #[must_use]
    pub fn commission_cash_per_contract(self) -> f64 {
        match self.commission {
            Some(StrategyCommission::CashPerContract(value)) => value,
            Some(StrategyCommission::CashPerOrder(_))
            | Some(StrategyCommission::Percent(_))
            | None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StrategySettings;

    #[test]
    fn default_use_bar_magnifier_is_false() {
        assert!(!StrategySettings::default().use_bar_magnifier);
    }

    #[test]
    fn explicit_false_use_bar_magnifier_is_false() {
        let settings = StrategySettings {
            use_bar_magnifier: false,
            ..StrategySettings::default()
        };
        assert!(!settings.use_bar_magnifier);
        assert_eq!(settings, StrategySettings::default());
    }
}
