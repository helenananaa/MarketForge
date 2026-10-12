use super::append_history::{APPEND_LEAF_SIZE, AppendHistory};
use super::historical::HistoricalRuntime;
use crate::{BoxOutput, LabelOutput, LineFillOutput, LineOutput, PolylineOutput, TableOutput};

impl HistoricalRuntime<'_> {
    pub(crate) fn display_skip(&self) -> usize {
        self.display_origin.saturating_sub(self.stored_origin)
    }

    pub(crate) fn apply_display_origin(&mut self, origin: usize) {
        if origin <= self.display_origin {
            return;
        }
        self.display_origin = origin;
        self.trim_drawing_snapshots();
        self.trim_alerts();
        let skip = self.display_skip();
        if skip >= APPEND_LEAF_SIZE {
            self.drop_stored_display_prefix(skip);
        }
    }

    fn drop_stored_display_prefix(&mut self, skip: usize) {
        if skip == 0 {
            return;
        }
        for plot in self.plots_mut() {
            plot.values.drop_prefix(skip);
            plot.colors.drop_prefix(skip);
        }
        for item in &mut self.plot_chars {
            item.values.drop_prefix(skip);
            item.chars.drop_prefix(skip);
            item.colors.drop_prefix(skip);
            item.locations.drop_prefix(skip);
            item.texts.drop_prefix(skip);
            item.text_colors.drop_prefix(skip);
            item.sizes.drop_prefix(skip);
        }
        for item in &mut self.plot_shapes {
            item.values.drop_prefix(skip);
            item.styles.drop_prefix(skip);
            item.locations.drop_prefix(skip);
            item.colors.drop_prefix(skip);
            item.texts.drop_prefix(skip);
            item.text_colors.drop_prefix(skip);
            item.sizes.drop_prefix(skip);
        }
        for item in &mut self.plot_arrows {
            item.values.drop_prefix(skip);
            item.color_ups.drop_prefix(skip);
            item.color_downs.drop_prefix(skip);
            item.min_heights.drop_prefix(skip);
            item.max_heights.drop_prefix(skip);
        }
        for item in &mut self.plot_bars {
            item.opens.drop_prefix(skip);
            item.highs.drop_prefix(skip);
            item.lows.drop_prefix(skip);
            item.closes.drop_prefix(skip);
            item.colors.drop_prefix(skip);
        }
        for item in &mut self.plot_candles {
            item.opens.drop_prefix(skip);
            item.highs.drop_prefix(skip);
            item.lows.drop_prefix(skip);
            item.closes.drop_prefix(skip);
            item.colors.drop_prefix(skip);
            item.wick_colors.drop_prefix(skip);
            item.border_colors.drop_prefix(skip);
        }
        for item in &mut self.bg_colors {
            item.values.drop_prefix(skip);
        }
        for item in &mut self.bar_colors {
            item.values.drop_prefix(skip);
        }
        for item in &mut self.fills {
            item.colors.drop_prefix(skip);
            if let Some(samples) = &mut item.gradient {
                samples.drop_prefix(skip);
            }
        }
        self.stored_origin += skip;
    }

    fn trim_alerts(&mut self) {
        let start = self
            .alerts
            .partition_point(|event| event.bar_index < self.display_origin);
        self.alerts.drop_prefix(start);
    }

    fn trim_drawing_snapshots(&mut self) {
        let origin = self.display_origin;
        macro_rules! trim {
            ($items:expr) => {{
                // Read-only inspection preserves identity for unaffected
                // drawings; only histories whose prefix changes are copied.
                let drops: Vec<_> = $items
                    .iter()
                    .filter_map(|item| {
                        let count = dropped_snapshot_count(&item.snapshots, origin);
                        (count > 0).then_some((item.id, count, item.snapshots.len()))
                    })
                    .collect();
                for (id, count, len) in drops {
                    if count == len {
                        $items.remove(id);
                    } else {
                        $items
                            .get_mut(id)
                            .expect("retained drawing")
                            .snapshots
                            .drop_prefix(count);
                    }
                }
            }};
        }
        trim!(self.labels);
        trim!(self.lines);
        trim!(self.line_fills);
        trim!(self.polylines);
        trim!(self.boxes);
        trim!(self.tables);
    }

    pub(crate) fn display_alerts(&self) -> Vec<crate::AlertEvent> {
        let start = self
            .alerts
            .partition_point(|event| event.bar_index < self.display_origin);
        self.alerts.tail(start)
    }

    pub(crate) fn display_labels(&self) -> Vec<LabelOutput> {
        self.labels
            .iter()
            .filter_map(|item| {
                let mut output = item.snapshot();
                retain_snapshots(&mut output.snapshots, self.display_origin);
                (!output.snapshots.is_empty()).then_some(output)
            })
            .collect()
    }

    pub(crate) fn display_lines(&self) -> Vec<LineOutput> {
        self.lines
            .iter()
            .filter_map(|item| {
                let mut output = item.snapshot();
                retain_snapshots(&mut output.snapshots, self.display_origin);
                (!output.snapshots.is_empty()).then_some(output)
            })
            .collect()
    }

    pub(crate) fn display_line_fills(&self) -> Vec<LineFillOutput> {
        self.line_fills
            .iter()
            .filter_map(|item| {
                let mut output = item.snapshot();
                retain_snapshots(&mut output.snapshots, self.display_origin);
                (!output.snapshots.is_empty()).then_some(output)
            })
            .collect()
    }

    pub(crate) fn display_polylines(&self) -> Vec<PolylineOutput> {
        self.polylines
            .iter()
            .filter_map(|item| {
                let mut output = item.snapshot();
                retain_snapshots(&mut output.snapshots, self.display_origin);
                (!output.snapshots.is_empty()).then_some(output)
            })
            .collect()
    }

    pub(crate) fn display_boxes(&self) -> Vec<BoxOutput> {
        self.boxes
            .iter()
            .filter_map(|item| {
                let mut output = item.snapshot();
                retain_snapshots(&mut output.snapshots, self.display_origin);
                (!output.snapshots.is_empty()).then_some(output)
            })
            .collect()
    }

    pub(crate) fn display_tables(&self) -> Vec<TableOutput> {
        self.tables
            .iter()
            .filter_map(|item| {
                let mut output = item.snapshot();
                retain_snapshots(&mut output.snapshots, self.display_origin);
                (!output.snapshots.is_empty()).then_some(output)
            })
            .collect()
    }
}

