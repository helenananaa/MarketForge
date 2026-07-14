use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::Path,
};

use serde::{Serialize, de::DeserializeOwned};

use crate::log::{CommandRecord, EventRecord};

pub fn write_command_log_jsonl(
    path: impl AsRef<Path>,
    commands: &[CommandRecord],
) -> io::Result<()> {
    write_jsonl(path, commands)
}

pub fn read_command_log_jsonl(path: impl AsRef<Path>) -> io::Result<Vec<CommandRecord>> {
    read_jsonl(path)
}

pub fn write_event_log_jsonl(path: impl AsRef<Path>, events: &[EventRecord]) -> io::Result<()> {
    write_jsonl(path, events)
}

pub fn read_event_log_jsonl(path: impl AsRef<Path>) -> io::Result<Vec<EventRecord>> {
    read_jsonl(path)
}

fn write_jsonl<T>(path: impl AsRef<Path>, records: &[T]) -> io::Result<()>
where
    T: Serialize,
{
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    for record in records {
        serde_json::to_writer(&mut writer, record).map_err(io::Error::other)?;
        writer.write_all(b"\n")?;
    }

    writer.flush()
}

fn read_jsonl<T>(path: impl AsRef<Path>) -> io::Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(&line).map_err(io::Error::other)?);
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{
        model::{CancelOrder, Command, NewOrder, OrderKind, Side},
        replay::{LoggedOrderBook, ReplayEngine},
    };

    fn limit(order_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
            reduce_only: false,
        })
    }

    fn market(order_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id: order_id + 1_000,
            side,
            kind: OrderKind::Market,
            qty,
            reduce_only: false,
        })
    }

    fn temp_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "marketforge-{name}-{}-{nanos}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn command_log_round_trips_through_jsonl_and_replays() {
        let mut book = LoggedOrderBook::new();
        book.apply(limit(1, Side::Buy, 100, 10));
        book.apply(limit(2, Side::Sell, 100, 4));
        book.apply(limit(3, Side::Sell, 101, 8));
        book.apply(market(4, Side::Buy, 10));
        book.apply(Command::CancelOrder(CancelOrder { order_id: 1 }));

        let path = temp_file("commands");
        write_command_log_jsonl(&path, book.command_log()).expect("command log should be writable");
        let restored_commands =
            read_command_log_jsonl(&path).expect("command log should be readable");
        fs::remove_file(&path).expect("temp command log should be removable");

        let replay = ReplayEngine::replay(&restored_commands);

        assert_eq!(restored_commands, book.command_log());
        assert_eq!(replay.events, book.event_log());
        assert_eq!(replay.final_snapshot, book.snapshot());
    }

    #[test]
    fn event_log_round_trips_through_jsonl() {
        let mut book = LoggedOrderBook::new();
        book.apply(limit(1, Side::Sell, 100, 3));
        book.apply(market(2, Side::Buy, 5));

        let path = temp_file("events");
        write_event_log_jsonl(&path, book.event_log()).expect("event log should be writable");
        let restored_events = read_event_log_jsonl(&path).expect("event log should be readable");
        fs::remove_file(&path).expect("temp event log should be removable");

        assert_eq!(restored_events, book.event_log());
    }

    #[test]
    fn empty_lines_are_ignored_when_reading_jsonl() {
        let path = temp_file("blank-lines");
        fs::write(
            &path,
            r#"
{"seq":0,"command":{"NewOrder":{"order_id":1,"account_id":1001,"side":"Buy","kind":{"Limit":{"price_tick":100}},"qty":10}}}

"#,
        )
        .expect("temp command log should be writable");

        let restored_commands =
            read_command_log_jsonl(&path).expect("command log should tolerate blank lines");
        fs::remove_file(&path).expect("temp command log should be removable");

        assert_eq!(restored_commands.len(), 1);
        assert_eq!(restored_commands[0].seq, 0);
    }
}
