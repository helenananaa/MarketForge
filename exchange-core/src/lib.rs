pub mod account;
pub mod actor;
pub mod agents;
pub mod bot_risk;
pub mod bots;
pub mod candles;
pub mod clock;
pub mod engine;
pub mod funding;
#[cfg(test)]
mod funding_tests;
pub mod gateway;
#[cfg(test)]
mod hedge_tests;
mod history;
pub mod jsonl;
pub mod log;
pub mod market;
#[cfg(test)]
mod market_behavior_integration_tests;
pub mod market_bots;
pub mod market_events;
#[cfg(test)]
mod market_microstructure_integration_tests;
pub mod model;
pub mod observation;
pub mod participant;
pub mod performance;
pub mod perp;
pub mod population;
pub mod portfolio;
pub mod price_link;
#[cfg(test)]
mod price_link_tests;
pub mod replay;
pub mod risk;
pub mod room;
pub mod scenario;
pub mod scheduler;
mod shared_map;
pub mod simulation;
pub mod spot;
pub mod trading;
pub mod training;
pub mod training_scenarios;
pub mod transfer;
pub mod venue_rules;

pub use account::{
    ClearingError, FeeRatePpm, Money, PositionQty, VenueAccountError, VenueAccountSnapshot,
    VenueAccountStore, VenueAssetBalance, VenueBalanceSnapshot,
};
pub use actor::{
    AccountSnapshot, AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason,
    ActorSeq, CommandOrigin, ExchangeActor, MarketActor, MarketExecution, MarketStatus, RoomId,
};
pub use agents::{
    AGENT_CONFIG_VERSION, AGENT_STATE_VERSION, AgentParticipantStep, AgentRuntime, AgentStep,
    AgentTemplate, CancelAtStepConfig, CancelAtStepTrader, ContinuousMarketMaker,
    ContinuousMmConfig, DcaTrader, DcaTraderConfig, GridTrader, GridTraderConfig, NoiseTrader,
    NoiseTraderConfig, PersistedAgentKindState,
};
pub use candles::{
    CANDLE_SCHEMA_VERSION, Candle, CandleError, Ticker, TimedTrade, aggregate_candles,
};
pub use clock::{ClockError, MAX_CLOCK_ADVANCE_STEPS, SimulationClock};
pub use engine::OrderBook;
pub use funding::{FundingConfig, FundingSettlement, FundingSnapshot, FundingStatus};
pub use gateway::{
    GatewayError, GatewayExecution, GatewayRequest, MarketView, OrderAction, OrderGateway,
    ParticipantId, TradingApi,
};
pub use jsonl::{
    read_command_log_jsonl, read_event_log_jsonl, write_command_log_jsonl, write_event_log_jsonl,
};
pub use log::{CommandRecord, EventLog, EventRecord, LogSeq, RecordedExecution};
pub use market::{
    AssetConfig, AssetId, AssetKind, AssetSelector, ExchangeConfig, InstrumentConfig, InstrumentId,
    MarketConfig, MarketConfigError, MarketEngine, MarketKind, PerpMarketConfig, SpotMarketConfig,
    VenueAssetPolicyConfig, VenueAssetPolicyConfigError, VenueId,
};
pub use model::{
    BookLevel, BookSnapshot, CancelOrder, Command, Event, NewOrder, Order, OrderId, OrderKind,
    PositionSide, PriceTick, Qty, RejectReason, SetMarkPrice, Side, Trade,
};
pub use observation::{
    BotMarketData, EXTERNAL_ACTIONS_PER_STEP, MAX_PUBLIC_TRADES_IN_OBSERVATION,
    PARTICIPANT_OBSERVATION_VERSION, ParticipantObservation, STRATEGY_PROTOCOL_VERSION,
};
pub use participant::{Participant, ParticipantConfig, ParticipantKind, run_participant_once};
pub use perp::{
    HedgePositions, PerpAccount, PerpAccountSnapshot, PerpAccountStore, PerpClearingConfig,
    PerpClearingEvent, PerpMarginStatus, PerpPositionLeg, PositionMode,
};
pub use portfolio::{
    PortfolioAccountSnapshot, PortfolioAssetBalance, PortfolioBalanceSnapshot, PortfolioError,
    PortfolioStore,
};
pub use price_link::{IndexPriceSource, PerpPriceLinkConfig, PerpPriceSnapshot, PriceLinkStatus};
pub use replay::{LoggedOrderBook, ReplayEngine, ReplayReport};
pub use risk::{PerpRiskConfig, PerpRiskEngine, RiskContext, SpotRiskConfig, SpotRiskEngine};
pub use room::{PendingRoomLiquidation, RoomBootstrap, RoomManager, RoomManagerError};
pub use scenario::{
    ScenarioAccount, ScenarioAllocation, ScenarioBootstrap, ScenarioConfig, ScenarioError,
    ScenarioPortfolio, ScenarioSeedOrder, ScenarioVenueAllocation,
};
pub use scheduler::{
    AgentContinuity, CrashPoint, DEFAULT_CATCH_UP_LIMIT, PersistedAgent, SCHEDULER_STATE_VERSION,
    SchedulerError, SchedulerMode, SchedulerPhase, SchedulerState, SchedulerStepOutcome,
    run_scheduler_step, run_scheduler_step_with_policy, run_scheduler_step_with_registry,
};
pub use simulation::{
    AccountNetWorthAssetSnapshot, AccountNetWorthSnapshot, AssetLedgerEntry, AssetLedgerKind,
    PendingVenueTransfer, RoomNetWorthSnapshot, SimulationBootstrap, SimulationRoom,
    SimulationRoomError, UserId, VenueAccountVenueSnapshot, VenueToVenueTransfer,
};
pub use spot::{
    SpotAccount, SpotAccountSnapshot, SpotAccountStore, SpotClearingConfig, SpotClearingEvent,
};
pub use trading::{
    PendingLiquidation, PerpTradingEngine, PerpTradingExecution, SpotTradingEngine,
    SpotTradingExecution,
};
pub use training::{
    LOW_SLIPPAGE_BUY_TASK_VERSION, SCORING_RULE_VERSION, TRAINING_SPEC_VERSION, TrainingError,
    TrainingFill, TrainingRun, TrainingScore, TrainingSpec, TrainingStatus, score_run,
    training_report_json, training_report_markdown,
};
pub use training_scenarios::{
    BASIC_EXECUTION_ID, INVENTORY_STRESS_ID, LIQUIDITY_WITHDRAWAL_ID, SCENARIO_SPEC_VERSION,
    TrainingScenario, basic_execution, child_seed, inventory_stress, liquidity_withdrawal,
};
pub use transfer::{
    TransferId, VenueTransfer, VenueTransferKind, VenueTransferRejectReason, VenueTransferStatus,
    VenueTransferStore,
};
pub use venue_rules::{
    CircuitBreakerRuleConfig, PendingSpotBuy, PriceLimitRuleConfig, SettlementRuleConfig,
    TradingSessionRuleConfig, TradingSessionWindow, TransferPolicyConfig, VenuePreset,
    VenueRuleConfig, VenueRuleConfigError, VenueRuleEngine, VenueRuleRejectReason,
};

pub use bots::{
    BOT_CONFIG_VERSION, BOT_PROTOCOL_VERSION, BotConfig, BotDecisionRequest, BotDecisionResponse,
    BotDescriptor, BotError, BotExecutionPolicy, BotFactory, BotParameter, BotRegistry,
    MAX_BOT_ACTIONS, ParameterType, ScheduledBot,
};

pub mod conditional_orders;
pub mod position_protection;
pub use position_protection::{PositionProtection, PositionProtectionSpec, ProtectionTrigger};
