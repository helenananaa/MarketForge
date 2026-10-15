//! Configurable, causal formula adaptations. They are not full ports of their
//! community authors' scripts. Parameters and actual OHLCV drive each instance.
use crate::Candle;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub const FAMILIES: [&str; 26] = [
    "SmaCross",
    "EmaCross",
    "Macd",
    "RsiReversion",
    "BollingerReversion",
    "BollingerBreakout",
    "Donchian",
    "Turtle",
    "Supertrend",
    "UtAtr",
    "Ssl",
    "WaveTrend",
    "Squeeze",
    "Cci",
    "Stochastic",
    "StochRsi",
    "Williams",
    "Adx",
    "VwapReversion",
    "Obv",
    "Ichimoku",
    "Roc",
    "Keltner",
    "HullCross",
    "AlmaCross",
    "MfiReversion",
];

pub fn parameter_keys(family: &str) -> &'static [&'static str] {
    match family {
        "SmaCross" | "EmaCross" | "HullCross" | "Obv" => &["fast", "slow"],
        "Macd" => &["fast", "slow", "signal"],
        "RsiReversion" | "MfiReversion" | "Cci" | "Williams" | "Roc" => &["length", "low", "high"],
        "BollingerReversion" | "BollingerBreakout" | "Supertrend" | "UtAtr" | "Keltner"
        | "VwapReversion" => &["length", "factor"],
        "Donchian" | "Turtle" => &["length", "exit_length"],
        "Ssl" => &["length"],
        "WaveTrend" => &["fast", "slow", "signal", "low", "high"],
        "Squeeze" => &["length", "factor", "channel_factor"],
        "Stochastic" | "StochRsi" => &["length", "smooth", "signal", "low", "high"],
        "Adx" => &["length", "signal", "high"],
        "Ichimoku" => &["fast", "slow", "length", "displacement"],
        "AlmaCross" => &["fast", "slow", "offset", "sigma"],
        _ => &[],
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub fast: usize,
    pub slow: usize,
    pub signal: usize,
    pub length: usize,
    pub exit_length: usize,
    pub smooth: usize,
    pub displacement: usize,
    pub low: f64,
    pub high: f64,
    pub factor: f64,
    pub channel_factor: f64,
    pub offset: f64,
    pub sigma: f64,
}

impl Params {
    pub fn defaults(family: &str) -> Self {
        let mut p = Self {
            fast: 12,
            slow: 26,
            signal: 9,
            length: 20,
            exit_length: 10,
            smooth: 3,
            displacement: 26,
            low: 30.,
            high: 70.,
            factor: 2.,
            channel_factor: 1.5,
            offset: 0.85,
            sigma: 6.,
        };
        match family {
            "SmaCross" => {
                p.fast = 3;
                p.slow = 8;
            }
            "RsiReversion" | "MfiReversion" => {
                p.length = 14;
                p.high = 60.;
            }
            "Turtle" => {
                p.length = 55;
                p.exit_length = 20;
            }
            "Supertrend" => {
                p.length = 10;
                p.factor = 3.;
            }
            "UtAtr" => {
                p.length = 10;
                p.factor = 1.;
            }
            "Ssl" => p.length = 10,
            "WaveTrend" => {
                p.fast = 10;
                p.slow = 21;
                p.signal = 4;
                p.low = -53.;
                p.high = 53.;
            }
            "Cci" => {
                p.low = -100.;
                p.high = 50.;
            }
            "Stochastic" | "StochRsi" => {
                p.length = 14;
                p.low = 25.;
                p.high = 75.;
                p.signal = 3;
            }
            "Williams" => {
                p.length = 14;
                p.low = -80.;
                p.high = -20.;
            }
            "Adx" => {
                p.length = 14;
                p.signal = 14;
                p.low = 0.;
                p.high = 25.;
            }
            "VwapReversion" => {
                p.length = 14;
                p.factor = 1.;
            }
            "Obv" => {
                p.fast = 10;
                p.slow = 30;
            }
            "Ichimoku" => {
                p.fast = 9;
                p.slow = 26;
                p.length = 52;
            }
            "Roc" => {
                p.low = 0.;
                p.high = 2.;
            }
            "HullCross" => {
                p.fast = 16;
                p.slow = 40;
            }
            "AlmaCross" => {
                p.fast = 9;
                p.slow = 21;
            }
            _ => {}
        }
        p
    }
    pub fn valid(&self) -> bool {
        [
            self.fast,
            self.slow,
            self.signal,
            self.length,
            self.exit_length,
            self.smooth,
            self.displacement,
        ]
        .iter()
        .all(|n| (1..=512).contains(n))
            && self.fast < self.slow
            && self.low.is_finite()
            && self.high.is_finite()
            && self.low < self.high
            && self.low.abs() <= 1000.
            && self.high.abs() <= 1000.
            && self.factor.is_finite()
            && (0.1..=20.).contains(&self.factor)
            && self.channel_factor.is_finite()
            && (0.1..=20.).contains(&self.channel_factor)
            && self.offset.is_finite()
            && (0.0..=1.).contains(&self.offset)
            && self.sigma.is_finite()
            && (0.1..=100.).contains(&self.sigma)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Node {
    n: usize,
    values: VecDeque<f64>,
    present: VecDeque<bool>,
    value: Option<f64>,
    samples: u64,
    sma: Option<crate::native_pine::extended::popular::Window>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub long_entry: bool,
    pub long_exit: bool,
    pub short_entry: bool,
    pub short_exit: bool,
    pub ready: bool,
    pub plots: BTreeMap<String, Option<f64>>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signals {
    nodes: BTreeMap<String, Node>,
    previous: Option<Candle>,
    previous_plots: BTreeMap<String, Option<f64>>,
    band_upper: Option<f64>,
    band_lower: Option<f64>,
    direction: i32,
    trail: f64,
    ssl_side: i32,
    obv: f64,
    vwap_day: Option<u64>,
    vwap_volume: f64,
    vwap_notional: f64,
    squeezed: bool,
}

fn gt(a: Option<f64>, b: Option<f64>) -> bool {
    a.zip(b)
        .is_some_and(|(a, b)| a > b && (a - b).abs() > 1e-10)
}
fn lt(a: Option<f64>, b: Option<f64>) -> bool {
    gt(b, a)
}

impl Signals {
    fn window(&mut self, key: &str, x: Option<f64>, n: usize) -> Option<Vec<f64>> {
        let node = self.nodes.entry(key.into()).or_default();
        node.n = n;
        node.values.push_back(x.unwrap_or(0.));
        node.present.push_back(x.is_some());
        if node.values.len() > n {
            node.values.pop_front();
            node.present.pop_front();
        }
        node.samples = node.samples.saturating_add(1);
        (node.values.len() == n && node.present.iter().all(|v| *v))
            .then(|| node.values.iter().copied().collect())
    }
    fn sma(&mut self, key: &str, x: Option<f64>, n: usize) -> Option<f64> {
        let node = self.nodes.entry(key.into()).or_default();
        node.n = n;
        node.samples = node.samples.saturating_add(1);
        node.sma.get_or_insert_with(Default::default).push_sma(x, n)
    }
    fn smooth(&mut self, key: &str, x: Option<f64>, n: usize, rma: bool) -> Option<f64> {
        let node = self.nodes.entry(key.into()).or_default();
        node.n = n;
        node.samples = node.samples.saturating_add(1);
        let value = if let (Some(x), Some(prev)) = (x, node.value) {
            if rma {
                Some((prev * (n - 1) as f64 + x) / n as f64)
            } else {
                let alpha = 2. / (n + 1) as f64;
                Some(alpha * x + (1. - alpha) * prev)
            }
        } else if node.value.is_none() {
            node.values.push_back(x.unwrap_or(0.));
            node.present.push_back(x.is_some());
            if node.values.len() > n {
                node.values.pop_front();
                node.present.pop_front();
            }
            (node.values.len() == n && node.present.iter().all(|v| *v))
                .then(|| node.values.iter().sum::<f64>() / n as f64)
        } else {
            None
        };
        if value.is_some() {
            node.values.clear();
            node.present.clear();
            node.value = value;
        }
        value
    }
    fn ema(&mut self, key: &str, x: Option<f64>, n: usize) -> Option<f64> {
        self.smooth(key, x, n, false)
    }
    fn rma(&mut self, key: &str, x: Option<f64>, n: usize) -> Option<f64> {
        self.smooth(key, x, n, true)
    }
    fn extremes(&mut self, key: &str, x: f64, n: usize) -> Option<(f64, f64)> {
        self.window(key, Some(x), n).map(|w| {
            (
                w.iter().copied().fold(f64::INFINITY, f64::min),
                w.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            )
        })
    }
    fn wma(&mut self, key: &str, x: Option<f64>, n: usize) -> Option<f64> {
        self.window(key, x, n).map(|w| {
            w.iter()
                .enumerate()
                .map(|(i, x)| (i + 1) as f64 * x)
                .sum::<f64>()
                / (n * (n + 1) / 2) as f64
        })
    }
    fn hma(&mut self, key: &str, x: f64, n: usize) -> Option<f64> {
        let half = self.wma(&format!("{key}-half"), Some(x), (n / 2).max(1));
        let full = self.wma(&format!("{key}-full"), Some(x), n);
        self.wma(
            key,
            half.zip(full).map(|(h, f)| 2. * h - f),
            (n as f64).sqrt().round().max(1.) as usize,
        )
    }
    fn alma(&mut self, key: &str, x: f64, n: usize, p: &Params) -> Option<f64> {
        self.window(key, Some(x), n).map(|w| {
            let m = p.offset * (n - 1) as f64;
            let s = n as f64 / p.sigma;
            let weights: Vec<_> = (0..n)
                .map(|i| (-((i as f64 - m).powi(2)) / (2. * s * s)).exp())
                .collect();
            w.iter().zip(&weights).map(|(x, w)| x * w).sum::<f64>() / weights.iter().sum::<f64>()
        })
    }
    fn rsi(&mut self, close: f64, previous: Option<f64>, n: usize) -> Option<f64> {
        let delta = previous.map(|v| close - v);
        let up = self.rma("rsi-up", delta.map(|d| d.max(0.)), n);
        let down = self.rma("rsi-down", delta.map(|d| (-d).max(0.)), n);
        up.zip(down).map(|(u, d)| {
            if d == 0. {
                100.
            } else if u == 0. {
                0.
            } else {
                100. - 100. / (1. + u / d)
            }
        })
    }
    fn cross_up(&self, a: Option<f64>, b: Option<f64>, ak: &str, bk: &str) -> bool {
        a.zip(b).is_some_and(|(a, b)| a > b)
            && self
                .previous_plots
                .get(ak)
                .copied()
                .flatten()
                .zip(self.previous_plots.get(bk).copied().flatten())
                .is_some_and(|(a, b)| a <= b)
    }
    fn cross_down(&self, a: Option<f64>, b: Option<f64>, ak: &str, bk: &str) -> bool {
        self.cross_up(b, a, bk, ak)
    }
    pub fn valid(&self) -> bool {
        self.nodes.len() <= 64
            && self.nodes.values().all(|n| {
                (1..=1024).contains(&n.n)
                    && n.values.len() <= n.n
                    && n.present.len() == n.values.len()
                    && n.values.iter().all(|v| v.is_finite())
                    && n.value.is_none_or(f64::is_finite)
                    && n.sma.as_ref().is_none_or(|s| s.valid(n.n, n.samples))
            })
            && [self.trail, self.obv, self.vwap_volume, self.vwap_notional]
                .iter()
                .all(|x| x.is_finite())
            && self.vwap_volume >= 0.
            && self.band_upper.is_none_or(f64::is_finite)
            && self.band_lower.is_none_or(f64::is_finite)
            && self
                .previous_plots
                .values()
                .all(|v| v.is_none_or(f64::is_finite))
    }

    pub fn update(&mut self, family: &str, p: &Params, b: &Candle) -> Frame {
        let close = b.close_tick as f64;
        let hi = b.high_tick as f64;
        let lo = b.low_tick as f64;
        let previous = self.previous.as_ref().map(|b| b.close_tick as f64);
        let tr = previous.map_or(hi - lo, |prev| {
            (hi - lo).max((hi - prev).abs()).max((lo - prev).abs())
        });
        let typical = (hi + lo + close) / 3.;
        let mut f = Frame::default();
        let (mut buy, mut leave, mut short, mut cover) = (false, false, false, false);
        let mut ready;
        match family {
            "SmaCross" | "EmaCross" | "HullCross" | "AlmaCross" | "Obv" => {
                let x = if family == "Obv" {
                    if let Some(prev) = previous {
                        self.obv += if close > prev {
                            b.volume as f64
                        } else if close < prev {
                            -(b.volume as f64)
                        } else {
                            0.
                        };
                    }
                    self.obv
                } else {
                    close
                };
                let (a, z) = match family {
                    "SmaCross" => (
                        self.sma("fast", Some(x), p.fast),
                        self.sma("slow", Some(x), p.slow),
                    ),
                    "HullCross" => (self.hma("fast", x, p.fast), self.hma("slow", x, p.slow)),
                    "AlmaCross" => (
                        self.alma("fast", x, p.fast, p),
                        self.alma("slow", x, p.slow, p),
                    ),
                    _ => (
                        self.ema("fast", Some(x), p.fast),
                        self.ema("slow", Some(x), p.slow),
                    ),
                };
                ready = a.zip(z).is_some();
                buy = gt(a, z);
                leave = lt(a, z);
                short = leave;
                cover = buy;
                f.plots.extend([("fast".into(), a), ("slow".into(), z)]);
            }
            "Macd" => {
                let a = self.ema("ema-fast", Some(close), p.fast);
                let z = self.ema("ema-slow", Some(close), p.slow);
                let m = a.zip(z).map(|(a, z)| a - z);
                let s = self.ema("macd-signal", m, p.signal);
                ready = m.zip(s).is_some();
                buy = gt(m, s);
                leave = lt(m, s);
                short = leave;
                cover = buy;
                f.plots.extend([("macd".into(), m), ("signal".into(), s)]);
            }
            "RsiReversion" | "StochRsi" => {
                let r = self.rsi(close, previous, p.length);
                if family == "RsiReversion" {
                    ready = r.is_some();
                    buy = lt(r, Some(p.low));
                    leave = gt(r, Some(p.high));
                    short = leave;
                    cover = buy;
                    f.plots.insert("rsi".into(), r);
                } else {
                    self.window("rsi-range", r, p.length);
                    let node = &self.nodes["rsi-range"];
                    let range = (node.values.len() == p.length && node.present.iter().any(|p| *p))
                        .then(|| {
                            let values: Vec<_> = node
                                .values
                                .iter()
                                .zip(&node.present)
                                .filter_map(|(v, p)| p.then_some(*v))
                                .collect();
                            (
                                values.iter().copied().fold(f64::INFINITY, f64::min),
                                values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                            )
                        });
                    let raw = r.zip(range).and_then(|(r, (low, high))| {
                        (high > low).then_some(100. * (r - low) / (high - low))
                    });
                    let k = self.sma("stoch-k", raw, p.smooth);
                    let d = self.sma("stoch-d", k, p.signal);
                    ready = k.zip(d).is_some();
                    buy = self.cross_up(k, d, "k", "d") && lt(k, Some(p.low));
                    leave = self.cross_down(k, d, "k", "d") && gt(k, Some(p.high));
                    short = leave;
                    cover = buy;
                    f.plots.extend([("k".into(), k), ("d".into(), d)]);
                }
            }
            "BollingerReversion" | "BollingerBreakout" | "Squeeze" => {
                let w = self.window("bb", Some(close), p.length);
                let mid = w.as_ref().map(|w| w.iter().sum::<f64>() / w.len() as f64);
                let dev = w.as_ref().zip(mid).map(|(w, m)| {
                    (w.iter().map(|x| (x - m).powi(2)).sum::<f64>() / w.len() as f64).sqrt()
                        * p.factor
                });
                let upper = mid.zip(dev).map(|(m, d)| m + d);
                let lower = mid.zip(dev).map(|(m, d)| m - d);
                ready = mid.is_some();
                if family == "BollingerReversion" {
                    buy = lt(Some(close), lower);
                    leave = gt(Some(close), mid);
                    short = gt(Some(close), upper);
                    cover = lt(Some(close), mid);
                } else if family == "BollingerBreakout" {
                    buy = gt(Some(close), upper);
                    leave = lt(Some(close), mid);
                    short = lt(Some(close), lower);
                    cover = gt(Some(close), mid);
                } else {
                    let range = self
                        .sma("squeeze-range", Some(tr), p.length)
                        .map(|r| r * p.channel_factor);
                    let hh = self.extremes("squeeze-high", hi, p.length).map(|(_, h)| h);
                    let ll = self.extremes("squeeze-low", lo, p.length).map(|(l, _)| l);
                    let val = hh
                        .zip(ll)
                        .zip(mid)
                        .map(|((h, l), m)| close - ((h + l) / 2. + m) / 2.);
                    let mom = self.window("squeeze-mom", val, p.length).map(|w| {
                        let n = w.len() as f64;
                        let meanx = (n - 1.) / 2.;
                        let meany = w.iter().sum::<f64>() / n;
                        let denom = (0..w.len())
                            .map(|i| (i as f64 - meanx).powi(2))
                            .sum::<f64>();
                        if denom == 0. {
                            meany
                        } else {
                            meany
                                + w.iter()
                                    .enumerate()
                                    .map(|(i, y)| (i as f64 - meanx) * (y - meany))
                                    .sum::<f64>()
                                    / denom
                                    * meanx
                        }
                    });
                    let squeezed = lt(dev, range);
                    ready = mom.is_some();
                    buy = self.squeezed && !squeezed && gt(mom, Some(0.));
                    short = self.squeezed && !squeezed && lt(mom, Some(0.));
                    let old = self.previous_plots.get("momentum").copied().flatten();
                    leave = lt(mom, old);
                    cover = gt(mom, old);
                    self.squeezed = squeezed;
                    f.plots.insert("momentum".into(), mom);
                }
                f.plots.extend([
                    ("mid".into(), mid),
                    ("upper".into(), upper),
                    ("lower".into(), lower),
                ]);
            }
            "Donchian" | "Turtle" => {
                // Read the previous windows before inserting this bar: no current-bar channel leakage.
                let old = |nodes: &BTreeMap<String, Node>, key: &str, n: usize, high: bool| {
                    nodes.get(key).filter(|x| x.values.len() == n).map(|x| {
                        x.values.iter().copied().fold(
                            if high {
                                f64::NEG_INFINITY
                            } else {
                                f64::INFINITY
                            },
                            if high { f64::max } else { f64::min },
                        )
                    })
                };
                let upper = old(&self.nodes, "entry-high", p.length, true);
                let lower = old(&self.nodes, "entry-low", p.length, false);
                let exitlow = old(&self.nodes, "exit-low", p.exit_length, false);
                let exithi = old(&self.nodes, "exit-high", p.exit_length, true);
                ready = upper.zip(lower).zip(exitlow).zip(exithi).is_some();
                buy = gt(Some(close), upper);
                short = lt(Some(close), lower);
                leave = lt(Some(close), exitlow);
                cover = gt(Some(close), exithi);
                self.window("entry-high", Some(hi), p.length);
                self.window("entry-low", Some(lo), p.length);
                self.window("exit-high", Some(hi), p.exit_length);
                self.window("exit-low", Some(lo), p.exit_length);
                f.plots.extend([
                    ("upper".into(), upper),
                    ("lower".into(), lower),
                    ("exit_lower".into(), exitlow),
                    ("exit_upper".into(), exithi),
                ]);
            }
            "Supertrend" | "UtAtr" | "Keltner" | "VwapReversion" => {
                let atr = self.rma("atr", Some(tr), p.length);
                ready = atr.is_some();
                if family == "Supertrend" {
                    if let Some(atr) = atr {
                        let upper = (hi + lo) / 2. + p.factor * atr;
                        let lower = (hi + lo) / 2. - p.factor * atr;
                        let oldup = self.band_upper.unwrap_or(upper);
                        let oldlo = self.band_lower.unwrap_or(lower);
                        let upper = if upper < oldup || previous.is_some_and(|c| c > oldup) {
                            upper
                        } else {
                            oldup
                        };
                        let lower = if lower > oldlo || previous.is_some_and(|c| c < oldlo) {
                            lower
                        } else {
                            oldlo
                        };
                        if self.direction == 0 {
                            self.direction = 1;
                        } else if self.direction > 0 && close > upper {
                            self.direction = -1;
                        } else if self.direction < 0 && close < lower {
                            self.direction = 1;
                        }
                        self.band_upper = Some(upper);
                        self.band_lower = Some(lower);
                        buy = self.direction < 0;
                        leave = self.direction > 0;
                        short = leave;
                        cover = buy;
                        f.plots.insert(
                            "line".into(),
                            Some(if self.direction < 0 { lower } else { upper }),
                        );
                    }
                } else if family == "UtAtr" {
                    if let Some(atr) = atr {
                        let loss = p.factor * atr;
                        let prev = self.trail;
                        let trail = if close > prev && previous.is_some_and(|c| c > prev) {
                            prev.max(close - loss)
                        } else if close < prev && previous.is_some_and(|c| c < prev) {
                            prev.min(close + loss)
                        } else if close > prev {
                            close - loss
                        } else {
                            close + loss
                        };
                        buy = self.cross_up(Some(close), Some(trail), "close", "trail");
                        leave = self.cross_down(Some(close), Some(trail), "close", "trail");
                        short = leave;
                        cover = buy;
                        self.trail = trail;
                        f.plots.insert("trail".into(), Some(trail));
                    }
                    f.plots.insert("close".into(), Some(close));
                } else if family == "Keltner" {
                    let mid = self.ema("kc-mid", Some(close), p.length);
                    let upper = mid.zip(atr).map(|(m, a)| m + p.factor * a);
                    let lower = mid.zip(atr).map(|(m, a)| m - p.factor * a);
                    buy = gt(Some(close), upper);
                    leave = lt(Some(close), mid);
                    short = lt(Some(close), lower);
                    cover = gt(Some(close), mid);
                    f.plots.extend([
                        ("mid".into(), mid),
                        ("upper".into(), upper),
                        ("lower".into(), lower),
                    ]);
                } else {
                    let day = b.open_time_ms / 86_400_000;
                    if self.vwap_day != Some(day) {
                        self.vwap_day = Some(day);
                        self.vwap_volume = 0.;
                        self.vwap_notional = 0.;
                    }
                    self.vwap_volume += b.volume as f64;
                    self.vwap_notional += typical * b.volume as f64;
                    let v =
                        (self.vwap_volume > 0.).then_some(self.vwap_notional / self.vwap_volume);
                    let lower = v.zip(atr).map(|(v, a)| v - p.factor * a);
                    let upper = v.zip(atr).map(|(v, a)| v + p.factor * a);
                    ready = v.zip(atr).is_some();
                    buy = lt(Some(close), lower);
                    leave = Some(close).zip(v).is_some_and(|(c, v)| c >= v);
                    short = gt(Some(close), upper);
                    cover = Some(close).zip(v).is_some_and(|(c, v)| c <= v);
                    f.plots.insert("vwap".into(), v);
                }
                f.plots.insert("atr".into(), atr);
            }
            "Ssl" => {
                let a = self.sma("ssl-high", Some(hi), p.length);
                let z = self.sma("ssl-low", Some(lo), p.length);
                ready = a.zip(z).is_some();
                if gt(Some(close), a) {
                    self.ssl_side = 1;
                } else if lt(Some(close), z) {
                    self.ssl_side = -1;
                }
                buy = self.ssl_side > 0;
                leave = self.ssl_side < 0;
                short = leave;
                cover = buy;
                f.plots
                    .extend([("high_ma".into(), a), ("low_ma".into(), z)]);
            }
            "WaveTrend" => {
                let esa = self.ema("wt-esa", Some(typical), p.fast);
                let dev = self.ema("wt-dev", esa.map(|e| (typical - e).abs()), p.fast);
                let ci = Some(esa.zip(dev).map_or(0., |(e, d)| {
                    if d > 0. {
                        (typical - e) / (0.015 * d)
                    } else {
                        0.
                    }
                }));
                let wt = self.ema("wt", ci, p.slow);
                let sig = self.sma("wt-signal", wt, p.signal);
                ready = wt.zip(sig).is_some();
                buy = self.cross_up(wt, sig, "wt", "signal") && lt(wt, Some(p.low));
                leave = self.cross_down(wt, sig, "wt", "signal") && gt(wt, Some(p.high));
                short = leave;
                cover = buy;
                f.plots.extend([("wt".into(), wt), ("signal".into(), sig)]);
            }
            "Cci" => {
                let w = self.window("cci", Some(typical), p.length);
                let c = w.and_then(|w| {
                    let m = w.iter().sum::<f64>() / w.len() as f64;
                    let d = w.iter().map(|x| (x - m).abs()).sum::<f64>() / w.len() as f64;
                    (d > 0.).then_some((typical - m) / (0.015 * d))
                });
                ready = c.is_some();
                buy = lt(c, Some(p.low));
                leave = gt(c, Some(p.high));
                short = gt(c, Some(-p.low));
                cover = lt(c, Some(-p.high));
                f.plots.insert("cci".into(), c);
            }
            "Stochastic" | "Williams" => {
                let hh = self.extremes("stoch-high", hi, p.length).map(|(_, h)| h);
                let ll = self.extremes("stoch-low", lo, p.length).map(|(l, _)| l);
                let raw = hh
                    .zip(ll)
                    .and_then(|(h, l)| (h > l).then_some(100. * (close - l) / (h - l)));
                if family == "Williams" {
                    let w = raw.map(|x| x - 100.);
                    ready = w.is_some();
                    buy = lt(w, Some(p.low));
                    leave = gt(w, Some(p.high));
                    short = leave;
                    cover = buy;
                    f.plots.insert("williams".into(), w);
                } else {
                    let k = self.sma("stoch-k", raw, p.smooth);
                    let d = self.sma("stoch-d", k, p.signal);
                    ready = k.zip(d).is_some();
                    buy = self.cross_up(k, d, "k", "d") && lt(k, Some(p.low));
                    leave = self.cross_down(k, d, "k", "d") && gt(k, Some(p.high));
                    short = leave;
                    cover = buy;
                    f.plots.extend([("k".into(), k), ("d".into(), d)]);
                }
            }
            "Adx" => {
                let up = self.previous.as_ref().map(|b| hi - b.high_tick as f64);
                let down = self.previous.as_ref().map(|b| b.low_tick as f64 - lo);
                let plus = up
                    .zip(down)
                    .map(|(u, d)| if u > d && u > 0. { u } else { 0. });
                let minus = up
                    .zip(down)
                    .map(|(u, d)| if d > u && d > 0. { d } else { 0. });
                let tr = self.rma("adx-tr", previous.map(|_| tr), p.length);
                let plus = self.rma("adx-plus", plus, p.length);
                let minus = self.rma("adx-minus", minus, p.length);
                let plus = plus
                    .zip(tr)
                    .and_then(|(x, t)| (t > 0.).then_some(100. * x / t));
                let minus = minus
                    .zip(tr)
                    .and_then(|(x, t)| (t > 0.).then_some(100. * x / t));
                let dx = plus.zip(minus).map(|(p, m)| {
                    if p + m == 0. {
                        0.
                    } else {
                        100. * (p - m).abs() / (p + m)
                    }
                });
                let adx = self.rma("adx", dx, p.signal);
                ready = plus.zip(minus).zip(adx).is_some();
                buy = gt(plus, minus) && gt(adx, Some(p.high));
                leave = lt(plus, minus);
                short = lt(plus, minus) && gt(adx, Some(p.high));
                cover = gt(plus, minus);
                f.plots.extend([
                    ("plus".into(), plus),
                    ("minus".into(), minus),
                    ("adx".into(), adx),
                ]);
            }
            "Ichimoku" => {
                let highfast = self.extremes("ichi-highfast", hi, p.fast).map(|(_, h)| h);
                let lowfast = self.extremes("ichi-lowfast", lo, p.fast).map(|(l, _)| l);
                let highslow = self.extremes("ichi-highslow", hi, p.slow).map(|(_, h)| h);
                let lowslow = self.extremes("ichi-lowslow", lo, p.slow).map(|(l, _)| l);
                let highlong = self.extremes("ichi-highlong", hi, p.length).map(|(_, h)| h);
                let lowlong = self.extremes("ichi-lowlong", lo, p.length).map(|(l, _)| l);
                let tenkan = highfast.zip(lowfast).map(|(h, l)| (h + l) / 2.);
                let kijun = highslow.zip(lowslow).map(|(h, l)| (h + l) / 2.);
                let span_a = tenkan.zip(kijun).map(|(a, b)| (a + b) / 2.);
                let span_b = highlong.zip(lowlong).map(|(h, l)| (h + l) / 2.);
                let displaced = |nodes: &BTreeMap<String, Node>, key: &str| {
                    nodes
                        .get(key)
                        .filter(|n| {
                            n.values.len() == p.displacement && n.present.front() == Some(&true)
                        })
                        .and_then(|n| n.values.front().copied())
                };
                let a = displaced(&self.nodes, "ichi-span-a");
                let z = displaced(&self.nodes, "ichi-span-b");
                self.window("ichi-span-a", span_a, p.displacement);
                self.window("ichi-span-b", span_b, p.displacement);
                ready = tenkan.zip(kijun).zip(a).zip(z).is_some();
                buy = gt(tenkan, kijun) && gt(Some(close), a) && gt(Some(close), z);
                leave = lt(tenkan, kijun) || lt(Some(close), a);
                short = lt(tenkan, kijun) && lt(Some(close), a) && lt(Some(close), z);
                cover = gt(tenkan, kijun) || gt(Some(close), a);
                f.plots.extend([
                    ("tenkan".into(), tenkan),
                    ("kijun".into(), kijun),
                    ("span_a".into(), a),
                    ("span_b".into(), z),
                ]);
            }
            "Roc" => {
                let old = self
                    .nodes
                    .get("roc")
                    .filter(|n| n.values.len() == p.length)
                    .and_then(|n| n.values.front().copied());
                self.window("roc", Some(close), p.length);
                let r = old.map(|x| 100. * (close - x) / x);
                ready = r.is_some();
                buy = gt(r, Some(p.high));
                leave = lt(r, Some(p.low));
                short = lt(r, Some(-p.high));
                cover = gt(r, Some(-p.low));
                f.plots.insert("roc".into(), r);
            }
            "MfiReversion" => {
                let old = self
                    .previous
                    .as_ref()
                    .map(|b| (b.high_tick + b.low_tick + b.close_tick) as f64 / 3.);
                let cash = typical * b.volume as f64;
                let pos = self.sma(
                    "mfi-positive",
                    old.map(|v| if typical > v { cash } else { 0. }),
                    p.length,
                );
                let neg = self.sma(
                    "mfi-negative",
                    old.map(|v| if typical < v { cash } else { 0. }),
                    p.length,
                );
                let mfi = pos.zip(neg).map(|(p, n)| {
                    if n == 0. {
                        if p == 0. { 50. } else { 100. }
                    } else {
                        100. - 100. / (1. + p / n)
                    }
                });
                ready = mfi.is_some();
                buy = lt(mfi, Some(p.low));
                leave = gt(mfi, Some(p.high));
                short = leave;
                cover = buy;
                f.plots.insert("mfi".into(), mfi);
            }
            _ => unreachable!("validated family"),
        }
        f.ready = ready;
        f.long_entry = buy;
        f.long_exit = leave;
        f.short_entry = short;
        f.short_exit = cover;
        self.previous = Some(b.clone());
        self.previous_plots = f.plots.clone();
        f
    }
}
