use std::collections::{HashMap, HashSet};

use pine_ir::{HirProgram, SeriesId};

/// Host-selected display window. Compute series, `var`/`varip`, collections and
/// broker records that scripts can still read are not trimmed by this policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputRetention {
    keep_confirmed_bars: Option<usize>,
}

impl OutputRetention {
    #[must_use]
    pub fn unlimited() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn keep_confirmed_bars(count: usize) -> Self {
        Self {
            keep_confirmed_bars: Some(count),
        }
    }

    #[must_use]
    pub fn confirmed_bar_limit(&self) -> Option<usize> {
        self.keep_confirmed_bars
    }

    #[must_use]
    pub fn origin(self, confirmed_bars: usize) -> usize {
        self.keep_confirmed_bars
            .map(|keep| confirmed_bars.saturating_sub(keep))
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRetentionMode {
    StaticTrimmed,
    DynamicFull,
    MaxBarsBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeriesRetention {
    static_depths: HashMap<SeriesId, usize>,
    dynamic_series: HashSet<SeriesId>,
    has_dynamic_offsets: bool,
    max_bars_back: Option<usize>,
    series_max_bars_back: HashMap<SeriesId, usize>,
}

impl SeriesRetention {
    pub(crate) fn from_program(program: &HirProgram) -> Self {
        let series_max_bars_back = program
            .series_max_bars_back
            .iter()
            .map(|value| (value.series_id, value.max_bars_back as usize))
            .collect();
        Self {
            static_depths: program
                .series_history
                .iter()
                .map(|requirement| {
                    (
                        requirement.series_id,
                        requirement.max_constant_offset as usize,
                    )
                })
                .collect(),
            dynamic_series: program
                .series_history
                .iter()
                .filter(|requirement| requirement.has_dynamic_offsets)
                .map(|requirement| requirement.series_id)
                .collect(),
            has_dynamic_offsets: program.history.has_dynamic_offsets,
            max_bars_back: program.max_bars_back.map(|value| value as usize),
            series_max_bars_back,
        }
    }

    pub(crate) fn max_depth_for(&self, series_id: SeriesId) -> Option<usize> {
        let base_depth = if self.dynamic_series.contains(&series_id) {
            self.max_bars_back
        } else {
            let static_depth = self.static_depths.get(&series_id).copied().unwrap_or(0);
            Some(self.max_bars_back.map_or(static_depth, |max_bars_back| {
                static_depth.min(max_bars_back)
            }))
        };
        match (
            base_depth,
            self.series_max_bars_back.get(&series_id).copied(),
        ) {
            (Some(base_depth), Some(series_max_bars_back)) => {
                Some(base_depth.min(series_max_bars_back))
            }
            (None, Some(series_max_bars_back)) => Some(series_max_bars_back),
            (base_depth, None) => base_depth,
        }
    }

    pub(crate) fn mode(&self) -> HistoryRetentionMode {
        if self.max_bars_back.is_some() || !self.series_max_bars_back.is_empty() {
            HistoryRetentionMode::MaxBarsBack
        } else if self.has_dynamic_offsets {
            HistoryRetentionMode::DynamicFull
        } else {
            HistoryRetentionMode::StaticTrimmed
        }
    }
}
