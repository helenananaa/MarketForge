#[derive(Debug, Clone, PartialEq)]
pub struct ChartPointValue {
    pub time: Box<PineValue>,
    pub index: Box<PineValue>,
    pub price: Box<PineValue>,
}

impl ChartPointValue {
    #[must_use]
    pub fn new(time: PineValue, index: PineValue, price: PineValue) -> Self {
        Self {
            time: Box::new(time),
            index: Box::new(index),
            price: Box::new(price),
        }
    }

    #[must_use]
    pub fn field(&self, index: usize) -> PineValue {
        match index {
            0 => (*self.time).clone(),
            1 => (*self.index).clone(),
            2 => (*self.price).clone(),
            _ => PineValue::Na,
        }
    }

    pub fn set_field(&mut self, index: usize, value: PineValue) {
        match index {
            0 => *self.time = value,
            1 => *self.index = value,
            2 => *self.price = value,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PineValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Color(u64),
    Plot(u32),
    HLine(u32),
    Label(u32),
    Line(u32),
    LineFill(u32),
    Polyline(u32),
    Box(u32),
    Table(u32),
    ChartPoint(ChartPointValue),
    Array(u32),
    Matrix(u32),
    Map(u32),
    UserType(Vec<PineValue>),
    /// Runtime-owned UDT identity. The owning runtime resolves its fields.
    UserTypeRef(u32),
    Tuple(Vec<PineValue>),
    Na,
    Void,
}

/// Selects one of the chart's built-in price series for `input.source`.
/// External indicator plots require a separate host capability contract.
pub fn chart_source_input_override(source: &str) -> Result<PineValue, String> {
    if matches!(
        source,
        "open" | "high" | "low" | "close" | "hl2" | "hlc3" | "ohlc4" | "hlcc4"
    ) {
        Ok(PineValue::String(source.to_owned()))
    } else {
        Err(format!(
            "input.source override must be open, high, low, close, hl2, hlc3, ohlc4, or hlcc4; got `{source}`"
        ))
    }
}

/// Encodes an RGB or RGBA literal without conflating low-valued RGBA payloads
/// (for example transparent green) with ordinary `0xRRGGBB` colors.
#[must_use]
pub fn encode_color_literal(value: u32, includes_alpha: bool) -> u64 {
    if !includes_alpha {
        return u64::from(value);
    }
    encode_color_rgba(value >> 8, value & 0xFF)
}

/// Encodes separate RGB and alpha channels into Pine's unambiguous color value.
#[must_use]
pub fn encode_color_rgba(rgb: u32, alpha: u32) -> u64 {
    let rgb = rgb & 0xFF_FFFF;
    let alpha = alpha & 0xFF;
    if alpha == 0xFF {
        return u64::from(rgb);
    }
    let encoded = u64::from((rgb << 8) | alpha);
    if encoded <= 0xFF_FFFF {
        (1 << 32) | encoded
    } else {
        encoded
    }
}

/// Returns whether `value` is a valid value in the public numeric color
/// contract.
///
/// Most colors fit in a `u32`. Low-valued RGBA payloads additionally use bit
/// 32 as an alpha discriminator, so valid public colors can exceed `u32::MAX`
/// but never set bits outside that flag and its 24-bit payload.
#[must_use]
pub const fn is_valid_public_color(value: u64) -> bool {
    const COLOR_ALPHA_FLAG: u64 = 1 << 32;
    const LOW_RGBA_PAYLOAD_MASK: u64 = 0xFF_FFFF;

    value <= u32::MAX as u64
        || value & !(COLOR_ALPHA_FLAG | LOW_RGBA_PAYLOAD_MASK) == 0 && value & COLOR_ALPHA_FLAG != 0
}

impl PineValue {
    pub(crate) fn can_reference_collection(&self) -> bool {
        matches!(
            self,
            Self::Array(_)
                | Self::Map(_)
                | Self::Matrix(_)
                | Self::UserTypeRef(_)
                | Self::Tuple(_)
                | Self::UserType(_)
                | Self::ChartPoint(_)
        )
    }

    #[must_use]
    pub fn is_na(&self) -> bool {
        matches!(self, Self::Na)
    }

    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(value) => Some(*value as f64),
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Truncate a numeric value toward zero, matching Pine `int()`.
    #[must_use]
    pub fn as_trunc_i64(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            Self::Float(value) if value.is_finite() => {
                let truncated = value.trunc();
                if truncated > i64::MAX as f64 || truncated < i64::MIN as f64 {
                    None
                } else {
                    Some(truncated as i64)
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PineValue, is_valid_public_color};

    #[test]
    fn truncates_numeric_values_toward_zero() {
        assert_eq!(PineValue::Int(3).as_trunc_i64(), Some(3));
        assert_eq!(PineValue::Float(2.9).as_trunc_i64(), Some(2));
        assert_eq!(PineValue::Float(-2.9).as_trunc_i64(), Some(-2));
        assert_eq!(PineValue::Float(2.0).as_trunc_i64(), Some(2));
        assert_eq!(PineValue::Na.as_trunc_i64(), None);
        assert_eq!(PineValue::Float(f64::NAN).as_trunc_i64(), None);
        assert_eq!(PineValue::Float(f64::INFINITY).as_trunc_i64(), None);
    }

    #[test]
    fn validates_public_numeric_color_encodings() {
        assert!(is_valid_public_color(0));
        assert!(is_valid_public_color(u64::from(u32::MAX)));
        assert!(is_valid_public_color((1 << 32) | 0x00FF_0080));
        assert!(is_valid_public_color((1 << 32) | 0x00FF_FFFF));

        assert!(!is_valid_public_color((1 << 32) | 0x0100_0000));
        assert!(!is_valid_public_color(u64::MAX));
    }
}
