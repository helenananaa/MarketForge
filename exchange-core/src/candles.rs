use serde::{Deserialize, Serialize};

use crate::model::{PriceTick, Qty, Side, Trade};

pub const CANDLE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Ticker {
    pub bid_tick: Option<PriceTick>,
    pub ask_tick: Option<PriceTick>,
    pub mid_tick: Option<PriceTick>,
    pub last_trade_tick: Option<PriceTick>,
}

impl Ticker {
    pub fn from_book_and_last(
        bid_tick: Option<PriceTick>,
        ask_tick: Option<PriceTick>,
        last_trade_tick: Option<PriceTick>,
    ) -> Self {
        let mid_tick = match (bid_tick, ask_tick) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2),
            _ => None,
        };
        Self {
            bid_tick,
            ask_tick,
            mid_tick,
            last_trade_tick,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Candle {
    pub schema_version: u16,
    pub open_time_ms: u64,
    pub close_time_ms: u64,
    pub open_tick: PriceTick,
    pub high_tick: PriceTick,
    pub low_tick: PriceTick,
    pub close_tick: PriceTick,
    pub volume: Qty,
    pub quote_volume: i128,
    pub trades: u64,
    pub is_final: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimedTrade {
    pub market_time_ms: u64,
    pub price_tick: PriceTick,
    pub qty: Qty,
    pub taker_side: Side,
}

impl TimedTrade {
    pub fn from_trade(market_time_ms: u64, trade: &Trade) -> Self {
        Self {
            market_time_ms,
            price_tick: trade.price_tick,
            qty: trade.qty,
            taker_side: trade.taker_side,
        }
    }
}

/// Aggregate trades onto `[floor(t/interval)*interval, next)` bars.
///
/// - A trade at exactly `open + interval` opens the next bar.
/// - Multiple trades at the same `market_time_ms` apply in the given order.
/// - Empty intervals emit no candle.
/// - A bar is final when `now_ms >= open + interval`. Querying does not
///   require the caller to advance the simulation clock.
pub fn aggregate_candles(
    trades: &[TimedTrade],
    interval_ms: u64,
    now_ms: u64,
) -> Result<Vec<Candle>, CandleError> {
    if interval_ms == 0 {
        return Err(CandleError::InvalidInterval);
    }
    let mut candles: Vec<Candle> = Vec::new();
    for trade in trades {
        if trade.qty == 0 || trade.price_tick <= 0 {
            return Err(CandleError::InvalidTrade);
        }
        let open_time_ms = (trade.market_time_ms / interval_ms) * interval_ms;
        let close_time_ms = open_time_ms.saturating_add(interval_ms);
        let quote = i128::from(trade.price_tick) * i128::from(trade.qty);
        match candles.last_mut() {
            Some(candle) if candle.open_time_ms == open_time_ms => {
                candle.high_tick = candle.high_tick.max(trade.price_tick);
                candle.low_tick = candle.low_tick.min(trade.price_tick);
                candle.close_tick = trade.price_tick;
                candle.volume = candle.volume.saturating_add(trade.qty);
                candle.quote_volume += quote;
                candle.trades = candle.trades.saturating_add(1);
            }
            _ => candles.push(Candle {
                schema_version: CANDLE_SCHEMA_VERSION,
                open_time_ms,
                close_time_ms,
                open_tick: trade.price_tick,
                high_tick: trade.price_tick,
                low_tick: trade.price_tick,
                close_tick: trade.price_tick,
                volume: trade.qty,
                quote_volume: quote,
                trades: 1,
                is_final: now_ms >= close_time_ms,
            }),
        }
    }
    for candle in &mut candles {
        candle.is_final = now_ms >= candle.close_time_ms;
    }
    Ok(candles)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandleError {
    InvalidInterval,
    InvalidTrade,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(time: u64, price: PriceTick, qty: Qty) -> TimedTrade {
        TimedTrade {
            market_time_ms: time,
            price_tick: price,
            qty,
            taker_side: Side::Buy,
        }
    }

    #[test]
    fn hand_computed_ohlcv_fixture_is_exact() {
        let trades = [
            trade(1_000, 100, 1),
            trade(1_500, 110, 2),
            trade(2_000, 105, 1),
            trade(2_000, 99, 3),
        ];
        let candles = aggregate_candles(&trades, 1_000, 2_500).unwrap();
        assert_eq!(candles.len(), 2);
        assert_eq!(
            candles[0],
            Candle {
                schema_version: 1,
                open_time_ms: 1_000,
                close_time_ms: 2_000,
                open_tick: 100,
                high_tick: 110,
                low_tick: 100,
                close_tick: 110,
                volume: 3,
                quote_volume: 320,
                trades: 2,
                is_final: true,
            }
        );
        assert_eq!(candles[1].open_tick, 105);
        assert_eq!(candles[1].high_tick, 105);
        assert_eq!(candles[1].low_tick, 99);
        assert_eq!(candles[1].close_tick, 99);
        assert_eq!(candles[1].volume, 4);
        assert_eq!(candles[1].quote_volume, 402);
        assert!(!candles[1].is_final);
        let later = aggregate_candles(&trades, 1_000, 3_000).unwrap();
        assert!(later[1].is_final);
    }

    #[test]
    fn empty_intervals_emit_no_candle_and_last_trade_is_optional() {
        assert!(aggregate_candles(&[], 1_000, 5_000).unwrap().is_empty());
        let ticker = Ticker::from_book_and_last(Some(10), Some(12), None);
        assert_eq!(ticker.mid_tick, Some(11));
        assert_eq!(ticker.last_trade_tick, None);
        let one_sided = Ticker::from_book_and_last(Some(10), None, Some(9));
        assert_eq!(one_sided.mid_tick, None);
        assert_eq!(one_sided.last_trade_tick, Some(9));
    }
}
