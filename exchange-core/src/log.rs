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

    pub fn events(&self) -> &[EventRecord] {
        &self.events
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
