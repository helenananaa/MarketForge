//! Schema-4 durable append log for the exact ecosystem cohort. This is a replay
//! log, not a VM snapshot: cold recovery executes every original append batch.
//! Compressed immutable chunks avoid cloning a JSON tree per historical pass.
use super::*;

const KIND: &str = "marketforge.pine-replay/1";
const ARCHIVE_EVERY: usize = 8;
const MAX_DECODED_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn empty() -> Value {
    json!({"kind":KIND,"bars":0,"previous_time":null,"chunks":[],"tail_batches":[]})
}

fn pack(value: &Value) -> Result<String, BotError> {
    let bytes = serde_json::to_vec(value).map_err(io)?;
    if bytes.len() > MAX_DECODED_BYTES {
        return Err(error("replay chunk exceeds decoded size limit"));
    }
    let compressed = lz4_flex::compress_prepend_size(&bytes);
    let mut result = String::with_capacity(compressed.len() * 2);
    const HEX: &[u8] = b"0123456789abcdef";
    for byte in compressed {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(result)
}

fn unpack(value: &Value) -> Result<Value, BotError> {
    let encoded = text(value)?.as_bytes();
    if encoded.len() < 8 || encoded.len() % 2 != 0 || encoded.len() > MAX_DECODED_BYTES * 2 {
        return Err(error("invalid replay chunk encoding"));
    }
    let nibble = |v: u8| match v {
        b'0'..=b'9' => Ok(v - b'0'),
        b'a'..=b'f' => Ok(v - b'a' + 10),
        _ => Err(error("invalid replay chunk encoding")),
    };
    let mut compressed = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.chunks_exact(2) {
        compressed.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    let size = u32::from_le_bytes(compressed[..4].try_into().unwrap()) as usize;
    if size > MAX_DECODED_BYTES {
        return Err(error("replay chunk exceeds decoded size limit"));
    }
    let decoded = lz4_flex::decompress_size_prepended(&compressed).map_err(io)?;
    serde_json::from_slice(&decoded).map_err(io)
}

fn replay(
    runtime: &mut HistoricalRuntime<'static>,
    passes: &[Value],
    batches: &[Value],
    intents: &Value,
    deadline: Instant,
) -> Result<(), BotError> {
    let mut offset = 0usize;
    let mut actual = Vec::new();
    for batch in batches {
        let count = usize::try_from(number(batch, 1)?).map_err(io)?;
        let end = offset
            .checked_add(count)
            .filter(|end| *end <= passes.len())
            .ok_or_else(|| error("replay batch cursor mismatch"))?;
        actual.extend(checkpoint::append(
            runtime,
            &passes[offset..end],
            deadline,
            false,
        )?);
        offset = end;
    }
    if offset != passes.len() || checkpoint::freeze(Value::Array(actual)) != *intents {
        return Err(error("replay batch intents or cursor differ on recovery"));
    }
    Ok(())
}

pub(super) fn restore(
    runtime: &mut HistoricalRuntime<'static>,
    state: &Value,
    deadline: Instant,
) -> Result<(), BotError> {
    let archive = &state["vm_checkpoint"];
    if archive["kind"] != KIND {
        return Err(error("unsupported replay log version"));
    }
    let mut count = 0u64;
    let mut last_time = Value::Null;
    for encoded in array(&archive["chunks"])? {
        if Instant::now() >= deadline {
            return Err(error("Pine replay recovery timed out"));
        }
        let chunk = unpack(encoded)?;
        let passes = array(&chunk["passes"])?;
        if passes.is_empty() {
            return Err(error("empty replay chunk"));
        }
        replay(
            runtime,
            passes,
            array(&chunk["batches"])?,
            &chunk["intents"],
            deadline,
        )?;
        count = count
            .checked_add(passes.len() as u64)
            .ok_or_else(|| error("replay cursor overflow"))?;
        last_time = passes.last().unwrap()["bar"]["time"].clone();
    }
    if archive["bars"].as_u64() != Some(count) || archive["previous_time"] != last_time {
        return Err(error("replay archive cursor mismatch"));
    }
    let tail = array(&state["vm_tail"])?;
    replay(
        runtime,
        tail,
        array(&archive["tail_batches"])?,
        &state["vm_tail_intents"],
        deadline,
    )?;
    if state["evaluated_bars"].as_u64() != Some(count + tail.len() as u64) {
        return Err(error("replay evaluation cursor mismatch"));
    }
    Ok(())
}

pub(super) fn commit(
    state: &mut Value,
    passes: Vec<Value>,
    intents: Vec<Value>,
    count: u64,
) -> Result<(), BotError> {
    let appended = passes.len();
    let mut tail = take_array(state, "vm_tail")?;
    tail.extend(passes);
    let mut tail_intents = take_array(state, "vm_tail_intents")?;
    tail_intents.extend(intents.into_iter().map(checkpoint::freeze));
    let archive = &mut state["vm_checkpoint"];
    if archive["kind"] != KIND {
        return Err(error("unsupported replay log version"));
    }
    let mut batches = take_array(archive, "tail_batches")?;
    batches.push(json!(appended));
    if tail.len() >= ARCHIVE_EVERY {
        archive["previous_time"] = tail.last().unwrap()["bar"]["time"].clone();
        archive["bars"] = json!(count);
        let encoded = pack(&json!({"passes":tail,"batches":batches,"intents":tail_intents}))?;
        archive["chunks"]
            .as_array_mut()
            .ok_or_else(|| error("missing replay chunks"))?
            .push(json!(encoded));
        tail.clear();
        tail_intents.clear();
        batches.clear();
    }
    archive["tail_batches"] = json!(batches);
    state["vm_tail"] = json!(tail);
    state["vm_tail_intents"] = json!(tail_intents);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_roundtrip_frozen_floats_and_refuse_corruption_or_large_allocation() {
        let value = checkpoint::freeze(json!({"x":[-0.0,1e-200,1e20,"策略",null],"batches":[2]}));
        assert_eq!(unpack(&json!(pack(&value).unwrap())).unwrap(), value);
        for invalid in ["", "00", "ffffffff00", "gggggggg", "00000000ff", "0000000"] {
            assert!(unpack(&json!(invalid)).is_err(), "{invalid}");
        }
    }
}
