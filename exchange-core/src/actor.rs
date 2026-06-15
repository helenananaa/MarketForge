use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, Money, PositionQty},
    market::{MarketConfig, MarketConfigError, MarketEngine, MarketKind},
    model::{AccountId, BookSnapshot, Command},
    perp::PerpAccountSnapshot,
    spot::SpotAccountSnapshot,
    trading::{PerpTradingExecution, SpotTradingExecution},
};

pub type RoomId = String;
pub type ActorSeq = u64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MarketStatus {
    Running,
    Paused,
    Closed,
}

#[derive(Debug)]
pub struct MarketActor {
    room_id: RoomId,
    config: MarketConfig,
    engine: MarketEngine,
    status: MarketStatus,
    next_command_seq: ActorSeq,
}

impl MarketActor {
    pub fn new(
        room_id: impl Into<RoomId>,
        config: MarketConfig,
    ) -> Result<Self, MarketConfigError> {
        let engine = config.build_engine()?;
        Ok(Self {
            room_id: room_id.into(),
            config,
            engine,
            status: MarketStatus::Running,
            next_command_seq: 0,
        })
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    pub fn config(&self) -> &MarketConfig {
        &self.config
    }

    pub fn kind(&self) -> MarketKind {
        self.engine.kind()
    }

    pub fn status(&self) -> MarketStatus {
        self.status
    }

    pub fn pause(&mut self) {
        if self.status == MarketStatus::Running {
            self.status = MarketStatus::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.status == MarketStatus::Paused {
            self.status = MarketStatus::Running;
        }
    }

    pub fn close(&mut self) {
        self.status = MarketStatus::Closed;
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> AccountSnapshot {
        self.engine.create_account(account_id, cash_balance)
    }

    pub fn create_spot_account_with_position(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    ) -> Result<SpotAccountSnapshot, ActorRejectReason> {
        match &mut self.engine {
            MarketEngine::Spot(engine) => {
                Ok(engine.create_account_with_position(account_id, cash_balance, position_qty))
            }
            MarketEngine::Perp(_) => Err(ActorRejectReason::WrongMarketKind),
        }
    }

    pub fn apply(&mut self, command: Command) -> ActorExecution {
        let seq = self.take_command_seq();

        if self.status == MarketStatus::Closed {
            return ActorExecution {
                room_id: self.room_id.clone(),
                command_seq: seq,
                status: self.status,
                result: ActorExecutionResult::Rejected(ActorRejectReason::MarketClosed),
            };
        }

        if self.status == MarketStatus::Paused && matches!(command, Command::NewOrder(_)) {
            return ActorExecution {
                room_id: self.room_id.clone(),
                command_seq: seq,
                status: self.status,
                result: ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused),
            };
        }

        let result = match self.engine.apply(command) {
            Ok(result) => ActorExecutionResult::Accepted(result),
            Err(error) => ActorExecutionResult::Rejected(ActorRejectReason::Clearing(error)),
        };

        ActorExecution {
            room_id: self.room_id.clone(),
            command_seq: seq,
            status: self.status,
            result,
        }
    }

    pub fn book_snapshot(&self) -> BookSnapshot {
        self.engine.book_snapshot()
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<AccountSnapshot> {
        self.engine.account_snapshot(account_id)
    }

    pub fn account_snapshots(&self) -> AccountSnapshots {
        self.engine.account_snapshots()
    }

    fn take_command_seq(&mut self) -> ActorSeq {
        let seq = self.next_command_seq;
        self.next_command_seq += 1;
        seq
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActorExecution {
    pub room_id: RoomId,
    pub command_seq: ActorSeq,
    pub status: MarketStatus,
    pub result: ActorExecutionResult,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorExecutionResult {
    Accepted(MarketExecution),
    Rejected(ActorRejectReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorRejectReason {
    MarketPaused,
    MarketClosed,
    WrongMarketKind,
    Clearing(ClearingError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketExecution {
    Spot(SpotTradingExecution),
    Perp(PerpTradingExecution),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountSnapshot {
    Spot(SpotAccountSnapshot),
    Perp(PerpAccountSnapshot),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountSnapshots {
    Spot(Vec<SpotAccountSnapshot>),
    Perp(Vec<PerpAccountSnapshot>),
}

impl MarketEngine {
    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> AccountSnapshot {
        match self {
            Self::Spot(engine) => {
                AccountSnapshot::Spot(engine.create_account(account_id, cash_balance))
            }
            Self::Perp(engine) => {
                AccountSnapshot::Perp(engine.create_account(account_id, cash_balance))
            }
        }
    }

    pub fn apply(&mut self, command: Command) -> Result<MarketExecution, ClearingError> {
        match self {
            Self::Spot(engine) => engine.apply(command).map(MarketExecution::Spot),
            Self::Perp(engine) => engine.apply(command).map(MarketExecution::Perp),
        }
    }

    pub fn book_snapshot(&self) -> BookSnapshot {
        match self {
            Self::Spot(engine) => engine.snapshot(),
            Self::Perp(engine) => engine.snapshot(),
        }
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<AccountSnapshot> {
        match self {
            Self::Spot(engine) => engine
                .account_snapshot(account_id)
                .map(AccountSnapshot::Spot),
            Self::Perp(engine) => engine
                .account_snapshot(account_id)
                .map(AccountSnapshot::Perp),
        }
    }

    pub fn account_snapshots(&self) -> AccountSnapshots {
        match self {
            Self::Spot(engine) => AccountSnapshots::Spot(engine.account_snapshots()),
            Self::Perp(engine) => AccountSnapshots::Perp(engine.account_snapshots()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpotRiskConfig,
        market::{InstrumentConfig, SpotMarketConfig},
        model::{CancelOrder, Event, NewOrder, OrderKind, Side},
        spot::SpotClearingConfig,
    };

    fn spot_config() -> MarketConfig {
        MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })
    }

    fn limit(order_id: u64, account_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
        })
    }

    #[test]
    fn actor_runs_spot_market_and_returns_room_scoped_execution() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor
            .create_spot_account_with_position(10, 1_000, 5)
            .unwrap();
        actor.create_account(20, 1_000);

        actor.apply(limit(1, 10, Side::Sell, 100, 5));
        let execution = actor.apply(limit(2, 20, Side::Buy, 100, 2));

        assert_eq!(execution.room_id, "room-1");
        assert_eq!(execution.command_seq, 1);
        let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) = execution.result else {
            panic!("expected accepted spot execution");
        };
        assert_eq!(result.clearing_events.len(), 1);
        assert!(
            result
                .events
                .iter()
                .any(|record| matches!(record.event, Event::TradePrinted(_)))
        );
    }

    #[test]
    fn paused_actor_rejects_new_orders_without_changing_book() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.create_account(20, 1_000);
        actor.pause();

        let execution = actor.apply(limit(1, 20, Side::Buy, 100, 1));

        assert_eq!(actor.status(), MarketStatus::Paused);
        assert_eq!(
            execution.result,
            ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused)
        );
        assert!(actor.book_snapshot().bids.is_empty());
    }

    #[test]
    fn paused_actor_allows_cancel_orders() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.create_account(20, 1_000);
        actor.apply(limit(1, 20, Side::Buy, 100, 1));
        actor.pause();

        let execution = actor.apply(Command::CancelOrder(CancelOrder { order_id: 1 }));

        let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) = execution.result else {
            panic!("expected accepted cancel");
        };
        assert!(
            result
                .events
                .iter()
                .any(|record| matches!(record.event, Event::OrderCanceled { order_id: 1, .. }))
        );
        assert!(actor.book_snapshot().bids.is_empty());
    }

    #[test]
    fn closed_actor_rejects_all_commands() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.close();

        let execution = actor.apply(limit(1, 20, Side::Buy, 100, 1));

        assert_eq!(
            execution.result,
            ActorExecutionResult::Rejected(ActorRejectReason::MarketClosed)
        );
    }

    #[test]
    fn actor_exposes_account_snapshots() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.create_account(20, 1_000);

        assert!(matches!(
            actor.account_snapshot(20),
            Some(AccountSnapshot::Spot(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 1_000,
                ..
            }))
        ));
        assert!(matches!(
            actor.account_snapshots(),
            AccountSnapshots::Spot(accounts) if accounts.len() == 1
        ));
    }
}
