use serde::{Deserialize, Serialize};

use crate::{
    account::ClearingError,
    model::{PriceTick, Qty},
    perp::PerpClearingConfig,
    risk::{PerpRiskConfig, SpotRiskConfig},
    spot::SpotClearingConfig,
    trading::{PerpTradingEngine, SpotTradingEngine},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MarketKind {
    Spot,
    Perp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstrumentConfig {
    pub symbol: String,
    pub tick_size: PriceTick,
    pub lot_size: Qty,
}

impl InstrumentConfig {
    pub fn new(
        symbol: impl Into<String>,
        tick_size: PriceTick,
        lot_size: Qty,
    ) -> Result<Self, MarketConfigError> {
        let config = Self {
            symbol: symbol.into(),
            tick_size,
            lot_size,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), MarketConfigError> {
        if self.symbol.trim().is_empty() {
            return Err(MarketConfigError::EmptySymbol);
        }
        if self.tick_size <= 0 {
            return Err(MarketConfigError::InvalidTickSize);
        }
        if self.lot_size == 0 {
            return Err(MarketConfigError::InvalidLotSize);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotMarketConfig {
    pub instrument: InstrumentConfig,
    pub clearing: SpotClearingConfig,
    pub risk: SpotRiskConfig,
}

impl SpotMarketConfig {
    pub fn validate(&self) -> Result<(), MarketConfigError> {
        self.instrument.validate()
    }

    pub fn build_engine(&self) -> Result<SpotTradingEngine, MarketConfigError> {
        self.validate()?;
        Ok(SpotTradingEngine::new_with_risk(
            self.clearing,
            SpotRiskConfig {
                price_tick_size: Some(self.instrument.tick_size),
                lot_size: Some(self.instrument.lot_size),
                ..self.risk
            },
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpMarketConfig {
    pub instrument: InstrumentConfig,
    pub clearing: PerpClearingConfig,
    pub risk: PerpRiskConfig,
    pub initial_mark_price_tick: PriceTick,
}

impl PerpMarketConfig {
    pub fn validate(&self) -> Result<(), MarketConfigError> {
        self.instrument.validate()?;
        if self.initial_mark_price_tick <= 0 {
            return Err(MarketConfigError::InvalidInitialMarkPrice);
        }
        if self.clearing.leverage == 0 {
            return Err(MarketConfigError::InvalidLeverage);
        }
        Ok(())
    }

    pub fn build_engine(&self) -> Result<PerpTradingEngine, MarketConfigError> {
        self.validate()?;
        PerpTradingEngine::new_with_risk(
            self.clearing,
            self.initial_mark_price_tick,
            PerpRiskConfig {
                price_tick_size: Some(self.instrument.tick_size),
                lot_size: Some(self.instrument.lot_size),
                ..self.risk
            },
        )
        .map_err(MarketConfigError::Clearing)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MarketConfig {
    Spot(SpotMarketConfig),
    Perp(PerpMarketConfig),
}

impl MarketConfig {
    pub fn kind(&self) -> MarketKind {
        match self {
            Self::Spot(_) => MarketKind::Spot,
            Self::Perp(_) => MarketKind::Perp,
        }
    }

    pub fn instrument(&self) -> &InstrumentConfig {
        match self {
            Self::Spot(config) => &config.instrument,
            Self::Perp(config) => &config.instrument,
        }
    }

    pub fn symbol(&self) -> &str {
        &self.instrument().symbol
    }

    pub fn validate(&self) -> Result<(), MarketConfigError> {
        match self {
            Self::Spot(config) => config.validate(),
            Self::Perp(config) => config.validate(),
        }
    }

    pub fn build_engine(&self) -> Result<MarketEngine, MarketConfigError> {
        match self {
            Self::Spot(config) => config.build_engine().map(MarketEngine::Spot),
            Self::Perp(config) => config.build_engine().map(MarketEngine::Perp),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum MarketEngine {
    Spot(SpotTradingEngine),
    Perp(PerpTradingEngine),
}

impl MarketEngine {
    pub fn kind(&self) -> MarketKind {
        match self {
            Self::Spot(_) => MarketKind::Spot,
            Self::Perp(_) => MarketKind::Perp,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketConfigError {
    EmptySymbol,
    InvalidTickSize,
    InvalidLotSize,
    InvalidInitialMarkPrice,
    InvalidLeverage,
    Clearing(ClearingError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Command, Event, NewOrder, OrderKind, RiskRejectReason, Side},
        risk::SpotRiskConfig,
    };

    #[test]
    fn builds_spot_engine_from_market_config() {
        let config = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
            clearing: SpotClearingConfig {
                maker_fee_ppm: 100,
                taker_fee_ppm: 200,
            },
            risk: SpotRiskConfig {
                max_order_qty: Some(10),
                max_order_notional: Some(10_000),
                allow_short: false,
                ..SpotRiskConfig::default()
            },
        });

        assert_eq!(config.kind(), MarketKind::Spot);
        assert_eq!(config.symbol(), "V-BTC-SPOT");
        let engine = config.build_engine().expect("spot engine should build");
        assert_eq!(engine.kind(), MarketKind::Spot);
    }

    #[test]
    fn builds_perp_engine_from_market_config() {
        let config = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
            clearing: PerpClearingConfig {
                maker_fee_ppm: 100,
                taker_fee_ppm: 200,
                leverage: 10,
            },
            risk: PerpRiskConfig {
                max_order_qty: Some(100),
                max_order_notional: Some(1_000_000),
                max_abs_position_qty: Some(200),
                ..PerpRiskConfig::default()
            },
            initial_mark_price_tick: 100,
        });

        assert_eq!(config.kind(), MarketKind::Perp);
        assert_eq!(config.symbol(), "V-BTC-PERP");
        let engine = config.build_engine().expect("perp engine should build");
        assert_eq!(engine.kind(), MarketKind::Perp);
    }

    #[test]
    fn rejects_invalid_instrument_config() {
        assert_eq!(
            InstrumentConfig::new("", 1, 1),
            Err(MarketConfigError::EmptySymbol)
        );
        assert_eq!(
            InstrumentConfig::new("V-BTC", 0, 1),
            Err(MarketConfigError::InvalidTickSize)
        );
        assert_eq!(
            InstrumentConfig::new("V-BTC", 1, 0),
            Err(MarketConfigError::InvalidLotSize)
        );
    }

    #[test]
    fn rejects_invalid_perp_market_config() {
        let invalid_mark = PerpMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 0,
        };
        assert_eq!(
            invalid_mark.validate(),
            Err(MarketConfigError::InvalidInitialMarkPrice)
        );

        let invalid_leverage = PerpMarketConfig {
            initial_mark_price_tick: 100,
            clearing: PerpClearingConfig {
                leverage: 0,
                ..PerpClearingConfig::default()
            },
            ..invalid_mark
        };
        assert_eq!(
            invalid_leverage.validate(),
            Err(MarketConfigError::InvalidLeverage)
        );
    }

    #[test]
    fn built_spot_engine_uses_configured_risk() {
        let config = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig {
                max_order_qty: Some(1),
                max_order_notional: None,
                allow_short: false,
                ..SpotRiskConfig::default()
            },
        });

        let MarketEngine::Spot(mut engine) = config.build_engine().unwrap() else {
            panic!("expected spot engine");
        };
        engine.create_account(1, 10_000);

        let execution = engine
            .apply(Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 1,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 100 },
                qty: 2,
            }))
            .unwrap();

        assert!(matches!(
            execution.events[0].event,
            Event::RiskRejected {
                order_id: 1,
                reason: RiskRejectReason::MaxOrderQtyExceeded
            }
        ));
    }

    #[test]
    fn built_engine_rejects_orders_outside_tick_size() {
        let config = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 5, 1).unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        });

        let MarketEngine::Spot(mut engine) = config.build_engine().unwrap() else {
            panic!("expected spot engine");
        };
        engine.create_account(1, 10_000);

        let execution = engine
            .apply(Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 1,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 102 },
                qty: 1,
            }))
            .unwrap();

        assert!(matches!(
            execution.events[0].event,
            Event::RiskRejected {
                order_id: 1,
                reason: RiskRejectReason::InvalidPriceTick
            }
        ));
    }

    #[test]
    fn built_engine_rejects_orders_outside_lot_size() {
        let config = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-PERP", 1, 10).unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
        });

        let MarketEngine::Perp(mut engine) = config.build_engine().unwrap() else {
            panic!("expected perp engine");
        };
        engine.create_account(1, 10_000);

        let execution = engine
            .apply(Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 1,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 100 },
                qty: 7,
            }))
            .unwrap();

        assert!(matches!(
            execution.events[0].event,
            Event::RiskRejected {
                order_id: 1,
                reason: RiskRejectReason::InvalidLotSize
            }
        ));
    }
}
