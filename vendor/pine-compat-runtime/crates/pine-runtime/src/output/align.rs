use crate::PineValue;

pub(crate) trait BarAlignedOutput {
    type Point;

    fn id(&self) -> u32;
    fn new_padded(id: u32, current_bar: usize) -> Self;
    fn len(&self) -> usize;
    fn pad_to(&mut self, current_bar: usize);
    fn push_point(&mut self, point: Self::Point);
    fn update_point(&mut self, point: Self::Point);
    fn push_na_point(&mut self);
}

pub(crate) fn push_bar_aligned_output<T: BarAlignedOutput>(
    outputs: &mut Vec<T>,
    current_bar: usize,
    id: u32,
    point: T::Point,
) {
    if let Some(output) = outputs.iter_mut().find(|output| output.id() == id) {
        output.pad_to(current_bar);
        if output.len() == current_bar {
            output.push_point(point);
        } else {
            output.update_point(point);
        }
    } else {
        let mut output = T::new_padded(id, current_bar);
        output.push_point(point);
        outputs.push(output);
    }
}

pub(crate) fn finalize_bar_aligned_outputs<T: BarAlignedOutput>(
    outputs: &mut [T],
    current_bar: usize,
) {
    for output in outputs {
        output.pad_to(current_bar);
        if output.len() == current_bar {
            output.push_na_point();
        }
    }
}

pub(crate) struct PlotCharPoint {
    pub(crate) value: PineValue,
    pub(crate) char_value: PineValue,
    pub(crate) color: PineValue,
    pub(crate) location: PineValue,
    pub(crate) text: PineValue,
    pub(crate) text_color: PineValue,
    pub(crate) size: PineValue,
}

pub(crate) struct PlotShapePoint {
    pub(crate) value: PineValue,
    pub(crate) style: PineValue,
    pub(crate) location: PineValue,
    pub(crate) color: PineValue,
    pub(crate) text: PineValue,
    pub(crate) text_color: PineValue,
    pub(crate) size: PineValue,
}

pub(crate) struct PlotArrowPoint {
    pub(crate) value: PineValue,
    pub(crate) color_up: PineValue,
    pub(crate) color_down: PineValue,
    pub(crate) min_height: PineValue,
    pub(crate) max_height: PineValue,
}

pub(crate) struct PlotBarPoint {
    pub(crate) open: PineValue,
    pub(crate) high: PineValue,
    pub(crate) low: PineValue,
    pub(crate) close: PineValue,
    pub(crate) color: PineValue,
}

pub(crate) struct PlotCandlePoint {
    pub(crate) open: PineValue,
    pub(crate) high: PineValue,
    pub(crate) low: PineValue,
    pub(crate) close: PineValue,
    pub(crate) color: PineValue,
    pub(crate) wick_color: PineValue,
    pub(crate) border_color: PineValue,
}
