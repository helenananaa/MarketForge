use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    account::{ClearingError, Money, VenueAccountStore},
    market::{
        AssetId, AssetKind, AssetSelector, InstrumentConfig, InstrumentId, MarketConfig,
        MarketKind, VenueAssetPolicyConfig,
    },
    model::{Command, NewOrder, PriceTick, Qty, Side},
    spot::SpotClearingEvent,
};

const PPM_DENOMINATOR: i128 = 1_000_000;
const HOUR_MS: u64 = 3_600_000;
const MINUTE_MS: u64 = 60_000;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueRuleConfig {
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerRuleConfig,
    #[serde(default)]
    pub trading_session: TradingSessionRuleConfig,
    #[serde(default)]
    pub price_limits: Vec<PriceLimitRuleConfig>,
    #[serde(default)]
    pub settlement: SettlementRuleConfig,
    #[serde(default)]
    pub transfers: TransferPolicyConfig,
}

impl VenueRuleConfig {
    pub fn validate(&self) -> Result<(), VenueRuleConfigError> {
        for price_limit in &self.price_limits {
            price_limit.validate()?;
        }
        Ok(())
    }

    pub fn merge_overrides(mut self, overrides: Self) -> Self {
        if overrides.circuit_breaker != CircuitBreakerRuleConfig::default() {
            self.circuit_breaker = overrides.circuit_breaker;
        }
        if !overrides.trading_session.sessions.is_empty() {
            self.trading_session = overrides.trading_session;
        }
        if !overrides.price_limits.is_empty() {
            self.price_limits = overrides.price_limits;
        }
        if overrides.settlement != SettlementRuleConfig::default() {
            self.settlement = overrides.settlement;
        }
        if overrides.transfers != TransferPolicyConfig::default() {
            self.transfers = overrides.transfers;
        }
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum VenuePreset {
    BinanceLike,
    SseLike {
        reference_price_ticks: BTreeMap<InstrumentId, PriceTick>,
    },
    NasdaqLike,
}

impl VenuePreset {
    pub fn rules_for_markets(
        &self,
        markets: &[MarketConfig],
    ) -> Result<VenueRuleConfig, VenueRuleConfigError> {
        match self {
            Self::BinanceLike => Ok(VenueRuleConfig {
                transfers: TransferPolicyConfig {
                    deposit_delay_steps: 1,
                    withdrawal_delay_steps: 3,
                },
                ..VenueRuleConfig::default()
            }),
            Self::SseLike {
                reference_price_ticks,
            } => {
                let price_limits = markets
                    .iter()
                    .filter(|market| market.kind() == MarketKind::Spot)
                    .map(|market| {
                        let instrument_id = market.instrument_id().to_string();
                        let reference_price_tick = reference_price_ticks
                            .get(&instrument_id)
                            .copied()
                            .ok_or_else(|| VenueRuleConfigError::MissingPresetReferencePrice {
                                preset: "SseLike",
                                instrument_id: instrument_id.clone(),
                            })?;
                        Ok(PriceLimitRuleConfig {
                            instrument_id,
                            reference_price_tick,
                            limit_up_ppm: 100_000,
                            limit_down_ppm: 100_000,
                        })
                    })
                    .collect::<Result<Vec<_>, VenueRuleConfigError>>()?;
                Ok(VenueRuleConfig {
                    trading_session: TradingSessionRuleConfig {
                        sessions: vec![
                            TradingSessionWindow::from_hm(9, 30, 11, 30),
                            TradingSessionWindow::from_hm(13, 0, 15, 0),
                        ],
                    },
                    price_limits,
                    settlement: SettlementRuleConfig {
                        spot_sell_delay_steps: 1,
                    },
                    transfers: TransferPolicyConfig {
                        deposit_delay_steps: 1,
                        withdrawal_delay_steps: 1,
                    },
                    ..VenueRuleConfig::default()
                })
            }
            Self::NasdaqLike => Ok(VenueRuleConfig {
                trading_session: TradingSessionRuleConfig {
                    sessions: vec![TradingSessionWindow::from_hm(9, 30, 16, 0)],
                },
                transfers: TransferPolicyConfig {
                    deposit_delay_steps: 1,
                    withdrawal_delay_steps: 2,
                },
                ..VenueRuleConfig::default()
            }),
        }
    }

    pub fn asset_policy_for_markets(&self, _markets: &[MarketConfig]) -> VenueAssetPolicyConfig {
        match self {
            Self::BinanceLike => VenueAssetPolicyConfig {
                deposit_rules: vec![
                    AssetSelector::Kinds {
                        kinds: kind_set([AssetKind::Crypto, AssetKind::Stablecoin]),
                    },
                    AssetSelector::AssetIds {
                        asset_ids: asset_set(["USD"]),
                    },
                ],
                withdrawal_rules: vec![
                    AssetSelector::Kinds {
                        kinds: kind_set([AssetKind::Crypto, AssetKind::Stablecoin]),
                    },
                    AssetSelector::AssetIds {
                        asset_ids: asset_set(["USD"]),
                    },
                ],
                settlement_rules: vec![
                    AssetSelector::Kinds {
                        kinds: kind_set([AssetKind::Crypto, AssetKind::Stablecoin]),
                    },
                    AssetSelector::AssetIds {
                        asset_ids: asset_set(["USD"]),
                    },
                ],
                margin_rules: vec![AssetSelector::Kinds {
                    kinds: kind_set([AssetKind::Crypto, AssetKind::Stablecoin]),
                }],
                ..VenueAssetPolicyConfig::default()
            },
            Self::NasdaqLike => VenueAssetPolicyConfig {
                deposit_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["USD"]),
                }],
                withdrawal_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["USD"]),
                }],
                settlement_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["USD"]),
                }],
                margin_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["USD"]),
                }],
                ..VenueAssetPolicyConfig::default()
            },
            Self::SseLike { .. } => VenueAssetPolicyConfig {
                deposit_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["CNY"]),
                }],
                withdrawal_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["CNY"]),
                }],
                settlement_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["CNY"]),
                }],
                margin_rules: vec![AssetSelector::AssetIds {
                    asset_ids: asset_set(["CNY"]),
                }],
                ..VenueAssetPolicyConfig::default()
            },
        }
    }
}

