//! Incremental ports of the explicitly named formula-level Pine references.
//! These retain reference trading rules rather than claiming full community
//! strategy compatibility. The shared native Pine host owns orders and recovery.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod common_a;
mod common_b;
pub(crate) mod popular;

// Match pine-ir's pinned series-expression comparison boundary. Builtin TA
// algorithms have their own exact comparisons and must not use this helper.
pub(crate) fn pine_gt(left: f64, right: f64) -> bool {
    left > right && (left - right).abs() > 1e-10
}

pub(crate) fn pine_lt(left: f64, right: f64) -> bool {
    left < right && (left - right).abs() > 1e-10
}

#[cfg(test)]
mod comparison_tests {
    use super::{pine_gt, pine_lt};

    #[test]
    fn script_comparisons_preserve_pinned_absolute_boundary() {
        let macd = -0.0001220703125;
        let signal = -0.00012207031249999997;
        assert!(macd < signal);
        assert!(!pine_lt(macd, signal));
        assert!(!pine_gt(signal, macd));
        assert!(!pine_gt(1e-10, 0.0));
        assert!(pine_gt(1.000001e-10, 0.0));
        assert!(pine_lt(-1.000001e-10, 0.0));
        assert!(pine_gt(1e12 + 0.0001220703125, 1e12));
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BarInput {
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Signals {
    pub buy: bool,
    pub sell: bool,
    pub plots: BTreeMap<String, Option<f64>>,
}

impl Signals {
    pub(crate) fn valid(&self, kind: Kind) -> bool {
        self.plots.len() == kind.plot_names().len()
            && kind
                .plot_names()
                .iter()
                .all(|key| self.plots.contains_key(*key))
            && self
                .plots
                .values()
                .all(|value| value.is_none_or(f64::is_finite))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Kind {
    EmaCross,
    Macd,
    BollingerReversion,
    Roc,
    Stochastic,
    Cci,
    Williams,
    Adx,
    Supertrend,
    UtAtr,
    Ssl,
    WaveTrend,
    Squeeze,
}

pub(crate) const KINDS: [Kind; 13] = [
    Kind::EmaCross,
    Kind::Macd,
    Kind::BollingerReversion,
    Kind::Roc,
    Kind::Stochastic,
    Kind::Cci,
    Kind::Williams,
    Kind::Adx,
    Kind::Supertrend,
    Kind::UtAtr,
    Kind::Ssl,
    Kind::WaveTrend,
    Kind::Squeeze,
];

impl Kind {
    fn plot_names(self) -> &'static [&'static str] {
        match self {
            Self::EmaCross => &["fast", "slow"],
            Self::Macd => &["macd", "signal", "histogram"],
            Self::BollingerReversion => &["mid", "upper", "lower"],
            Self::Roc => &["roc"],
            Self::Stochastic => &["k", "d"],
            Self::Cci => &["cci"],
            Self::Williams => &["williams"],
            Self::Adx => &["plus", "minus", "adx"],
            Self::Supertrend => &["supertrend", "direction"],
            Self::UtAtr => &["atr", "trail"],
            Self::Ssl => &["high_ma", "low_ma", "side"],
            Self::WaveTrend => &["wt", "signal"],
            Self::Squeeze => &["mid", "deviation", "k_range", "momentum", "squeezed"],
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::EmaCross => "NativePineEmaCross",
            Self::Macd => "NativePineMacd",
            Self::BollingerReversion => "NativePineBollingerReversion",
            Self::Roc => "NativePineRoc",
            Self::Stochastic => "NativePineStochastic",
            Self::Cci => "NativePineCci",
            Self::Williams => "NativePineWilliams",
            Self::Adx => "NativePineAdx",
            Self::Supertrend => "NativePineSupertrend",
            Self::UtAtr => "NativePineUtAtr",
            Self::Ssl => "NativePineSsl",
            Self::WaveTrend => "NativePineWaveTrend",
            Self::Squeeze => "NativePineSqueeze",
        }
    }

    pub(crate) fn common_a(self) -> Option<common_a::Kind> {
        match self {
            Self::EmaCross => Some(common_a::Kind::EmaCross),
            Self::Macd => Some(common_a::Kind::Macd),
            Self::BollingerReversion => Some(common_a::Kind::BollingerReversion),
            Self::Roc => Some(common_a::Kind::Roc),
            _ => None,
        }
    }

    pub(crate) fn common_b(self) -> Option<common_b::Kind> {
        match self {
            Self::Stochastic => Some(common_b::Kind::Stochastic),
            Self::Cci => Some(common_b::Kind::Cci),
            Self::Williams => Some(common_b::Kind::Williams),
            Self::Adx => Some(common_b::Kind::Adx),
            _ => None,
        }
    }

    pub(crate) fn popular(self) -> Option<popular::Kind> {
        match self {
            Self::Supertrend => Some(popular::Kind::Supertrend),
            Self::UtAtr => Some(popular::Kind::UtAtr),
            Self::Ssl => Some(popular::Kind::Ssl),
            Self::WaveTrend => Some(popular::Kind::WaveTrend),
            Self::Squeeze => Some(popular::Kind::Squeeze),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) enum State {
    CommonA(Box<common_a::State>),
    CommonB(Box<common_b::State>),
    Popular(Box<popular::State>),
}

impl State {
    pub(crate) fn new(kind: Kind) -> Self {
        if let Some(k) = kind.common_a() {
            Self::CommonA(Box::new(common_a::new(k)))
        } else if let Some(k) = kind.common_b() {
            Self::CommonB(Box::new(common_b::new(k)))
        } else {
            Self::Popular(Box::new(popular::new(
                kind.popular().expect("classified kind"),
            )))
        }
    }

    pub(crate) fn valid(&self, kind: Kind, bars: u64) -> bool {
        match self {
            Self::CommonA(state) => kind
                .common_a()
                .is_some_and(|k| common_a::valid(state, k, bars)),
            Self::CommonB(state) => kind
                .common_b()
                .is_some_and(|k| common_b::valid(state, k, bars)),
            Self::Popular(state) => kind
                .popular()
                .is_some_and(|k| popular::valid(state, k, bars)),
        }
    }

    pub(crate) fn update(&mut self, kind: Kind, bar: BarInput) -> Signals {
        match self {
            Self::CommonA(state) => {
                common_a::update(state, kind.common_a().expect("state kind"), bar)
            }
            Self::CommonB(state) => {
                common_b::update(state, kind.common_b().expect("state kind"), bar)
            }
            Self::Popular(state) => {
                popular::update(state, kind.popular().expect("state kind"), bar)
            }
        }
    }
}
