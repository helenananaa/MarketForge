use crate::{
    engine::OrderBook,
    log::{CommandRecord, EventLog, EventRecord, RecordedExecution},
    model::{BookSnapshot, Command},
};

#[derive(Debug, Default)]
pub struct LoggedOrderBook {
    book: OrderBook,
    log: EventLog,
}

impl LoggedOrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, command: Command) -> RecordedExecution {
        let events = self.book.apply(command.clone());
        self.log.record(command, events)
    }

    pub fn snapshot(&self) -> BookSnapshot {
        self.book.snapshot()
    }

    pub fn command_log(&self) -> &[CommandRecord] {
        self.log.commands()
    }

    pub fn event_log(&self) -> &[EventRecord] {
        self.log.events()
    }

    pub fn into_replay_input(self) -> Vec<CommandRecord> {
        let (commands, _) = self.log.into_parts();
        commands
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayReport {
    pub commands: Vec<CommandRecord>,
    pub events: Vec<EventRecord>,
    pub final_snapshot: BookSnapshot,
}

pub struct ReplayEngine;

impl ReplayEngine {
    pub fn replay(command_log: &[CommandRecord]) -> ReplayReport {
        let mut book = OrderBook::new();
        let mut log = EventLog::new();

        for command in command_log {
            let events = book.apply(command.command.clone());
            log.record_with_command(command.clone(), events);
        }

        let final_snapshot = book.snapshot();
        let (commands, events) = log.into_parts();

        ReplayReport {
            commands,
            events,
            final_snapshot,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BookLevel, CancelOrder, Event, NewOrder, OrderKind, Side, Trade};

    fn limit(order_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
        })
    }

    fn market(order_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Market,
            qty,
        })
    }

    #[test]
    fn logged_order_book_records_command_and_event_sequences() {
        let mut book = LoggedOrderBook::new();

        let first = book.apply(limit(1, Side::Buy, 100, 10));
        let second = book.apply(limit(2, Side::Sell, 99, 4));

        assert_eq!(first.command.seq, 0);
        assert_eq!(second.command.seq, 1);
        assert_eq!(book.command_log().len(), 2);

        let event_log = book.event_log();
        assert_eq!(event_log[0].seq, 0);
        assert_eq!(event_log[0].command_seq, 0);
        assert_eq!(event_log[1].seq, 1);
        assert_eq!(event_log[1].command_seq, 0);
        assert!(event_log.iter().any(
            |record| record.command_seq == 1 && matches!(record.event, Event::TradePrinted(_))
        ));
    }

    #[test]
    fn replay_from_command_log_matches_original_events_and_snapshot() {
        let mut original = LoggedOrderBook::new();

        original.apply(limit(1, Side::Buy, 100, 10));
        original.apply(limit(2, Side::Buy, 99, 5));
        original.apply(limit(3, Side::Sell, 100, 4));
        original.apply(Command::CancelOrder(CancelOrder { order_id: 2 }));
        original.apply(limit(4, Side::Sell, 101, 7));
        original.apply(market(5, Side::Buy, 10));

        let command_log = original.command_log().to_vec();
        let expected_events = original.event_log().to_vec();
        let expected_snapshot = original.snapshot();

        let replay = ReplayEngine::replay(&command_log);

        assert_eq!(replay.commands, command_log);
        assert_eq!(replay.events, expected_events);
        assert_eq!(replay.final_snapshot, expected_snapshot);
        assert_eq!(
            replay.final_snapshot.bids,
            vec![BookLevel {
                price_tick: 100,
                qty: 6,
            }]
        );
        assert!(replay.final_snapshot.asks.is_empty());
    }

    #[test]
    fn replay_preserves_trade_ids_and_order_flow_events() {
        let mut original = LoggedOrderBook::new();

        original.apply(limit(1, Side::Sell, 100, 3));
        original.apply(limit(2, Side::Sell, 101, 4));
        original.apply(market(3, Side::Buy, 10));

        let replay = ReplayEngine::replay(original.command_log());
        let trades = replay
            .events
            .iter()
            .filter_map(|record| match &record.event {
                Event::TradePrinted(trade) => Some(trade.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            trades,
            vec![
                Trade {
                    trade_id: 0,
                    maker_order_id: 1,
                    taker_order_id: 3,
                    price_tick: 100,
                    qty: 3,
                    taker_side: Side::Buy,
                },
                Trade {
                    trade_id: 1,
                    maker_order_id: 2,
                    taker_order_id: 3,
                    price_tick: 101,
                    qty: 4,
                    taker_side: Side::Buy,
                },
            ]
        );
    }
}
