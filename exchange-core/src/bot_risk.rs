//! A deterministic risk overlay shared by native background strategies.
use crate::{AccountSnapshot, ParticipantObservation};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BotRiskConfig {
    pub max_drawdown_ppm: u32,
    pub trailing_stop_ppm: u32,
    pub max_volatility_ticks: i64,
    pub min_margin_buffer_ppm: u32,
    pub cooldown_ms: u64,
}
impl BotRiskConfig {
    pub fn validate(&self) -> bool {
        self.max_drawdown_ppm <= 1_000_000
            && self.trailing_stop_ppm <= 1_000_000
            && self.min_margin_buffer_ppm <= 1_000_000
            && self.max_volatility_ticks >= 0
            && self.cooldown_ms <= 86_400_000
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BotRiskState {
    pub peak_equity: Option<String>,
    pub favorable_price: Option<i64>,
    pub previous_position: String,
    pub exiting: bool,
    pub cooldown_until_ms: u64,
    pub trigger: Option<String>,
    pub exits: u64,
}
impl BotRiskState {
    pub fn valid(&self) -> bool {
        self.peak_equity
            .as_ref()
            .is_none_or(|v| v.parse::<i128>().is_ok())
            && (self.previous_position.is_empty() || self.previous_position.parse::<i128>().is_ok())
            && self.favorable_price.is_none_or(|p| p > 0)
    }
    /// True blocks the strategy. Exits are latched until actual flat inventory.
    pub fn evaluate(
        &mut self,
        c: &BotRiskConfig,
        v: &ParticipantObservation,
        mid: i64,
        vol: i64,
    ) -> bool {
        let Some(account) = &v.own_account else {
            return false;
        };
        let (position, equity, buffer) = match account {
            AccountSnapshot::Spot(a) => (
                a.position_qty,
                a.cash_balance
                    .saturating_add(a.position_qty.saturating_mul(mid.into())),
                None,
            ),
            AccountSnapshot::Perp(a) => (
                a.position_qty,
                a.equity,
                Some((
                    a.equity
                        .saturating_sub(a.portfolio_maintenance_margin.max(a.maintenance_margin)),
                    a.equity,
                )),
            ),
        };
        if self.exiting && position == 0 {
            self.exiting = false;
            self.cooldown_until_ms = v.market_time_ms.saturating_add(c.cooldown_ms);
            self.peak_equity = Some(equity.to_string());
            self.favorable_price = None;
        }
        let peak = self
            .peak_equity
            .as_ref()
            .and_then(|p| p.parse::<i128>().ok())
            .unwrap_or(equity)
            .max(equity);
        self.peak_equity = Some(peak.to_string());
        let previous = self.previous_position.parse::<i128>().unwrap_or(0);
        if position.signum() != previous.signum() {
            self.favorable_price = None;
        }
        self.previous_position = position.to_string();
        let favorable = self.favorable_price.unwrap_or(mid);
        let favorable = if position < 0 {
            favorable.min(mid)
        } else {
            favorable.max(mid)
        };
        self.favorable_price = (position != 0).then_some(favorable);
        let adverse = if position < 0 {
            mid.saturating_sub(favorable)
        } else {
            favorable.saturating_sub(mid)
        };
        let reason = if c.max_drawdown_ppm > 0
            && peak > 0
            && peak.saturating_sub(equity).saturating_mul(1_000_000)
                >= peak.saturating_mul(c.max_drawdown_ppm.into())
        {
            Some("drawdown")
        } else if c.trailing_stop_ppm > 0
            && position != 0
            && i128::from(adverse).saturating_mul(1_000_000)
                >= i128::from(favorable).saturating_mul(c.trailing_stop_ppm.into())
        {
            Some("trailing_stop")
        } else if c.max_volatility_ticks > 0 && vol >= c.max_volatility_ticks {
            Some("volatility")
        } else if c.min_margin_buffer_ppm > 0
            && position != 0
            && buffer.is_some_and(|(b, e)| {
                e <= 0
                    || b.saturating_mul(1_000_000)
                        <= e.saturating_mul(c.min_margin_buffer_ppm.into())
            })
        {
            Some("margin_buffer")
        } else {
            None
        };
        if let Some(reason) = reason {
            if !self.exiting && position != 0 {
                self.exits = self.exits.saturating_add(1);
                self.trigger = Some(reason.into());
                self.exiting = true;
            }
            if position == 0 {
                self.cooldown_until_ms = v.market_time_ms.saturating_add(c.cooldown_ms);
            }
        }
        self.exiting || v.market_time_ms < self.cooldown_until_ms || reason.is_some()
    }
}
