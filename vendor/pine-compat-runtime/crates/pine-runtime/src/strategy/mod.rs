mod broker;

use pine_ir::StrategyCommission;

pub(crate) fn percent_of_equity_order_qty(
    equity: f64,
    percent: f64,
    price: f64,
    commission: Option<StrategyCommission>,
    quantity_scale: Option<u32>,
) -> f64 {
    let budget = equity * percent / 100.0;
    // Native percent sizing rounds the cash budget to ten significant decimal
    // digits before dividing by the execution price. This can move an order
    // across a contract boundary in either direction.
    let budget = if budget.is_finite() {
        format!("{budget:.9e}")
            .parse::<f64>()
            .expect("formatted finite budget")
    } else {
        budget
    };
    let qty = match commission {
        Some(StrategyCommission::Percent(rate)) => budget / (price * (1.0 + rate / 100.0)),
        Some(StrategyCommission::CashPerContract(fee)) => budget / (price + fee),
        Some(StrategyCommission::CashPerOrder(fee)) => (budget - fee).max(0.0) / price,
        None => budget / price,
    };
    quantity_on_chart_grid(qty, quantity_scale)
}

pub(crate) fn quantity_on_chart_grid(qty: f64, quantity_scale: Option<u32>) -> f64 {
    quantity_scale.map_or(qty, |scale| {
        (qty * f64::from(scale)).floor() / f64::from(scale)
    })
}

pub(crate) fn explicit_quantity_on_chart_grid(qty: f64, quantity_scale: Option<u32>) -> f64 {
    let Some(scale) = quantity_scale else {
        return qty;
    };
    // Native explicit orders truncate the shortest decimal representation.
    // Multiplying by the scale first can round a value just below a contract
    // boundary up to that boundary (e.g. 488565 * 0.000001). Conversely, an
    // already aligned decimal such as 0.129515 must not lose one contract.
    // ChartContext guarantees a power-of-ten scale. Display uses ordinary
    // decimal notation, including for very small and very large finite values.
    let decimal = qty.to_string();
    let Some(point) = decimal.find('.') else {
        return qty;
    };
    let precision = scale.ilog10() as usize;
    let end = if precision == 0 {
        point
    } else {
        (point + 1 + precision).min(decimal.len())
    };
    decimal[..end].parse().expect("truncated finite quantity")
}

pub use broker::BrokerState;
pub(crate) use broker::PendingCloseQuantity;
pub(crate) use broker::{EntryPathTick, PathEventOutcome};
pub(crate) use broker::{
    LossLimitBracketSpec, LossProfitBracketSpec, StopProfitBracketSpec, StrategyExitMetadata,
    StrategyOrderMetadata, TrailPointsExitSpec, TrailPriceExitSpec,
};