pub(crate) trait HasBar {
    fn bar_index(&self) -> usize;
    fn exists(&self) -> bool;
}

impl HasBar for crate::LabelSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn exists(&self) -> bool {
        self.exists
    }
}
impl HasBar for crate::LineSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn exists(&self) -> bool {
        self.exists
    }
}
impl HasBar for crate::LineFillSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn exists(&self) -> bool {
        self.exists
    }
}
impl HasBar for crate::PolylineSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn exists(&self) -> bool {
        self.exists
    }
}
impl HasBar for crate::BoxSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn exists(&self) -> bool {
        self.exists
    }
}
impl HasBar for crate::TableSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn exists(&self) -> bool {
        self.exists
    }
}

fn retain_snapshots<S: HasBar>(snapshots: &mut Vec<S>, origin: usize) {
    if snapshots.is_empty() {
        return;
    }
    let keep_last = snapshots.last().is_some_and(HasBar::exists);
    let mut first_kept = snapshots
        .iter()
        .position(|snapshot| snapshot.bar_index() >= origin)
        .unwrap_or(snapshots.len());
    if keep_last && first_kept == snapshots.len() {
        first_kept = snapshots.len() - 1;
    }
    if first_kept > 0 {
        snapshots.drain(..first_kept);
    }
}

pub(crate) fn dropped_snapshot_count<S: Clone + HasBar>(
    history: &AppendHistory<S>,
    origin: usize,
) -> usize {
    let mut count = history.partition_point(|snapshot| snapshot.bar_index() < origin);
    if count == history.len() && history.last().is_some_and(HasBar::exists) {
        count = count.saturating_sub(1);
    }
    count
}
