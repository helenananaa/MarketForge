use crate::*;
use pine_ir::ScriptMode;
use std::collections::HashSet;

use super::valuewhen_history::ValueWhenHistory;

impl HistoricalRuntime<'_> {
    #[must_use]
    pub fn profile(&self) -> RuntimeProfile {
        let request_cache_contexts = self
            .request_cache
            .keys()
            .map(RequestCacheKey::context)
            .collect::<HashSet<_>>()
            .len();
        let request_cache_values = self
            .request_cache
            .values()
            .map(|values| values.len())
            .sum::<usize>();
        let request_cache_value_capacity = self
            .request_cache
            .values()
            .map(|values| values.capacity())
            .sum::<usize>();
        let series_buffers = self.series_store.buffers.len();
        let series_values = self
            .series_store
            .buffers
            .values()
            .map(|values| values.len())
            .sum::<usize>();
        let series_capacity = self
            .series_store
            .buffers
            .values()
            .map(|values| values.capacity())
            .sum::<usize>();
        let plot_values = self
            .plots
            .iter()
            .map(|plot| plot.values.len())
            .sum::<usize>();
        let plot_capacity = self
            .plots
            .iter()
            .map(|plot| plot.values.capacity())
            .sum::<usize>();
        let plot_char_values = self
            .plot_chars
            .iter()
            .map(|plot_char| plot_char.values.len())
            .sum::<usize>();
        let plot_char_capacity = self
            .plot_chars
            .iter()
            .map(|plot_char| {
                plot_char.values.capacity()
                    + plot_char.chars.capacity()
                    + plot_char.colors.capacity()
            })
            .sum::<usize>();
        let plot_shape_values = self
            .plot_shapes
            .iter()
            .map(|plot_shape| plot_shape.values.len())
            .sum::<usize>();
        let plot_shape_capacity = self
            .plot_shapes
            .iter()
            .map(|plot_shape| {
                plot_shape.values.capacity()
                    + plot_shape.styles.capacity()
                    + plot_shape.locations.capacity()
                    + plot_shape.colors.capacity()
                    + plot_shape.texts.capacity()
                    + plot_shape.text_colors.capacity()
                    + plot_shape.sizes.capacity()
            })
            .sum::<usize>();
        let plot_arrow_values = self
            .plot_arrows
            .iter()
            .map(|plot_arrow| plot_arrow.values.len())
            .sum::<usize>();
        let plot_arrow_capacity = self
            .plot_arrows
            .iter()
            .map(|plot_arrow| {
                plot_arrow.values.capacity()
                    + plot_arrow.color_ups.capacity()
                    + plot_arrow.color_downs.capacity()
                    + plot_arrow.min_heights.capacity()
                    + plot_arrow.max_heights.capacity()
            })
            .sum::<usize>();
        let plot_bar_values = self
            .plot_bars
            .iter()
            .map(|plot_bar| plot_bar.opens.len())
            .sum::<usize>();
        let plot_bar_capacity = self
            .plot_bars
            .iter()
            .map(|plot_bar| {
                plot_bar.opens.capacity()
                    + plot_bar.highs.capacity()
                    + plot_bar.lows.capacity()
                    + plot_bar.closes.capacity()
                    + plot_bar.colors.capacity()
            })
            .sum::<usize>();
        let plot_candle_values = self
            .plot_candles
            .iter()
            .map(|plot_candle| plot_candle.opens.len())
            .sum::<usize>();
        let plot_candle_capacity = self
            .plot_candles
            .iter()
            .map(|plot_candle| {
                plot_candle.opens.capacity()
                    + plot_candle.highs.capacity()
                    + plot_candle.lows.capacity()
                    + plot_candle.closes.capacity()
                    + plot_candle.colors.capacity()
                    + plot_candle.wick_colors.capacity()
                    + plot_candle.border_colors.capacity()
            })
            .sum::<usize>();
        let bg_color_values = self
            .bg_colors
            .iter()
            .map(|colors| colors.values.len())
            .sum::<usize>();
        let bg_color_capacity = self
            .bg_colors
            .iter()
            .map(|colors| colors.values.capacity())
            .sum::<usize>();
        let bar_color_values = self
            .bar_colors
            .iter()
            .map(|colors| colors.values.len())
            .sum::<usize>();
        let bar_color_capacity = self
            .bar_colors
            .iter()
            .map(|colors| colors.values.capacity())
            .sum::<usize>();
        let rolling_window_values = self
            .ta_state
            .rolling_windows
            .values()
            .map(RollingWindowState::retained_values)
            .sum::<usize>()
            + self
                .ta_state
                .extreme_windows
                .values()
                .map(crate::algorithms::rolling_extreme::RollingExtremeState::retained_values)
                .sum::<usize>();
        let rolling_window_value_capacity = self
            .ta_state
            .rolling_windows
            .values()
            .map(RollingWindowState::retained_capacity)
            .sum::<usize>()
            + self
                .ta_state
                .extreme_windows
                .values()
                .map(crate::algorithms::rolling_extreme::RollingExtremeState::retained_capacity)
                .sum::<usize>()
            + self.selection_scratch.capacity()
            + self.alma_weights.capacity();
        let valuewhen_state_values = self
            .ta_state
            .valuewhen_state
            .values()
            .map(ValueWhenHistory::len)
            .sum::<usize>();
        let valuewhen_state_value_capacity = self
            .ta_state
            .valuewhen_state
            .values()
            .map(ValueWhenHistory::capacity)
            .sum::<usize>();
        let array_values = self
            .array_store
            .values()
            .map(|values| values.len())
            .sum::<usize>();
        let array_value_capacity = self
            .array_store
            .values()
            .map(|values| values.capacity())
            .sum::<usize>();
        let matrix_profile = self.matrix_store_profile();
        let label_snapshots = self
            .labels
            .iter()
            .map(|label| label.snapshots.len())
            .sum::<usize>();
        let label_snapshot_capacity = self
            .labels
            .iter()
            .map(|label| label.snapshots.capacity())
            .sum::<usize>();
        let line_snapshots = self
            .lines
            .iter()
            .map(|line| line.snapshots.len())
            .sum::<usize>();
        let line_snapshot_capacity = self
            .lines
            .iter()
            .map(|line| line.snapshots.capacity())
            .sum::<usize>();
        let line_fill_snapshots = self
            .line_fills
            .iter()
            .map(|line_fill| line_fill.snapshots.len())
            .sum::<usize>();
        let line_fill_snapshot_capacity = self
            .line_fills
            .iter()
            .map(|line_fill| line_fill.snapshots.capacity())
            .sum::<usize>();
        let polyline_snapshots = self
            .polylines
            .iter()
            .map(|polyline| polyline.snapshots.len())
            .sum::<usize>();
        let polyline_snapshot_capacity = self
            .polylines
            .iter()
            .map(|polyline| polyline.snapshots.capacity())
            .sum::<usize>();
        let polyline_points = self
            .polylines
            .iter()
            .flat_map(|polyline| polyline.snapshots.iter())
            .map(|snapshot| snapshot.points.len())
            .sum::<usize>();
        let polyline_point_capacity = self
            .polylines
            .iter()
            .flat_map(|polyline| polyline.snapshots.iter())
            .map(|snapshot| snapshot.points.capacity())
            .sum::<usize>();
        let box_snapshots = self
            .boxes
            .iter()
            .map(|box_output| box_output.snapshots.len())
            .sum::<usize>();
        let box_snapshot_capacity = self
            .boxes
            .iter()
            .map(|box_output| box_output.snapshots.capacity())
            .sum::<usize>();
        let table_cells = self
            .tables
            .iter()
            .flat_map(|table| table.snapshots.iter())
            .map(|snapshot| snapshot.cells.len())
            .sum::<usize>();
        let table_snapshot_capacity = self
            .tables
            .iter()
            .map(|table| table.snapshots.capacity())
            .sum::<usize>();
        let table_cell_capacity = self
            .tables
            .iter()
            .flat_map(|table| table.snapshots.iter())
            .map(|snapshot| snapshot.cells.capacity())
            .sum::<usize>();

        RuntimeProfile {
            bars: self.bars,
            series_buffers,
            series_values,
            series_capacity,
            max_series_depth: self.series_store.max_depth(),
            history_retention_mode: self.series_retention.mode(),
            history_max_constant_offset: self.program.history.max_constant_offset,
            history_max_bars_back: self.program.max_bars_back,
            history_has_dynamic_offsets: self.program.history.has_dynamic_offsets,
            history_dynamic_retention_misses: self.history_dynamic_retention_misses,
            history_dynamic_retention_max_missed_offset: self
                .history_dynamic_retention_max_missed_offset,
            request_cache_entries: self.request_cache.len(),
            request_cache_contexts,
            request_cache_values,
            request_cache_value_capacity,
            symbol_slots: self.current_symbols.len(),
            symbol_capacity: self.current_symbols.capacity(),
            current_series_slots: self.current_series.len(),
            current_series_capacity: self.current_series.capacity(),
            var_slots: self.var_store.len(),
            var_capacity: self.var_store.capacity(),
            array_slots: self.array_store.len(),
            array_capacity: self.array_store.capacity(),
            array_values,
            array_value_capacity,
            matrix_slots: matrix_profile.slots,
            matrix_capacity: matrix_profile.capacity,
            matrix_cells: matrix_profile.cells,
            matrix_cell_capacity: matrix_profile.cell_capacity,
            call_state_slots: self.ta_state.call_state.len(),
            call_state_capacity: self.ta_state.call_state.capacity(),
            valuewhen_state_slots: self.ta_state.valuewhen_state.len(),
            valuewhen_state_capacity: self.ta_state.valuewhen_state.capacity(),
            valuewhen_state_values,
            valuewhen_state_value_capacity,
            rolling_window_slots: self.ta_state.rolling_windows.len()
                + self.ta_state.extreme_windows.len(),
            rolling_window_capacity: self.ta_state.rolling_windows.capacity()
                + self.ta_state.extreme_windows.capacity(),
            rolling_window_values,
            rolling_window_value_capacity,
            rsi_state_slots: self.ta_state.rsi_state.len(),
            rsi_state_capacity: self.ta_state.rsi_state.capacity(),
            macd_state_slots: self.ta_state.macd_state.len(),
            macd_state_capacity: self.ta_state.macd_state.capacity(),
            plots: self.plots.len(),
            plot_values,
            plot_capacity,
            plot_chars: self.plot_chars.len(),
            plot_char_values,
            plot_char_capacity,
            plot_shapes: self.plot_shapes.len(),
            plot_shape_values,
            plot_shape_capacity,
            plot_arrows: self.plot_arrows.len(),
            plot_arrow_values,
            plot_arrow_capacity,
            plot_bars: self.plot_bars.len(),
            plot_bar_values,
            plot_bar_capacity,
            plot_candles: self.plot_candles.len(),
            plot_candle_values,
            plot_candle_capacity,
            bg_colors: self.bg_colors.len(),
            bg_color_values,
            bg_color_capacity,
            bar_colors: self.bar_colors.len(),
            bar_color_values,
            bar_color_capacity,
            hlines: self.hlines.len(),
            hline_capacity: self.hlines.capacity(),
            fills: self.fills.len(),
            fill_capacity: self.fills.capacity(),
            labels: self.labels.len(),
            label_snapshots,
            label_capacity: self.labels.capacity(),
            label_snapshot_capacity,
            lines: self.lines.len(),
            line_snapshots,
            line_capacity: self.lines.capacity(),
            line_snapshot_capacity,
            line_fills: self.line_fills.len(),
            line_fill_snapshots,
            line_fill_capacity: self.line_fills.capacity(),
            line_fill_snapshot_capacity,
            polylines: self.polylines.len(),
            polyline_snapshots,
            polyline_points,
            polyline_capacity: self.polylines.capacity(),
            polyline_snapshot_capacity,
            polyline_point_capacity,
            boxes: self.boxes.len(),
            box_snapshots,
            box_capacity: self.boxes.capacity(),
            box_snapshot_capacity,
            tables: self.tables.len(),
            table_cells,
            table_capacity: self.tables.capacity(),
            table_snapshot_capacity,
            table_cell_capacity,
            strategy_script_passes: if self.program.script_mode == ScriptMode::Strategy {
                self.strategy_scheduler.script_passes()
            } else {
                0
            },
            strategy_recalculation_passes: if self.program.script_mode == ScriptMode::Strategy {
                self.strategy_scheduler.recalculation_passes()
            } else {
                0
            },
            strategy_max_passes_on_bar: if self.program.script_mode == ScriptMode::Strategy {
                self.strategy_scheduler.max_passes_on_bar() as usize
            } else {
                0
            },
            strategy_max_recalculation_passes: if self.program.script_mode == ScriptMode::Strategy {
                self.strategy_scheduler.max_recalculation_passes() as usize
            } else {
                0
            },
        }
    }
}
