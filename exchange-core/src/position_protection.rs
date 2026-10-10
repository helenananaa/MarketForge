//! Exchange-owned position exits. Triggered exits stay latched until flat.
use crate::model::AccountId;
use crate::{PositionSide, Side};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProtectionTrigger {
    #[default]
    Mark,
    Last,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PositionProtectionSpec {
    pub take_profit_tick: Option<i64>,
    pub stop_loss_tick: Option<i64>,
    #[serde(default)]
    pub trigger: ProtectionTrigger,
    #[serde(default)]
    pub trailing_distance_tick: Option<i64>,
    #[serde(default)]
    pub exit_price_tick: Option<i64>,
    #[serde(default)]
    pub exit_qty: Option<u64>,
    #[serde(default)]
    pub take_profit_steps: Vec<TakeProfitStep>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TakeProfitStep {
    pub price_tick: i64,
    pub qty: u64,
}

impl PositionProtectionSpec {
    pub fn valid(&self) -> bool {
        (self.take_profit_tick.is_some()
            || self.stop_loss_tick.is_some()
            || self.trailing_distance_tick.is_some()
            || !self.take_profit_steps.is_empty())
            && self
                .take_profit_tick
                .into_iter()
                .chain(self.stop_loss_tick)
                .chain(self.trailing_distance_tick)
                .chain(self.exit_price_tick)
                .all(|p| p > 0)
            && self.exit_qty.is_none_or(|q| q > 0)
            && self.take_profit_steps.len() <= 16
            && !(self.take_profit_tick.is_some() && !self.take_profit_steps.is_empty())
            && self
                .take_profit_steps
                .iter()
                .all(|s| s.price_tick > 0 && s.qty > 0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PositionProtection {
    pub instrument_id: String,
    pub account_id: AccountId,
    pub position_side: PositionSide,
    pub exit_side: Side,
    pub spec: PositionProtectionSpec,
    pub entry_order_id: Option<u64>,
    pub status: String,
    pub triggered_by: Option<String>,
    pub trigger_price_tick: Option<i64>,
    pub triggered_at_market_time_ms: Option<u64>,
    pub last_exit_order_id: Option<u64>,
    #[serde(default)]
    pub last_checked_event_seq: u64,
    #[serde(default)]
    pub trailing_watermark_tick: Option<i64>,
    #[serde(default)]
    pub take_profit_step: usize,
    #[serde(default)]
    pub remaining_exit_qty: Option<u128>,
    #[serde(default)]
    pub last_observed_qty: Option<u128>,
}

pub fn position_qty(account: &crate::PerpAccountSnapshot, side: PositionSide) -> Option<i128> {
    match (&account.hedge_positions, side) {
        (None, PositionSide::Both) => Some(account.position_qty),
        (Some(p), PositionSide::Long) => Some(p.long.qty),
        (Some(p), PositionSide::Short) => Some(-p.short.qty),
        _ => None,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PositionRisk {
    pub mark_price_tick: i64,
    pub margin_buffer: i128,
    pub margin_ratio_ppm: Option<i128>,
    pub liquidation_price_estimate_tick: Option<i64>,
    pub estimate_assumption: String,
}
impl PositionRisk {
    pub fn from_account(a: &crate::PerpAccountSnapshot, mark: i64, rate: u32) -> Self {
        let gross = a
            .hedge_positions
            .as_ref()
            .map_or(a.position_qty.abs(), |p| p.gross_qty());
        let net = a
            .hedge_positions
            .as_ref()
            .map_or(a.position_qty, |p| p.long.qty - p.short.qty);
        let estimate = (|| {
            let denominator = net
                .checked_mul(1_000_000)?
                .checked_sub(gross.checked_mul(i128::from(rate))?)?;
            if denominator == 0 || gross == 0 {
                return None;
            }
            let other = a
                .portfolio_maintenance_margin
                .checked_sub(a.maintenance_margin)?;
            let numerator = net
                .checked_mul(i128::from(mark))?
                .checked_add(other)?
                .checked_sub(a.equity)?
                .checked_mul(1_000_000)?;
            let price = numerator.checked_div(denominator)?;
            if price <= 0 {
                return None;
            }
            i64::try_from(price).ok()
        })();
        Self { mark_price_tick: mark, margin_buffer: a.equity.saturating_sub(a.portfolio_maintenance_margin), margin_ratio_ppm: if a.equity > 0 { a.portfolio_maintenance_margin.checked_mul(1_000_000).map(|v| v / a.equity) } else { None }, liquidation_price_estimate_tick: estimate, estimate_assumption: "Estimate only: other instrument marks unchanged; maintenance rounding, fees, liquidity and subsequent funding may change the boundary".into() }
    }
}

/// Internal mutation request; cancellations precede the new reduce-only exit.
pub type PositionExitRequest = (String, AccountId, PositionSide, u64, Vec<u64>, Option<i64>);
