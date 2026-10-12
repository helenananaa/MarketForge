use crate::{PineValue, RuntimeError};
pub(super) fn combine(
    left: PineValue,
    right: PineValue,
    subtract: bool,
) -> Result<PineValue, RuntimeError> {
    let (PineValue::String(left), PineValue::String(right)) = (left, right) else {
        return Err(RuntimeError {
            message: "display arithmetic requires display values".into(),
        });
    };
    pine_builtins::combine_display_values(&left, &right, subtract)
        .map(PineValue::String)
        .ok_or_else(|| RuntimeError {
            message: "invalid display value in arithmetic".into(),
        })
}
