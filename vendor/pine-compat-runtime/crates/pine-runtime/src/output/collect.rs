use crate::PineValue;

use super::model::SeriesOutput;
use crate::runtime::plot_history::{RuntimePlot, na_history};

pub(crate) fn push_plot_value(
    outputs: &mut Vec<RuntimePlot>,
    current_bar: usize,
    id: u32,
    value: PineValue,
    color: PineValue,
) {
    if let Some(output) = outputs.iter_mut().find(|output| output.id == id) {
        while output.values.len() < current_bar {
            output.values.push_compact(PineValue::Na);
            output.colors.push_compact(PineValue::Na);
        }
        if output.values.len() == current_bar {
            output.values.push_compact(value);
            output.colors.push_compact(color);
        } else {
            if let Some(current) = output.values.last_mut() {
                *current = value;
            }
            if let Some(current) = output.colors.last_mut() {
                *current = color;
            }
        }
    } else {
        let mut values = vec![PineValue::Na; current_bar];
        values.push(value);
        let mut output = RuntimePlot::new(id, values);
        *output.colors.last_mut().expect("new plot has current bar") = color;
        outputs.push(output);
    }
}

pub(crate) fn finalize_plot_values(outputs: &mut [RuntimePlot], current_bar: usize) {
    for output in outputs {
        while output.values.len() < current_bar {
            output.values.push_compact(PineValue::Na);
            output.colors.push_compact(PineValue::Na);
        }
        if output.values.len() == current_bar {
            output.values.push_compact(PineValue::Na);
            output.colors.push_compact(PineValue::Na);
        }
    }
}

pub(crate) fn push_series_value<T: SeriesOutput>(
    outputs: &mut Vec<T>,
    current_bar: usize,
    id: u32,
    value: PineValue,
) {
    if let Some(output) = outputs.iter_mut().find(|output| output.id() == id) {
        let values = output.values_mut();
        while values.len() < current_bar {
            values.push_compact(PineValue::Na);
        }
        if values.len() == current_bar {
            values.push_compact(value);
        } else if let Some(current) = values.last_mut() {
            *current = value;
        }
    } else {
        let mut values = na_history(current_bar);
        values.push_compact(value);
        outputs.push(T::new(id, values));
    }
}

pub(crate) fn finalize_series_values<T: SeriesOutput>(outputs: &mut [T], current_bar: usize) {
    for output in outputs {
        let values = output.values_mut();
        while values.len() < current_bar {
            values.push_compact(PineValue::Na);
        }
        if values.len() == current_bar {
            values.push_compact(PineValue::Na);
        }
    }
}
