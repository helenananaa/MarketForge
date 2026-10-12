use pine_ir::{CallSiteId, HirCallArg};

use crate::algorithms::numeric::{NumericExtreme, finite_float_or_na, numeric_extreme};
use crate::algorithms::random::{default_random_seed, mix_random_seed};
use crate::builtins::args::RuntimeArgs;
use crate::runtime::call_context::RuntimeCallContext;
use crate::{PineValue, RuntimeError};

#[path = "math_opcode.rs"]
mod opcode;

pub(crate) use opcode::MathOpcode;

pub(crate) fn eval_math_call(
    context: &mut RuntimeCallContext<'_, '_>,
    opcode: Option<MathOpcode>,
    call_site_id: CallSiteId,
    raw_args: &[HirCallArg],
    positional_args: bool,
) -> Option<Result<PineValue, RuntimeError>> {
    let opcode = opcode?;
    let args = RuntimeArgs::new(raw_args, positional_args);
    Some(match opcode {
        MathOpcode::Abs => eval_math_abs(context, args),
        MathOpcode::Max => eval_math_extreme(context, args, NumericExtreme::Max),
        MathOpcode::Min => eval_math_extreme(context, args, NumericExtreme::Min),
        MathOpcode::Avg => eval_math_avg(context, args),
        MathOpcode::Floor => eval_math_floor(context, args),
        MathOpcode::Ceil => eval_math_ceil(context, args),
        MathOpcode::Trunc => eval_math_trunc(context, args),
        MathOpcode::Sqrt => eval_math_unary_float(context, args, f64::sqrt),
        MathOpcode::Cbrt => eval_math_unary_float(context, args, f64::cbrt),
        MathOpcode::Log => eval_math_unary_float(context, args, f64::ln),
        MathOpcode::Log10 => eval_math_unary_float(context, args, f64::log10),
        MathOpcode::Exp => eval_math_unary_float(context, args, f64::exp),
        MathOpcode::Acos => eval_math_unary_float(context, args, f64::acos),
        MathOpcode::Asin => eval_math_unary_float(context, args, f64::asin),
        MathOpcode::Atan => eval_math_unary_float(context, args, f64::atan),
        MathOpcode::Sign => eval_math_sign(context, args),
        MathOpcode::ToDegrees => eval_math_unary_float(context, args, f64::to_degrees),
        MathOpcode::ToRadians => eval_math_unary_float(context, args, f64::to_radians),
        MathOpcode::Sin => eval_math_unary_float(context, args, f64::sin),
        MathOpcode::Cos => eval_math_unary_float(context, args, f64::cos),
        MathOpcode::Tan => eval_math_unary_float(context, args, f64::tan),
        MathOpcode::Pow => eval_math_pow(context, args),
        MathOpcode::Hypot => eval_math_hypot(context, args),
        MathOpcode::Round => eval_math_round(context, args),
        MathOpcode::RoundToMintick => eval_math_round_to_mintick(context, args),
        MathOpcode::Random => eval_math_random(context, call_site_id, args),
        MathOpcode::Sum => eval_math_sum(context, call_site_id, args),
    })
}

fn eval_math_abs(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    match args.value(context, 0, "number")? {
        PineValue::Int(value) => Ok(value
            .checked_abs()
            .map(PineValue::Int)
            .unwrap_or_else(|| PineValue::Float((value as f64).abs()))),
        PineValue::Float(value) => Ok(PineValue::Float(value.abs())),
        PineValue::Na => Ok(PineValue::Na),
        _ => Ok(PineValue::Na),
    }
}

fn eval_math_round(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let value = args.value(context, 0, "number")?;
    let precision = args.optional_value(context, 1, "precision")?;
    if precision.is_none() {
        return match value {
            PineValue::Int(value) => Ok(PineValue::Int(value)),
            PineValue::Float(value) => Ok(float_to_int_or_na(round_ties_up(value))),
            PineValue::Na => Ok(PineValue::Na),
            _ => Ok(PineValue::Na),
        };
    }

    let Some(value) = value.as_f64() else {
        return Ok(PineValue::Na);
    };
    let Some(precision) = precision.and_then(|value| value.as_i64()) else {
        return Ok(PineValue::Na);
    };
    let precision = precision.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    let factor = 10_f64.powi(precision);
    Ok(finite_float_or_na(round_ties_up(value * factor) / factor))
}

fn round_ties_up(value: f64) -> f64 {
    (value + 0.5).floor()
}

pub(crate) fn float_to_int_or_na(value: f64) -> PineValue {
    const I64_MAX_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    if !value.is_finite() || value < i64::MIN as f64 || value >= I64_MAX_EXCLUSIVE {
        PineValue::Na
    } else {
        PineValue::Int(value as i64)
    }
}

fn eval_math_round_to_mintick(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let Some(value) = args.value(context, 0, "number")?.as_f64() else {
        return Ok(PineValue::Na);
    };
    let mintick = context.min_tick();
    if !value.is_finite() || mintick <= 0.0 || !mintick.is_finite() {
        return Ok(PineValue::Na);
    }
    let rounded_ticks = (value / mintick + 0.5).floor();
    Ok(finite_float_or_na(rounded_ticks * mintick))
}

