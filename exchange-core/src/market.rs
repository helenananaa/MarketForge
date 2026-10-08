use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    account::ClearingError,
    model::{PriceTick, Qty},
    perp::PerpClearingConfig,
    risk::{PerpRiskConfig, SpotRiskConfig},
    spot::SpotClearingConfig,
    trading::{PerpTradingEngine, SpotTradingEngine},
    venue_rules::{VenueRuleConfig, VenueRuleConfigError},
};

pub type AssetId = String;
pub type VenueId = String;
pub type InstrumentId = String;

pub const DEFAULT_VENUE_ID: &str = "default-venue";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MarketKind {
    Spot,
    Perp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssetConfig {
    pub asset_id: AssetId,
    #[serde(default)]
    pub kind: AssetKind,
    #[serde(default)]
    pub tags: BTreeSet<String>,
    pub issuer: Option<String>,
    pub native_venue: Option<VenueId>,
    #[serde(default)]
    pub listed_venues: BTreeSet<VenueId>,
}

impl AssetConfig {
    pub fn new(asset_id: impl Into<AssetId>) -> Result<Self, MarketConfigError> {
        let config = Self {
            asset_id: asset_id.into(),
            kind: AssetKind::default(),
            tags: BTreeSet::new(),
            issuer: None,
            native_venue: None,
            listed_venues: BTreeSet::new(),
        };
        if config.asset_id.trim().is_empty() {
            return Err(MarketConfigError::EmptyAssetId);
        }
        Ok(config)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum AssetKind {
    Fiat,
    Crypto,
    Stablecoin,
    Equity,
    Commodity,
    Derivative,
    #[default]
    Custom,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InstrumentConfig {
    pub instrument_id: InstrumentId,
    pub venue_id: VenueId,
    pub symbol: String,
    pub base_asset: AssetId,
    pub quote_asset: AssetId,
    pub tick_size: PriceTick,
    pub lot_size: Qty,
}

impl<'de> Deserialize<'de> for InstrumentConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct InstrumentConfigWire {
            #[serde(default)]
            instrument_id: Option<InstrumentId>,
            #[serde(default)]
            venue_id: Option<VenueId>,
            symbol: String,
            #[serde(default)]
            base_asset: Option<AssetId>,
            #[serde(default)]
            quote_asset: Option<AssetId>,
            tick_size: PriceTick,
            lot_size: Qty,
        }

        let wire = InstrumentConfigWire::deserialize(deserializer)?;
        let symbol = wire.symbol;
        Ok(Self {
            instrument_id: wire.instrument_id.unwrap_or_else(|| symbol.clone()),
            venue_id: wire
                .venue_id
                .unwrap_or_else(|| DEFAULT_VENUE_ID.to_string()),
            base_asset: wire
                .base_asset
                .unwrap_or_else(|| inferred_base_asset(&symbol)),
            quote_asset: wire
                .quote_asset
                .unwrap_or_else(|| inferred_quote_asset(&symbol)),
            symbol,
            tick_size: wire.tick_size,
            lot_size: wire.lot_size,
        })
    }
}

impl InstrumentConfig {
    pub fn new(
        symbol: impl Into<String>,
        tick_size: PriceTick,
        lot_size: Qty,
    ) -> Result<Self, MarketConfigError> {
        let symbol = symbol.into();
        Self::new_for_venue(
            DEFAULT_VENUE_ID,
            symbol.clone(),
            inferred_base_asset(&symbol),
            inferred_quote_asset(&symbol),
            symbol,
            tick_size,
            lot_size,
        )
    }

    pub fn new_for_venue(
        venue_id: impl Into<VenueId>,
        instrument_id: impl Into<InstrumentId>,
        base_asset: impl Into<AssetId>,
        quote_asset: impl Into<AssetId>,
        symbol: impl Into<String>,
        tick_size: PriceTick,
        lot_size: Qty,
    ) -> Result<Self, MarketConfigError> {
        let config = Self {
            instrument_id: instrument_id.into(),
            venue_id: venue_id.into(),
            symbol: symbol.into(),
            base_asset: base_asset.into(),
            quote_asset: quote_asset.into(),
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
        if self.instrument_id.trim().is_empty() {
            return Err(MarketConfigError::EmptyInstrumentId);
        }
        if self.venue_id.trim().is_empty() {
            return Err(MarketConfigError::EmptyVenueId);
        }
        if self.base_asset.trim().is_empty() || self.quote_asset.trim().is_empty() {
            return Err(MarketConfigError::EmptyAssetId);
        }
        if self.base_asset == self.quote_asset {
            return Err(MarketConfigError::DuplicateAssetId);
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
        self.instrument.validate()?;
        if self.clearing.maker_fee_ppm > 1_000_000 || self.clearing.taker_fee_ppm > 1_000_000 {
            return Err(MarketConfigError::Clearing(ClearingError::InvalidFeeRate));
        }
        Ok(())
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_link: Option<crate::price_link::PerpPriceLinkConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding: Option<crate::FundingConfig>,
}

impl PerpMarketConfig {
    pub fn validate(&self) -> Result<(), MarketConfigError> {
        self.instrument.validate()?;
        if let Some(link) = &self.price_link
            && (link.spot_instrument_id.trim().is_empty() || link.max_age_ms == 0)
        {
            return Err(MarketConfigError::InvalidPriceLink);
        }
        if let Some(funding) = &self.funding
            && (self.price_link.is_none() || !funding.is_valid())
        {
            return Err(MarketConfigError::InvalidFundingConfig);
        }
        if self.initial_mark_price_tick <= 0 {
            return Err(MarketConfigError::InvalidInitialMarkPrice);
        }
        if self.clearing.leverage == 0 {
            return Err(MarketConfigError::InvalidLeverage);
        }
        if self.clearing.maker_fee_ppm > 1_000_000
            || self.clearing.taker_fee_ppm > 1_000_000
            || self.clearing.liquidation_fee_ppm > 1_000_000
        {
            return Err(MarketConfigError::Clearing(ClearingError::InvalidFeeRate));
        }
        if self.clearing.maintenance_margin_ppm > 1_000_000 {
            return Err(MarketConfigError::Clearing(
                ClearingError::InvalidMarginRate,
            ));
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

    pub fn instrument_id(&self) -> &str {
        &self.instrument().instrument_id
    }

    pub fn venue_id(&self) -> &str {
        &self.instrument().venue_id
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExchangeConfig {
    pub venue_id: VenueId,
    #[serde(default)]
    pub venue_rules: VenueRuleConfig,
    #[serde(default)]
    pub asset_policy: VenueAssetPolicyConfig,
    pub assets: Vec<AssetConfig>,
    pub markets: Vec<MarketConfig>,
}

impl ExchangeConfig {
    pub fn new(
        venue_id: impl Into<VenueId>,
        markets: Vec<MarketConfig>,
    ) -> Result<Self, MarketConfigError> {
        let venue_id = venue_id.into();
        let assets = assets_from_markets(&markets);
        let config = Self {
            venue_id,
            venue_rules: VenueRuleConfig::default(),
            asset_policy: VenueAssetPolicyConfig::default(),
            assets,
            markets,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn new_single(market: MarketConfig) -> Result<Self, MarketConfigError> {
        Self::new(market.venue_id().to_string(), vec![market])
    }

    pub fn validate(&self) -> Result<(), MarketConfigError> {
        if self.venue_id.trim().is_empty() {
            return Err(MarketConfigError::EmptyVenueId);
        }
        if self.markets.is_empty() {
            return Err(MarketConfigError::EmptyExchangeMarkets);
        }
        self.venue_rules
            .validate()
            .map_err(MarketConfigError::VenueRule)?;
        self.asset_policy
            .validate()
            .map_err(MarketConfigError::AssetPolicy)?;

        let mut asset_ids = BTreeSet::new();
        for asset in &self.assets {
            asset.validate()?;
            if !asset_ids.insert(asset.asset_id.clone()) {
                return Err(MarketConfigError::DuplicateAssetId);
            }
        }

        let mut instrument_ids = BTreeSet::new();
        for market in &self.markets {
            market.validate()?;
            if market.venue_id() != self.venue_id {
                return Err(MarketConfigError::VenueMismatch);
            }
            if !instrument_ids.insert(market.instrument_id().to_string()) {
                return Err(MarketConfigError::DuplicateInstrumentId);
            }

            let instrument = market.instrument();
            if !asset_ids.contains(&instrument.base_asset)
                || !asset_ids.contains(&instrument.quote_asset)
            {
                return Err(MarketConfigError::UnknownAssetId);
            }
            if let MarketConfig::Perp(perp) = market
                && let Some(link) = &perp.price_link
            {
                let source = self
                    .markets
                    .iter()
                    .find(|candidate| candidate.instrument_id() == link.spot_instrument_id);
                let Some(MarketConfig::Spot(spot)) = source else {
                    return Err(MarketConfigError::InvalidPriceLink);
                };
                if spot.instrument.base_asset != instrument.base_asset
                    || spot.instrument.quote_asset != instrument.quote_asset
                {
                    return Err(MarketConfigError::InvalidPriceLink);
                }
            }
        }

        Ok(())
    }

    pub fn primary_instrument_id(&self) -> &str {
        self.markets[0].instrument_id()
    }

    pub fn accepts_deposit_asset(&self, asset_id: &str) -> bool {
        self.asset_policy.allows_deposit(
            asset_id,
            self.asset_config(asset_id),
            self.assets.iter().map(|asset| asset.asset_id.as_str()),
        )
    }

    pub fn accepts_withdrawal_asset(&self, asset_id: &str) -> bool {
        self.asset_policy.allows_withdrawal(
            asset_id,
            self.asset_config(asset_id),
            self.assets.iter().map(|asset| asset.asset_id.as_str()),
        )
    }

    pub fn merge_asset_metadata(&mut self, assets: &[AssetConfig]) {
        for asset in assets {
            match self
                .assets
                .iter_mut()
                .find(|existing| existing.asset_id == asset.asset_id)
            {
                Some(existing) => *existing = asset.clone(),
                None => self.assets.push(asset.clone()),
            }
        }
    }

    fn asset_config(&self, asset_id: &str) -> Option<&AssetConfig> {
        self.assets.iter().find(|asset| asset.asset_id == asset_id)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueAssetPolicyConfig {
    #[serde(default)]
    pub deposit_assets: BTreeSet<AssetId>,
    #[serde(default)]
    pub withdrawal_assets: BTreeSet<AssetId>,
    #[serde(default)]
    pub settlement_assets: BTreeSet<AssetId>,
    #[serde(default)]
    pub margin_assets: BTreeSet<AssetId>,
    #[serde(default)]
    pub deposit_rules: Vec<AssetSelector>,
    #[serde(default)]
    pub withdrawal_rules: Vec<AssetSelector>,
    #[serde(default)]
    pub settlement_rules: Vec<AssetSelector>,
    #[serde(default)]
    pub margin_rules: Vec<AssetSelector>,
}

impl VenueAssetPolicyConfig {
    pub fn validate(&self) -> Result<(), VenueAssetPolicyConfigError> {
        for asset_id in self
            .deposit_assets
            .iter()
            .chain(self.withdrawal_assets.iter())
            .chain(self.settlement_assets.iter())
            .chain(self.margin_assets.iter())
        {
            if asset_id.trim().is_empty() {
                return Err(VenueAssetPolicyConfigError::EmptyAssetId);
            }
        }
        for selector in self
            .deposit_rules
            .iter()
            .chain(self.withdrawal_rules.iter())
            .chain(self.settlement_rules.iter())
            .chain(self.margin_rules.iter())
        {
            selector.validate()?;
        }
        Ok(())
    }

    pub fn merge_overrides(mut self, overrides: Self) -> Self {
        if !overrides.deposit_assets.is_empty() {
            self.deposit_assets = overrides.deposit_assets;
        }
        if !overrides.withdrawal_assets.is_empty() {
            self.withdrawal_assets = overrides.withdrawal_assets;
        }
        if !overrides.settlement_assets.is_empty() {
            self.settlement_assets = overrides.settlement_assets;
        }
        if !overrides.margin_assets.is_empty() {
            self.margin_assets = overrides.margin_assets;
        }
        if !overrides.deposit_rules.is_empty() {
            self.deposit_rules = overrides.deposit_rules;
        }
        if !overrides.withdrawal_rules.is_empty() {
            self.withdrawal_rules = overrides.withdrawal_rules;
        }
        if !overrides.settlement_rules.is_empty() {
            self.settlement_rules = overrides.settlement_rules;
        }
        if !overrides.margin_rules.is_empty() {
            self.margin_rules = overrides.margin_rules;
        }
        self
    }

    pub fn allows_deposit<'a>(
        &self,
        asset_id: &str,
        asset: Option<&AssetConfig>,
        fallback_assets: impl Iterator<Item = &'a str>,
    ) -> bool {
        asset_allowed_or_fallback(
            &self.deposit_assets,
            &self.deposit_rules,
            asset_id,
            asset,
            fallback_assets,
        )
    }

    pub fn allows_withdrawal<'a>(
        &self,
        asset_id: &str,
        asset: Option<&AssetConfig>,
        fallback_assets: impl Iterator<Item = &'a str>,
    ) -> bool {
        asset_allowed_or_fallback(
            &self.withdrawal_assets,
            &self.withdrawal_rules,
            asset_id,
            asset,
            fallback_assets,
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum AssetSelector {
    Any,
    AssetIds { asset_ids: BTreeSet<AssetId> },
    Kinds { kinds: BTreeSet<AssetKind> },
    TagsAny { tags: BTreeSet<String> },
    TagsAll { tags: BTreeSet<String> },
    Issuer { issuer: String },
    NativeVenue { venue_id: VenueId },
    ListedOnVenue { venue_id: VenueId },
}

impl AssetSelector {
    fn validate(&self) -> Result<(), VenueAssetPolicyConfigError> {
        match self {
            Self::Any => Ok(()),
            Self::AssetIds { asset_ids } => validate_non_empty_set(asset_ids),
            Self::Kinds { kinds } => {
                if kinds.is_empty() {
                    Err(VenueAssetPolicyConfigError::EmptySelector)
                } else {
                    Ok(())
                }
            }
            Self::TagsAny { tags } | Self::TagsAll { tags } => validate_non_empty_set(tags),
            Self::Issuer { issuer } => validate_non_empty_value(issuer),
            Self::NativeVenue { venue_id } | Self::ListedOnVenue { venue_id } => {
                validate_non_empty_value(venue_id)
            }
        }
    }

    fn matches(&self, asset_id: &str, asset: Option<&AssetConfig>) -> bool {
        match self {
            Self::Any => true,
            Self::AssetIds { asset_ids } => asset_ids.contains(asset_id),
            Self::Kinds { kinds } => asset.is_some_and(|asset| kinds.contains(&asset.kind)),
            Self::TagsAny { tags } => {
                asset.is_some_and(|asset| tags.iter().any(|tag| asset.tags.contains(tag)))
            }
            Self::TagsAll { tags } => {
                asset.is_some_and(|asset| tags.iter().all(|tag| asset.tags.contains(tag)))
            }
            Self::Issuer { issuer } => asset
                .and_then(|asset| asset.issuer.as_deref())
                .is_some_and(|asset_issuer| asset_issuer == issuer),
            Self::NativeVenue { venue_id } => asset
                .and_then(|asset| asset.native_venue.as_deref())
                .is_some_and(|asset_venue| asset_venue == venue_id),
            Self::ListedOnVenue { venue_id } => {
                asset.is_some_and(|asset| asset.listed_venues.contains(venue_id))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VenueAssetPolicyConfigError {
    EmptyAssetId,
    EmptySelector,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketConfigError {
    EmptyAssetId,
    EmptyVenueId,
    EmptyInstrumentId,
    EmptySymbol,
    DuplicateAssetId,
    DuplicateInstrumentId,
    EmptyExchangeMarkets,
    UnknownAssetId,
    VenueMismatch,
    InvalidTickSize,
    InvalidLotSize,
    InvalidInitialMarkPrice,
    InvalidPriceLink,
    InvalidFundingConfig,
    InvalidLeverage,
    VenueRule(VenueRuleConfigError),
    AssetPolicy(VenueAssetPolicyConfigError),
    Clearing(ClearingError),
}

impl AssetConfig {
    fn validate(&self) -> Result<(), MarketConfigError> {
        if self.asset_id.trim().is_empty() {
            return Err(MarketConfigError::EmptyAssetId);
        }
        if self.tags.iter().any(|tag| tag.trim().is_empty())
            || self
                .issuer
                .as_deref()
                .is_some_and(|issuer| issuer.trim().is_empty())
            || self
                .native_venue
                .as_deref()
                .is_some_and(|venue_id| venue_id.trim().is_empty())
            || self
                .listed_venues
                .iter()
                .any(|venue_id| venue_id.trim().is_empty())
        {
            return Err(MarketConfigError::EmptyAssetId);
        }
        Ok(())
    }
}

fn assets_from_markets(markets: &[MarketConfig]) -> Vec<AssetConfig> {
    let mut ids = BTreeSet::new();
    for market in markets {
        let instrument = market.instrument();
        ids.insert(instrument.base_asset.clone());
        ids.insert(instrument.quote_asset.clone());
    }

    ids.into_iter()
        .map(|asset_id| AssetConfig {
            asset_id,
            kind: AssetKind::default(),
            tags: BTreeSet::new(),
            issuer: None,
            native_venue: None,
            listed_venues: BTreeSet::new(),
        })
        .collect()
}

fn asset_allowed_or_fallback<'a>(
    explicit_assets: &BTreeSet<AssetId>,
    selectors: &[AssetSelector],
    asset_id: &str,
    asset: Option<&AssetConfig>,
    fallback_assets: impl Iterator<Item = &'a str>,
) -> bool {
    if explicit_assets.is_empty() && selectors.is_empty() {
        fallback_assets
            .into_iter()
            .any(|allowed| allowed == asset_id)
    } else {
        explicit_assets.contains(asset_id)
            || selectors
                .iter()
                .any(|selector| selector.matches(asset_id, asset))
    }
}

fn validate_non_empty_set<T: AsRef<str>>(
    values: &BTreeSet<T>,
) -> Result<(), VenueAssetPolicyConfigError> {
    if values.is_empty() || values.iter().any(|value| value.as_ref().trim().is_empty()) {
        Err(VenueAssetPolicyConfigError::EmptySelector)
    } else {
        Ok(())
    }
}

fn validate_non_empty_value(value: &str) -> Result<(), VenueAssetPolicyConfigError> {
    if value.trim().is_empty() {
        Err(VenueAssetPolicyConfigError::EmptySelector)
    } else {
        Ok(())
    }
}

fn inferred_base_asset(symbol: &str) -> AssetId {
    symbol
        .split(['-', '/', '_'])
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(symbol)
        .to_string()
}

fn inferred_quote_asset(symbol: &str) -> AssetId {
    symbol
        .split(['-', '/', '_'])
        .nth(1)
        .filter(|part| !part.is_empty())
        .unwrap_or("USD")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Command, Event, NewOrder, OrderKind, RiskRejectReason, Side},
        risk::SpotRiskConfig,
    };

    #[test]
    fn legacy_instrument_json_infers_new_identity_and_asset_fields() {
        let instrument: InstrumentConfig = serde_json::from_value(serde_json::json!({
            "symbol": "V-BTC-SPOT",
            "tick_size": 1,
            "lot_size": 1
        }))
        .unwrap();

        assert_eq!(
            instrument,
            InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap()
        );
    }

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
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig {
                max_order_qty: Some(100),
                max_order_notional: Some(1_000_000),
                max_abs_position_qty: Some(200),
                ..PerpRiskConfig::default()
            },
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
        });

        assert_eq!(config.kind(), MarketKind::Perp);
        assert_eq!(config.symbol(), "V-BTC-PERP");
        let engine = config.build_engine().expect("perp engine should build");
        assert_eq!(engine.kind(), MarketKind::Perp);
    }

    #[test]
    fn rejects_fee_rates_above_one_hundred_percent() {
        let spot = SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
            clearing: SpotClearingConfig {
                maker_fee_ppm: 1_000_001,
                taker_fee_ppm: 0,
            },
            risk: SpotRiskConfig::default(),
        };
        assert_eq!(
            spot.validate(),
            Err(MarketConfigError::Clearing(ClearingError::InvalidFeeRate))
        );

        let perp = PerpMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
            clearing: PerpClearingConfig {
                taker_fee_ppm: 1_000_001,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
        };
        assert_eq!(
            perp.validate(),
            Err(MarketConfigError::Clearing(ClearingError::InvalidFeeRate))
        );
    }

    #[test]
    fn exchange_config_groups_markets_by_venue_and_assets() {
        let spot = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:spot",
                "BTC",
                "USDT",
                "BTC-USDT Spot",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        });
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:perp",
                "BTC",
                "USDT",
                "BTC-USDT Perp",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
        });

        let exchange = ExchangeConfig::new("binance", vec![spot.clone(), perp]).unwrap();

        assert_eq!(exchange.venue_id, "binance");
        assert_eq!(exchange.primary_instrument_id(), "binance:btc-usdt:spot");
        assert_eq!(
            exchange
                .assets
                .iter()
                .map(|asset| asset.asset_id.as_str())
                .collect::<Vec<_>>(),
            vec!["BTC", "USDT"]
        );

        assert_eq!(
            ExchangeConfig::new("okx", vec![spot]).map(|_| ()),
            Err(MarketConfigError::VenueMismatch)
        );
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
            price_link: None,
            funding: None,
        };
        assert_eq!(
            invalid_mark.validate(),
            Err(MarketConfigError::InvalidInitialMarkPrice)
        );

        let invalid_leverage = PerpMarketConfig {
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
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
                reduce_only: false,
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
                reduce_only: false,
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
            price_link: None,
            funding: None,
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
                reduce_only: false,
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
