use super::{format_number_with_mintick, stringify_array_element};
use crate::PineValue;
use crate::runtime::array_values::ArrayValues;

pub(super) fn stringify_array_with_mintick<'a>(
    values: impl Iterator<Item = &'a PineValue>,
    format: &str,
    mintick: f64,
) -> String {
    let mut result = String::from("[");
    for (index, value) in values.enumerate() {
        if index > 0 {
            result.push_str(", ");
        }
        result.push_str(&match value {
            PineValue::Int(v) => format_number_with_mintick(*v as f64, format, mintick),
            PineValue::Float(v) => format_number_with_mintick(*v, format, mintick),
            _ => stringify_array_element(value, format),
        });
    }
    result.push(']');
    result
}

pub(super) fn stringify_matrix_with_mintick(
    values: &ArrayValues,
    rows: usize,
    columns: usize,
    format: &str,
    mintick: f64,
) -> String {
    let mut result = String::from("[");
    for row in 0..rows {
        if row > 0 {
            result.push_str(", ");
        }
        let start = row.saturating_mul(columns);
        let end = start.saturating_add(columns);
        if start > values.len() || end > values.len() {
            return "NaN".to_owned();
        }
        result.push_str(&stringify_array_with_mintick(
            values.view(start, end - start).iter(),
            format,
            mintick,
        ));
    }
    result.push(']');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_borrowed_paged_rows_and_slice_views_without_materializing_payloads() {
        let values = ArrayValues::from((0..390).map(PineValue::Int).collect::<Vec<_>>());
        let rows: Vec<_> = (0..3)
            .map(|row| {
                format!(
                    "[{}]",
                    ((row * 130)..((row + 1) * 130))
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect();
        assert_eq!(
            stringify_matrix_with_mintick(&values, 3, 130, "#.########", 0.01),
            format!("[{}]", rows.join(", "))
        );
        assert_eq!(
            stringify_array_with_mintick(values.view(126, 5).iter(), "#.########", 0.01),
            "[126, 127, 128, 129, 130]"
        );
        assert_eq!(
            stringify_matrix_with_mintick(&values, 4, 130, "#.########", 0.01),
            "NaN"
        );
        assert_eq!(
            stringify_matrix_with_mintick(&values, 2, 0, "#.########", 0.01),
            "[[], []]"
        );
    }
}