fn eval_math_random(
    context: &mut RuntimeCallContext<'_, '_>,
    call_site_id: CallSiteId,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let min = match args.optional_value(context, 0, "min")? {
        Some(value) => {
            let Some(value) = value.as_f64() else {
                return Ok(PineValue::Na);
            };
            value
        }
        None => 0.0,
    };
    let max = match args.optional_value(context, 1, "max")? {
        Some(value) => {
            let Some(value) = value.as_f64() else {
                return Ok(PineValue::Na);
            };
            value
        }
        None => 1.0,
    };
    let seed = match args.optional_value(context, 2, "seed")? {
        Some(value) => value.as_i64(),
        None => None,
    };

    if !min.is_finite() || !max.is_finite() || min >= max {
        return Ok(PineValue::Na);
    }

    let initial_state = seed.map_or_else(
        || default_random_seed(call_site_id),
        |seed| mix_random_seed(seed as u64),
    );
    let unit = context.next_random_unit(call_site_id, initial_state);
    Ok(finite_float_or_na(min + (max - min) * unit))
}

fn eval_math_sum(
    context: &mut RuntimeCallContext<'_, '_>,
    call_site_id: CallSiteId,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let source = args.value(context, 0, "source")?;
    let length = args.value(context, 1, "length")?.as_i64().unwrap_or(0);
    if length <= 0 {
        return Ok(PineValue::Na);
    }

    let Ok(length) = usize::try_from(length) else {
        return Ok(PineValue::Na);
    };
    let window = context.update_sum_window(call_site_id, source, length);
    if !window.is_ready(length) {
        return Ok(PineValue::Na);
    }

    Ok(finite_float_or_na(window.sum))
}

fn eval_math_floor(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    match args.value(context, 0, "number")? {
        PineValue::Int(value) => Ok(PineValue::Int(value)),
        PineValue::Float(value) => Ok(float_to_int_or_na(value.floor())),
        PineValue::Na => Ok(PineValue::Na),
        _ => Ok(PineValue::Na),
    }
}

fn eval_math_ceil(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    match args.value(context, 0, "number")? {
        PineValue::Int(value) => Ok(PineValue::Int(value)),
        PineValue::Float(value) => Ok(float_to_int_or_na(value.ceil())),
        PineValue::Na => Ok(PineValue::Na),
        _ => Ok(PineValue::Na),
    }
}

fn eval_math_trunc(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    match args.value(context, 0, "number")? {
        PineValue::Int(value) => Ok(PineValue::Int(value)),
        PineValue::Float(value) => Ok(float_to_int_or_na(value.trunc())),
        PineValue::Na => Ok(PineValue::Na),
        _ => Ok(PineValue::Na),
    }
}

fn eval_math_unary_float(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
    op: impl FnOnce(f64) -> f64,
) -> Result<PineValue, RuntimeError> {
    let Some(value) = args.value(context, 0, "number")?.as_f64() else {
        return Ok(PineValue::Na);
    };
    Ok(finite_float_or_na(op(value)))
}

fn eval_math_sign(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let Some(value) = args.value(context, 0, "number")?.as_f64() else {
        return Ok(PineValue::Na);
    };
    Ok(PineValue::Float(if value > 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else {
        0.0
    }))
}

fn eval_math_pow(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let Some(base) = args.value(context, 0, "base")?.as_f64() else {
        return Ok(PineValue::Na);
    };
    let Some(exponent) = args.value(context, 1, "exponent")?.as_f64() else {
        return Ok(PineValue::Na);
    };
    Ok(finite_float_or_na(base.powf(exponent)))
}

fn eval_math_hypot(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let Some(left) = args.value(context, 0, "number1")?.as_f64() else {
        return Ok(PineValue::Na);
    };
    let Some(right) = args.value(context, 1, "number2")?.as_f64() else {
        return Ok(PineValue::Na);
    };
    Ok(finite_float_or_na(left.hypot(right)))
}

fn eval_math_avg(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
) -> Result<PineValue, RuntimeError> {
    let mut total = 0.0;
    let mut count = 0.0;
    let mut has_na = false;

    for expr in args.exprs() {
        match context.eval_expr(expr)?.as_f64() {
            Some(value) => {
                total += value;
                count += 1.0;
            }
            None => has_na = true,
        }
    }

    if has_na || count == 0.0 {
        return Ok(PineValue::Na);
    }
    Ok(finite_float_or_na(total / count))
}

fn eval_math_extreme(
    context: &mut RuntimeCallContext<'_, '_>,
    args: RuntimeArgs<'_>,
    mode: NumericExtreme,
) -> Result<PineValue, RuntimeError> {
    let mut current_int: Option<i64> = None;
    let mut current_float: Option<f64> = None;
    let mut has_na = false;

    for expr in args.exprs() {
        match context.eval_expr(expr)? {
            PineValue::Int(value) => {
                if let Some(current) = current_float.as_mut() {
                    *current = numeric_extreme(*current, value as f64, mode);
                } else {
                    current_int = Some(match current_int {
                        Some(current) => match mode {
                            NumericExtreme::Max => current.max(value),
                            NumericExtreme::Min => current.min(value),
                        },
                        None => value,
                    });
                }
            }
            PineValue::Float(value) => {
                current_float = Some(if let Some(current) = current_float {
                    numeric_extreme(current, value, mode)
                } else if let Some(current) = current_int.take() {
                    numeric_extreme(current as f64, value, mode)
                } else {
                    value
                });
            }
            _ => has_na = true,
        }
    }

    if has_na {
        return Ok(PineValue::Na);
    }

    match (current_float, current_int) {
        (Some(value), _) => Ok(PineValue::Float(value)),
        (None, Some(value)) => Ok(PineValue::Int(value)),
        (None, None) => Ok(PineValue::Na),
    }
}
