use crate::history::History;
use crate::model::{Command, Event};
use serde::{Deserialize, Serialize};

pub type LogSeq = u64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandRecord {
    pub seq: LogSeq,
    pub command: Command,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CancelOrder;

    #[test]
    fn checkpoint_forks_keep_history_and_sequence_isolated() {
        let mut live = EventLog::new();
        live.record(Command::CancelOrder(CancelOrder { order_id: 1 }), vec![]);
        let checkpoint = live.clone();
        let encoded = serde_json::to_value(&checkpoint).unwrap();
        assert!(encoded["commands"].is_array());
        assert!(encoded["events"].is_array());
        let mut recovered: EventLog = serde_json::from_value(encoded.clone()).unwrap();
        live.record(Command::CancelOrder(CancelOrder { order_id: 2 }), vec![]);
        recovered.record(Command::CancelOrder(CancelOrder { order_id: 3 }), vec![]);
        assert_eq!(serde_json::to_value(&checkpoint).unwrap(), encoded);
        assert_eq!(live.commands()[1].seq, recovered.commands()[1].seq);
        assert_ne!(live.commands()[1].command, recovered.commands()[1].command);
        assert_eq!(checkpoint.into_parts().0.len(), 1);
    }

    #[test]
    fn events_after_matches_a_full_scan() {
        let mut log = EventLog::new();
        // Enough events to span several sealed history chunks.
        for id in 0..150 {
            log.record(
                Command::CancelOrder(CancelOrder { order_id: id }),
                vec![Event::OrderAccepted { order_id: id }; id as usize % 3],
            );
        }
        for seq in [0, 1, 2, 3, 4, 5, 31, 32, 33, 64, 148, 149, 150, 1000] {
            let scan: Vec<_> = log.events().iter().filter(|e| e.seq > seq).collect();
            assert_eq!(log.events_after(seq).collect::<Vec<_>>(), scan);
        }
        assert_eq!(log.events_iter().next_back(), log.events().last());
        assert_eq!(
            log.events_iter().rev().collect::<Vec<_>>(),
            log.events().iter().rev().collect::<Vec<_>>()
        );
        assert!(EventLog::new().events_after(0).next().is_none());
        assert!(EventLog::new().events_iter().next_back().is_none());
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventRecord {
    pub seq: LogSeq,
    pub command_seq: LogSeq,
    pub event: Event,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordedExecution {
    pub command: CommandRecord,
    pub events: Vec<EventRecord>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventLog {
    commands: History<CommandRecord>,
    events: History<EventRecord>,
    next_command_seq: LogSeq,
    next_event_seq: LogSeq,
}

impl EventLog {
    /// Research archive compaction only. Keep unread trigger events and the last
    /// event; sequence allocators must never rewind when old records move to disk.
    pub(crate) fn compact_archived(&mut self, first_event_seq: u64) {
        // Last-price protections also read the most recent trade even after
        // later order lifecycle events have advanced the event cursor.
        let last_trade = self.events.iter().rev().find_map(|record| {
            matches!(record.event, Event::TradePrinted(_)).then_some(record.seq)
        });
        self.commands = History::default();
        self.events = self
            .events
            .iter()
            .filter(|e| e.seq >= first_event_seq || Some(e.seq) == last_trade)
            .cloned()
            .collect::<Vec<_>>()
            .into();
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, command: Command, events: Vec<Event>) -> RecordedExecution {
        let command = CommandRecord {
            seq: self.take_command_seq(),
            command,
        };
        self.record_with_command(command, events)
    }

    pub fn record_with_command(
        &mut self,
        command: CommandRecord,
        events: Vec<Event>,
    ) -> RecordedExecution {
        self.next_command_seq = self.next_command_seq.max(command.seq + 1);

        let event_records = events
            .into_iter()
            .map(|event| EventRecord {
                seq: self.take_event_seq(),
                command_seq: command.seq,
                event,
            })
            .collect::<Vec<_>>();

        self.commands.push(command.clone());
        self.events.extend(event_records.clone());

        RecordedExecution {
            command,
            events: event_records,
        }
    }

    pub fn commands(&self) -> &[CommandRecord] {
        &self.commands
    }

    pub(crate) fn appended_commands<'a>(
        &'a self,
        previous: &Self,
    ) -> Option<impl Iterator<Item = &'a CommandRecord>> {
        (self.commands.extends(&previous.commands) && self.events.extends(&previous.events))
            .then(|| self.commands.iter_from(previous.commands.len()))
    }

    pub(crate) fn appended_events<'a>(
        &'a self,
        previous: &Self,
    ) -> impl Iterator<Item = &'a EventRecord> {
        self.events.iter_from(previous.events.len())
    }

    pub fn events(&self) -> &[EventRecord] {
        &self.events
    }

    /// Chunk-wise iteration. Unlike `events`, this never copies the whole log
    /// into a contiguous slice, which every append would invalidate again.
    pub(crate) fn events_iter(&self) -> impl DoubleEndedIterator<Item = &EventRecord> {
        self.events.iter()
    }

    /// Records strictly after `seq`. Event sequence numbers are assigned in
    /// append order, so this is a binary search rather than a full-log scan.
    pub(crate) fn events_after(
        &self,
        seq: LogSeq,
    ) -> impl DoubleEndedIterator<Item = &EventRecord> {
        self.events
            .iter_from(self.events.partition_point(|e| e.seq <= seq))
    }

    pub fn into_parts(self) -> (Vec<CommandRecord>, Vec<EventRecord>) {
        (self.commands.to_vec(), self.events.to_vec())
    }

    fn take_command_seq(&mut self) -> LogSeq {
        let seq = self.next_command_seq;
        self.next_command_seq += 1;
        seq
    }

    fn take_event_seq(&mut self) -> LogSeq {
        let seq = self.next_event_seq;
        self.next_event_seq += 1;
        seq
    }
}
