use crate::HirBinaryOp;
use std::cmp::Ordering;

/// Pine's observable numeric comparison boundary. Values retain their full
/// precision; this applies only to comparison, not arithmetic or math.sign.
/// Native controls distinguish this from decimal quantization or relative error.
pub fn pine_numeric_comparison(op: HirBinaryOp, left: f64, right: f64) -> Option<bool> {
    numeric_comparison(op, left, right, true)
}

/// Legacy (v1-v4) constant expressions use exact numeric comparison, while
/// their input/series comparisons use the native runtime boundary.
pub fn exact_numeric_comparison(op: HirBinaryOp, left: f64, right: f64) -> Option<bool> {
    numeric_comparison(op, left, right, false)
}

fn numeric_comparison(
    op: HirBinaryOp,
    left: f64,
    right: f64,
    native_boundary: bool,
) -> Option<bool> {
    let raw = left.partial_cmp(&right)?;
    let order = if native_boundary && (left - right).abs() <= 1e-10 {
        Ordering::Equal
    } else {
        raw
    };
    Some(match op {
        HirBinaryOp::Eq => order == Ordering::Equal,
        HirBinaryOp::NotEq => order != Ordering::Equal,
        HirBinaryOp::Lt => order == Ordering::Less,
        HirBinaryOp::Lte => order != Ordering::Greater,
        HirBinaryOp::Gt => order == Ordering::Greater,
        HirBinaryOp::Gte => order != Ordering::Less,
        _ => return None,
    })
}
