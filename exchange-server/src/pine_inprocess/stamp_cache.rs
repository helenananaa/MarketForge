//! A private, complete byte cache for schema-3 VM reuse. It is neither a hash
//! nor durable state. Public execution seals still validate first; a mismatch
//! takes the original VM checkpoint/tail reconstruction path.
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};

#[derive(Default)]
pub(super) struct Stamp {
    bytes: Vec<u8>,
}

// Match the old stamp's sorted JSON object. Value indexing intentionally maps
// both absent and explicit-null fields to null, as the previous comparison did.
#[derive(Serialize)]
struct Payload<'a> {
    checkpoint: &'a Value,
    intents: &'a Value,
    tail: &'a Value,
}

fn payload(state: &Value) -> Payload<'_> {
    Payload {
        checkpoint: &state["vm_checkpoint"],
        intents: &state["vm_tail_intents"],
        tail: &state["vm_tail"],
    }
}

impl Stamp {
    pub(super) fn update(&mut self, state: &Value) -> Result<(), serde_json::Error> {
        self.bytes.clear();
        serde_json::to_writer(&mut self.bytes, &payload(state))
    }

    pub(super) fn matches(&self, state: &Value) -> bool {
        let mut writer = CompareWriter::new(&self.bytes);
        serde_json::to_writer(&mut writer, &payload(state)).is_ok() && writer.finished()
    }
}

struct CompareWriter<'a> {
    expected: &'a [u8],
    offset: usize,
    mismatched: bool,
}

impl<'a> CompareWriter<'a> {
    fn new(expected: &'a [u8]) -> Self {
        Self {
            expected,
            offset: 0,
            mismatched: false,
        }
    }

    fn finished(&self) -> bool {
        !self.mismatched && self.offset == self.expected.len()
    }
}

impl Write for CompareWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(end) = self.offset.checked_add(bytes.len()) {
            self.mismatched |= self.expected.get(self.offset..end) != Some(bytes);
            self.offset = end;
        } else {
            self.mismatched = true;
        }
        // Compare through the end without allocating an error for a mismatch.
        // Complete length equality prevents matching a prefix or trailing data.
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "stamp_cache_tests.rs"]
mod tests;
