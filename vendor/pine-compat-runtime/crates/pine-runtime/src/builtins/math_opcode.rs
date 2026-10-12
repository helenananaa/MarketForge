//! Immutable math operation selection; numerical and argument semantics stay in the kernels.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum MathOpcode {
    Abs,
    Max,
    Min,
    Avg,
    Floor,
    Ceil,
    Trunc,
    Sqrt,
    Cbrt,
    Log,
    Log10,
    Exp,
    Acos,
    Asin,
    Atan,
    Sign,
    ToDegrees,
    ToRadians,
    Sin,
    Cos,
    Tan,
    Pow,
    Hypot,
    Round,
    RoundToMintick,
    Random,
    Sum,
}

impl MathOpcode {
    pub(crate) fn for_name(callee: &str) -> Option<Self> {
        Some(match callee {
            "math.abs" => Self::Abs,
            "math.max" => Self::Max,
            "math.min" => Self::Min,
            "math.avg" => Self::Avg,
            "math.floor" => Self::Floor,
            "math.ceil" => Self::Ceil,
            "math.trunc" => Self::Trunc,
            "math.sqrt" => Self::Sqrt,
            "math.cbrt" => Self::Cbrt,
            "math.log" => Self::Log,
            "math.log10" => Self::Log10,
            "math.exp" => Self::Exp,
            "math.acos" => Self::Acos,
            "math.asin" => Self::Asin,
            "math.atan" => Self::Atan,
            "math.sign" => Self::Sign,
            "math.todegrees" => Self::ToDegrees,
            "math.toradians" => Self::ToRadians,
            "math.sin" => Self::Sin,
            "math.cos" => Self::Cos,
            "math.tan" => Self::Tan,
            "math.pow" => Self::Pow,
            "math.hypot" => Self::Hypot,
            "math.round" => Self::Round,
            "math.round_to_mintick" => Self::RoundToMintick,
            "math.random" => Self::Random,
            "math.sum" => Self::Sum,
            _ => return None,
        })
    }
}