fn asset_set<const N: usize>(assets: [&str; N]) -> BTreeSet<AssetId> {
    assets.into_iter().map(str::to_string).collect()
}

fn kind_set<const N: usize>(kinds: [AssetKind; N]) -> BTreeSet<AssetKind> {
    kinds.into_iter().collect()
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CircuitBreakerRuleConfig {
    pub halted: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TradingSessionRuleConfig {
    #[serde(default)]
    pub sessions: Vec<TradingSessionWindow>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TradingSessionWindow {
    pub open_time_ms: u64,
    pub close_time_ms: u64,
}

impl TradingSessionRuleConfig {
    fn is_open(&self, market_time_ms: u64) -> bool {
        if self.sessions.is_empty() {
            return true;
        }
        let day_time_ms = market_time_ms % 86_400_000;
        self.sessions
            .iter()
            .any(|session| session.contains(day_time_ms))
    }
}

impl TradingSessionWindow {
    pub fn from_hm(open_hour: u64, open_minute: u64, close_hour: u64, close_minute: u64) -> Self {
        Self {
            open_time_ms: open_hour
                .saturating_mul(HOUR_MS)
                .saturating_add(open_minute.saturating_mul(MINUTE_MS)),
            close_time_ms: close_hour
                .saturating_mul(HOUR_MS)
                .saturating_add(close_minute.saturating_mul(MINUTE_MS)),
        }
    }

    fn contains(&self, day_time_ms: u64) -> bool {
        if self.open_time_ms == self.close_time_ms {
            return false;
        }
        if self.open_time_ms < self.close_time_ms {
            day_time_ms >= self.open_time_ms && day_time_ms < self.close_time_ms
        } else {
            day_time_ms >= self.open_time_ms || day_time_ms < self.close_time_ms
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PriceLimitRuleConfig {
    pub instrument_id: InstrumentId,
    pub reference_price_tick: PriceTick,
    pub limit_up_ppm: u32,
    pub limit_down_ppm: u32,
}

impl PriceLimitRuleConfig {
    pub fn validate(&self) -> Result<(), VenueRuleConfigError> {
        if self.instrument_id.trim().is_empty() {
            return Err(VenueRuleConfigError::EmptyInstrumentId);
        }
        if self.reference_price_tick <= 0 {
            return Err(VenueRuleConfigError::InvalidReferencePrice);
        }
        if self.limit_up_ppm > 1_000_000 || self.limit_down_ppm > 1_000_000 {
            return Err(VenueRuleConfigError::InvalidPriceLimit);
        }
        if self.price_bounds().is_none() {
            return Err(VenueRuleConfigError::InvalidPriceLimit);
        }
        Ok(())
    }

    fn contains_price(&self, price_tick: PriceTick) -> bool {
        price_tick >= self.lower_price_tick() && price_tick <= self.upper_price_tick()
    }

    fn upper_price_tick(&self) -> PriceTick {
        self.price_bounds()
            .map(|(_, upper)| upper)
            .unwrap_or(PriceTick::MAX)
    }

    fn lower_price_tick(&self) -> PriceTick {
        self.price_bounds()
            .map(|(lower, _)| lower)
            .unwrap_or(PriceTick::MIN)
    }

    fn price_bounds(&self) -> Option<(PriceTick, PriceTick)> {
        let reference = i128::from(self.reference_price_tick);
        let upper_factor = PPM_DENOMINATOR.checked_add(i128::from(self.limit_up_ppm))?;
        let lower_factor = PPM_DENOMINATOR.checked_sub(i128::from(self.limit_down_ppm))?;
        let upper = reference
            .checked_mul(upper_factor)?
            .checked_div(PPM_DENOMINATOR)?;
        let lower = reference
            .checked_mul(lower_factor)?
            .checked_div(PPM_DENOMINATOR)?;
        Some((
            PriceTick::try_from(lower).ok()?,
            PriceTick::try_from(upper).ok()?,
        ))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SettlementRuleConfig {
    pub spot_sell_delay_steps: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferPolicyConfig {
    pub deposit_delay_steps: u64,
    pub withdrawal_delay_steps: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VenueRuleEngine {
    config: VenueRuleConfig,
    pending_spot_buys: BTreeMap<u64, BTreeMap<InstrumentId, Vec<PendingSpotBuy>>>,
}

impl VenueRuleEngine {
    pub fn new(config: VenueRuleConfig) -> Result<Self, VenueRuleConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            pending_spot_buys: BTreeMap::new(),
        })
    }

    pub fn config(&self) -> &VenueRuleConfig {
        &self.config
    }

    pub fn check_order(
        &self,
        context: VenueRuleOrderContext<'_>,
    ) -> Result<(), VenueRuleRejectReason> {
        let VenueRuleOrderContext {
            market_step,
            market_time_ms,
            instrument_id,
            instrument,
            market_kind,
            command,
            venue_accounts,
        } = context;
        let may_increase_risk = match command {
            Command::NewOrder(_) | Command::NewOrderWithProtection { .. } => true,
            // The matching engine only accepts quantity reductions. Price
            // changes can still increase a perp order's notional even when
            // they make the order less aggressive, so keep those blocked
            // while the venue is halted or outside its trading session.
            Command::AmendOrder(amend) => amend.price_tick.is_some(),
            Command::CancelOrder(_)
            | Command::ExpireOrder { .. }
            | Command::SetMarkPrice(_)
            | Command::SettleFunding(_)
            | Command::SetConditionalOrder { .. }
            | Command::SetPositionProtection { .. } => false,
        };
        if may_increase_risk && self.config.circuit_breaker.halted {
            return Err(VenueRuleRejectReason::CircuitBreakerHalted {
                reason: self.config.circuit_breaker.reason.clone(),
            });
        }
        if may_increase_risk && !self.config.trading_session.is_open(market_time_ms) {
            return Err(VenueRuleRejectReason::TradingSessionClosed { market_time_ms });
        }

        match command {
            Command::NewOrder(order) | Command::NewOrderWithProtection { order, .. } => {
                self.check_order_price_limit(instrument_id, order)?;
                self.check_spot_settlement(
                    market_step,
                    instrument_id,
                    instrument,
                    market_kind,
                    order,
                    venue_accounts,
                )
            }
            Command::AmendOrder(amend) => self.check_price_limit(instrument_id, amend.price_tick),
            Command::CancelOrder(_)
            | Command::ExpireOrder { .. }
            | Command::SetMarkPrice(_)
            | Command::SettleFunding(_)
            | Command::SetConditionalOrder { .. }
            | Command::SetPositionProtection { .. } => Ok(()),
        }
    }

    pub fn record_spot_clearing(
        &mut self,
        market_step: u64,
        instrument_id: &str,
        clearing_events: &[SpotClearingEvent],
    ) {
        if self.config.settlement.spot_sell_delay_steps == 0 {
            return;
        }

        let available_after_step =
            market_step.saturating_add(self.config.settlement.spot_sell_delay_steps);
        for event in clearing_events {
            let SpotClearingEvent::TradeSettled {
                buyer_account_id,
                qty,
                ..
            } = event;
            self.pending_spot_buys
                .entry(*buyer_account_id)
                .or_default()
                .entry(instrument_id.to_string())
                .or_default()
                .push(PendingSpotBuy {
                    qty: *qty,
                    available_after_step,
                    legacy_available_after_seq: None,
                });
        }
    }

    pub(crate) fn normalize_after_restore(
        &mut self,
        next_command_seq: u64,
        market_step: u64,
    ) -> Result<(), ClearingError> {
        for by_instrument in self.pending_spot_buys.values_mut() {
            for pending_buys in by_instrument.values_mut() {
                for pending_buy in pending_buys {
                    let Some(legacy_deadline) = pending_buy.legacy_available_after_seq.take()
                    else {
                        continue;
                    };
                    let remaining_commands = legacy_deadline.saturating_sub(next_command_seq);
                    pending_buy.available_after_step = market_step
                        .checked_add(remaining_commands)
                        .ok_or(ClearingError::BalanceOverflow)?;
                }
            }
        }
        Ok(())
    }

    fn check_order_price_limit(
        &self,
        instrument_id: &str,
        order: &NewOrder,
    ) -> Result<(), VenueRuleRejectReason> {
        self.check_price_limit(instrument_id, order.kind.limit_price_tick())
    }

    fn check_price_limit(
        &self,
        instrument_id: &str,
        price_tick: Option<PriceTick>,
    ) -> Result<(), VenueRuleRejectReason> {
        let Some(price_limit) = self
            .config
            .price_limits
            .iter()
            .find(|price_limit| price_limit.instrument_id == instrument_id)
        else {
            return Ok(());
        };

        let Some(price_tick) = price_tick else {
            return Ok(());
        };

        if price_limit.contains_price(price_tick) {
            Ok(())
        } else {
            Err(VenueRuleRejectReason::PriceLimitExceeded {
                price_tick,
                lower_price_tick: price_limit.lower_price_tick(),
                upper_price_tick: price_limit.upper_price_tick(),
            })
        }
    }

    fn check_spot_settlement(
        &self,
        market_step: u64,
        instrument_id: &str,
        instrument: &InstrumentConfig,
        market_kind: MarketKind,
        order: &NewOrder,
        venue_accounts: &VenueAccountStore,
    ) -> Result<(), VenueRuleRejectReason> {
        if market_kind != MarketKind::Spot
            || order.side != Side::Sell
            || self.config.settlement.spot_sell_delay_steps == 0
        {
            return Ok(());
        }

        let available_base = venue_accounts
            .balance_snapshot(order.account_id, &instrument.base_asset)
            .map(|balance| balance.available)
            .unwrap_or(0);
        let unsettled = self.unsettled_spot_buy_qty(order.account_id, instrument_id, market_step);
        let sellable = available_base.saturating_sub(Money::from(unsettled));

        if sellable < Money::from(order.qty) {
            return Err(VenueRuleRejectReason::SpotPositionUnsettled {
                account_id: order.account_id,
                instrument_id: instrument_id.to_string(),
                requested_qty: order.qty,
                sellable_qty: Qty::try_from(sellable.max(0)).unwrap_or(Qty::MAX),
                unsettled_qty: unsettled,
            });
        }

        Ok(())
    }

    fn unsettled_spot_buy_qty(
        &self,
        account_id: u64,
        instrument_id: &str,
        market_step: u64,
    ) -> Qty {
        self.pending_spot_buys
            .get(&account_id)
            .and_then(|by_instrument| by_instrument.get(instrument_id))
            .map(|pending| {
                pending
                    .iter()
                    .filter(|buy| buy.available_after_step > market_step)
                    .map(|buy| buy.qty)
                    .sum()
            })
            .unwrap_or(0)
    }
}

pub struct VenueRuleOrderContext<'a> {
    pub market_step: u64,
    pub market_time_ms: u64,
    pub instrument_id: &'a str,
    pub instrument: &'a InstrumentConfig,
    pub market_kind: MarketKind,
    pub command: &'a Command,
    pub venue_accounts: &'a VenueAccountStore,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PendingSpotBuy {
    pub qty: Qty,
    pub available_after_step: u64,
    #[serde(skip)]
    legacy_available_after_seq: Option<u64>,
}

impl<'de> Deserialize<'de> for PendingSpotBuy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct PendingSpotBuyWire {
            qty: Qty,
            #[serde(default)]
            available_after_step: Option<u64>,
            #[serde(default)]
            available_after_seq: Option<u64>,
        }

        let wire = PendingSpotBuyWire::deserialize(deserializer)?;
        if let Some(available_after_step) = wire.available_after_step {
            return Ok(Self {
                qty: wire.qty,
                available_after_step,
                legacy_available_after_seq: None,
            });
        }
        let available_after_seq = wire
            .available_after_seq
            .ok_or_else(|| serde::de::Error::missing_field("available_after_step"))?;
        Ok(Self {
            qty: wire.qty,
            available_after_step: available_after_seq,
            legacy_available_after_seq: Some(available_after_seq),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VenueRuleConfigError {
    EmptyInstrumentId,
    InvalidReferencePrice,
    InvalidPriceLimit,
    MissingPresetReferencePrice {
        preset: &'static str,
        instrument_id: InstrumentId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VenueRuleRejectReason {
    CircuitBreakerHalted {
        reason: Option<String>,
    },
    TradingSessionClosed {
        market_time_ms: u64,
    },
    PriceLimitExceeded {
        price_tick: PriceTick,
        lower_price_tick: PriceTick,
        upper_price_tick: PriceTick,
    },
    SpotPositionUnsettled {
        account_id: u64,
        instrument_id: InstrumentId,
        requested_qty: Qty,
        sellable_qty: Qty,
        unsettled_qty: Qty,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AmendOrder, CancelOrder, SetMarkPrice};

    fn rule_context<'a>(
        command: &'a Command,
        instrument: &'a InstrumentConfig,
        venue_accounts: &'a VenueAccountStore,
        market_step: u64,
        market_time_ms: u64,
    ) -> VenueRuleOrderContext<'a> {
        VenueRuleOrderContext {
            market_step,
            market_time_ms,
            instrument_id: &instrument.instrument_id,
            instrument,
            market_kind: MarketKind::Spot,
            command,
            venue_accounts,
        }
    }

    fn test_new_order() -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id: 1,
            account_id: 20,
            side: Side::Buy,
            kind: crate::model::OrderKind::Limit { price_tick: 100 },
            qty: 1,
            reduce_only: false,
        })
    }

    #[test]
    fn circuit_breaker_blocks_new_risk_but_allows_risk_reducing_commands() {
        let engine = VenueRuleEngine::new(VenueRuleConfig {
            circuit_breaker: CircuitBreakerRuleConfig {
                halted: true,
                reason: Some("risk halt".to_string()),
            },
            ..VenueRuleConfig::default()
        })
        .unwrap();
        let instrument = InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap();
        let accounts = VenueAccountStore::new();

        assert!(matches!(
            engine.check_order(rule_context(
                &test_new_order(),
                &instrument,
                &accounts,
                0,
                0
            )),
            Err(VenueRuleRejectReason::CircuitBreakerHalted { .. })
        ));
        for command in [
            Command::CancelOrder(CancelOrder { order_id: 1 }),
            Command::AmendOrder(AmendOrder {
                order_id: 1,
                price_tick: None,
                qty: Some(1),
            }),
            Command::SetMarkPrice(SetMarkPrice { price_tick: 100 }),
        ] {
            assert_eq!(
                engine.check_order(rule_context(&command, &instrument, &accounts, 0, 0)),
                Ok(())
            );
        }
        let price_amend = Command::AmendOrder(AmendOrder {
            order_id: 1,
            price_tick: Some(101),
            qty: None,
        });
        assert!(matches!(
            engine.check_order(rule_context(&price_amend, &instrument, &accounts, 0, 0)),
            Err(VenueRuleRejectReason::CircuitBreakerHalted { .. })
        ));
    }

    #[test]
    fn closed_session_blocks_new_orders_but_allows_cancel_and_mark_updates() {
        let engine = VenueRuleEngine::new(VenueRuleConfig {
            trading_session: TradingSessionRuleConfig {
                sessions: vec![TradingSessionWindow {
                    open_time_ms: 1_000,
                    close_time_ms: 2_000,
                }],
            },
            ..VenueRuleConfig::default()
        })
        .unwrap();
        let instrument = InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap();
        let accounts = VenueAccountStore::new();

        assert!(matches!(
            engine.check_order(rule_context(
                &test_new_order(),
                &instrument,
                &accounts,
                0,
                0
            )),
            Err(VenueRuleRejectReason::TradingSessionClosed { .. })
        ));
        for command in [
            Command::CancelOrder(CancelOrder { order_id: 1 }),
            Command::SetMarkPrice(SetMarkPrice { price_tick: 100 }),
        ] {
            assert_eq!(
                engine.check_order(rule_context(&command, &instrument, &accounts, 0, 0)),
                Ok(())
            );
        }
        let price_amend = Command::AmendOrder(AmendOrder {
            order_id: 1,
            price_tick: Some(101),
            qty: None,
        });
        assert!(matches!(
            engine.check_order(rule_context(&price_amend, &instrument, &accounts, 0, 0)),
            Err(VenueRuleRejectReason::TradingSessionClosed { .. })
        ));
    }

    #[test]
    fn validates_price_limit_config() {
        let config = VenueRuleConfig {
            price_limits: vec![PriceLimitRuleConfig {
                instrument_id: "SSE:600000:CNY".to_string(),
                reference_price_tick: 100,
                limit_up_ppm: 100_000,
                limit_down_ppm: 100_000,
            }],
            ..VenueRuleConfig::default()
        };

        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn legacy_command_seq_settlement_deadline_normalizes_to_market_step() {
        let legacy_buy: PendingSpotBuy = serde_json::from_value(serde_json::json!({
            "qty": 5,
            "available_after_seq": 12
        }))
        .unwrap();
        let mut engine = VenueRuleEngine::new(VenueRuleConfig {
            settlement: SettlementRuleConfig {
                spot_sell_delay_steps: 2,
            },
            ..VenueRuleConfig::default()
        })
        .unwrap();
        engine
            .pending_spot_buys
            .entry(20)
            .or_default()
            .entry("V-BTC-SPOT".to_string())
            .or_default()
            .push(legacy_buy);

        engine.normalize_after_restore(10, 7).unwrap();

        assert_eq!(engine.unsettled_spot_buy_qty(20, "V-BTC-SPOT", 8), 5);
        assert_eq!(engine.unsettled_spot_buy_qty(20, "V-BTC-SPOT", 9), 0);
        let serialized = serde_json::to_value(&engine).unwrap();
        assert!(serialized.to_string().contains("available_after_step"));
        assert!(!serialized.to_string().contains("available_after_seq"));
    }

    #[test]
    fn rejects_price_limit_bounds_that_do_not_fit_without_panicking() {
        let too_wide = PriceLimitRuleConfig {
            instrument_id: "BTC-USDT".to_string(),
            reference_price_tick: PriceTick::MAX,
            limit_up_ppm: 1_000_000,
            limit_down_ppm: 0,
        };
        assert_eq!(
            too_wide.validate(),
            Err(VenueRuleConfigError::InvalidPriceLimit)
        );

        let invalid_rate = PriceLimitRuleConfig {
            instrument_id: "BTC-USDT".to_string(),
            reference_price_tick: 100,
            limit_up_ppm: 1_000_001,
            limit_down_ppm: 0,
        };
        assert_eq!(
            invalid_rate.validate(),
            Err(VenueRuleConfigError::InvalidPriceLimit)
        );
    }

    #[test]
    fn sse_preset_builds_sessions_price_limits_and_t_plus_one() {
        let market = MarketConfig::Spot(crate::market::SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "sse",
                "sse:600000:cny",
                "600000",
                "CNY",
                "600000",
                1,
                100,
            )
            .unwrap(),
            clearing: crate::spot::SpotClearingConfig::default(),
            risk: crate::risk::SpotRiskConfig::default(),
        });
        let preset = VenuePreset::SseLike {
            reference_price_ticks: BTreeMap::from([("sse:600000:cny".to_string(), 100)]),
        };

        let rules = preset.rules_for_markets(&[market]).unwrap();

        assert_eq!(rules.trading_session.sessions.len(), 2);
        assert_eq!(rules.price_limits[0].limit_up_ppm, 100_000);
        assert_eq!(rules.settlement.spot_sell_delay_steps, 1);
    }

    #[test]
    fn venue_presets_build_asset_policies_for_transfer_rails() {
        let binance = VenuePreset::BinanceLike.asset_policy_for_markets(&[]);
        let usdt = crate::AssetConfig {
            asset_id: "USDT".to_string(),
            kind: AssetKind::Stablecoin,
            tags: BTreeSet::new(),
            issuer: None,
            native_venue: None,
            listed_venues: BTreeSet::new(),
        };
        assert!(binance.allows_deposit("USDT", Some(&usdt), std::iter::empty()));
        let btc = crate::AssetConfig {
            asset_id: "BTC".to_string(),
            kind: AssetKind::Crypto,
            tags: BTreeSet::new(),
            issuer: None,
            native_venue: None,
            listed_venues: BTreeSet::new(),
        };
        assert!(binance.allows_withdrawal("BTC", Some(&btc), std::iter::empty()));

        let nasdaq = VenuePreset::NasdaqLike.asset_policy_for_markets(&[]);
        assert!(nasdaq.allows_deposit("USD", None, std::iter::empty()));
        assert!(!nasdaq.allows_deposit("USDT", None, std::iter::empty()));

        let sse = (VenuePreset::SseLike {
            reference_price_ticks: BTreeMap::new(),
        })
        .asset_policy_for_markets(&[]);
        assert!(sse.allows_deposit("CNY", None, std::iter::empty()));
        assert!(!sse.allows_deposit("USD", None, std::iter::empty()));
    }
}
