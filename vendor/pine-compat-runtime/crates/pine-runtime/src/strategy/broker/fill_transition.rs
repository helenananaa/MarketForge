#![allow(dead_code)]

use super::ledger::{TradeAllocation, TradeDirection};
use super::types::InternalOrderKey;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FillTriggerReason {
    Market,
    Limit,
    Stop,
    StopLimit,
    Exit,
    Close,
    CloseAll,
    MarginCall,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FillRequest {
    pub(super) order_key: InternalOrderKey,
    pub(super) bar_index: usize,
    pub(super) time: i64,
    pub(super) raw_price: f64,
    pub(super) trigger_reason: FillTriggerReason,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PositionSnapshot {
    pub(super) signed_size: f64,
    pub(super) avg_price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct OpenedExposure {
    pub(super) direction: TradeDirection,
    pub(super) quantity: f64,
    pub(super) price: f64,
    pub(super) commission: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct FillTransition {
    pub(super) request: FillRequest,
    pub(super) closed_allocations: Vec<TradeAllocation>,
    pub(super) opened_trade: Option<OpenedExposure>,
    pub(super) filled_quantity: f64,
    pub(super) close_quantity: f64,
    pub(super) open_quantity: f64,
    pub(super) fill_price: f64,
    pub(super) cash_delta: f64,
    pub(super) realized_profit: f64,
    pub(super) commission: f64,
    pub(super) routable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FillCalcError {
    NonFiniteQuantity,
    NonPositiveQuantity,
    NonFinitePrice,
    NonFinitePosition,
    NotSameSide,
    NoReduciblePosition,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FillQuantitySplit {
    pub(super) close_quantity: f64,
    pub(super) open_quantity: f64,
    pub(super) close_direction: Option<TradeDirection>,
    pub(super) open_direction: Option<TradeDirection>,
}

fn direction_of(signed: f64) -> Option<TradeDirection> {
    if signed > 0.0 {
        Some(TradeDirection::Long)
    } else if signed < 0.0 {
        Some(TradeDirection::Short)
    } else {
        None
    }
}

fn validate_inputs(
    snapshot: &PositionSnapshot,
    signed_quantity: f64,
    fill_price: f64,
) -> Result<(), FillCalcError> {
    if !signed_quantity.is_finite() {
        return Err(FillCalcError::NonFiniteQuantity);
    }
    if signed_quantity == 0.0 {
        return Err(FillCalcError::NonPositiveQuantity);
    }
    if !fill_price.is_finite() || fill_price <= 0.0 {
        return Err(FillCalcError::NonFinitePrice);
    }
    if !snapshot.signed_size.is_finite() || !snapshot.avg_price.is_finite() {
        return Err(FillCalcError::NonFinitePosition);
    }
    Ok(())
}

pub(super) fn split_fill_quantities(
    signed_position: f64,
    signed_quantity: f64,
) -> Result<FillQuantitySplit, FillCalcError> {
    if !signed_position.is_finite() {
        return Err(FillCalcError::NonFinitePosition);
    }
    if !signed_quantity.is_finite() {
        return Err(FillCalcError::NonFiniteQuantity);
    }
    if signed_quantity == 0.0 {
        return Err(FillCalcError::NonPositiveQuantity);
    }

    let position_abs = signed_position.abs();
    let fill_abs = signed_quantity.abs();
    if signed_position == 0.0 || signed_position.signum() == signed_quantity.signum() {
        return Ok(FillQuantitySplit {
            close_quantity: 0.0,
            open_quantity: fill_abs,
            close_direction: None,
            open_direction: direction_of(signed_quantity),
        });
    }

    let close_quantity = fill_abs.min(position_abs);
    let open_quantity = fill_abs - close_quantity;
    Ok(FillQuantitySplit {
        close_quantity,
        open_quantity,
        close_direction: direction_of(signed_position),
        open_direction: if open_quantity > 0.0 {
            direction_of(signed_quantity)
        } else {
            None
        },
    })
}

fn close_cash_delta(
    direction: TradeDirection,
    quantity: f64,
    price: f64,
    exit_commission: f64,
) -> f64 {
    direction.signed_quantity(quantity) * price - exit_commission
}

fn open_cash_delta(
    direction: TradeDirection,
    quantity: f64,
    price: f64,
    entry_commission: f64,
) -> f64 {
    -(direction.signed_quantity(quantity) * price + entry_commission)
}

fn realized_from_allocations(
    allocations: &[TradeAllocation],
    price: f64,
    exit_commission: f64,
) -> f64 {
    if allocations.is_empty() {
        return 0.0;
    }
    let closed_qty: f64 = allocations
        .iter()
        .map(|allocation| allocation.quantity)
        .sum();
    allocations
        .iter()
        .map(|allocation| {
            let allocated_exit = if closed_qty > 0.0 {
                exit_commission * (allocation.quantity / closed_qty)
            } else {
                0.0
            };
            let commission = allocation.entry_commission + allocated_exit;
            let signed_qty = allocation.direction.signed_quantity(allocation.quantity);
            (price - allocation.entry_price) * signed_qty - commission
        })
        .sum()
}

fn scale_commission(total: f64, quantity: f64, filled: f64) -> f64 {
    if filled <= 0.0 {
        0.0
    } else {
        total * (quantity / filled)
    }
}

pub(super) fn calculate_same_side_addition(
    snapshot: &PositionSnapshot,
    request: FillRequest,
    signed_quantity: f64,
    fill_price: f64,
    entry_commission: f64,
) -> Result<FillTransition, FillCalcError> {
    validate_inputs(snapshot, signed_quantity, fill_price)?;
    if !entry_commission.is_finite() || entry_commission < 0.0 {
        return Err(FillCalcError::NonFiniteQuantity);
    }
    let split = split_fill_quantities(snapshot.signed_size, signed_quantity)?;
    if split.close_quantity != 0.0 {
        return Err(FillCalcError::NotSameSide);
    }
    let open_direction = split
        .open_direction
        .ok_or(FillCalcError::NonPositiveQuantity)?;
    Ok(FillTransition {
        request,
        closed_allocations: Vec::new(),
        opened_trade: Some(OpenedExposure {
            direction: open_direction,
            quantity: split.open_quantity,
            price: fill_price,
            commission: entry_commission,
        }),
        filled_quantity: split.open_quantity,
        close_quantity: 0.0,
        open_quantity: split.open_quantity,
        fill_price,
        cash_delta: open_cash_delta(
            open_direction,
            split.open_quantity,
            fill_price,
            entry_commission,
        ),
        realized_profit: 0.0,
        commission: entry_commission,
        routable: true,
    })
}

pub(super) fn calculate_reduce_only(
    snapshot: &PositionSnapshot,
    request: FillRequest,
    signed_quantity: f64,
    fill_price: f64,
    exit_commission: f64,
    allocations: Vec<TradeAllocation>,
) -> Result<FillTransition, FillCalcError> {
    validate_inputs(snapshot, signed_quantity, fill_price)?;
    if snapshot.signed_size == 0.0 || snapshot.signed_size.signum() == signed_quantity.signum() {
        return Err(FillCalcError::NoReduciblePosition);
    }
    if !exit_commission.is_finite() || exit_commission < 0.0 {
        return Err(FillCalcError::NonFiniteQuantity);
    }
    let split = split_fill_quantities(snapshot.signed_size, signed_quantity)?;
    let close_direction = split
        .close_direction
        .ok_or(FillCalcError::NoReduciblePosition)?;
    let close_quantity = split.close_quantity;
    let filled_exit_commission =
        scale_commission(exit_commission, close_quantity, signed_quantity.abs());
    Ok(FillTransition {
        request,
        realized_profit: realized_from_allocations(
            &allocations,
            fill_price,
            filled_exit_commission,
        ),
        closed_allocations: allocations,
        opened_trade: None,
        filled_quantity: close_quantity,
        close_quantity,
        open_quantity: 0.0,
        fill_price,
        cash_delta: close_cash_delta(
            close_direction,
            close_quantity,
            fill_price,
            filled_exit_commission,
        ),
        commission: filled_exit_commission,
        routable: true,
    })
}

pub(super) fn calculate_netting_transition(
    snapshot: &PositionSnapshot,
    request: FillRequest,
    signed_quantity: f64,
    fill_price: f64,
    entry_commission: f64,
    exit_commission: f64,
    allocations: Vec<TradeAllocation>,
) -> Result<FillTransition, FillCalcError> {
    validate_inputs(snapshot, signed_quantity, fill_price)?;
    if !entry_commission.is_finite()
        || entry_commission < 0.0
        || !exit_commission.is_finite()
        || exit_commission < 0.0
    {
        return Err(FillCalcError::NonFiniteQuantity);
    }
    let split = split_fill_quantities(snapshot.signed_size, signed_quantity)?;
    let filled = signed_quantity.abs();
    let close_commission = scale_commission(exit_commission, split.close_quantity, filled);
    let open_commission = scale_commission(entry_commission, split.open_quantity, filled);
    let mut cash_delta = 0.0;
    let mut realized_profit = 0.0;
    if let Some(close_direction) = split.close_direction {
        cash_delta += close_cash_delta(
            close_direction,
            split.close_quantity,
            fill_price,
            close_commission,
        );
        realized_profit = realized_from_allocations(&allocations, fill_price, close_commission);
    }
    let opened_trade = split.open_direction.map(|open_direction| {
        cash_delta += open_cash_delta(
            open_direction,
            split.open_quantity,
            fill_price,
            open_commission,
        );
        OpenedExposure {
            direction: open_direction,
            quantity: split.open_quantity,
            price: fill_price,
            commission: open_commission,
        }
    });
    let routable = split.close_quantity == 0.0 || split.open_quantity == 0.0;
    Ok(FillTransition {
        request,
        closed_allocations: allocations,
        opened_trade,
        filled_quantity: filled,
        close_quantity: split.close_quantity,
        open_quantity: split.open_quantity,
        fill_price,
        cash_delta,
        realized_profit,
        commission: close_commission + open_commission,
        routable,
    })
}

#[cfg(test)]
mod tests {
    use super::super::BrokerState;
    use super::super::types::StrategyOrderMetadata;
    use super::*;

    fn request() -> FillRequest {
        FillRequest {
            order_key: InternalOrderKey(7),
            bar_index: 2,
            time: 20,
            raw_price: 110.0,
            trigger_reason: FillTriggerReason::Market,
        }
    }

    fn snapshot(signed_size: f64, avg_price: f64) -> PositionSnapshot {
        PositionSnapshot {
            signed_size,
            avg_price,
        }
    }

    fn allocation(direction: TradeDirection, quantity: f64, entry_price: f64) -> TradeAllocation {
        TradeAllocation {
            trade_index: 0,
            trade_key: 0,
            entry_id: "E".to_owned(),
            direction,
            entry_price,
            entry_bar_index: 0,
            entry_time: 10,
            quantity,
            entry_commission: 0.0,
            entry_metadata: StrategyOrderMetadata::default(),
        }
    }

    #[test]
    fn split_covers_flat_same_side_reduce_flatten_and_cross_zero() {
        let cases = [
            (0.0, 2.0, 0.0, 2.0, None, Some(TradeDirection::Long)),
            (0.0, -2.0, 0.0, 2.0, None, Some(TradeDirection::Short)),
            (1.0, 2.0, 0.0, 2.0, None, Some(TradeDirection::Long)),
            (-1.0, -2.0, 0.0, 2.0, None, Some(TradeDirection::Short)),
            (3.0, -1.0, 1.0, 0.0, Some(TradeDirection::Long), None),
            (-3.0, 1.0, 1.0, 0.0, Some(TradeDirection::Short), None),
            (3.0, -3.0, 3.0, 0.0, Some(TradeDirection::Long), None),
            (-2.0, 2.0, 2.0, 0.0, Some(TradeDirection::Short), None),
            (
                2.0,
                -5.0,
                2.0,
                3.0,
                Some(TradeDirection::Long),
                Some(TradeDirection::Short),
            ),
            (
                -2.0,
                5.0,
                2.0,
                3.0,
                Some(TradeDirection::Short),
                Some(TradeDirection::Long),
            ),
        ];
        for (position, delta, close_qty, open_qty, close_dir, open_dir) in cases {
            let split = split_fill_quantities(position, delta).expect("split");
            assert_eq!(split.close_quantity, close_qty, "close {position} {delta}");
            assert_eq!(split.open_quantity, open_qty, "open {position} {delta}");
            assert_eq!(split.close_direction, close_dir);
            assert_eq!(split.open_direction, open_dir);
        }
    }

    #[test]
    fn same_side_addition_from_flat_debits_cash_and_opens_trade() {
        let transition =
            calculate_same_side_addition(&snapshot(0.0, 0.0), request(), 2.0, 100.0, 1.0)
                .expect("flat add");
        assert_eq!(transition.close_quantity, 0.0);
        assert_eq!(transition.open_quantity, 2.0);
        assert_eq!(transition.cash_delta, -201.0);
        assert_eq!(transition.realized_profit, 0.0);
        assert_eq!(
            transition.opened_trade,
            Some(OpenedExposure {
                direction: TradeDirection::Long,
                quantity: 2.0,
                price: 100.0,
                commission: 1.0,
            })
        );
        assert!(transition.routable);
    }

    #[test]
    fn same_side_short_addition_credits_cash() {
        let transition =
            calculate_same_side_addition(&snapshot(-1.0, 50.0), request(), -2.0, 40.0, 0.0)
                .expect("short add");
        assert_eq!(transition.open_quantity, 2.0);
        assert_eq!(transition.cash_delta, 80.0);
        assert_eq!(
            transition.opened_trade.map(|opened| opened.direction),
            Some(TradeDirection::Short)
        );
    }

    #[test]
    fn same_side_helper_rejects_opposite_fill() {
        let error =
            calculate_same_side_addition(&snapshot(2.0, 100.0), request(), -1.0, 110.0, 0.0)
                .expect_err("opposite");
        assert_eq!(error, FillCalcError::NotSameSide);
    }

    #[test]
    fn reduce_only_partial_close_uses_allocations() {
        let transition = calculate_reduce_only(
            &snapshot(2.0, 100.0),
            request(),
            -0.75,
            110.0,
            0.0,
            vec![allocation(TradeDirection::Long, 0.75, 100.0)],
        )
        .expect("partial reduce");
        assert_eq!(transition.close_quantity, 0.75);
        assert_eq!(transition.open_quantity, 0.0);
        assert_eq!(transition.cash_delta, 82.5);
        assert_eq!(transition.realized_profit, 7.5);
        assert!(transition.opened_trade.is_none());
        assert!(transition.routable);
    }

    #[test]
    fn reduce_only_flatten_discards_oversized_remainder() {
        let transition = calculate_reduce_only(
            &snapshot(1.0, 100.0),
            request(),
            -3.0,
            110.0,
            0.0,
            vec![allocation(TradeDirection::Long, 1.0, 100.0)],
        )
        .expect("flatten reduce-only");
        assert_eq!(transition.close_quantity, 1.0);
        assert_eq!(transition.open_quantity, 0.0);
        assert_eq!(transition.filled_quantity, 1.0);
        assert_eq!(transition.cash_delta, 110.0);
        assert_eq!(transition.realized_profit, 10.0);
    }

    #[test]
    fn reduce_only_rejects_flat_or_same_side() {
        assert_eq!(
            calculate_reduce_only(&snapshot(0.0, 0.0), request(), -1.0, 100.0, 0.0, Vec::new())
                .expect_err("flat"),
            FillCalcError::NoReduciblePosition
        );
        assert_eq!(
            calculate_reduce_only(
                &snapshot(1.0, 100.0),
                request(),
                1.0,
                100.0,
                0.0,
                Vec::new()
            )
            .expect_err("same side"),
            FillCalcError::NoReduciblePosition
        );
    }

    #[test]
    fn netting_cross_zero_is_calculated_but_unrouted() {
        let transition = calculate_netting_transition(
            &snapshot(2.0, 100.0),
            request(),
            -5.0,
            110.0,
            0.0,
            0.0,
            vec![allocation(TradeDirection::Long, 2.0, 100.0)],
        )
        .expect("cross-zero");
        assert_eq!(transition.close_quantity, 2.0);
        assert_eq!(transition.open_quantity, 3.0);
        assert_eq!(transition.filled_quantity, 5.0);
        assert_eq!(transition.cash_delta, 220.0 + 330.0);
        assert_eq!(transition.realized_profit, 20.0);
        assert_eq!(
            transition.opened_trade.map(|opened| opened.direction),
            Some(TradeDirection::Short)
        );
        assert!(!transition.routable);
    }

    #[test]
    fn invalid_quantity_and_price_return_errors() {
        assert_eq!(
            calculate_same_side_addition(&snapshot(0.0, 0.0), request(), f64::NAN, 100.0, 0.0)
                .expect_err("nan qty"),
            FillCalcError::NonFiniteQuantity
        );
        assert_eq!(
            calculate_same_side_addition(&snapshot(0.0, 0.0), request(), 0.0, 100.0, 0.0)
                .expect_err("zero qty"),
            FillCalcError::NonPositiveQuantity
        );
        assert_eq!(
            calculate_same_side_addition(&snapshot(0.0, 0.0), request(), 1.0, f64::INFINITY, 0.0)
                .expect_err("inf price"),
            FillCalcError::NonFinitePrice
        );
        assert_eq!(
            split_fill_quantities(f64::NAN, 1.0).expect_err("nan position"),
            FillCalcError::NonFinitePosition
        );
    }

    #[test]
    fn invalid_calculation_does_not_mutate_broker_state() {
        let mut broker = BrokerState::new(100_000.0);
        assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
        let before = broker.clone();

        let error = calculate_same_side_addition(
            &snapshot(broker.position_size, broker.avg_price),
            request(),
            f64::NAN,
            110.0,
            0.0,
        )
        .expect_err("invalid");
        assert_eq!(error, FillCalcError::NonFiniteQuantity);
        assert_eq!(broker, before);
        assert_eq!(broker.position_size, 2.0);
        assert_eq!(broker.cash, 99_800.0);
    }
}
