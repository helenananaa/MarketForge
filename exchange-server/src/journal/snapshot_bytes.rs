//! Versioned, lossless snapshot transport. Legacy JSON bytes remain readable.
use super::JournalError;
use std::borrow::Cow;

const MAGIC: &[u8; 8] = b"\0MFSNP\x01\0";
const HEADER_LEN: usize = 16;
// A legacy PostgreSQL BYTEA JSON snapshot must fit its 1 GiB varlena limit.
// Preserve that original bound on decoded data as well as the stored block.
const MAX_JSON_LEN: usize = (1 << 30) - 1;

pub(super) fn encode(json: Vec<u8>) -> Vec<u8> {
    if (1024..=MAX_JSON_LEN).contains(&json.len()) {
        let compressed = lz4_flex::block::compress(&json);
        if compressed.len().saturating_add(HEADER_LEN) < json.len() {
            let mut bytes = Vec::with_capacity(HEADER_LEN + compressed.len());
            bytes.extend_from_slice(MAGIC);
            bytes.extend_from_slice(&(json.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&compressed);
            return bytes;
        }
    }
    json
}

pub(super) fn decode(bytes: &[u8]) -> Result<Cow<'_, [u8]>, JournalError> {
    // Valid legacy JSON cannot start with a zero byte.
    if bytes.first() != Some(&0) {
        return Ok(Cow::Borrowed(bytes));
    }
    if bytes.len() < HEADER_LEN || !bytes.starts_with(MAGIC) {
        return Err(JournalError::Recovery(
            "unsupported or truncated snapshot byte envelope".into(),
        ));
    }
    let length = u64::from_le_bytes(bytes[8..HEADER_LEN].try_into().expect("checked header"));
    let length = usize::try_from(length)
        .ok()
        .filter(|n| (1024..=MAX_JSON_LEN).contains(n))
        .ok_or_else(|| JournalError::Recovery("snapshot decoded length out of range".into()))?;
    let mut output = Vec::new();
    output.try_reserve_exact(length).map_err(|error| {
        JournalError::Recovery(format!("allocate snapshot decode buffer: {error}"))
    })?;
    output.resize(length, 0);
    let written = lz4_flex::block::decompress_into(&bytes[HEADER_LEN..], &mut output)
        .map_err(|error| JournalError::Recovery(format!("decode snapshot block: {error}")))?;
    if written != length {
        return Err(JournalError::Recovery(
            "snapshot decoded length mismatch".into(),
        ));
    }
    Ok(Cow::Owned(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_bytes_are_lossless_and_read_legacy_json() {
        let large = format!("{{\"text\":\"{}\"}}", "汉字🦀0123456789".repeat(1000)).into_bytes();
        for json in [b"{}".to_vec(), large.clone()] {
            assert_eq!(&*decode(&json).unwrap(), json);
            let bytes = encode(json.clone());
            assert_eq!(&*decode(&bytes).unwrap(), json);
            assert!(bytes.len() <= json.len());
        }
        assert!(encode(large).starts_with(MAGIC));
        let mut random = Vec::new();
        let mut value = 0x1234_5678_u32;
        for _ in 0..4096 {
            value ^= value << 13;
            value ^= value >> 17;
            value ^= value << 5;
            random.push(value as u8);
        }
        assert_eq!(encode(random.clone()), random);
    }

    #[test]
    fn snapshot_corruption_unknown_version_and_invalid_lengths_fail() {
        let valid = encode(b"0123456789".repeat(1000));
        for count in [1, 8, HEADER_LEN - 1, valid.len() - 1] {
            assert!(decode(&valid[..count]).is_err());
        }
        let mut bad = valid.clone();
        bad[6] = 2;
        assert!(decode(&bad).is_err());
        for length in [0_u64, 1023, 10_001, u64::MAX] {
            let mut bad = valid.clone();
            bad[8..HEADER_LEN].copy_from_slice(&length.to_le_bytes());
            assert!(decode(&bad).is_err());
        }
    }
}
