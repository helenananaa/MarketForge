//! Lossless in-memory journal storage. No history is discarded or coalesced.
use super::{JournalError, JournalMutation};
use std::borrow::Cow;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum StoredMutation {
    Raw(Box<[u8]>),
    Lz4 { json_len: usize, bytes: Box<[u8]> },
}

impl StoredMutation {
    pub(super) fn new(json: Vec<u8>) -> Self {
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::JOURNAL_COMPRESS,
        );
        if json.len() >= 1024 {
            let compressed = lz4_flex::block::compress(&json);
            if compressed
                .len()
                .saturating_add(std::mem::size_of::<usize>())
                < json.len()
            {
                return Self::Lz4 {
                    json_len: json.len(),
                    bytes: compressed.into_boxed_slice(),
                };
            }
        }
        // serde_json grows a Vec geometrically. Keep only its initialized bytes.
        Self::Raw(json.into_boxed_slice())
    }

    pub(super) fn len(&self) -> usize {
        match self {
            Self::Raw(bytes) | Self::Lz4 { bytes, .. } => bytes.len(),
        }
    }

    pub(super) fn json_len(&self) -> usize {
        match self {
            Self::Raw(bytes) => bytes.len(),
            Self::Lz4 { json_len, .. } => *json_len,
        }
    }

    fn json(&self) -> Result<Cow<'_, [u8]>, JournalError> {
        match self {
            Self::Raw(bytes) => Ok(Cow::Borrowed(bytes)),
            Self::Lz4 { json_len, bytes } => {
                // Length is retained from our validated encoder, not parsed
                // from a user-supplied compressed header.
                let mut output = vec![0; *json_len];
                let written =
                    lz4_flex::block::decompress_into(bytes, &mut output).map_err(|error| {
                        JournalError::Recovery(format!("decode memory mutation: {error}"))
                    })?;
                if written != *json_len {
                    return Err(JournalError::Recovery(
                        "memory mutation length mismatch".into(),
                    ));
                }
                Ok(Cow::Owned(output))
            }
        }
    }

    pub(super) fn decode(&self) -> Result<JournalMutation, JournalError> {
        serde_json::from_slice(&self.json()?).map_err(JournalError::Serialize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_bytes_and_tight_storage_cover_raw_compressed_and_incompressible() {
        let mut random = Vec::new();
        let mut value = 0x1234_5678_u32;
        for _ in 0..4096 {
            value ^= value << 13;
            value ^= value >> 17;
            value ^= value << 5;
            random.push(value as u8);
        }
        let repetitive = "汉字\\\"🦀0123456789".repeat(400).into_bytes();
        for input in [
            Vec::new(),
            b"small payload".to_vec(),
            random,
            repetitive.clone(),
        ] {
            let stored = StoredMutation::new(input.clone());
            assert_eq!(&*stored.json().unwrap(), input.as_slice());
            assert_eq!(stored.json_len(), input.len());
            assert!(stored.len() <= input.len());
            assert_eq!(&*stored.clone().json().unwrap(), input.as_slice());
        }
        assert!(StoredMutation::new(repetitive).len() < 1024);
    }

    #[test]
    fn corrupt_or_truncated_memory_record_fails_recovery() {
        let input = b"0123456789".repeat(1000);
        let stored = StoredMutation::Lz4 {
            json_len: input.len() + 1,
            bytes: lz4_flex::block::compress(&input).into_boxed_slice(),
        };
        assert!(stored.decode().is_err());
        let stored = StoredMutation::Lz4 {
            json_len: 64,
            bytes: vec![0xff].into_boxed_slice(),
        };
        assert!(stored.decode().is_err());
    }
}
