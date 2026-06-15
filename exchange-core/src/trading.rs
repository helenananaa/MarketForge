use crate::{
    OrderBook,
    account::{ClearingError, Money},
    log::{CommandRecord, EventLog, EventRecord},
    model::{AccountId, BookSnapshot, Command, Event},
    perp::{PerpAccountSnapshot, PerpAccountStore, PerpClearingConfig, PerpClearingEvent},
    spot::{SpotAccountSnapshot, SpotAccountStore, SpotClearingConfig, SpotClearingEvent},
};

#[derive(Debug, Default)]
pub struct SpotTradingEngine {
    book: OrderBook,
    accounts: SpotAccountStore,
    log: EventLog,
    clearing_events: Vec<SpotClearingEvent>,
}

impl SpotTradingEngine {
    pub fn new(config: SpotClearingConfig) -> Self {
        Self {
            book: OrderBook::new(),
            accounts: SpotAccountStore::new(config),
            log: EventLog::new(),
            clearing_events: Vec::new(),
        }
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> SpotAccountSnapshot {
        self.accounts.create_account(account_id, cash_balance)
    }

    pub fn apply(&mut self, command: Command) -> Result<SpotTradingExecution, ClearingError> {
        let events = self.book.apply(command.clone());
        let recorded = self.log.record(command, events);
        let clearing_events = self.settle_recorded_events(&recorded.events)?;

        Ok(SpotTradingExecution {
            command: recorded.command,
            events: recorded.events,
            clearing_events,
        })
    }

    pub fn snapshot(&self) -> BookSnapshot {
        self.book.snapshot()
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<SpotAccountSnapshot> {
        self.accounts.account_snapshot(account_id)
    }

    pub fn account_snapshots(&self) -> Vec<SpotAccountSnapshot> {
        self.accounts.snapshots()
    }

    pub fn command_log(&self) -> &[CommandRecord] {
        self.log.commands()
    }

    pub fn event_log(&self) -> &[EventRecord] {
        self.log.events()
    }

    pub fn clearing_log(&self) -> &[SpotClearingEvent] {
        &self.clearing_events
    }

    fn settle_recorded_events(
        &mut self,
        events: &[EventRecord],
    ) -> Result<Vec<SpotClearingEvent>, ClearingError> {
        let mut clearing_events = Vec::new();

        for record in events {
            if let Event::TradePrinted(trade) = &record.event {
                let clearing_event = self.accounts.settle_trade(trade)?;
                self.clearing_events.push(clearing_event.clone());
                clearing_events.push(clearing_event);
            }
        }

        Ok(clearing_events)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpotTradingExecution {
    pub command: CommandRecord,
    pub events: Vec<EventRecord>,
    pub clearing_events: Vec<SpotClearingEvent>,
}

#[derive(Debug)]
pub struct PerpTradingEngine {
    book: OrderBook,
    accounts: PerpAccountStore,
    log: EventLog,
    clearing_events: Vec<PerpClearingEvent>,
}

impl PerpTradingEngine {
    pub fn new(
        config: PerpClearingConfig,
        initial_mark_price_tick: crate::model::PriceTick,
    ) -> Result<Self, ClearingError> {
        Ok(Self {
            book: OrderBook::new(),
            accounts: PerpAccountStore::new(config, initial_mark_price_tick)?,
            log: EventLog::new(),
            clearing_events: Vec::new(),
        })
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> PerpAccountSnapshot {
        self.accounts.create_account(account_id, cash_balance)
    }

    pub fn apply(&mut self, command: Command) -> Result<PerpTradingExecution, ClearingError> {
        let events = self.book.apply(command.clone());
        let recorded = self.log.record(command, events);
        let clearing_events = self.settle_recorded_events(&recorded.events)?;

        Ok(PerpTradingExecution {
            command: recorded.command,
            events: recorded.events,
            clearing_events,
        })
    }

    pub fn set_mark_price_tick(
        &mut self,
        mark_price_tick: crate::model::PriceTick,
    ) -> Result<(), ClearingError> {
        self.accounts.set_mark_price_tick(mark_price_tick)
    }

    pub fn snapshot(&self) -> BookSnapshot {
        self.book.snapshot()
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<PerpAccountSnapshot> {
        self.accounts.account_snapshot(account_id)
    }

    pub fn account_snapshots(&self) -> Vec<PerpAccountSnapshot> {
        self.accounts.snapshots()
    }

    pub fn command_log(&self) -> &[CommandRecord] {
        self.log.commands()
    }

    pub fn event_log(&self) -> &[EventRecord] {
        self.log.events()
    }

    pub fn clearing_log(&self) -> &[PerpClearingEvent] {
        &self.clearing_events
    }

    fn settle_recorded_events(
        &mut self,
        events: &[EventRecord],
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        let mut clearing_events = Vec::new();

        for record in events {
            if let Event::TradePrinted(trade) = &record.event {
                let clearing_event = self.accounts.settle_trade(trade)?;
                self.clearing_events.push(clearing_event.clone());
                clearing_events.push(clearing_event);
            }
        }

        Ok(clearing_events)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PerpTradingExecution {
    pub command: CommandRecord,
    pub events: Vec<EventRecord>,
    pub clearing_events: Vec<PerpClearingEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewOrder, OrderKind, Side};

    fn limit(order_id: u64, account_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
        })
    }

    fn market(order_id: u64, account_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id,
            side,
            kind: OrderKind::Market,
            qty,
        })
    }

    #[test]
    fn spot_trading_engine_settles_cash_and_position_after_match() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig {
            maker_fee_ppm: 0,
            taker_fee_ppm: 1_000,
        });
        engine.create_account(10, 10_000);
        engine.create_account(20, 10_000);

        let resting = engine
            .apply(limit(1, 10, Side::Sell, 100, 10))
            .expect("resting order should apply");
        assert!(resting.clearing_events.is_empty());

        let execution = engine
            .apply(limit(2, 20, Side::Buy, 100, 4))
            .expect("crossing order should apply");

        assert_eq!(execution.clearing_events.len(), 1);
        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 9_600,
                position_qty: 4,
                fees_paid: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 10_400,
                position_qty: -4,
                fees_paid: 0,
            })
        );
    }

    #[test]
    fn spot_trading_engine_applies_taker_fee_when_notional_is_large_enough() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig {
            maker_fee_ppm: 500,
            taker_fee_ppm: 1_000,
        });

        engine.apply(limit(1, 10, Side::Sell, 1_000, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();

        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: -10_010,
                position_qty: 10,
                fees_paid: 10,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 9_995,
                position_qty: -10,
                fees_paid: 5,
            })
        );
    }

    #[test]
    fn spot_trading_engine_clears_multiple_trades_from_one_market_order() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());

        engine.apply(limit(1, 10, Side::Sell, 100, 3)).unwrap();
        engine.apply(limit(2, 11, Side::Sell, 101, 4)).unwrap();
        let execution = engine.apply(market(3, 20, Side::Buy, 10)).unwrap();

        assert_eq!(execution.clearing_events.len(), 2);
        assert_eq!(engine.clearing_log().len(), 2);
        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: -704,
                position_qty: 7,
                fees_paid: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 300,
                position_qty: -3,
                fees_paid: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(11),
            Some(SpotAccountSnapshot {
                account_id: 11,
                cash_balance: 404,
                position_qty: -4,
                fees_paid: 0,
            })
        );
    }

    #[test]
    fn perp_trading_engine_uses_same_order_book_but_contract_clearing() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 1_000,
                leverage: 10,
            },
            100,
        )
        .expect("valid perp engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 10_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        let execution = engine.apply(market(2, 20, Side::Buy, 4)).unwrap();

        assert_eq!(execution.clearing_events.len(), 1);
        assert_eq!(
            engine.account_snapshot(20),
            Some(PerpAccountSnapshot {
                account_id: 20,
                cash_balance: 10_000,
                position_qty: 4,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 10_000,
                initial_margin: 40,
                fees_paid: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(PerpAccountSnapshot {
                account_id: 10,
                cash_balance: 10_000,
                position_qty: -4,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 10_000,
                initial_margin: 40,
                fees_paid: 0,
            })
        );
    }

    #[test]
    fn perp_trading_engine_mark_price_updates_unrealized_pnl() {
        let mut engine =
            PerpTradingEngine::new(PerpClearingConfig::default(), 100).expect("valid engine");

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.set_mark_price_tick(110).unwrap();

        assert_eq!(engine.account_snapshot(20).unwrap().unrealized_pnl, 100);
        assert_eq!(engine.account_snapshot(10).unwrap().unrealized_pnl, -100);
    }
}
