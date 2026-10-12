use std::cmp::Ordering;

use crate::magnifier::{
    MagnifierInput, MagnifierTickSource, magnifier_absence_diagnostic, magnifier_gap_diagnostic,
    magnifier_host_ticks,
};
use crate::{Bar, RuntimeDiagnostic};

/// Inferred four-point historical path for a standard OHLC bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoricalPathKind {
    OpenHighLowClose,
    OpenLowHighClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathPointKind {
    Open,
    High,
    Low,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathLegDirection {
    Rising,
    Falling,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PathPoint {
    pub index: u8,
    pub kind: PathPointKind,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PathLeg {
    pub index: u8,
    pub from: PathPoint,
    pub to: PathPoint,
    pub direction: PathLegDirection,
}

impl PathLeg {
    pub(crate) fn point(price: f64) -> Self {
        let point = PathPoint {
            index: 0,
            kind: PathPointKind::Open,
            price,
        };
        Self::new(0, point, point)
    }

    fn new(index: u8, from: PathPoint, to: PathPoint) -> Self {
        let direction = match from.price.total_cmp(&to.price) {
            Ordering::Less => PathLegDirection::Rising,
            Ordering::Greater => PathLegDirection::Falling,
            Ordering::Equal => PathLegDirection::Flat,
        };
        Self {
            index,
            from,
            to,
            direction,
        }
    }

    /// Rank used to visit crossings along this monotonic leg.
    ///
    /// Lower rank is visited first: lower prices on a rising leg, higher
    /// prices on a falling leg. A flat leg has a single mark; every finite
    /// price shares the same rank so a zero-length leg cannot order two
    /// distinct crossings.
    pub(crate) fn crossing_rank(self, price: f64) -> f64 {
        match self.direction {
            PathLegDirection::Rising => price,
            PathLegDirection::Falling => -price,
            PathLegDirection::Flat => 0.0,
        }
    }

    pub(crate) fn cmp_crossing_prices(self, left: f64, right: f64) -> Ordering {
        self.crossing_rank(left)
            .total_cmp(&self.crossing_rank(right))
    }

    pub(crate) fn contains_price(self, price: f64) -> bool {
        if !price.is_finite() {
            return false;
        }
        let low = self.from.price.min(self.to.price);
        let high = self.from.price.max(self.to.price);
        price >= low && price <= high
    }

    /// Remaining prices on this monotonic leg, including the current mark.
    pub(crate) fn contains_unconsumed(self, mark: f64, price: f64) -> bool {
        if !self.contains_price(price) {
            return false;
        }
        match self.direction {
            PathLegDirection::Rising => price.total_cmp(&mark) != Ordering::Less,
            PathLegDirection::Falling => price.total_cmp(&mark) != Ordering::Greater,
            PathLegDirection::Flat => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HistoricalPath {
    pub kind: HistoricalPathKind,
    pub points: [PathPoint; 4],
}

impl HistoricalPath {
    /// Build the inferred path from a validated-style OHLC tuple.
    ///
    /// Non-finite prices and bars that violate `low <= open,close <= high`
    /// return `None`. Existing bar-input validation remains the public
    /// failure boundary; this constructor does not invent a second one.
    ///
    /// Equal open-to-high and open-to-low distance selects open-low-high-close
    /// from the 2026-09-03 ADAUSDT sample lock (LIM then STP). That is not a
    /// claim about equal-distance dojis or close-direction variants.
    pub(crate) fn from_ohlc(open: f64, high: f64, low: f64, close: f64) -> Option<Self> {
        if ![open, high, low, close]
            .iter()
            .all(|price| price.is_finite())
        {
            return None;
        }
        if high < open || high < close || low > open || low > close {
            return None;
        }

        let to_high = (open - high).abs();
        let to_low = (open - low).abs();
        let kind = match to_high.total_cmp(&to_low) {
            Ordering::Less => HistoricalPathKind::OpenHighLowClose,
            Ordering::Greater | Ordering::Equal => HistoricalPathKind::OpenLowHighClose,
        };

        let kinds = match kind {
            HistoricalPathKind::OpenHighLowClose => [
                PathPointKind::Open,
                PathPointKind::High,
                PathPointKind::Low,
                PathPointKind::Close,
            ],
            HistoricalPathKind::OpenLowHighClose => [
                PathPointKind::Open,
                PathPointKind::Low,
                PathPointKind::High,
                PathPointKind::Close,
            ],
        };
        let prices = match kind {
            HistoricalPathKind::OpenHighLowClose => [open, high, low, close],
            HistoricalPathKind::OpenLowHighClose => [open, low, high, close],
        };
        let points = [
            PathPoint {
                index: 0,
                kind: kinds[0],
                price: prices[0],
            },
            PathPoint {
                index: 1,
                kind: kinds[1],
                price: prices[1],
            },
            PathPoint {
                index: 2,
                kind: kinds[2],
                price: prices[2],
            },
            PathPoint {
                index: 3,
                kind: kinds[3],
                price: prices[3],
            },
        ];
        Some(Self { kind, points })
    }

    pub(crate) fn from_bar(bar: &Bar) -> Option<Self> {
        Self::from_ohlc(bar.open, bar.high, bar.low, bar.close)
    }

    /// Infer a path for a bar that already passed public validation.
    ///
    /// Inconsistent high/low extremes are clamped onto `open`/`close` so the
    /// broker still walks four points instead of skipping fills. `from_ohlc`
    /// stays strict for unit tests of the pure constructor.
    pub(crate) fn from_validated_bar(bar: &Bar) -> Option<Self> {
        Self::from_bar(bar).or_else(|| {
            let high = bar.high.max(bar.open);
            let low = bar.low.min(bar.open);
            Self::from_ohlc(bar.open, high, low, bar.close)
        })
    }

    pub(crate) fn legs(self) -> [PathLeg; 3] {
        [
            PathLeg::new(0, self.points[0], self.points[1]),
            PathLeg::new(1, self.points[1], self.points[2]),
            PathLeg::new(2, self.points[2], self.points[3]),
        ]
    }
}

/// One validated host bar in the Stage 23 chart-bar event sequence.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MagnifierHostBar {
    pub chart_bar_index: usize,
    pub host_bar_index: usize,
    pub event_time: i64,
    pub source: MagnifierTickSource,
    pub bar: Bar,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MagnifierHostSequence {
    pub bars: Vec<MagnifierHostBar>,
    pub warning: Option<RuntimeDiagnostic>,
}

/// Select the host-bar sequence for one chart bar.
///
/// Disabled magnifier and missing/empty coverage produce one standard chart
/// bar. Enabled coverage walks each lower bar independently and never
/// aggregates a group into one synthetic OHLC bar.
pub(crate) fn magnifier_host_sequence(
    chart_bar_index: usize,
    chart_bar: Bar,
    input: &MagnifierInput,
    enabled: bool,
) -> MagnifierHostSequence {
    if !enabled {
        return MagnifierHostSequence {
            bars: vec![standard_host_bar(chart_bar_index, chart_bar)],
            warning: None,
        };
    }

    let warning = match input.bars_for_chart_bar(chart_bar_index) {
        Some([]) => Some(magnifier_gap_diagnostic(chart_bar_index)),
        None => Some(magnifier_absence_diagnostic(chart_bar_index)),
        Some(_) => None,
    };
    let ticks = magnifier_host_ticks(chart_bar_index, chart_bar, input);
    let bars = ticks
        .ticks
        .into_iter()
        .enumerate()
        .map(|(host_bar_index, bar)| MagnifierHostBar {
            chart_bar_index,
            host_bar_index,
            event_time: bar.time,
            source: ticks.source,
            bar,
        })
        .collect();
    MagnifierHostSequence { bars, warning }
}

fn standard_host_bar(chart_bar_index: usize, chart_bar: Bar) -> MagnifierHostBar {
    MagnifierHostBar {
        chart_bar_index,
        host_bar_index: 0,
        event_time: chart_bar.time,
        source: MagnifierTickSource::StandardOhlc,
        bar: chart_bar,
    }
}

/// Lower-bar gap from the previous host close to the next host open.
///
/// This is a point at `next_open`, not a tradable close-to-open segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MagnifierHostGap {
    pub previous_close: f64,
    pub next_open: f64,
}

impl MagnifierHostGap {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn between(previous: &Bar, next: &Bar) -> Option<Self> {
        if previous.close == next.open {
            None
        } else {
            Some(Self {
                previous_close: previous.close,
                next_open: next.open,
            })
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn crosses(self, price: f64) -> bool {
        if !price.is_finite() {
            return false;
        }
        let low = self.previous_close.min(self.next_open);
        let high = self.previous_close.max(self.next_open);
        price > low && price < high
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MagnifierInput, magnifier_input_from_groups};

    fn kinds(path: &HistoricalPath) -> [PathPointKind; 4] {
        path.points.map(|point| point.kind)
    }

    fn prices(path: &HistoricalPath) -> [f64; 4] {
        path.points.map(|point| point.price)
    }

    fn assert_monotonic_legs(path: &HistoricalPath) {
        for (index, leg) in path.legs().iter().enumerate() {
            assert_eq!(leg.index as usize, index);
            assert_eq!(leg.from, path.points[index]);
            assert_eq!(leg.to, path.points[index + 1]);
            let expected = match leg.from.price.total_cmp(&leg.to.price) {
                Ordering::Less => PathLegDirection::Rising,
                Ordering::Greater => PathLegDirection::Falling,
                Ordering::Equal => PathLegDirection::Flat,
            };
            assert_eq!(leg.direction, expected);
        }
    }

    #[test]
    fn high_first_selects_open_high_low_close() {
        let path = HistoricalPath::from_ohlc(10.0, 11.0, 8.0, 9.0).expect("path");
        assert_eq!(path.kind, HistoricalPathKind::OpenHighLowClose);
        assert_eq!(
            kinds(&path),
            [
                PathPointKind::Open,
                PathPointKind::High,
                PathPointKind::Low,
                PathPointKind::Close,
            ]
        );
        assert_eq!(prices(&path), [10.0, 11.0, 8.0, 9.0]);
        assert_monotonic_legs(&path);
        let legs = path.legs();
        assert_eq!(legs[0].direction, PathLegDirection::Rising);
        assert_eq!(legs[1].direction, PathLegDirection::Falling);
        assert_eq!(legs[2].direction, PathLegDirection::Rising);
    }

    #[test]
    fn low_first_selects_open_low_high_close() {
        let path = HistoricalPath::from_ohlc(10.0, 12.0, 9.0, 11.0).expect("path");
        assert_eq!(path.kind, HistoricalPathKind::OpenLowHighClose);
        assert_eq!(
            kinds(&path),
            [
                PathPointKind::Open,
                PathPointKind::Low,
                PathPointKind::High,
                PathPointKind::Close,
            ]
        );
        assert_eq!(prices(&path), [10.0, 9.0, 12.0, 11.0]);
        assert_monotonic_legs(&path);
        let legs = path.legs();
        assert_eq!(legs[0].direction, PathLegDirection::Falling);
        assert_eq!(legs[1].direction, PathLegDirection::Rising);
        assert_eq!(legs[2].direction, PathLegDirection::Falling);
    }

    #[test]
    fn equal_distance_selects_sample_locked_open_low_high_close() {
        let synthetic = HistoricalPath::from_ohlc(10.0, 12.0, 8.0, 10.0).expect("path");
        assert_eq!(synthetic.kind, HistoricalPathKind::OpenLowHighClose);
        assert_eq!(prices(&synthetic), [10.0, 8.0, 12.0, 10.0]);

        let analogue = HistoricalPath::from_ohlc(0.1939, 0.1941, 0.1937, 0.1939).expect("path");
        assert_eq!(analogue.kind, HistoricalPathKind::OpenLowHighClose);
        assert_eq!(prices(&analogue), [0.1939, 0.1937, 0.1941, 0.1939]);
        let [open_to_low, low_to_high, _] = analogue.legs();
        assert_eq!(open_to_low.direction, PathLegDirection::Falling);
        assert_eq!(
            open_to_low.cmp_crossing_prices(0.1938, 0.19375),
            Ordering::Less
        );
        assert_eq!(low_to_high.direction, PathLegDirection::Rising);
        assert_eq!(
            low_to_high.cmp_crossing_prices(0.1938, 0.1940),
            Ordering::Less,
            "after the low, 0.1938 is visited before STP at 0.1940"
        );
    }

    #[test]
    fn bullish_and_bearish_closes_do_not_change_path_kind() {
        let bearish_high_first = HistoricalPath::from_ohlc(10.0, 11.0, 8.0, 9.0).expect("path");
        let bullish_high_first = HistoricalPath::from_ohlc(10.0, 11.0, 8.0, 10.5).expect("path");
        assert_eq!(
            bearish_high_first.kind,
            HistoricalPathKind::OpenHighLowClose
        );
        assert_eq!(
            bullish_high_first.kind,
            HistoricalPathKind::OpenHighLowClose
        );

        let bearish_low_first = HistoricalPath::from_ohlc(10.0, 12.0, 9.0, 9.5).expect("path");
        let bullish_low_first = HistoricalPath::from_ohlc(10.0, 12.0, 9.0, 11.0).expect("path");
        assert_eq!(bearish_low_first.kind, HistoricalPathKind::OpenLowHighClose);
        assert_eq!(bullish_low_first.kind, HistoricalPathKind::OpenLowHighClose);
    }

    #[test]
    fn open_equals_high_is_high_first_with_a_flat_first_leg() {
        let path = HistoricalPath::from_ohlc(11.0, 11.0, 8.0, 9.0).expect("path");
        assert_eq!(path.kind, HistoricalPathKind::OpenHighLowClose);
        assert_eq!(path.legs()[0].direction, PathLegDirection::Flat);
        assert_eq!(path.legs()[1].direction, PathLegDirection::Falling);
    }

    #[test]
    fn open_equals_low_is_low_first_with_a_flat_first_leg() {
        let path = HistoricalPath::from_ohlc(8.0, 11.0, 8.0, 9.0).expect("path");
        assert_eq!(path.kind, HistoricalPathKind::OpenLowHighClose);
        assert_eq!(path.legs()[0].direction, PathLegDirection::Flat);
        assert_eq!(path.legs()[1].direction, PathLegDirection::Rising);
    }

    #[test]
    fn four_price_doji_keeps_four_points_and_three_flat_legs() {
        let path = HistoricalPath::from_ohlc(5.0, 5.0, 5.0, 5.0).expect("path");
        assert_eq!(path.kind, HistoricalPathKind::OpenLowHighClose);
        assert_eq!(path.points.len(), 4);
        assert!(
            path.legs()
                .iter()
                .all(|leg| leg.direction == PathLegDirection::Flat)
        );
        assert_eq!(
            path.legs()[0].cmp_crossing_prices(5.0, 5.0),
            Ordering::Equal
        );
    }

    #[test]
    fn negative_and_fractional_prices_are_accepted_when_finite() {
        let path = HistoricalPath::from_ohlc(-1.25, -1.0, -2.0, -1.5).expect("path");
        assert_eq!(path.kind, HistoricalPathKind::OpenHighLowClose);
        assert_eq!(prices(&path), [-1.25, -1.0, -2.0, -1.5]);
        let from_bar = HistoricalPath::from_bar(&Bar {
            time: 0,
            open: -1.25,
            high: -1.0,
            low: -2.0,
            close: -1.5,
            volume: 1.0,
        })
        .expect("bar path");
        assert_eq!(from_bar, path);
    }

    #[test]
    fn rising_falling_and_flat_legs_order_crossings() {
        let high_first = HistoricalPath::from_ohlc(10.0, 12.0, 7.0, 9.0).expect("path");
        let [rising, falling, close_leg] = high_first.legs();
        assert_eq!(rising.direction, PathLegDirection::Rising);
        assert_eq!(rising.cmp_crossing_prices(10.5, 11.5), Ordering::Less);
        assert_eq!(rising.cmp_crossing_prices(11.5, 10.5), Ordering::Greater);

        assert_eq!(falling.direction, PathLegDirection::Falling);
        assert_eq!(falling.cmp_crossing_prices(11.0, 9.0), Ordering::Less);
        assert_eq!(falling.cmp_crossing_prices(9.0, 11.0), Ordering::Greater);

        let doji = HistoricalPath::from_ohlc(3.0, 3.0, 3.0, 3.0).expect("path");
        let flat = doji.legs()[0];
        assert_eq!(flat.direction, PathLegDirection::Flat);
        assert_eq!(flat.cmp_crossing_prices(3.0, 3.0), Ordering::Equal);
        assert_eq!(close_leg.direction, PathLegDirection::Rising);
        assert!(rising.contains_unconsumed(10.0, 10.5));
        assert!(!rising.contains_unconsumed(11.0, 10.5));
        assert!(falling.contains_unconsumed(12.0, 11.0));
        assert!(!falling.contains_unconsumed(9.0, 11.0));
    }

    #[test]
    fn non_finite_or_inconsistent_ohlc_is_rejected() {
        assert!(HistoricalPath::from_ohlc(f64::NAN, 2.0, 1.0, 1.5).is_none());
        assert!(HistoricalPath::from_ohlc(1.0, f64::INFINITY, 0.0, 1.0).is_none());
        assert!(HistoricalPath::from_ohlc(1.0, 0.5, 0.0, 1.0).is_none());
        assert!(HistoricalPath::from_ohlc(1.0, 2.0, 1.5, 1.0).is_none());
    }

    #[test]
    fn validated_bar_clamps_inconsistent_extremes() {
        let bar = Bar {
            time: 0,
            open: 3.5,
            high: 4.0,
            low: 3.6,
            close: 3.8,
            volume: 1.0,
        };
        assert!(HistoricalPath::from_bar(&bar).is_none());
        let path = HistoricalPath::from_validated_bar(&bar).expect("clamped path");
        assert_eq!(path.points.map(|point| point.price), [3.5, 3.5, 4.0, 3.8]);

        let close_outside = Bar {
            time: 0,
            open: 96.0,
            high: 96.0,
            low: 96.0,
            close: 40.0,
            volume: 1.0,
        };
        assert!(HistoricalPath::from_bar(&close_outside).is_none());
        assert!(HistoricalPath::from_validated_bar(&close_outside).is_none());
    }

    fn host_bar(time: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            time,
            open,
            high,
            low,
            close,
            volume: 1.0,
        }
    }

    #[test]
    fn disabled_magnifier_sequence_is_one_standard_chart_bar() {
        let chart = host_bar(2_000, 10.0, 12.0, 8.0, 11.0);
        let input = MagnifierInput::new();
        let sequence = magnifier_host_sequence(1, chart, &input, false);
        assert!(sequence.warning.is_none());
        assert_eq!(sequence.bars.len(), 1);
        assert_eq!(sequence.bars[0].host_bar_index, 0);
        assert_eq!(sequence.bars[0].source, MagnifierTickSource::StandardOhlc);
        assert_eq!(sequence.bars[0].bar, chart);
        assert_eq!(sequence.bars[0].chart_bar_index, 1);
    }

    #[test]
    fn missing_and_empty_groups_fall_back_once() {
        let chart = host_bar(2_000, 10.0, 12.0, 8.0, 11.0);
        let missing = magnifier_host_sequence(1, chart, &MagnifierInput::new(), true);
        assert_eq!(missing.bars.len(), 1);
        assert_eq!(missing.bars[0].source, MagnifierTickSource::StandardOhlc);
        assert_eq!(
            missing
                .warning
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("W_MAGNIFIER_FALLBACK")
        );

        let empty = magnifier_input_from_groups(vec![crate::MagnifierChartBarInput {
            chart_bar_index: 1,
            bars: Vec::new(),
        }])
        .expect("empty group");
        let gapped = magnifier_host_sequence(1, chart, &empty, true);
        assert_eq!(gapped.bars.len(), 1);
        assert_eq!(gapped.bars[0].source, MagnifierTickSource::StandardOhlc);
        assert_eq!(
            gapped
                .warning
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("W_MAGNIFIER_GAP")
        );
    }

    #[test]
    fn three_lower_bars_walk_independent_paths() {
        let chart = host_bar(2_000, 10.0, 12.0, 8.0, 11.0);
        let input = magnifier_input_from_groups(vec![crate::MagnifierChartBarInput {
            chart_bar_index: 1,
            bars: vec![
                host_bar(2_000, 10.0, 10.4, 9.8, 10.2),
                host_bar(2_300, 10.2, 10.8, 10.1, 10.6),
                host_bar(2_600, 10.6, 11.8, 10.5, 11.0),
            ],
        }])
        .expect("valid");
        let sequence = magnifier_host_sequence(1, chart, &input, true);
        assert!(sequence.warning.is_none());
        assert_eq!(sequence.bars.len(), 3);
        assert!(
            sequence
                .bars
                .iter()
                .all(|host| host.source == MagnifierTickSource::Intrabars)
        );
        assert_eq!(sequence.bars[0].host_bar_index, 0);
        assert_eq!(sequence.bars[1].host_bar_index, 1);
        assert_eq!(sequence.bars[2].host_bar_index, 2);
        assert_eq!(sequence.bars[0].chart_bar_index, 1);
        let paths: Vec<_> = sequence
            .bars
            .iter()
            .map(|host| HistoricalPath::from_validated_bar(&host.bar).expect("path"))
            .collect();
        assert_ne!(paths[0].points, paths[1].points);
        assert_ne!(paths[1].points, paths[2].points);
        assert_eq!(
            MagnifierHostGap::between(&sequence.bars[0].bar, &sequence.bars[1].bar),
            None
        );
        let gap = MagnifierHostGap::between(&sequence.bars[1].bar, &sequence.bars[2].bar);
        assert!(gap.is_none(), "10.6 to 10.6 is not a gap");
    }

    #[test]
    fn lower_bar_gap_is_a_point_at_next_open() {
        let previous = host_bar(2_000, 10.0, 10.2, 9.9, 10.1);
        let next = host_bar(2_300, 11.0, 11.2, 10.8, 11.1);
        let gap = MagnifierHostGap::between(&previous, &next).expect("gap");
        assert_eq!(gap.previous_close, 10.1);
        assert_eq!(gap.next_open, 11.0);
        assert!(gap.crosses(10.5));
        assert!(!gap.crosses(10.1));
        assert!(!gap.crosses(11.0));
        assert!(!gap.crosses(9.5));
        let next_path = HistoricalPath::from_validated_bar(&next).expect("path");
        assert_eq!(next_path.points[0].price, 11.0);
        assert!(
            next_path.legs().iter().all(|leg| !leg.contains_price(10.5)),
            "gap must not become a tradable close-to-open segment"
        );
    }

    #[test]
    fn sparse_groups_keep_chart_bar_identity() {
        let chart0 = host_bar(1_000, 1.0, 1.0, 1.0, 1.0);
        let chart1 = host_bar(2_000, 2.0, 2.0, 2.0, 2.0);
        let input = magnifier_input_from_groups(vec![crate::MagnifierChartBarInput {
            chart_bar_index: 1,
            bars: vec![host_bar(2_000, 2.1, 2.2, 2.0, 2.1)],
        }])
        .expect("valid");
        let bar0 = magnifier_host_sequence(0, chart0, &input, true);
        assert_eq!(bar0.bars[0].chart_bar_index, 0);
        assert_eq!(bar0.bars[0].source, MagnifierTickSource::StandardOhlc);
        let bar1 = magnifier_host_sequence(1, chart1, &input, true);
        assert_eq!(bar1.bars[0].chart_bar_index, 1);
        assert_eq!(bar1.bars[0].host_bar_index, 0);
        assert_eq!(bar1.bars[0].source, MagnifierTickSource::Intrabars);
    }
}
